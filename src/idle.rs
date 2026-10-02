//! Idle and Blanked: when the screen switches off, and what brings it back.
//!
//! Time comes in from outside, so the whole policy is testable without a
//! clock or a screen. The compositor feeds it activity and inhibitors, polls
//! it from a timer, and switches the screen off when it says so.

use std::time::{Duration, Instant};

use smithay::{
    delegate_idle_inhibit,
    reexports::{
        calloop::timer::{TimeoutAction, Timer},
        wayland_server::{Resource, protocol::wl_surface::WlSurface},
    },
    wayland::idle_inhibit::IdleInhibitHandler,
};

use crate::state::Emrakul;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Screen {
    /// Showing. Idle once `since + timeout` passes with nothing inhibiting.
    Awake {
        since: Instant,
    },
    Blanked,
}

/// `S` is whatever names an inhibitor: a surface in the compositor.
pub struct Idle<S> {
    timeout: Duration,
    screen: Screen,
    /// The surface of every live inhibitor; one surface can appear more
    /// than once. Any of them is enough to hold idle off.
    inhibitors: Vec<S>,
}

/// What an input event did besides being activity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    /// It woke the screen, and does nothing else.
    Woke,
    /// The screen was on: the event means whatever it normally means.
    Passed,
}

/// What a timer poll found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Poll {
    /// Idle just now: switch the screen off.
    Blank,
    /// Not yet. Poll again then.
    At(Instant),
    /// Blanked, or inhibited: nothing happens until activity or the last
    /// inhibitor goes, so stop polling.
    Stop,
}

impl<S: PartialEq> Idle<S> {
    pub fn new(timeout: Duration, now: Instant) -> Self {
        Self {
            timeout,
            screen: Screen::Awake { since: now },
            inhibitors: Vec::new(),
        }
    }

    pub fn is_blanked(&self) -> bool {
        self.screen == Screen::Blanked
    }

    pub fn activity(&mut self, now: Instant) -> Activity {
        let woke = self.is_blanked();
        self.screen = Screen::Awake { since: now };
        if woke {
            Activity::Woke
        } else {
            Activity::Passed
        }
    }

    pub fn inhibit(&mut self, surface: S) {
        self.inhibitors.push(surface);
    }

    /// One inhibitor on `surface` went. A surface can hold several.
    pub fn uninhibit(&mut self, surface: &S, now: Instant) {
        if let Some(i) = self.inhibitors.iter().position(|s| s == surface) {
            self.inhibitors.swap_remove(i);
            self.released(now);
        }
    }

    /// `surface` was destroyed, and every inhibitor on it with it: a client
    /// killed mid-video never destroys its inhibitors itself. Any surface
    /// may be passed; one that held none changes nothing, so a closing
    /// tooltip doesn't restart the countdown.
    pub fn surface_gone(&mut self, surface: &S, now: Instant) {
        let before = self.inhibitors.len();
        self.inhibitors.retain(|s| s != surface);
        if self.inhibitors.len() < before {
            self.released(now);
        }
    }

    /// The countdown restarts when the last inhibitor goes: a video ending
    /// gets the full timeout before the screen goes off, not whatever was
    /// left from the last button press.
    fn released(&mut self, now: Instant) {
        if let Screen::Awake { since } = &mut self.screen
            && self.inhibitors.is_empty()
        {
            *since = now;
        }
    }

    pub fn poll(&mut self, now: Instant) -> Poll {
        let Screen::Awake { since } = self.screen else {
            return Poll::Stop;
        };
        if !self.inhibitors.is_empty() {
            return Poll::Stop;
        }
        let deadline = since + self.timeout;
        if now < deadline {
            return Poll::At(deadline);
        }
        self.screen = Screen::Blanked;
        Poll::Blank
    }
}

impl Emrakul {
    /// Someone is at the TV. Wakes the screen if it was Blanked.
    pub fn on_activity(&mut self) -> Activity {
        let activity = self.idle.activity(Instant::now());
        if activity == Activity::Woke {
            tracing::info!("woke");
            self.backend.wake();
            self.backend.request_redraw(&self.loop_handle);
            self.arm_idle_timer();
        }
        activity
    }

    /// Starts polling for Idle, unless a poll is already pending.
    pub fn arm_idle_timer(&mut self) {
        if self.idle_timer.is_some() {
            return;
        }
        let timer =
            self.loop_handle
                .insert_source(
                    Timer::from_deadline(Instant::now()),
                    |_, _, state| match state.idle.poll(Instant::now()) {
                        Poll::At(deadline) => TimeoutAction::ToInstant(deadline),
                        Poll::Blank => {
                            tracing::info!("idle, blanking");
                            state.backend.blank();
                            state.idle_timer = None;
                            TimeoutAction::Drop
                        }
                        Poll::Stop => {
                            state.idle_timer = None;
                            TimeoutAction::Drop
                        }
                    },
                );
        match timer {
            Ok(token) => self.idle_timer = Some(token),
            Err(err) => tracing::error!("scheduling the idle timer: {err}"),
        }
    }
}

