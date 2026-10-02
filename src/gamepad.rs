//! Controllers, read straight from their evdev nodes. libinput ignores
//! joysticks, so it never sees them.
//!
//! The Steam button goes Home from anywhere. On Home, a minimal stand-in
//! moves the focus (D-pad, or the left stick past its deadzone) and launches
//! (A). The full controller table is still to be decided; [`Pad::on_event`]
//! is the one place it would replace.

use std::{
    os::fd::OwnedFd,
    path::{Path, PathBuf},
};

use evdev::{AbsoluteAxisCode, EventSummary, InputEvent, KeyCode};
use smithay::reexports::{
    calloop::{Interest, Mode, PostAction, RegistrationToken, generic::Generic},
    udev,
};

use crate::{idle::Activity, lifecycle::HomeKey, state::Emrakul};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PadAction {
    GoHome,
    Home(HomeKey),
}

/// How far a stick axis must lean, as a fraction of its half-range, to count
/// as a push. The Steam Controller's sticks wander by about 1.5% at rest.
const PUSH: f32 = 0.5;
/// How far back it must come before the next push counts.
const RECENTRE: f32 = 0.25;
/// How far a stick must lean, or a trigger be pulled, to count as activity:
/// far enough that drift at rest never holds the screen on.
const ACTIVE: f32 = 0.25;

/// One controller's view of the stand-in mapping. Sticks only step the focus
/// on the way out past [`PUSH`], so holding one steps once.
#[derive(Debug)]
pub struct Pad {
    x: Axis,
    y: Axis,
    /// Every absolute axis the controller reports, for telling activity
    /// from drift.
    ranges: Vec<(AbsoluteAxisCode, AxisRange)>,
}

#[derive(Debug, Clone, Copy)]
pub struct AxisRange {
    pub min: i32,
    pub max: i32,
}

#[derive(Debug)]
struct Axis {
    range: AxisRange,
    pushed: bool,
}

impl Axis {
    fn new(range: AxisRange) -> Self {
        Self {
            range,
            pushed: false,
        }
    }

    /// The focus step this value makes, if it is a fresh push. Negative
    /// (left, or up: evdev's Y grows downwards) is Previous.
    fn step(&mut self, value: i32) -> Option<HomeKey> {
        let lean = self.range.lean(value);
        if self.pushed {
            self.pushed = lean.abs() > RECENTRE;
            return None;
        }
        if lean.abs() < PUSH {
            return None;
        }
        self.pushed = true;
        Some(if lean < 0.0 {
            HomeKey::Previous
        } else {
            HomeKey::Next
        })
    }
}

fn range_of(ranges: &[(AbsoluteAxisCode, AxisRange)], code: AbsoluteAxisCode) -> Option<AxisRange> {
    ranges.iter().find(|(c, _)| *c == code).map(|(_, r)| *r)
}

impl AxisRange {
    /// How far `value` is from the middle, as a fraction of the half-range.
    fn lean(self, value: i32) -> f32 {
        let half = (self.max - self.min) as f32 / 2.0;
        if half <= 0.0 {
            return 0.0;
        }
        (value as f32 - (self.min as f32 + half)) / half
    }

    /// How far `value` is from the minimum, as a fraction of the range.
    fn pull(self, value: i32) -> f32 {
        let range = (self.max - self.min) as f32;
        if range <= 0.0 {
            return 0.0;
        }
        (value - self.min) as f32 / range
    }
}

impl Pad {
    pub fn new(ranges: &[(AbsoluteAxisCode, AxisRange)]) -> Self {
        let range = |code| range_of(ranges, code).unwrap_or(AxisRange { min: 0, max: 0 });
        Self {
            x: Axis::new(range(AbsoluteAxisCode::ABS_X)),
            y: Axis::new(range(AbsoluteAxisCode::ABS_Y)),
            ranges: ranges.to_vec(),
        }
    }

