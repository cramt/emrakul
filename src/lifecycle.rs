//! One app at a time: Home launches it, going Home ends it, and its exit,
//! however it happens, brings Home back.

use std::{
    os::fd::{AsFd, OwnedFd},
    os::unix::process::CommandExt,
    process::Command,
    time::Duration,
};

use anyhow::Context;
use rustix::process::{Pid, PidfdFlags, Signal, WaitId, WaitIdOptions, pidfd_open, waitid};
use smithay::reexports::calloop::{
    Interest, Mode, PostAction,
    generic::Generic,
    timer::{TimeoutAction, Timer},
};

use crate::{
    apps::{self, App, AppId, Argv, Back, Quit},
    gamepad::PadReader,
    osk::OnScreenKeyboard,
    state::Emrakul,
    tv::Showing,
};

/// How long an app gets to end on its own before the next, blunter step.
const GRACE: Duration = Duration::from_secs(5);

/// How often Home reads the app list again while it is on screen. Entries
/// can appear as the session runs (nixconf's emrakul-games writes a Game per
/// app a paired gaming desktop lists), not only between apps. Polled rather
/// than watched: a data dir may not exist until its first entry does, and
/// reading a dozen small files every few seconds costs nothing.
const RESCAN: Duration = Duration::from_secs(3);

pub enum Session {
    Home(Home),
    /// The app, and the on-screen keyboard over it while open.
    Running(Running, Option<OnScreenKeyboard>),
    /// Going Home asked the app to end. Home is on screen and focus moves,
    /// but nothing launches until the app has actually gone: one app at a
    /// time.
    Ending(Running, Home),
}

#[derive(Default)]
pub struct Home {
    /// Most recently launched first.
    pub apps: Vec<App>,
    /// Index into `apps`. Wraps, so it is only out of range when `apps` is
    /// empty, and `apps.get` covers that.
    pub focus: usize,
}

impl Home {
    /// This Home with `apps` in place of its own, focus staying on the app
    /// it was on (the first, if that one went). `None` if nothing changed.
    fn replaced_by(&self, apps: Vec<App>) -> Option<Self> {
        if apps == self.apps {
            return None;
        }
        let focused = self.apps.get(self.focus).map(|app| &app.id);
        let focus = focused
            .and_then(|id| apps.iter().position(|app| app.id == *id))
            .unwrap_or(0);
        Some(Self { apps, focus })
    }
}

pub struct Running {
    pub id: AppId,
    quit: Quit,
    tv_profile: Option<String>,
    pub back: Back,
    pad_reader: PadReader,
    /// Also its process group, so a signal reaches whatever it forked.
    pid: Pid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeKey {
    Previous,
    Next,
    Launch,
}

impl Emrakul {
    pub fn enter_home(&mut self) {
        self.session = Session::Home(self.discover_home());
        self.restack();
    }

    /// Reads the app list again every [`RESCAN`] while Home is up, and shows
    /// it if it changed.
    pub fn rescan_home_while_shown(&mut self) {
        let timer = self
            .loop_handle
            .insert_source(Timer::from_duration(RESCAN), |_, _, state| {
                state.rescan_home();
                TimeoutAction::ToDuration(RESCAN)
            });
        if let Err(err) = timer {
            tracing::error!("scheduling Home's rescan: {err}");
        }
    }

    fn rescan_home(&mut self) {
        let Session::Home(home) = &self.session else {
            return;
        };
        let apps = self.recency.order(apps::discover(&apps::data_dirs()));
        let Some(home) = home.replaced_by(apps) else {
            return;
        };
        tracing::info!(
            count = home.apps.len(),
            first = ?home.apps.iter().take(5).map(|a| &a.name).collect::<Vec<_>>(),
            "Home changed"
        );
        self.home_view.forget_apps();
        self.session = Session::Home(home);
        self.backend.request_redraw(&self.loop_handle);
    }

    /// Every app, in Home's order. Focus starts on the first: the app just
    /// quit, since launching it made it the most recent.
    fn discover_home(&mut self) -> Home {
        let apps = self.recency.order(apps::discover(&apps::data_dirs()));
        tracing::info!(
            count = apps.len(),
            first = ?apps.iter().take(5).map(|a| &a.name).collect::<Vec<_>>(),
            "Home"
        );
        // Icons and brands may have changed with the list.
        self.home_view.forget_apps();
        Home { apps, focus: 0 }
    }

