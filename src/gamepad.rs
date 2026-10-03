//! Controllers, read straight from their evdev nodes. libinput ignores
//! joysticks, so it never sees them.
//!
//! [`Pad`] turns a controller into what a web app understands: keys on the
//! seat keyboard, and a pointer. Home reads the same keys (arrows and
//! Enter), so it has no mapping of its own. The Steam button goes Home from
//! anywhere.

use std::{
    os::fd::OwnedFd,
    path::{Path, PathBuf},
};

use evdev::{AbsoluteAxisCode, EventSummary, InputEvent, KeyCode, SynchronizationCode};
use smithay::{
    backend::input::{ButtonState, KeyState},
    reexports::{
        calloop::{Interest, Mode, PostAction, RegistrationToken, generic::Generic},
        udev,
    },
    utils::{Logical, Point},
};

use crate::{idle::Activity, osk, state::Emrakul};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PadAction {
    GoHome,
    /// A key on the seat keyboard, as an evdev keyboard code.
    Key(KeyCode, KeyState),
    /// Move the pointer this far.
    Move(Point<f64, Logical>),
    /// The pointer's left button.
    Click(ButtonState),
    /// Scroll this far, in wl_pointer's terms: positive is down or right.
    Scroll(Point<f64, Logical>),
    /// The finger left the scrolling trackpad, so the client may coast.
    ScrollStop,
    OpenKeyboard,
    /// The on-screen keyboard is open and has this press.
    Keyboard(osk::Input),
}

/// What the controller drives besides the Steam button, which always goes
/// Home.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    /// Keys and the pointer, for Home or the app.
    App,
    /// The on-screen keyboard. Nothing reaches the app but what it types.
    Keyboard,
}