    /// Whether this event is someone at the controller, per Idle: a button
    /// press, a stick past its deadzone, a trigger pulled, or a trackpad
    /// touch. Gyro is on another node, which is never opened.
    pub fn is_activity(&self, event: &InputEvent) -> bool {
        use AbsoluteAxisCode as A;
        let (code, value) = match event.destructure() {
            EventSummary::Key(_, _, value) => return value != 0,
            EventSummary::AbsoluteAxis(_, code, value) => (code, value),
            _ => return false,
        };
        let range = || range_of(&self.ranges, code);
        match code {
            // The Steam Controller's trackpads, which say nothing until
            // touched. On other pads these are the D-pad, equally quiet.
            A::ABS_HAT0X | A::ABS_HAT0Y | A::ABS_HAT1X | A::ABS_HAT1Y => true,
            A::ABS_X | A::ABS_Y | A::ABS_RX | A::ABS_RY => {
                range().is_some_and(|r| r.lean(value).abs() > ACTIVE)
            }
            // Analog triggers: the Steam Controller's on HAT2, the usual
            // gamepad's on Z/RZ. They rest at the minimum.
            A::ABS_HAT2X | A::ABS_HAT2Y | A::ABS_Z | A::ABS_RZ => {
                range().is_some_and(|r| r.pull(value) > ACTIVE)
            }
            _ => false,
        }
    }

    /// What this event does. `on_home` is whether Home is in front; stick
    /// state is tracked either way, so a stick held while an app ends doesn't
    /// step the focus the moment Home appears.
    pub fn on_event(&mut self, event: InputEvent, on_home: bool) -> Option<PadAction> {
        let home_key = match event.destructure() {
            // Presses only: releases and autorepeat (2) do nothing.
            EventSummary::Key(_, key, 1) => match key {
                KeyCode::BTN_MODE => return Some(PadAction::GoHome),
                KeyCode::BTN_DPAD_UP | KeyCode::BTN_DPAD_LEFT => Some(HomeKey::Previous),
                KeyCode::BTN_DPAD_DOWN | KeyCode::BTN_DPAD_RIGHT => Some(HomeKey::Next),
                KeyCode::BTN_SOUTH => Some(HomeKey::Launch),
                _ => None,
            },
            EventSummary::AbsoluteAxis(_, AbsoluteAxisCode::ABS_X, value) => self.x.step(value),
            EventSummary::AbsoluteAxis(_, AbsoluteAxisCode::ABS_Y, value) => self.y.step(value),
            _ => None,
        };
        home_key.filter(|_| on_home).map(PadAction::Home)
    }
}

/// Controllers open right now. The gamepad node only exists while the
/// controller's wireless link is up, so these come and go with udev.
#[derive(Default)]
pub struct Gamepads(Vec<Gamepad>);

struct Gamepad {
    node: PathBuf,
    /// What the session handed out, kept to hand back on close.
    session_fd: OwnedFd,
    device: evdev::Device,
    pad: Pad,
    source: RegistrationToken,
}

impl Emrakul {
    /// Open the controllers already plugged in, and follow udev for the rest.
    pub fn watch_gamepads(&mut self) -> anyhow::Result<()> {
        let monitor = udev::MonitorBuilder::new()?
            .match_subsystem("input")?
            .listen()?;
        self.loop_handle
            .insert_source(
                Generic::new(monitor, Interest::READ, Mode::Level),
                |_, monitor, state| {
                    for event in monitor.iter() {
                        match event.event_type() {
                            udev::EventType::Add => state.open_gamepad(&event),
                            udev::EventType::Remove => {
                                if let Some(node) = event.devnode() {
                                    state.close_gamepad(node);
                                }
                            }
                            _ => {}
                        }
                    }
                    Ok(PostAction::Continue)
                },
            )
            .map_err(|e| anyhow::anyhow!("watching udev for controllers: {e}"))?;
        self.scan_gamepads();
        Ok(())
    }

    pub fn scan_gamepads(&mut self) {
        let devices = (|| {
            let mut e = udev::Enumerator::new()?;
            e.match_subsystem("input")?;
            e.match_property("ID_INPUT_JOYSTICK", "1")?;
            std::io::Result::Ok(e.scan_devices()?.collect::<Vec<_>>())
        })();
        match devices {
            Ok(devices) => devices.iter().for_each(|device| self.open_gamepad(device)),
            Err(err) => tracing::warn!(?err, "listing controllers"),
        }
    }

    /// Hands every controller back to the session, which revokes them anyway
    /// while it is paused.
    pub fn close_gamepads(&mut self) {
        for gamepad in std::mem::take(&mut self.gamepads.0) {
            self.drop_gamepad(gamepad);
        }
    }