    /// The Home on screen, if one is.
    pub fn home(&self) -> Option<&Home> {
        match &self.session {
            Session::Home(home) | Session::Ending(_, home) => Some(home),
            Session::Running(..) => None,
        }
    }

    /// What the TV's settings should be for.
    pub fn showing(&self) -> Showing<'_> {
        match &self.session {
            Session::Home(_) | Session::Ending(..) => Showing::Home,
            Session::Running(running, _) => Showing::App {
                profile: running.tv_profile.as_deref(),
            },
        }
    }

    /// Whether keys belong to Home rather than a client.
    pub fn home_has_keyboard(&self) -> bool {
        self.home().is_some() && self.space.elements().next().is_none()
    }

    /// Who reads the controller right now: the running app if its entry
    /// says so (a Game), emrakul otherwise. Home is emrakul's as soon as
    /// going Home starts, while the app is still ending.
    /// [`Self::restack`] applies a change.
    pub fn pad_reader(&self) -> PadReader {
        match &self.session {
            Session::Running(running, _) => running.pad_reader,
            Session::Home(_) | Session::Ending(..) => PadReader::Emrakul,
        }
    }

    pub fn on_home_key(&mut self, key: HomeKey) {
        let (home, can_launch) = match &mut self.session {
            Session::Home(home) => (home, true),
            Session::Ending(_, home) => (home, false),
            Session::Running(..) => return,
        };
        let count = home.apps.len().max(1);
        match key {
            HomeKey::Previous => home.focus = (home.focus + count - 1) % count,
            HomeKey::Next => home.focus = (home.focus + 1) % count,
            HomeKey::Launch => {
                if can_launch && let Some(app) = home.apps.get(home.focus).cloned() {
                    self.launch(app);
                }
                return;
            }
        }
        tracing::debug!(app = ?home.apps.get(home.focus).map(|a| &a.name), "Home focus");
        self.backend.request_redraw(&self.loop_handle);
    }

    fn launch(&mut self, app: App) {
        tracing::info!(app = %app.id, exec = ?app.exec, "launching");
        let pid = match self.spawn_app(&app) {
            Ok(pid) => pid,
            Err(err) => {
                tracing::error!("launching {}: {err:#}", app.id);
                return;
            }
        };
        if let Err(err) = self.recency.touch(&app.id) {
            tracing::warn!("{err:#}");
        }
        if let (Some(tv), Some(profile)) = (&self.tv, &app.tv_profile)
            && !tv.layers().knows(profile)
        {
            tracing::warn!(app = %app.id, profile, "no such TV profile, using the app one");
        }
        self.session = Session::Running(
            Running {
                id: app.id,
                quit: app.quit,
                tv_profile: app.tv_profile,
                back: app.back,
                pad_reader: app.pad_reader,
                pid,
            },
            None,
        );
        self.restack();
    }

    fn spawn_app(&mut self, app: &App) -> anyhow::Result<Pid> {
        let child = self
            .command(&app.exec)
            .process_group(0)
            .spawn()
            .with_context(|| format!("starting {}", app.exec.program))?;
        let pid = Pid::from_child(&child);
        // A pidfd can't be confused with a reused PID, and it polls readable
        // once the process exits, so exit needs no thread and no SIGCHLD.
        let pidfd = pidfd_open(pid, PidfdFlags::empty()).context("pidfd_open")?;
        self.loop_handle
            .insert_source(
                Generic::new(pidfd, Interest::READ, Mode::Level),
                move |_, pidfd, state| {
                    state.on_app_exit(pid, pidfd);
                    Ok(PostAction::Remove)
                },
            )
            .map_err(|e| anyhow::anyhow!("watching the app's exit: {e}"))?;
        Ok(pid)
    }