#[derive(Debug, Clone, Copy)]
enum Binding {
    GoHome,
    Click,
    OpenKeyboard,
    /// Pressed in order and released in reverse, so a modifier wraps its key.
    Keys(&'static [KeyCode]),
}

/// What each button does: one map for every web app, from
/// <https://github.com/cramt/emrakul/issues/8>.
fn binding(button: KeyCode) -> Option<Binding> {
    use KeyCode as K;
    Some(match button {
        K::BTN_MODE => Binding::GoHome,
        K::BTN_THUMB2 => Binding::Click,
        K::BTN_START => Binding::OpenKeyboard,
        K::BTN_DPAD_UP => Binding::Keys(&[K::KEY_UP]),
        K::BTN_DPAD_DOWN => Binding::Keys(&[K::KEY_DOWN]),
        K::BTN_DPAD_LEFT => Binding::Keys(&[K::KEY_LEFT]),
        K::BTN_DPAD_RIGHT => Binding::Keys(&[K::KEY_RIGHT]),
        K::BTN_SOUTH => Binding::Keys(&[K::KEY_ENTER]),
        // Back. Alt+Left is history back in YouTube and Jellyfin alike;
        // Escape only closes menus in YouTube.
        K::BTN_EAST => Binding::Keys(&[K::KEY_LEFTALT, K::KEY_LEFT]),
        // X: Alex found clicking a page's buttons with the pointer
        // impractical, so focus navigation (Tab, Enter) does the work, and
        // X clicks wherever the pointer already is.
        K::BTN_NORTH => Binding::Click,
        K::BTN_WEST => Binding::Keys(&[K::KEY_K]),
        K::BTN_TR => Binding::Keys(&[K::KEY_TAB]),
        K::BTN_TL => Binding::Keys(&[K::KEY_LEFTSHIFT, K::KEY_TAB]),
        // The triggers' own full-pull buttons, not their analog axes: they
        // fire near the end of the pull and come back with hysteresis.
        K::BTN_TR2 => Binding::Keys(&[K::KEY_L]),
        K::BTN_TL2 => Binding::Keys(&[K::KEY_J]),
        K::BTN_SELECT => Binding::Keys(&[K::KEY_ESC]),
        // Lower back grips (R5, L5): page zoom, which Chromium remembers per
        // site. Codes 551/550 are BTN_GRIPR2/BTN_GRIPL2; evdev-rs predates them.
        K(551) => Binding::Keys(&[K::KEY_LEFTCTRL, K::KEY_EQUAL]),
        K(550) => Binding::Keys(&[K::KEY_LEFTCTRL, K::KEY_MINUS]),
        _ => return None,
    })
}

/// What a press does while the on-screen keyboard is open.
fn keyboard_binding(button: KeyCode) -> Option<osk::Input> {
    use KeyCode as K;
    use osk::{Dir, Input};
    Some(match button {
        K::BTN_DPAD_UP => Input::Move(Dir::Up),
        K::BTN_DPAD_DOWN => Input::Move(Dir::Down),
        K::BTN_DPAD_LEFT => Input::Move(Dir::Left),
        K::BTN_DPAD_RIGHT => Input::Move(Dir::Right),
        K::BTN_SOUTH => Input::Press,
        K::BTN_EAST => Input::Backspace,
        K::BTN_START => Input::Close,
        _ => return None,
    })
}

/// How far a stick axis must lean, as a fraction of its half-range, to count
/// as a push. The Steam Controller's sticks wander by about 1.5% at rest.
const PUSH: f32 = 0.5;
/// How far back it must come before the push ends.
const RECENTRE: f32 = 0.25;
/// How far a stick must lean, or a trigger be pulled, to count as activity:
/// far enough that drift at rest never holds the screen on.
const ACTIVE: f32 = 0.25;
/// Screen pixels per trackpad unit: a swipe across the whole pad (65534
/// units) moves the pointer, or scrolls, the width of the TV. A first guess,
/// to be tuned on the couch.
const PAD_PIXELS: f64 = 3840.0 / 65534.0;

/// One controller's state. A stick holds an arrow while pushed past
/// [`PUSH`]; a trackpad moves by how far the finger went since the last
/// frame.
#[derive(Debug)]
pub struct Pad {
    x: Stick,
    y: Stick,
    pointer: Trackpad,
    scroll: Trackpad,
    /// Buttons whose press never reached the app: it woke the screen, or the
    /// on-screen keyboard took it. Their release goes with it.
    swallowed: Vec<KeyCode>,
    /// Every absolute axis the controller reports, for telling activity
    /// from drift.
    ranges: Vec<(AbsoluteAxisCode, AxisRange)>,
}

#[derive(Debug, Clone, Copy)]
pub struct AxisRange {
    pub min: i32,
    pub max: i32,
    /// The kernel drops changes smaller than half this, and smooths ones up
    /// to twice this towards the last value it let through.
    pub fuzz: i32,
}

#[derive(Debug)]
struct Stick {
    range: AxisRange,
    /// Leaning negative (left, or up: evdev's Y grows downwards) and
    /// positive.
    dirs: (osk::Dir, osk::Dir),
    lean: Lean,
}

#[derive(Debug, Clone, Copy)]
enum Lean {
    Rest,
    Held(KeyCode),
    /// Pushed to wake the screen, or to move the on-screen keyboard's focus:
    /// holds nothing, and its return does nothing.
    Swallowed,
}

fn arrow(dir: osk::Dir) -> KeyCode {
    match dir {
        osk::Dir::Up => KeyCode::KEY_UP,
        osk::Dir::Down => KeyCode::KEY_DOWN,
        osk::Dir::Left => KeyCode::KEY_LEFT,
        osk::Dir::Right => KeyCode::KEY_RIGHT,
    }
}

impl Stick {
    fn new(range: AxisRange, dirs: (osk::Dir, osk::Dir)) -> Self {
        Self {
            range,
            dirs,
            lean: Lean::Rest,
        }
    }

    fn on_value(&mut self, value: i32, wakes: bool, layer: Layer) -> Option<PadAction> {
        let lean = self.range.lean(value);
        match self.lean {
            Lean::Held(_) | Lean::Swallowed if lean.abs() > RECENTRE => None,
            Lean::Held(key) => {
                self.lean = Lean::Rest;
                Some(PadAction::Key(key, KeyState::Released))
            }
            Lean::Swallowed => {
                self.lean = Lean::Rest;
                None
            }
            Lean::Rest if lean.abs() < PUSH => None,
            Lean::Rest if wakes => {
                self.lean = Lean::Swallowed;
                None
            }
            Lean::Rest => {
                let dir = if lean < 0.0 { self.dirs.0 } else { self.dirs.1 };
                match layer {
                    Layer::App => {
                        self.lean = Lean::Held(arrow(dir));
                        Some(PadAction::Key(arrow(dir), KeyState::Pressed))
                    }
                    Layer::Keyboard => {
                        self.lean = Lean::Swallowed;
                        Some(PadAction::Keyboard(osk::Input::Move(dir)))
                    }
                }
            }
        }
    }
}

/// A trackpad reports where the finger is, and (0, 0) once it lifts. Its
/// axes arrive one at a time, so the position is only whole at the end of a
/// frame (`SYN_REPORT`).
#[derive(Debug, Default)]
struct Trackpad {
    /// The latest value of each axis. evdev only sends the ones that change.
    now: (i32, i32),
    /// Where the finger was at the end of the last frame.
    touch: Option<(i32, i32)>,
    /// How near the middle, on each axis, counts as lifted: the axis's
    /// fuzz. The kernel's fuzz filter turns a lift from within twice the
    /// fuzz of the middle into half its value, then creeps towards 0 and
    /// stops short, so (0, 0) alone would miss it and the next touch would
    /// jump the pointer. A finger crossing the small square in the middle
    /// loses only those frames' travel.
    lifted: (i32, i32),
}

enum Stroke {
    Moved(Point<f64, Logical>),
    Lifted,
}

impl Trackpad {
    fn new(x: AxisRange, y: AxisRange) -> Self {
        Self {
            lifted: (x.fuzz, y.fuzz),
            ..Self::default()
        }
    }