    fn open_gamepad(&mut self, device: &udev::Device) {
        let property = |name| device.property_value(name).and_then(|v| v.to_str());
        let seat = property("ID_SEAT").unwrap_or("seat0");
        // jsN nodes carry the same tag, but they are joydev's, not evdev.
        let is_evdev = device.sysname().to_string_lossy().starts_with("event");
        if !is_evdev
            || property("ID_INPUT_JOYSTICK") != Some("1")
            || seat != self.backend.seat_name()
        {
            return;
        }
        let Some(node) = device.devnode() else {
            return;
        };
        if self.gamepads.0.iter().any(|g| g.node == node) {
            return;
        }
        if let Err(err) = self.try_open_gamepad(node) {
            tracing::warn!("opening controller {}: {err:#}", node.display());
        }
    }

    fn try_open_gamepad(&mut self, node: &Path) -> anyhow::Result<()> {
        let session_fd = self.backend.open_input(node)?;
        let opened = (|| {
            let device = evdev::Device::from_fd(session_fd.try_clone()?)?;
            device.set_nonblocking(true)?;
            anyhow::Ok(device)
        })();
        let device = match opened {
            Ok(device) => device,
            Err(err) => {
                self.backend.close_input(session_fd);
                return Err(err);
            }
        };
        // Joystick-shaped but not a gamepad: the Steam Controller's motion
        // node, which streams the IMU for as long as anything holds it open.
        let is_gamepad = device
            .supported_keys()
            .is_some_and(|k| k.contains(KeyCode::BTN_MODE) || k.contains(KeyCode::BTN_SOUTH));
        if !is_gamepad {
            tracing::debug!(node = %node.display(), name = ?device.name(), "not a gamepad");
            drop(device);
            self.backend.close_input(session_fd);
            return Ok(());
        }
        let ranges: Vec<_> = device
            .get_absinfo()
            .map(|axes| {
                axes.map(|(code, info)| {
                    let (min, max) = (info.minimum(), info.maximum());
                    (code, AxisRange { min, max })
                })
                .collect()
            })
            .unwrap_or_default();
        let pad = Pad::new(&ranges);
        let watched = node.to_owned();
        let source = self
            .loop_handle
            .insert_source(
                Generic::new(session_fd.try_clone()?, Interest::READ, Mode::Level),
                move |_, _, state| {
                    state.on_gamepad_readable(&watched);
                    Ok(PostAction::Continue)
                },
            )
            .map_err(|e| anyhow::anyhow!("watching the controller: {e}"))?;
        tracing::info!(node = %node.display(), name = ?device.name(), "controller connected");
        self.gamepads.0.push(Gamepad {
            node: node.to_owned(),
            session_fd,
            device,
            pad,
            source,
        });
        // Switching the controller on is the natural "I'm back".
        self.on_activity();
        Ok(())
    }

    fn close_gamepad(&mut self, node: &Path) {
        if let Some(i) = self.gamepads.0.iter().position(|g| g.node == node) {
            let gamepad = self.gamepads.0.swap_remove(i);
            tracing::info!(node = %node.display(), "controller disconnected");
            self.drop_gamepad(gamepad);
        }
    }

    fn drop_gamepad(&mut self, gamepad: Gamepad) {
        self.loop_handle.remove(gamepad.source);
        drop(gamepad.device);
        self.backend.close_input(gamepad.session_fd);
    }

