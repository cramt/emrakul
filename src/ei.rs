//! Remote input: a keyboard and pointer driven by another program over EI
//! (libei's protocol), such as KDE Connect turning a phone into a touchpad
//! and keyboard. Programs get here through the RemoteDesktop portal
//! (`emrakul-portal`), which connects them to emrakul's EIS socket.
//!
//! Remote input is a real keyboard and mouse as far as the rest of emrakul
//! goes: it is activity, the press that wakes the screen does nothing else,
//! keys reach Home or the foreground app the way a USB keyboard's do, and
//! the pointer is the trackpad's.

mod keymap;

use std::{collections::HashMap, io::Write, os::fd::AsFd};

use reis::{
    calloop::{EisListenerSource, EisRequestSource, EisRequestSourceEvent},
    eis::{
        self,
        device::DeviceType,
        handshake::ContextType,
        keyboard::{KeyState as EiKeyState, KeymapType},
    },
    enumflags2::BitFlags,
    request::{Connection, Device, DeviceCapability, EisRequest},
};
use rustix::fs::{MemfdFlags, SealFlags};
use smithay::{
    backend::input::{ButtonState, KeyState},
    input::keyboard::{Keycode, xkb},
    reexports::calloop::PostAction,
    utils::{Logical, Point},
};

use crate::{eis_socket, idle::Activity, state::Emrakul};

pub use keymap::{RemoteKeymap, SeatKey};

/// `KEY_LEFTSHIFT`, as an xkb keycode: what a shifted remote key holds.
const SHIFT: Keycode = Keycode::new(42 + 8);

/// A request from a sender, cut down to what emrakul does with it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Request {
    /// A key in the remote keymap, as an xkb keycode.
    Key(Keycode, KeyState),
    /// Relative motion, in client (logical) pixels.
    Motion(Point<f64, Logical>),
    /// Where on the screen, in client pixels.
    MotionAbsolute(Point<f64, Logical>),
    /// A `BTN_*` code.
    Button(u32, ButtonState),
    /// Smooth scrolling, in client pixels.
    Scroll(Point<f64, Logical>),
    /// Wheel clicks, in 120ths of a click.
    ScrollDiscrete(Point<i32, Logical>),
    ScrollStop,
}

impl Request {
    /// Releases and stops aren't activity: only something the person does is.
    fn is_activity(&self) -> bool {
        match self {
            Request::Key(_, state) => *state == KeyState::Pressed,
            Request::Button(_, state) => *state == ButtonState::Pressed,
            Request::Motion(_)
            | Request::MotionAbsolute(_)
            | Request::Scroll(_)
            | Request::ScrollDiscrete(_) => true,
            Request::ScrollStop => false,
        }
    }
}

/// What a request does to the seat.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    Key(Keycode, KeyState),
    MoveBy(Point<f64, Logical>),
    MoveTo(Point<f64, Logical>),
    Button(u32, ButtonState),
    Scroll(Point<f64, Logical>),
    ScrollDiscrete(Point<i32, Logical>),
    ScrollStop,
}

/// A key or button a sender is holding down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Held<T> {
    /// Its press woke the screen, so its release does nothing either.
    Swallowed,
    Down(T),
}

/// One connected sender: what it holds down, so a sender that goes away
/// mid-press leaves nothing stuck.
#[derive(Debug, Default)]
pub struct Sender {
    keys: HashMap<Keycode, Held<SeatKey>>,
    buttons: HashMap<u32, Held<()>>,
}