    fn end_frame(&mut self) -> Option<Stroke> {
        let (x, y) = self.now;
        let lifted = x.abs() <= self.lifted.0 && y.abs() <= self.lifted.1;
        let touch = (!lifted).then_some(self.now);
        let stroke = match (self.touch, touch) {
            (Some(was), Some(is)) if was != is => Some(Stroke::Moved(Point::from((
                f64::from(is.0 - was.0) * PAD_PIXELS,
                f64::from(is.1 - was.1) * PAD_PIXELS,
            )))),
            (Some(_), None) => Some(Stroke::Lifted),
            _ => None,
        };
        self.touch = touch;
        stroke
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
        use AbsoluteAxisCode as A;
        let range = |code| {
            range_of(ranges, code).unwrap_or(AxisRange {
                min: 0,
                max: 0,
                fuzz: 0,
            })
        };
        Self {
            x: Stick::new(range(A::ABS_X), (osk::Dir::Left, osk::Dir::Right)),
            y: Stick::new(range(A::ABS_Y), (osk::Dir::Up, osk::Dir::Down)),
            pointer: Trackpad::new(range(A::ABS_HAT1X), range(A::ABS_HAT1Y)),
            scroll: Trackpad::new(range(A::ABS_HAT0X), range(A::ABS_HAT0Y)),
            swallowed: Vec::new(),
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

    /// What this event does, with `layer` being what the controller drives.
    /// `wakes` is whether it just woke the screen, in which case it does
    /// nothing else, and nor does the release of a press or the return of a
    /// stick that woke it. Positions are tracked either way, so nothing
    /// jumps once the screen is back.
    pub fn on_event(&mut self, event: InputEvent, wakes: bool, layer: Layer) -> Vec<PadAction> {
        use AbsoluteAxisCode as A;
        match event.destructure() {
            EventSummary::Key(_, button, 1) if wakes => {
                self.swallowed.push(button);
                Vec::new()
            }
            EventSummary::Key(_, button, 0) if self.swallowed.contains(&button) => {
                self.swallowed.retain(|b| *b != button);
                Vec::new()
            }
            EventSummary::Key(_, button, 1) if layer == Layer::Keyboard => {
                self.swallowed.push(button);
                match (binding(button), keyboard_binding(button)) {
                    (Some(Binding::GoHome), _) => vec![PadAction::GoHome],
                    (_, Some(input)) => vec![PadAction::Keyboard(input)],
                    _ => Vec::new(),
                }
            }
            EventSummary::Key(_, button, 1) => match binding(button) {
                Some(Binding::GoHome) => vec![PadAction::GoHome],
                Some(Binding::OpenKeyboard) => vec![PadAction::OpenKeyboard],
                Some(Binding::Click) => vec![PadAction::Click(ButtonState::Pressed)],
                Some(Binding::Keys(keys)) => keys
                    .iter()
                    .map(|key| PadAction::Key(*key, KeyState::Pressed))
                    .collect(),
                None => Vec::new(),
            },
            // Pressed before the keyboard opened, so the app has it held.
            EventSummary::Key(_, button, 0) => match binding(button) {
                Some(Binding::Click) => vec![PadAction::Click(ButtonState::Released)],
                Some(Binding::Keys(keys)) => keys
                    .iter()
                    .rev()
                    .map(|key| PadAction::Key(*key, KeyState::Released))
                    .collect(),
                Some(Binding::GoHome | Binding::OpenKeyboard) | None => Vec::new(),
            },
            EventSummary::AbsoluteAxis(_, code, value) => {
                match code {
                    A::ABS_X => return self.x.on_value(value, wakes, layer).into_iter().collect(),
                    A::ABS_Y => return self.y.on_value(value, wakes, layer).into_iter().collect(),
                    A::ABS_HAT1X => self.pointer.now.0 = value,
                    A::ABS_HAT1Y => self.pointer.now.1 = value,
                    A::ABS_HAT0X => self.scroll.now.0 = value,
                    A::ABS_HAT0Y => self.scroll.now.1 = value,
                    _ => {}
                }
                Vec::new()
            }
            EventSummary::Synchronization(_, SynchronizationCode::SYN_REPORT, _) => {
                let (pointer, scroll) = (self.pointer.end_frame(), self.scroll.end_frame());
                // The pad's y grows upwards, the screen's downwards.
                let pointer = match pointer {
                    Some(Stroke::Moved(by)) if layer == Layer::App => {
                        Some(PadAction::Move(Point::from((by.x, -by.y))))
                    }
                    _ => None,
                };
                // Wheel-style: the pad's y grows upwards, so a finger moving
                // up scrolls up, towards the top.
                let scroll = match scroll {
                    Some(Stroke::Moved(by)) if layer == Layer::App => {
                        Some(PadAction::Scroll(Point::from((-by.x, -by.y))))
                    }
                    Some(Stroke::Lifted) => Some(PadAction::ScrollStop),
                    _ => None,
                };
                pointer.into_iter().chain(scroll).collect()
            }
            // Autorepeat (2) included: the client repeats a held key itself.
            _ => Vec::new(),
        }
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
    /// Holds `EVIOCGRAB`: nothing else reading the node sees its events.
    grabbed: bool,
}

/// Who reads the controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PadReader {
    /// emrakul grabs it and turns it into keys and a pointer: on Home and in
    /// web apps.
    Emrakul,
    /// The app reads the controller's nodes itself: Moonlight, in a Game.
    /// emrakul lets go of its grab and only watches for the Steam button and
    /// for activity.
    #[expect(
        dead_code,
        reason = "Games aren't built yet; a Game's session will read as App"
    )]
    App,
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
                    let (min, max, fuzz) = (info.minimum(), info.maximum(), info.fuzz());
                    (code, AxisRange { min, max, fuzz })
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
            grabbed: false,
        });
        self.sync_gamepad_grabs();
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
        // One at a time: a Launch, or the keyboard opening, changes what the
        // next event means.
        for event in events {
            let Some(gamepad) = self.gamepads.0.iter().find(|g| g.node == node) else {
                return;
            };
            let wakes = gamepad.pad.is_activity(&event) && self.on_activity() == Activity::Woke;
            let layer = self.pad_layer();
            let Some(gamepad) = self.gamepads.0.iter_mut().find(|g| g.node == node) else {
                return;
            };
            for action in gamepad.pad.on_event(event, wakes, layer) {
                tracing::trace!(?action, "controller");
                match (self.pad_reader(), action) {
                    (_, PadAction::GoHome) => self.go_home(),
                    (PadReader::App, _) => {}
                    (PadReader::Emrakul, PadAction::OpenKeyboard) => self.open_keyboard(),
                    (PadReader::Emrakul, PadAction::Keyboard(input)) => {
                        self.on_keyboard_input(input)
                    }
                    (PadReader::Emrakul, PadAction::Key(key, state)) => self.pad_key(key, state),
                    (PadReader::Emrakul, PadAction::Move(by)) => self.move_pointer(by),
                    (PadReader::Emrakul, PadAction::Click(state)) => self.click(state),
                    (PadReader::Emrakul, PadAction::Scroll(by)) => self.scroll(Some(by)),
                    (PadReader::Emrakul, PadAction::ScrollStop) => self.scroll(None),
                }
            }
        }
    }

    /// Grabs every controller while emrakul reads it, so nothing else sees
    /// the presses twice (Jellyfin has its own Gamepad API code), and lets
    /// go while the app reads it.
    pub fn sync_gamepad_grabs(&mut self) {
        let grab = self.pad_reader() == PadReader::Emrakul;
        for gamepad in self.gamepads.0.iter_mut().filter(|g| g.grabbed != grab) {
            let result = if grab {
                gamepad.device.grab()
            } else {
                gamepad.device.ungrab()
            };
            match result {
                Ok(()) => {
                    gamepad.grabbed = grab;
                    tracing::debug!(grab, node = %gamepad.node.display(), "controller grab");
                }
                Err(err) => {
                    tracing::warn!(?err, grab, node = %gamepad.node.display(), "grabbing controller")
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use evdev::EventType;
    use smithay::backend::input::{ButtonState, KeyState};

    use super::*;

    // As the captures in docs/hardware report them.
    const STEAM_STICK: AxisRange = AxisRange {
        min: -32767,
        max: 32767,
        fuzz: 0,
    };
    const STEAM_TRACKPAD: AxisRange = AxisRange {
        min: -32767,
        max: 32767,
        fuzz: 256,
    };
    const TRIGGER: AxisRange = AxisRange {
        min: 0,
        max: 32767,
        fuzz: 0,
    };

    fn steam_controller() -> Pad {
        use AbsoluteAxisCode as A;
        Pad::new(&[
            (A::ABS_X, STEAM_STICK),
            (A::ABS_Y, STEAM_STICK),
            (A::ABS_RX, STEAM_STICK),
            (A::ABS_RY, STEAM_STICK),
            (A::ABS_HAT0X, STEAM_TRACKPAD),
            (A::ABS_HAT0Y, STEAM_TRACKPAD),
            (A::ABS_HAT1X, STEAM_TRACKPAD),
            (A::ABS_HAT1Y, STEAM_TRACKPAD),
            (A::ABS_HAT2X, TRIGGER),
            (A::ABS_HAT2Y, TRIGGER),
        ])
    }

    fn key(code: KeyCode, value: i32) -> InputEvent {
        InputEvent::new(EventType::KEY.0, code.0, value)
    }

    fn abs(code: AbsoluteAxisCode, value: i32) -> InputEvent {
        InputEvent::new(EventType::ABSOLUTE.0, code.0, value)
    }

    fn syn() -> InputEvent {
        InputEvent::new(EventType::SYNCHRONIZATION.0, 0, 0)
    }

    /// Every action a run of events makes, none of them waking the screen.
    fn feed(pad: &mut Pad, events: impl IntoIterator<Item = InputEvent>) -> Vec<PadAction> {
        events
            .into_iter()
            .flat_map(|event| pad.on_event(event, false, Layer::App))
            .collect()
    }

    /// The same, with the on-screen keyboard open.
    fn feed_keyboard(
        pad: &mut Pad,
        events: impl IntoIterator<Item = InputEvent>,
    ) -> Vec<PadAction> {
        events
            .into_iter()
            .flat_map(|event| pad.on_event(event, false, Layer::Keyboard))
            .collect()
    }

    fn keyboard(input: osk::Input) -> PadAction {
        PadAction::Keyboard(input)
    }

    fn tap(code: KeyCode) -> [InputEvent; 4] {
        [key(code, 1), syn(), key(code, 0), syn()]
    }

    fn down(code: KeyCode) -> PadAction {
        PadAction::Key(code, KeyState::Pressed)
    }

    fn up(code: KeyCode) -> PadAction {
        PadAction::Key(code, KeyState::Released)
    }

    /// One trackpad frame: both axes, then the report that ends it.
    fn touch(x: AbsoluteAxisCode, y: AbsoluteAxisCode, at: (i32, i32)) -> [InputEvent; 3] {
        [abs(x, at.0), abs(y, at.1), syn()]
    }

    fn right_pad(at: (i32, i32)) -> [InputEvent; 3] {
        touch(AbsoluteAxisCode::ABS_HAT1X, AbsoluteAxisCode::ABS_HAT1Y, at)
    }

    fn left_pad(at: (i32, i32)) -> [InputEvent; 3] {
        touch(AbsoluteAxisCode::ABS_HAT0X, AbsoluteAxisCode::ABS_HAT0Y, at)
    }

    #[test]
    fn steam_button_goes_home() {
        assert_eq!(
            feed(&mut steam_controller(), tap(KeyCode::BTN_MODE)),
            [PadAction::GoHome]
        );
    }

    #[test]
    fn buttons_press_and_release_the_web_app_keys() {
        for (button, key) in [
            (KeyCode::BTN_DPAD_UP, KeyCode::KEY_UP),
            (KeyCode::BTN_DPAD_DOWN, KeyCode::KEY_DOWN),
            (KeyCode::BTN_DPAD_LEFT, KeyCode::KEY_LEFT),
            (KeyCode::BTN_DPAD_RIGHT, KeyCode::KEY_RIGHT),
            (KeyCode::BTN_SOUTH, KeyCode::KEY_ENTER),
            (KeyCode::BTN_WEST, KeyCode::KEY_K),
            (KeyCode::BTN_TR, KeyCode::KEY_TAB),
            (KeyCode::BTN_TR2, KeyCode::KEY_L),
            (KeyCode::BTN_TL2, KeyCode::KEY_J),
            (KeyCode::BTN_SELECT, KeyCode::KEY_ESC),
        ] {
            assert_eq!(
                feed(&mut steam_controller(), tap(button)),
                [down(key), up(key)],
                "{button:?}"
            );
        }
    }

    #[test]
    fn b_is_alt_left_released_in_reverse() {
        assert_eq!(
            feed(&mut steam_controller(), tap(KeyCode::BTN_EAST)),
            [
                down(KeyCode::KEY_LEFTALT),
                down(KeyCode::KEY_LEFT),
                up(KeyCode::KEY_LEFT),
                up(KeyCode::KEY_LEFTALT),
            ]
        );
    }

    #[test]
    fn lb_is_shift_tab_released_in_reverse() {
        assert_eq!(
            feed(&mut steam_controller(), tap(KeyCode::BTN_TL)),
            [
                down(KeyCode::KEY_LEFTSHIFT),
                down(KeyCode::KEY_TAB),
                up(KeyCode::KEY_TAB),
                up(KeyCode::KEY_LEFTSHIFT),
            ]
        );
    }

    #[test]
    fn a_held_button_holds_its_key_until_released() {
        let mut pad = steam_controller();
        assert_eq!(
            feed(&mut pad, [key(KeyCode::BTN_DPAD_DOWN, 1), syn()]),
            [down(KeyCode::KEY_DOWN)]
        );
        // Kernel autorepeat, if any: the client repeats the held key itself.
        assert_eq!(feed(&mut pad, [key(KeyCode::BTN_DPAD_DOWN, 2), syn()]), []);
        assert_eq!(
            feed(&mut pad, [key(KeyCode::BTN_DPAD_DOWN, 0), syn()]),
            [up(KeyCode::KEY_DOWN)]
        );
    }

    #[test]
    fn lower_grips_zoom_the_page() {
        assert_eq!(
            feed(&mut steam_controller(), tap(KeyCode(551))),
            [
                down(KeyCode::KEY_LEFTCTRL),
                down(KeyCode::KEY_EQUAL),
                up(KeyCode::KEY_EQUAL),
                up(KeyCode::KEY_LEFTCTRL)
            ]
        );
        assert_eq!(
            feed(&mut steam_controller(), tap(KeyCode(550))),
            [
                down(KeyCode::KEY_LEFTCTRL),
                down(KeyCode::KEY_MINUS),
                up(KeyCode::KEY_MINUS),
                up(KeyCode::KEY_LEFTCTRL)
            ]
        );
    }

    #[test]
    fn unmapped_controls_do_nothing() {
        let mut pad = steam_controller();
        for button in [
            KeyCode::BTN_THUMBL,
            KeyCode::BTN_THUMBR,
            KeyCode::BTN_THUMB,
            KeyCode::BTN_BASE,
            KeyCode(548),
        ] {
            assert_eq!(feed(&mut pad, tap(button)), [], "{button:?}");
        }
        assert_eq!(
            feed(
                &mut pad,
                [
                    abs(AbsoluteAxisCode::ABS_RX, 30000),
                    abs(AbsoluteAxisCode::ABS_HAT2X, 30000),
                    syn()
                ]
            ),
            []
        );
    }

    #[test]
    fn a_press_that_wakes_the_screen_is_swallowed_with_its_release() {
        let mut pad = steam_controller();
        assert_eq!(
            pad.on_event(key(KeyCode::BTN_EAST, 1), true, Layer::App),
            []
        );
        assert_eq!(
            feed(&mut pad, [syn(), key(KeyCode::BTN_EAST, 0), syn()]),
            []
        );
        assert_eq!(
            pad.on_event(key(KeyCode::BTN_MODE, 1), true, Layer::App),
            []
        );
        assert_eq!(feed(&mut pad, [key(KeyCode::BTN_MODE, 0)]), []);
        // The next press is an ordinary one.
        assert_eq!(
            feed(&mut pad, tap(KeyCode::BTN_EAST)).len(),
            4,
            "Alt+Left, pressed and released"
        );
    }

    #[test]
    fn stick_jitter_at_rest_is_ignored() {
        let mut pad = steam_controller();
        for value in [-500, 480, 0, -320, 500] {
            assert_eq!(
                feed(
                    &mut pad,
                    [
                        abs(AbsoluteAxisCode::ABS_X, value),
                        abs(AbsoluteAxisCode::ABS_Y, value),
                        syn()
                    ]
                ),
                []
            );
        }
    }

    #[test]
    fn a_pushed_stick_holds_an_arrow_until_it_recentres() {
        let mut pad = steam_controller();
        let x = |v| abs(AbsoluteAxisCode::ABS_X, v);
        assert_eq!(feed(&mut pad, [x(20000)]), [down(KeyCode::KEY_RIGHT)]);
        assert_eq!(feed(&mut pad, [x(32767)]), []);
        // Easing off a little, still past the recentre line: still held.
        assert_eq!(feed(&mut pad, [x(12000), x(20000)]), []);
        assert_eq!(feed(&mut pad, [x(400)]), [up(KeyCode::KEY_RIGHT)]);
        assert_eq!(feed(&mut pad, [x(-20000)]), [down(KeyCode::KEY_LEFT)]);
    }

    #[test]
    fn stick_up_is_the_up_arrow_and_down_is_down() {
        let mut pad = steam_controller();
        let y = |v| abs(AbsoluteAxisCode::ABS_Y, v);
        assert_eq!(
            feed(&mut pad, [y(-30000), y(0), y(30000)]),
            [
                down(KeyCode::KEY_UP),
                up(KeyCode::KEY_UP),
                down(KeyCode::KEY_DOWN)
            ]
        );
    }

    #[test]
    fn a_stick_push_that_wakes_the_screen_presses_nothing() {
        let mut pad = steam_controller();
        let x = |v| abs(AbsoluteAxisCode::ABS_X, v);
        assert_eq!(pad.on_event(x(30000), true, Layer::App), []);
        assert_eq!(feed(&mut pad, [x(31000), x(0)]), []);
        assert_eq!(feed(&mut pad, [x(30000)]), [down(KeyCode::KEY_RIGHT)]);
    }

    #[test]
    fn unsigned_ranges_centre_on_their_midpoint() {
        let mut pad = Pad::new(&[(
            AbsoluteAxisCode::ABS_X,
            AxisRange {
                min: 0,
                max: 255,
                fuzz: 0,
            },
        )]);
        let x = |v| abs(AbsoluteAxisCode::ABS_X, v);
        assert_eq!(feed(&mut pad, [x(128)]), []);
        assert_eq!(feed(&mut pad, [x(250)]), [down(KeyCode::KEY_RIGHT)]);
    }

    #[test]
    fn the_right_trackpad_moves_the_pointer_by_the_fingers_travel() {
        let mut pad = steam_controller();
        // Touching down only says where the finger is.
        assert_eq!(feed(&mut pad, right_pad((-3000, 1000))), []);
        // Half the pad's width is half the screen's (1920 px). The pad's y
        // grows upwards, the screen's downwards.
        let moved = feed(&mut pad, right_pad((-3000 + 32767, 1000 + 16383)));
        let [PadAction::Move(by)] = moved[..] else {
            panic!("{moved:?}");
        };
        assert!((by.x - 1920.0).abs() < 1.0, "{by:?}");
        assert!((by.y + 960.0).abs() < 1.0, "{by:?}");
    }

    #[test]
    fn lifting_off_the_trackpad_does_not_jump_the_pointer() {
        let mut pad = steam_controller();
        feed(&mut pad, right_pad((10000, 10000)));
        // hid-steam reports a lift as the pad going to (0, 0).
        assert_eq!(feed(&mut pad, right_pad((0, 0))), []);
        // Nor does the next touch, wherever it lands.
        assert_eq!(feed(&mut pad, right_pad((-20000, -20000))), []);
        let moved = feed(&mut pad, [abs(AbsoluteAxisCode::ABS_HAT1Y, -19000), syn()]);
        assert!(matches!(moved[..], [PadAction::Move(_)]), "{moved:?}");
    }

    #[test]
    fn a_lift_the_kernel_smooths_short_of_zero_still_lifts() {
        let mut pad = steam_controller();
        feed(&mut pad, right_pad((300, 6000)));
        // The kernel's fuzz filter only lets a change near the last value
        // through part way: lifting from x = 300 reports x = 150, then
        // stops at about 112, never 0. y jumps straight to 0.
        assert_eq!(feed(&mut pad, right_pad((150, 0))), []);
        assert_eq!(feed(&mut pad, right_pad((112, 0))), []);
        // So the next touch, far away, must not be read as a swipe there.
        assert_eq!(feed(&mut pad, right_pad((-20000, 15000))), []);
    }

    #[test]
    fn a_frame_with_only_one_axis_moving_moves_along_it() {
        let mut pad = steam_controller();
        feed(&mut pad, right_pad((5000, 5000)));
        let moved = feed(&mut pad, [abs(AbsoluteAxisCode::ABS_HAT1X, 7000), syn()]);
        let [PadAction::Move(by)] = moved[..] else {
            panic!("{moved:?}");
        };
        assert!(by.x > 0.0 && by.y == 0.0, "{by:?}");
    }

    #[test]
    fn x_and_the_right_trackpad_click_are_the_left_button() {
        for button in [KeyCode::BTN_NORTH, KeyCode::BTN_THUMB2] {
            assert_eq!(
                feed(&mut steam_controller(), tap(button)),
                [
                    PadAction::Click(ButtonState::Pressed),
                    PadAction::Click(ButtonState::Released)
                ],
                "{button:?}"
            );
        }
    }

    #[test]
    fn the_left_trackpad_scrolls_like_a_wheel_then_stops_on_lift() {
        let mut pad = steam_controller();
        assert_eq!(feed(&mut pad, left_pad((0, 10000))), []);
        // Finger down the pad (its y grows upwards): scrolls down.
        let scrolled = feed(&mut pad, left_pad((0, 10000 - 3277)));
        let [PadAction::Scroll(by)] = scrolled[..] else {
            panic!("{scrolled:?}");
        };
        assert!(by.y > 0.0 && by.x == 0.0, "{by:?}");
        assert_eq!(feed(&mut pad, left_pad((0, 0))), [PadAction::ScrollStop]);
    }

    #[test]
    fn a_touch_on_one_trackpad_does_not_drive_the_other() {
        let mut pad = steam_controller();
        feed(&mut pad, right_pad((5000, 5000)));
        assert_eq!(feed(&mut pad, left_pad((9000, 9000))), []);
    }

    #[test]
    fn menu_opens_the_keyboard() {
        assert_eq!(
            feed(&mut steam_controller(), tap(KeyCode::BTN_START)),
            [PadAction::OpenKeyboard]
        );
    }

    #[test]
    fn with_the_keyboard_open_the_buttons_drive_it_and_type_nothing() {
        use osk::{Dir, Input};
        for (button, input) in [
            (KeyCode::BTN_DPAD_UP, Input::Move(Dir::Up)),
            (KeyCode::BTN_DPAD_DOWN, Input::Move(Dir::Down)),
            (KeyCode::BTN_DPAD_LEFT, Input::Move(Dir::Left)),
            (KeyCode::BTN_DPAD_RIGHT, Input::Move(Dir::Right)),
            (KeyCode::BTN_SOUTH, Input::Press),
            (KeyCode::BTN_EAST, Input::Backspace),
            (KeyCode::BTN_START, Input::Close),
        ] {
            assert_eq!(
                feed_keyboard(&mut steam_controller(), tap(button)),
                [keyboard(input)],
                "{button:?}"
            );
        }
        for button in [KeyCode::BTN_NORTH, KeyCode::BTN_SELECT, KeyCode::BTN_THUMB2] {
            assert_eq!(
                feed_keyboard(&mut steam_controller(), tap(button)),
                [],
                "{button:?}"
            );
        }
    }

    #[test]
    fn steam_still_goes_home_with_the_keyboard_open() {
        assert_eq!(
            feed_keyboard(&mut steam_controller(), tap(KeyCode::BTN_MODE)),
            [PadAction::GoHome]
        );
    }

    #[test]
    fn a_key_held_as_the_keyboard_opens_is_released_to_the_app() {
        let mut pad = steam_controller();
        assert_eq!(
            feed(&mut pad, [key(KeyCode::BTN_DPAD_DOWN, 1), syn()]),
            [down(KeyCode::KEY_DOWN)]
        );
        assert_eq!(
            feed_keyboard(&mut pad, [key(KeyCode::BTN_DPAD_DOWN, 0), syn()]),
            [up(KeyCode::KEY_DOWN)]
        );
    }

    #[test]
    fn a_press_the_keyboard_took_releases_nothing_once_it_has_closed() {
        let mut pad = steam_controller();
        feed_keyboard(&mut pad, [key(KeyCode::BTN_SOUTH, 1), syn()]);
        assert_eq!(feed(&mut pad, [key(KeyCode::BTN_SOUTH, 0), syn()]), []);
    }

    #[test]
    fn with_the_keyboard_open_a_stick_push_moves_once() {
        let mut pad = steam_controller();
        let x = |v| abs(AbsoluteAxisCode::ABS_X, v);
        assert_eq!(
            feed_keyboard(&mut pad, [x(30000), x(32000)]),
            [keyboard(osk::Input::Move(osk::Dir::Right))]
        );
        assert_eq!(feed_keyboard(&mut pad, [x(0)]), []);
        assert_eq!(
            feed_keyboard(&mut pad, [x(-30000)]),
            [keyboard(osk::Input::Move(osk::Dir::Left))]
        );
    }

    #[test]
    fn with_the_keyboard_open_the_trackpads_move_nothing() {
        let mut pad = steam_controller();
        feed_keyboard(&mut pad, right_pad((5000, 5000)));
        feed_keyboard(&mut pad, left_pad((5000, 5000)));
        assert_eq!(feed_keyboard(&mut pad, right_pad((9000, 9000))), []);
        assert_eq!(feed_keyboard(&mut pad, left_pad((9000, 9000))), []);
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
        assert!(!pad.is_activity(&syn()));
        assert!(!pad.is_activity(&abs(AbsoluteAxisCode::ABS_PRESSURE, 9000)));
    }
}