    fn on_gamepad_readable(&mut self, node: &Path) {
        let Some(gamepad) = self.gamepads.0.iter_mut().find(|g| g.node == node) else {
            return;
        };
        let fetched = gamepad
            .device
            .fetch_events()
            .map(|events| events.collect::<Vec<_>>());
        let events = match fetched {
            Ok(events) => events,
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => return,
            Err(err) => {
                // ENODEV as the link drops; udev's remove follows.
                tracing::debug!(?err, node = %node.display(), "reading controller");
                self.close_gamepad(node);
                return;
            }
        };
        // One at a time: a Launch changes what the next event means.
        for event in events {
            let on_home = self.home_has_keyboard();
            let Some(gamepad) = self.gamepads.0.iter_mut().find(|g| g.node == node) else {
                return;
            };
            let active = gamepad.pad.is_activity(&event);
            // Read even when it's about to be swallowed, so a stick push
            // that wakes the screen doesn't step the focus once it's back.
            let action = gamepad.pad.on_event(event, on_home);
            if active && self.on_activity() == Activity::Woke {
                continue;
            }
            match action {
                Some(PadAction::GoHome) => self.go_home(),
                Some(PadAction::Home(key)) => self.on_home_key(key),
                None => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use evdev::EventType;

    use super::*;

    const STEAM_STICK: AxisRange = AxisRange {
        min: -32767,
        max: 32767,
    };

    fn pad() -> Pad {
        Pad::new(&[
            (AbsoluteAxisCode::ABS_X, STEAM_STICK),
            (AbsoluteAxisCode::ABS_Y, STEAM_STICK),
        ])
    }

    fn key(code: KeyCode, value: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, code.0, value)
    }

    fn abs(code: AbsoluteAxisCode, value: i32) -> InputEvent {
        InputEvent::new(EventType::ABSOLUTE.0, code.0, value)
    }

    #[test]
    fn steam_button_goes_home_from_anywhere() {
        assert_eq!(
            pad().on_event(key(KeyCode::BTN_MODE, 1), false),
            Some(PadAction::GoHome)
        );
        assert_eq!(
            pad().on_event(key(KeyCode::BTN_MODE, 1), true),
            Some(PadAction::GoHome)
        );
    }

    #[test]
    fn releases_and_repeats_do_nothing() {
        let mut pad = pad();
        assert_eq!(pad.on_event(key(KeyCode::BTN_MODE, 0), false), None);
        assert_eq!(pad.on_event(key(KeyCode::BTN_SOUTH, 0), true), None);
        assert_eq!(pad.on_event(key(KeyCode::BTN_DPAD_DOWN, 2), true), None);
    }

    #[test]
    fn dpad_and_a_drive_home() {
        let mut pad = pad();
        for (code, home_key) in [
            (KeyCode::BTN_DPAD_UP, HomeKey::Previous),
            (KeyCode::BTN_DPAD_LEFT, HomeKey::Previous),
            (KeyCode::BTN_DPAD_DOWN, HomeKey::Next),
            (KeyCode::BTN_DPAD_RIGHT, HomeKey::Next),
            (KeyCode::BTN_SOUTH, HomeKey::Launch),
        ] {
            assert_eq!(
                pad.on_event(key(code, 1), true),
                Some(PadAction::Home(home_key))
            );
        }
    }

    #[test]
    fn home_navigation_does_nothing_over_an_app() {
        let mut pad = pad();
        assert_eq!(pad.on_event(key(KeyCode::BTN_SOUTH, 1), false), None);
        assert_eq!(pad.on_event(key(KeyCode::BTN_DPAD_DOWN, 1), false), None);
        assert_eq!(
            pad.on_event(abs(AbsoluteAxisCode::ABS_X, 30000), false),
            None
        );
    }

    #[test]
    fn stick_jitter_at_rest_is_ignored() {
        let mut pad = pad();
        for value in [-500, 480, 0, -320, 500] {
            assert_eq!(
                pad.on_event(abs(AbsoluteAxisCode::ABS_X, value), true),
                None
            );
            assert_eq!(
                pad.on_event(abs(AbsoluteAxisCode::ABS_Y, value), true),
                None
            );
        }
    }

    #[test]
    fn a_held_stick_steps_once_until_it_recentres() {
        let mut pad = pad();
        let x = |v| abs(AbsoluteAxisCode::ABS_X, v);
        assert_eq!(
            pad.on_event(x(20000), true),
            Some(PadAction::Home(HomeKey::Next))
        );
        assert_eq!(pad.on_event(x(32767), true), None);
        // Easing off a little, still past the recentre line: no new step.
        assert_eq!(pad.on_event(x(12000), true), None);
        assert_eq!(pad.on_event(x(20000), true), None);
        assert_eq!(pad.on_event(x(400), true), None);
        assert_eq!(
            pad.on_event(x(-20000), true),
            Some(PadAction::Home(HomeKey::Previous))
        );
    }

    #[test]
    fn stick_up_is_previous_and_down_is_next() {
        let mut pad = pad();
        let y = |v| abs(AbsoluteAxisCode::ABS_Y, v);
        assert_eq!(
            pad.on_event(y(-30000), true),
            Some(PadAction::Home(HomeKey::Previous))
        );
        pad.on_event(y(0), true);
        assert_eq!(
            pad.on_event(y(30000), true),
            Some(PadAction::Home(HomeKey::Next))
        );
    }

    #[test]
    fn a_stick_pushed_over_an_app_does_not_step_when_home_appears() {
        let mut pad = pad();
        let x = |v| abs(AbsoluteAxisCode::ABS_X, v);
        assert_eq!(pad.on_event(x(30000), false), None);
        assert_eq!(pad.on_event(x(31000), true), None);
    }

    #[test]
    fn unsigned_ranges_centre_on_their_midpoint() {
        let mut pad = Pad::new(&[(AbsoluteAxisCode::ABS_X, AxisRange { min: 0, max: 255 })]);
        let x = |v| abs(AbsoluteAxisCode::ABS_X, v);
        assert_eq!(pad.on_event(x(128), true), None);
        assert_eq!(
            pad.on_event(x(250), true),
            Some(PadAction::Home(HomeKey::Next))
        );
    }
    const TRIGGER: AxisRange = AxisRange { min: 0, max: 32767 };

    fn steam_controller() -> Pad {
        use AbsoluteAxisCode as A;
        Pad::new(&[
            (A::ABS_X, STEAM_STICK),
            (A::ABS_Y, STEAM_STICK),
            (A::ABS_RX, STEAM_STICK),
            (A::ABS_RY, STEAM_STICK),
            (A::ABS_HAT0X, STEAM_STICK),
            (A::ABS_HAT0Y, STEAM_STICK),
            (A::ABS_HAT1X, STEAM_STICK),
            (A::ABS_HAT1Y, STEAM_STICK),
            (A::ABS_HAT2X, TRIGGER),
            (A::ABS_HAT2Y, TRIGGER),
        ])
    }

    #[test]
    fn every_button_press_is_activity() {
        let pad = steam_controller();
        for code in [
            KeyCode::BTN_SOUTH,
            KeyCode::BTN_MODE,
            KeyCode::BTN_TL2,
            KeyCode::BTN_THUMB,
            KeyCode::BTN_DPAD_LEFT,
            KeyCode(548), // a back grip
        ] {
            assert!(pad.is_activity(&key(code, 1)), "{code:?}");
        }
        assert!(!pad.is_activity(&key(KeyCode::BTN_SOUTH, 0)));
    }

    #[test]
    fn sticks_at_rest_are_not_activity_but_a_push_is() {
        let pad = steam_controller();
        for code in [
            AbsoluteAxisCode::ABS_X,
            AbsoluteAxisCode::ABS_Y,
            AbsoluteAxisCode::ABS_RX,
            AbsoluteAxisCode::ABS_RY,
        ] {
            for drift in [-500, 0, 480, -2000] {
                assert!(!pad.is_activity(&abs(code, drift)), "{code:?} at {drift}");
            }
            assert!(pad.is_activity(&abs(code, 12000)), "{code:?}");
            assert!(pad.is_activity(&abs(code, -12000)), "{code:?}");
        }
    }

    #[test]
    fn a_trigger_counts_once_pulled_past_its_deadzone() {
        let pad = steam_controller();
        assert!(!pad.is_activity(&abs(AbsoluteAxisCode::ABS_HAT2X, 0)));
        assert!(!pad.is_activity(&abs(AbsoluteAxisCode::ABS_HAT2Y, 1500)));
        assert!(pad.is_activity(&abs(AbsoluteAxisCode::ABS_HAT2Y, 12000)));
    }

    #[test]
    fn any_trackpad_touch_is_activity() {
        let pad = steam_controller();
        assert!(pad.is_activity(&abs(AbsoluteAxisCode::ABS_HAT0X, 3)));
        assert!(pad.is_activity(&abs(AbsoluteAxisCode::ABS_HAT1Y, -200)));
    }

    #[test]
    fn sync_and_unknown_axes_are_not_activity() {
        let pad = steam_controller();
        assert!(!pad.is_activity(&InputEvent::new(EventType::SYNCHRONIZATION.0, 0, 0)));
        assert!(!pad.is_activity(&abs(AbsoluteAxisCode::ABS_PRESSURE, 9000)));
    }
}