impl Sender {
    /// `woke`: the request was activity that woke the screen, and does
    /// nothing else.
    pub fn on_request(
        &mut self,
        keymap: &RemoteKeymap,
        request: Request,
        woke: bool,
    ) -> Vec<Action> {
        match request {
            Request::Key(code, KeyState::Pressed) => {
                if self.keys.contains_key(&code) {
                    return Vec::new();
                }
                if woke {
                    self.keys.insert(code, Held::Swallowed);
                    return Vec::new();
                }
                let Some(key) = keymap.seat_key(code) else {
                    tracing::debug!(?code, "remote key not in its keymap");
                    return Vec::new();
                };
                self.keys.insert(code, Held::Down(key));
                press(key)
            }
            Request::Key(code, KeyState::Released) => match self.keys.remove(&code) {
                Some(Held::Down(key)) => release(key),
                Some(Held::Swallowed) | None => Vec::new(),
            },
            Request::Button(button, ButtonState::Pressed) => {
                if self.buttons.contains_key(&button) {
                    return Vec::new();
                }
                if woke {
                    self.buttons.insert(button, Held::Swallowed);
                    return Vec::new();
                }
                self.buttons.insert(button, Held::Down(()));
                vec![Action::Button(button, ButtonState::Pressed)]
            }
            Request::Button(button, ButtonState::Released) => match self.buttons.remove(&button) {
                Some(Held::Down(())) => vec![Action::Button(button, ButtonState::Released)],
                Some(Held::Swallowed) | None => Vec::new(),
            },
            _ if woke => Vec::new(),
            Request::Motion(by) => vec![Action::MoveBy(by)],
            Request::MotionAbsolute(to) => vec![Action::MoveTo(to)],
            Request::Scroll(by) => vec![Action::Scroll(by)],
            Request::ScrollDiscrete(by) => vec![Action::ScrollDiscrete(by)],
            Request::ScrollStop => vec![Action::ScrollStop],
        }
    }

    /// Lets go of everything it holds, as it goes.
    pub fn release_all(&mut self) -> Vec<Action> {
        let keys = self.keys.drain().flat_map(|(_, held)| match held {
            Held::Down(key) => release(key),
            Held::Swallowed => Vec::new(),
        });
        let buttons = self.buttons.drain().filter_map(|(button, held)| {
            (held == Held::Down(())).then_some(Action::Button(button, ButtonState::Released))
        });
        keys.chain(buttons).collect()
    }
}

fn press(key: SeatKey) -> Vec<Action> {
    match key {
        SeatKey::Plain(code) => vec![Action::Key(code, KeyState::Pressed)],
        SeatKey::Shifted(code) => vec![
            Action::Key(SHIFT, KeyState::Pressed),
            Action::Key(code, KeyState::Pressed),
        ],
    }
}

fn release(key: SeatKey) -> Vec<Action> {
    match key {
        SeatKey::Plain(code) => vec![Action::Key(code, KeyState::Released)],
        SeatKey::Shifted(code) => vec![
            Action::Key(code, KeyState::Released),
            Action::Key(SHIFT, KeyState::Released),
        ],
    }
}

/// The capabilities emrakul's one seat offers. No touch screen: nothing on
/// the TV is drawn for touch.
fn seat_capabilities() -> BitFlags<DeviceCapability> {
    DeviceCapability::Keyboard
        | DeviceCapability::Pointer
        | DeviceCapability::PointerAbsolute
        | DeviceCapability::Button
        | DeviceCapability::Scroll
}

pub struct RemoteInput {
    keymap: RemoteKeymap,
    /// The keymap's text, NUL-terminated, sealed, for every keyboard sent out.
    keymap_fd: std::os::fd::OwnedFd,
    keymap_size: u32,
}

impl RemoteInput {
    /// The keymap is built from the same names (empty: the environment's,
    /// else us) as the seat keyboard's `XkbConfig::default()`.
    pub fn new() -> anyhow::Result<Self> {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let seat = xkb::Keymap::new_from_names(&context, "", "", "", "", None, 0)
            .ok_or_else(|| anyhow::anyhow!("compiling the seat keymap"))?;
        let keymap = RemoteKeymap::new(&seat);
        let fd = rustix::fs::memfd_create(
            "emrakul-remote-keymap",
            MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING,
        )?;
        let mut file = std::fs::File::from(fd);
        file.write_all(keymap.text().as_bytes())?;
        file.write_all(b"\0")?;
        let keymap_fd = std::os::fd::OwnedFd::from(file);
        rustix::fs::fcntl_add_seals(
            &keymap_fd,
            SealFlags::SEAL | SealFlags::SHRINK | SealFlags::GROW | SealFlags::WRITE,
        )?;
        Ok(Self {
            keymap_size: u32::try_from(keymap.text().len() + 1)?,
            keymap,
            keymap_fd,
        })
    }
}

