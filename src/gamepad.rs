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

use crate::{lifecycle::HomeKey, state::Emrakul};

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

/// One controller's view of the stand-in mapping. Sticks only step the focus
/// on the way out past [`PUSH`], so holding one steps once.
#[derive(Debug)]
pub struct Pad {
    x: Axis,
    y: Axis,
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
        let AxisRange { min, max } = self.range;
        let half = (max - min) as f32 / 2.0;
        if half <= 0.0 {
            return None;
        }
        let lean = (value as f32 - (min as f32 + half)) / half;
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

impl Pad {
    pub fn new(x: AxisRange, y: AxisRange) -> Self {
        Self {
            x: Axis::new(x),
            y: Axis::new(y),
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
        let range = |code| {
            device
                .get_absinfo()
                .ok()
                .and_then(|mut axes| axes.find(|(c, _)| *c == code))
                .map_or(AxisRange { min: 0, max: 0 }, |(_, info)| AxisRange {
                    min: info.minimum(),
                    max: info.maximum(),
                })
        };
        let pad = Pad::new(
            range(AbsoluteAxisCode::ABS_X),
            range(AbsoluteAxisCode::ABS_Y),
        );
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
            match gamepad.pad.on_event(event, on_home) {
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
        Pad::new(STEAM_STICK, STEAM_STICK)
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
        let mut pad = Pad::new(AxisRange { min: 0, max: 255 }, STEAM_STICK);
        let x = |v| abs(AbsoluteAxisCode::ABS_X, v);
        assert_eq!(pad.on_event(x(128), true), None);
        assert_eq!(
            pad.on_event(x(250), true),
            Some(PadAction::Home(HomeKey::Next))
        );
    }
}