impl IdleInhibitHandler for Emrakul {
    // Any client's inhibit counts. Games don't hold the screen on because
    // their Moonlight runs with --no-keep-awake, which never asks.
    fn inhibit(&mut self, surface: WlSurface) {
        tracing::debug!(surface = ?surface.id(), "idle inhibited");
        self.idle.inhibit(surface);
    }

    fn uninhibit(&mut self, surface: WlSurface) {
        tracing::debug!(surface = ?surface.id(), "idle uninhibited");
        self.idle.uninhibit(&surface, Instant::now());
        self.arm_idle_timer();
    }
}

delegate_idle_inhibit!(Emrakul);

#[cfg(test)]
mod tests {
    use super::*;

    const TIMEOUT: Duration = Duration::from_secs(600);

    fn secs(start: Instant, s: u64) -> Instant {
        start + Duration::from_secs(s)
    }

    fn idle() -> (Idle<&'static str>, Instant) {
        let start = Instant::now();
        (Idle::new(TIMEOUT, start), start)
    }

    #[test]
    fn blanks_once_the_timeout_passes_without_activity() {
        let (mut idle, t) = idle();
        assert_eq!(idle.poll(secs(t, 1)), Poll::At(secs(t, 600)));
        assert_eq!(idle.poll(secs(t, 599)), Poll::At(secs(t, 600)));
        assert_eq!(idle.poll(secs(t, 600)), Poll::Blank);
        assert!(idle.is_blanked());
        assert_eq!(idle.poll(secs(t, 900)), Poll::Stop);
    }

    #[test]
    fn activity_pushes_the_deadline_back() {
        let (mut idle, t) = idle();
        assert_eq!(idle.activity(secs(t, 300)), Activity::Passed);
        assert_eq!(idle.poll(secs(t, 600)), Poll::At(secs(t, 900)));
        assert_eq!(idle.poll(secs(t, 900)), Poll::Blank);
    }

    #[test]
    fn activity_while_blanked_wakes_and_does_nothing_else() {
        let (mut idle, t) = idle();
        idle.poll(secs(t, 600));
        assert_eq!(idle.activity(secs(t, 1000)), Activity::Woke);
        assert!(!idle.is_blanked());
        // The next press is an ordinary one again.
        assert_eq!(idle.activity(secs(t, 1001)), Activity::Passed);
        assert_eq!(idle.poll(secs(t, 1001)), Poll::At(secs(t, 1601)));
    }

    #[test]
    fn an_inhibitor_holds_idle_off_however_long_it_lasts() {
        let (mut idle, t) = idle();
        idle.inhibit("video");
        assert_eq!(idle.poll(secs(t, 600)), Poll::Stop);
        assert_eq!(idle.poll(secs(t, 7200)), Poll::Stop);
        assert!(!idle.is_blanked());
    }

    #[test]
    fn the_countdown_restarts_when_the_last_inhibitor_goes() {
        let (mut idle, t) = idle();
        idle.inhibit("video");
        idle.inhibit("other");
        idle.uninhibit(&"video", secs(t, 3000));
        assert_eq!(idle.poll(secs(t, 3000)), Poll::Stop);
        idle.uninhibit(&"other", secs(t, 4000));
        assert_eq!(idle.poll(secs(t, 4000)), Poll::At(secs(t, 4600)));
        assert_eq!(idle.poll(secs(t, 4600)), Poll::Blank);
    }

    #[test]
    fn a_surface_whose_inhibitor_is_replaced_stays_inhibited() {
        // What Chromium does as a video starts: a second inhibitor on the
        // same surface, then the first one destroyed.
        let (mut idle, t) = idle();
        idle.inhibit("video");
        idle.inhibit("video");
        idle.uninhibit(&"video", secs(t, 1));
        assert_eq!(idle.poll(secs(t, 600)), Poll::Stop);
        idle.uninhibit(&"video", secs(t, 700));
        assert_eq!(idle.poll(secs(t, 700)), Poll::At(secs(t, 1300)));
    }

    #[test]
    fn a_destroyed_surface_takes_all_its_inhibitors_with_it() {
        let (mut idle, t) = idle();
        idle.inhibit("video");
        idle.inhibit("video");
        idle.surface_gone(&"video", secs(t, 100));
        assert_eq!(idle.poll(secs(t, 100)), Poll::At(secs(t, 700)));
        // The client's own destroy requests can still trail in.
        idle.uninhibit(&"video", secs(t, 200));
        assert_eq!(idle.poll(secs(t, 200)), Poll::At(secs(t, 700)));
    }

    #[test]
    fn a_surface_that_never_inhibited_going_changes_nothing() {
        let (mut idle, t) = idle();
        idle.surface_gone(&"tooltip", secs(t, 500));
        assert_eq!(idle.poll(secs(t, 500)), Poll::At(secs(t, 600)));
    }

    #[test]
    fn an_inhibitor_appearing_while_blanked_does_not_wake() {
        let (mut idle, t) = idle();
        idle.poll(secs(t, 600));
        idle.inhibit("video");
        idle.uninhibit(&"video", secs(t, 700));
        assert!(idle.is_blanked());
        assert_eq!(idle.activity(secs(t, 800)), Activity::Woke);
    }
}