    /// Goes Home from an app: Home shows at once, and the app is asked to end.
    pub fn go_home(&mut self) {
        let running = match std::mem::replace(&mut self.session, Session::Home(Home::default())) {
            Session::Running(running, _) => running,
            other => {
                self.session = other;
                return;
            }
        };
        tracing::info!(app = %running.id, quit = ?running.quit, "going Home");
        match &running.quit {
            Quit::Close => {
                for toplevel in self.toplevels.iter().filter_map(|w| w.toplevel()) {
                    toplevel.send_close();
                }
            }
            Quit::Run(argv) => self.run_detached(argv),
        }
        self.escalate_after_grace(running.pid, Signal::TERM);
        self.session = Session::Ending(running, self.discover_home());
        self.restack();
    }

    fn escalate_after_grace(&self, pid: Pid, signal: Signal) {
        let timer =
            self.loop_handle
                .insert_source(Timer::from_duration(GRACE), move |_, _, state| {
                    let Session::Ending(running, _) = &state.session else {
                        return TimeoutAction::Drop;
                    };
                    if running.pid != pid {
                        return TimeoutAction::Drop;
                    }
                    tracing::warn!(app = %running.id, ?signal, "app is lingering, signalling it");
                    // Its own exit can't have been reaped yet (that ends Ending),
                    // so the group ID still names it.
                    if let Err(err) = rustix::process::kill_process_group(pid, signal) {
                        tracing::warn!(?err, "signalling the app");
                    }
                    if signal != Signal::KILL {
                        state.escalate_after_grace(pid, Signal::KILL);
                    }
                    TimeoutAction::Drop
                });
        if let Err(err) = timer {
            tracing::error!("scheduling the app's end: {err}");
        }
    }

    fn on_app_exit(&mut self, pid: Pid, pidfd: &OwnedFd) {
        let status = waitid(WaitId::PidFd(pidfd.as_fd()), WaitIdOptions::EXITED);
        tracing::info!(?pid, ?status, "app exited");
        match std::mem::replace(&mut self.session, Session::Home(Home::default())) {
            // Home is already up, and focus may have moved while it ended.
            Session::Ending(running, home) if running.pid == pid => {
                self.session = Session::Home(home);
                self.restack();
            }
            Session::Running(running, _) if running.pid == pid => self.enter_home(),
            other => self.session = other,
        }
    }

    pub fn command(&self, argv: &Argv) -> Command {
        let mut command = Command::new(&argv.program);
        command
            .args(&argv.args)
            .env("WAYLAND_DISPLAY", &self.socket_name)
            .env("XDG_SESSION_TYPE", "wayland")
            .env_remove("DISPLAY");
        command
    }

    /// Starts something that isn't an app, such as an app's quit command.
    pub fn run_detached(&self, argv: &Argv) {
        match self.command(argv).spawn() {
            // Reaped on its own thread so it never lingers as a zombie.
            Ok(mut child) => {
                std::thread::spawn(move || child.wait());
            }
            Err(err) => tracing::error!("starting {}: {err}", argv.program),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home(ids: &[&str], focus: usize) -> Home {
        let app = |id: &&str| App {
            id: AppId::new(*id),
            name: id.to_string(),
            exec: Argv::from_vec(vec![id.to_string()]).unwrap(),
            quit: Quit::Close,
            icon: None,
            declared: true,
            brand: None,
            tv_profile: None,
            back: Back::default(),
            pad_reader: PadReader::Emrakul,
        };
        Home {
            apps: ids.iter().map(app).collect(),
            focus,
        }
    }

    fn ids(home: &Home) -> (Vec<&str>, usize) {
        (
            home.apps.iter().map(|a| a.id.as_str()).collect(),
            home.focus,
        )
    }

    #[test]
    fn a_rescan_keeps_focus_on_the_same_app() {
        let before = home(&["youtube", "jellyfin"], 1);
        assert!(
            before
                .replaced_by(home(&["youtube", "jellyfin"], 0).apps)
                .is_none()
        );

        let grew = before
            .replaced_by(home(&["youtube", "desktop", "jellyfin"], 0).apps)
            .unwrap();
        assert_eq!(ids(&grew), (vec!["youtube", "desktop", "jellyfin"], 2));

        let lost_focused = before.replaced_by(home(&["youtube"], 0).apps).unwrap();
        assert_eq!(ids(&lost_focused), (vec!["youtube"], 0));

        let was_empty = Home::default()
            .replaced_by(home(&["desktop"], 0).apps)
            .unwrap();
        assert_eq!(ids(&was_empty), (vec!["desktop"], 0));
    }
}