/// The devices one sender has bound.
#[derive(Default)]
struct Devices {
    keyboard: Option<Device>,
    pointer: Option<Device>,
    absolute: Option<Device>,
}

impl Emrakul {
    /// Listens on the EIS socket. Without `XDG_RUNTIME_DIR` there is no
    /// remote input, and nothing else is affected.
    pub fn listen_for_remote_input(&mut self) -> anyhow::Result<()> {
        let path = eis_socket::path().ok_or_else(|| anyhow::anyhow!("XDG_RUNTIME_DIR is unset"))?;
        // Left behind by an emrakul that didn't get to clean up.
        match std::fs::remove_file(&path) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => return Err(err.into()),
            _ => {}
        }
        let listener = eis::Listener::bind(&path)?;
        self.loop_handle
            .insert_source(EisListenerSource::new(listener), |context, (), state| {
                state.accept_remote(context);
                Ok(PostAction::Continue)
            })
            .map_err(|e| anyhow::anyhow!("listening for remote input: {e}"))?;
        tracing::info!(path = %path.display(), "remote input");
        Ok(())
    }

    fn accept_remote(&mut self, context: eis::Context) {
        let mut sender = Sender::default();
        let mut devices = Devices::default();
        let source = EisRequestSource::new(context, 1);
        let inserted = self
            .loop_handle
            .insert_source(source, move |event, connection, state| {
                let post = match event {
                    Ok(EisRequestSourceEvent::Connected) => {
                        // A receiver wants to capture input, which emrakul never
                        // hands out.
                        if connection.context_type() != ContextType::Sender {
                            connection.disconnected(
                                eis::connection::DisconnectReason::Mode,
                                Some("emrakul only takes input"),
                            );
                            PostAction::Remove
                        } else {
                            tracing::info!(name = ?connection.name(), "remote input connected");
                            // The connection keeps its seats; Bind hands this one back.
                            let _ = connection.add_seat(Some("emrakul"), seat_capabilities());
                            PostAction::Continue
                        }
                    }
                    Ok(EisRequestSourceEvent::Request(EisRequest::Disconnect)) => {
                        tracing::info!(name = ?connection.name(), "remote input disconnected");
                        PostAction::Remove
                    }
                    Ok(EisRequestSourceEvent::Request(request)) => {
                        state.on_remote_request(connection, &mut devices, &mut sender, request);
                        PostAction::Continue
                    }
                    // kdeconnectd and libei senders close the socket
                    // rather than saying goodbye.
                    Err(reis::Error::Io(err))
                        if err.kind() == std::io::ErrorKind::UnexpectedEof =>
                    {
                        tracing::info!(name = ?connection.name(), "remote input disconnected");
                        PostAction::Remove
                    }
                    Err(err) => {
                        tracing::warn!(%err, name = ?connection.name(), "remote input");
                        PostAction::Remove
                    }
                };
                if post == PostAction::Remove {
                    for action in sender.release_all() {
                        state.apply_remote(action);
                    }
                }
                let _ = connection.flush();
                Ok(post)
            });
        if let Err(err) = inserted {
            tracing::warn!(?err, "accepting remote input");
        }
    }

    fn on_remote_request(
        &mut self,
        connection: &Connection,
        devices: &mut Devices,
        sender: &mut Sender,
        request: EisRequest,
    ) {
        let request = match request {
            EisRequest::Bind(bind) => {
                self.add_remote_devices(&bind.seat, bind.capabilities, devices);
                return;
            }
            EisRequest::DeviceClosed(closed) => {
                closed.device.remove();
                return;
            }
            EisRequest::KeyboardKey(key) => Request::Key(
                Keycode::new(key.key + 8),
                match key.state {
                    EiKeyState::Press => KeyState::Pressed,
                    EiKeyState::Released => KeyState::Released,
                },
            ),
            EisRequest::PointerMotion(m) => {
                Request::Motion((f64::from(m.dx), f64::from(m.dy)).into())
            }
            EisRequest::PointerMotionAbsolute(m) => {
                Request::MotionAbsolute((f64::from(m.dx_absolute), f64::from(m.dy_absolute)).into())
            }
            EisRequest::Button(b) => Request::Button(
                b.button,
                match b.state {
                    eis::button::ButtonState::Press => ButtonState::Pressed,
                    eis::button::ButtonState::Released => ButtonState::Released,
                },
            ),
            EisRequest::ScrollDelta(s) => {
                Request::Scroll((f64::from(s.dx), f64::from(s.dy)).into())
            }
            EisRequest::ScrollDiscrete(s) => {
                Request::ScrollDiscrete((s.discrete_dx, s.discrete_dy).into())
            }
            EisRequest::ScrollStop(_) | EisRequest::ScrollCancel(_) => Request::ScrollStop,
            // Frames group events; each is applied as it comes. The rest
            // are capabilities the seat doesn't offer, or bookkeeping.
            other => {
                tracing::trace!(?other, name = ?connection.name(), "remote request ignored");
                return;
            }
        };
        let woke = request.is_activity() && self.on_activity() == Activity::Woke;
        for action in sender.on_request(&self.remote_input.keymap, request, woke) {
            self.apply_remote(action);
        }
    }

    fn add_remote_devices(
        &mut self,
        seat: &reis::request::Seat,
        capabilities: BitFlags<DeviceCapability>,
        devices: &mut Devices,
    ) {
        let remote = &self.remote_input;
        if capabilities.contains(DeviceCapability::Keyboard) && devices.keyboard.is_none() {
            let device = seat.add_device(
                Some("emrakul keyboard"),
                DeviceType::Virtual,
                DeviceCapability::Keyboard.into(),
                |device| {
                    if let Some(keyboard) = device.interface::<eis::Keyboard>() {
                        keyboard.keymap(
                            KeymapType::Xkb,
                            remote.keymap_size,
                            remote.keymap_fd.as_fd(),
                        );
                    }
                },
            );
            device.resumed();
            devices.keyboard = Some(device);
        }
        if capabilities.contains(DeviceCapability::Pointer) && devices.pointer.is_none() {
            let device = seat.add_device(
                Some("emrakul pointer"),
                DeviceType::Virtual,
                DeviceCapability::Pointer | DeviceCapability::Button | DeviceCapability::Scroll,
                |_| {},
            );
            device.resumed();
            devices.pointer = Some(device);
        }
        // An absolute pointer needs the screen's size for its region, in
        // the client pixels the pointer lives in.
        if let Some(size) = self.backend.output_size()
            && capabilities.contains(DeviceCapability::PointerAbsolute)
            && devices.absolute.is_none()
        {
            let scale = self.backend.scale() as f32;
            let device = seat.add_device(
                Some("emrakul absolute pointer"),
                DeviceType::Virtual,
                DeviceCapability::PointerAbsolute
                    | DeviceCapability::Button
                    | DeviceCapability::Scroll,
                |device| {
                    device
                        .device()
                        .region(0, 0, size.w as u32, size.h as u32, scale);
                },
            );
            device.resumed();
            devices.absolute = Some(device);
        }
    }

    fn apply_remote(&mut self, action: Action) {
        tracing::trace!(?action, "remote input");
        match action {
            Action::Key(code, state) => self.key(code, state, self.clock.now().as_millis(), false),
            Action::MoveBy(by) => {
                if let Some(pointer) = self.seat.get_pointer() {
                    self.warp_pointer(pointer.current_location() + by);
                }
            }
            Action::MoveTo(to) => self.warp_pointer(to),
            Action::Button(button, state) => self.button(button, state),
            Action::Scroll(by) => self.scroll_continuous(by),
            Action::ScrollDiscrete(by) => self.scroll_wheel(by),
            Action::ScrollStop => self.scroll(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keymap() -> RemoteKeymap {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let us = xkb::Keymap::new_from_names(&context, "evdev", "pc105", "us", "", None, 0)
            .expect("the us layout");
        RemoteKeymap::new(&us)
    }

    const A: Keycode = Keycode::new(38);
    const LEFT: u32 = 0x110;

    /// The keycode the remote keymap gives `A`, past the seat's own.
    fn capital_a(keymap: &RemoteKeymap) -> Keycode {
        (256..2048)
            .map(Keycode::new)
            .find(|&code| keymap.seat_key(code) == Some(SeatKey::Shifted(A)))
            .expect("a key for A")
    }

    #[test]
    fn a_plain_key_presses_and_releases_its_seat_key() {
        let keymap = keymap();
        let mut sender = Sender::default();
        assert_eq!(
            sender.on_request(&keymap, Request::Key(A, KeyState::Pressed), false),
            [Action::Key(A, KeyState::Pressed)]
        );
        assert_eq!(
            sender.on_request(&keymap, Request::Key(A, KeyState::Released), false),
            [Action::Key(A, KeyState::Released)]
        );
    }

    #[test]
    fn a_shifted_key_holds_shift_around_it() {
        let keymap = keymap();
        let mut sender = Sender::default();
        let code = capital_a(&keymap);
        assert_eq!(
            sender.on_request(&keymap, Request::Key(code, KeyState::Pressed), false),
            [
                Action::Key(SHIFT, KeyState::Pressed),
                Action::Key(A, KeyState::Pressed)
            ]
        );
        assert_eq!(
            sender.on_request(&keymap, Request::Key(code, KeyState::Released), false),
            [
                Action::Key(A, KeyState::Released),
                Action::Key(SHIFT, KeyState::Released)
            ]
        );
    }

    #[test]
    fn a_press_that_wakes_the_screen_is_swallowed_with_its_release() {
        let keymap = keymap();
        let mut sender = Sender::default();
        assert_eq!(
            sender.on_request(&keymap, Request::Key(A, KeyState::Pressed), true),
            []
        );
        assert_eq!(
            sender.on_request(&keymap, Request::Key(A, KeyState::Released), false),
            []
        );
        assert_eq!(
            sender.on_request(&keymap, Request::Button(LEFT, ButtonState::Pressed), true),
            []
        );
        assert_eq!(
            sender.on_request(&keymap, Request::Button(LEFT, ButtonState::Released), false),
            []
        );
    }

    #[test]
    fn motion_that_wakes_the_screen_moves_nothing() {
        let keymap = keymap();
        let mut sender = Sender::default();
        let by = Point::from((10.0, 5.0));
        assert_eq!(sender.on_request(&keymap, Request::Motion(by), true), []);
        assert_eq!(
            sender.on_request(&keymap, Request::Motion(by), false),
            [Action::MoveBy(by)]
        );
    }

    #[test]
    fn a_sender_going_away_releases_what_it_held() {
        let keymap = keymap();
        let mut sender = Sender::default();
        let code = capital_a(&keymap);
        sender.on_request(&keymap, Request::Key(code, KeyState::Pressed), false);
        sender.on_request(&keymap, Request::Button(LEFT, ButtonState::Pressed), false);
        let mut released = sender.release_all();
        released.sort_by_key(|a| format!("{a:?}"));
        assert_eq!(
            released,
            [
                Action::Button(LEFT, ButtonState::Released),
                Action::Key(A, KeyState::Released),
                Action::Key(SHIFT, KeyState::Released),
            ]
        );
        assert_eq!(sender.release_all(), []);
    }

    #[test]
    fn a_repeated_press_presses_once() {
        let keymap = keymap();
        let mut sender = Sender::default();
        sender.on_request(&keymap, Request::Key(A, KeyState::Pressed), false);
        assert_eq!(
            sender.on_request(&keymap, Request::Key(A, KeyState::Pressed), false),
            []
        );
    }

    #[test]
    fn releases_and_stops_are_not_activity() {
        assert!(Request::Key(A, KeyState::Pressed).is_activity());
        assert!(!Request::Key(A, KeyState::Released).is_activity());
        assert!(!Request::Button(LEFT, ButtonState::Released).is_activity());
        assert!(!Request::ScrollStop.is_activity());
        assert!(Request::Scroll((0.0, 1.0).into()).is_activity());
    }
}
