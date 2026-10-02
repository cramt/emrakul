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
    apps::{self, App, AppId, Argv, Quit},
    gamepad::PadReader,
    state::Emrakul,
};

/// How long an app gets to end on its own before the next, blunter step.
const GRACE: Duration = Duration::from_secs(5);

pub enum Session {
    Home(Home),
    Running(Running),
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

pub struct Running {
    pub id: AppId,
    quit: Quit,
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
            Session::Running(_) => None,
        }
    }

    /// Whether keys belong to Home rather than a client.
    pub fn home_has_keyboard(&self) -> bool {
        self.home().is_some() && self.space.elements().next().is_none()
    }

    /// Who reads the controller right now. Every app is a web app until
    /// Game entries exist; a running Game is where this returns
    /// [`PadReader::App`], and [`Self::restack`] applies the change.
    pub fn pad_reader(&self) -> PadReader {
        PadReader::Emrakul
    }

    pub fn on_home_key(&mut self, key: HomeKey) {
        let (home, can_launch) = match &mut self.session {
            Session::Home(home) => (home, true),
            Session::Ending(_, home) => (home, false),
            Session::Running(_) => return,
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
        self.session = Session::Running(Running {
            id: app.id,
            quit: app.quit,
            pid,
        });
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
            Session::Running(running) => running,
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
            Session::Running(running) if running.pid == pid => self.enter_home(),
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
