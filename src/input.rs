//! Keys and the pointer, from a keyboard or a controller, to Home or the
//! foreground client.

use smithay::{
    backend::{
        input::{Axis, AxisSource, ButtonState, Event, InputEvent, KeyState, KeyboardKeyEvent},
        libinput::LibinputInputBackend,
    },
    desktop::WindowSurfaceType,
    input::{
        keyboard::{FilterResult, Keycode, Keysym, ModifiersState},
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, SERIAL_COUNTER},
};

use crate::{idle::Activity, lifecycle::HomeKey, state::Emrakul};

/// `BTN_LEFT`, which wl_pointer speaks in.
const LEFT_BUTTON: u32 = 0x110;

/// Keys the compositor keeps for itself. Everything else goes to the
/// foreground client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reserved {
    /// Ctrl+Alt+Backspace. While Plasma is still the fallback session, this is
    /// how a keyboard gets back to it.
    Quit,
    /// Ctrl+Alt+F1..F12, so a console is always reachable.
    SwitchVt(i32),
    /// Ctrl+Alt+H, the keyboard's stand-in for the Steam button.
    GoHome,
    /// Arrows and Enter, only while Home is on screen.
    Home(HomeKey),
}

fn reserved(modifiers: &ModifiersState, sym: Keysym, home_has_keyboard: bool) -> Option<Reserved> {
    if home_has_keyboard && !(modifiers.ctrl || modifiers.alt || modifiers.logo) {
        return match sym {
            Keysym::Left | Keysym::Up => Some(Reserved::Home(HomeKey::Previous)),
            Keysym::Right | Keysym::Down => Some(Reserved::Home(HomeKey::Next)),
            Keysym::Return | Keysym::KP_Enter => Some(Reserved::Home(HomeKey::Launch)),
            _ => None,
        };
    }
    if !(modifiers.ctrl && modifiers.alt) {
        return None;
    }
    match sym {
        Keysym::BackSpace => return Some(Reserved::Quit),
        Keysym::h | Keysym::H => return Some(Reserved::GoHome),
        _ => {}
    }
    let vt1 = Keysym::XF86_Switch_VT_1.raw();
    let raw = sym.raw();
    (vt1..vt1 + 12)
        .contains(&raw)
        .then(|| Reserved::SwitchVt((raw - vt1 + 1) as i32))
}

impl Emrakul {
    pub fn on_input(&mut self, event: InputEvent<LibinputInputBackend>) {
        let InputEvent::Keyboard { event } = event else {
            return;
        };
        let woke = event.state() == KeyState::Pressed && self.on_activity() == Activity::Woke;
        self.key(
            event.key_code(),
            event.state(),
            Event::time_msec(&event),
            woke,
        );
    }

    /// A controller's key. It has already been through Idle: a press that
    /// woke the screen never gets here.
    pub fn pad_key(&mut self, key: evdev::KeyCode, state: KeyState) {
        // xkb keycodes are evdev's plus 8.
        let code = Keycode::new(u32::from(key.0) + 8);
        self.key(code, state, self.clock.now().as_millis(), false);
    }

    /// A key on the seat keyboard. `woke` is whether its press just woke the
    /// screen, in which case it and its release do nothing.
    fn key(&mut self, code: Keycode, key_state: KeyState, time: u32, woke: bool) {
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        let pressed = key_state == KeyState::Pressed;
        let action = keyboard.input(
            self,
            code,
            key_state,
            SERIAL_COUNTER.next_serial(),
            time,
            |state, modifiers, handle| {
                if woke {
                    state.waking_key = Some(code);
                    return FilterResult::Intercept(None);
                }
                if !pressed && state.waking_key == Some(code) {
                    state.waking_key = None;
                    return FilterResult::Intercept(None);
                }
                match reserved(modifiers, handle.modified_sym(), state.home_has_keyboard()) {
                    Some(action) if pressed => FilterResult::Intercept(Some(action)),
                    // Swallow the release too, or the client sees half a keypress.
                    Some(_) => FilterResult::Intercept(None),
                    None => FilterResult::Forward,
                }
            },
        );
        match action.flatten() {
            Some(Reserved::Quit) => self.loop_signal.stop(),
            Some(Reserved::SwitchVt(vt)) => self.backend.change_vt(vt),
            Some(Reserved::GoHome) => self.go_home(),
            Some(Reserved::Home(key)) => self.on_home_key(key),
            None => {}
        }
    }

    /// Moves the pointer, kept on the screen, to whatever surface is under it.
    pub fn move_pointer(&mut self, by: Point<f64, Logical>) {
        let (Some(pointer), Some(size)) = (self.seat.get_pointer(), self.backend.output_size())
        else {
            return;
        };
        let to = pointer.current_location() + by;
        let to = Point::from((
            to.x.clamp(0.0, f64::from(size.w - 1)),
            to.y.clamp(0.0, f64::from(size.h - 1)),
        ));
        let event = MotionEvent {
            location: to,
            serial: SERIAL_COUNTER.next_serial(),
            time: self.clock.now().as_millis(),
        };
        pointer.motion(self, self.surface_under(to), &event);
        pointer.frame(self);
        self.backend.request_redraw(&self.loop_handle);
    }

    fn surface_under(&self, at: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        let (window, location) = self.space.element_under(at)?;
        let (surface, offset) =
            window.surface_under(at - location.to_f64(), WindowSurfaceType::ALL)?;
        Some((surface, (location + offset).to_f64()))
    }

    pub fn click(&mut self, state: ButtonState) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let event = ButtonEvent {
            serial: SERIAL_COUNTER.next_serial(),
            time: self.clock.now().as_millis(),
            button: LEFT_BUTTON,
            state,
        };
        pointer.button(self, &event);
        pointer.frame(self);
    }

    /// Scrolls by `by`, or with `None`, ends the scroll as the finger lifts.
    /// It is a touchpad-style scroll (`finger`), so the client scrolls
    /// smoothly and may coast once it ends.
    pub fn scroll(&mut self, by: Option<Point<f64, Logical>>) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let frame = AxisFrame::new(self.clock.now().as_millis()).source(AxisSource::Finger);
        let frame = match by {
            Some(by) => frame
                .value(Axis::Horizontal, by.x)
                .value(Axis::Vertical, by.y),
            None => frame.stop(Axis::Horizontal).stop(Axis::Vertical),
        };
        pointer.axis(self, frame);
        pointer.frame(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctrl_alt() -> ModifiersState {
        ModifiersState {
            ctrl: true,
            alt: true,
            ..Default::default()
        }
    }

    #[test]
    fn ctrl_alt_backspace_quits() {
        assert_eq!(
            reserved(&ctrl_alt(), Keysym::BackSpace, false),
            Some(Reserved::Quit)
        );
    }

    #[test]
    fn vt_switch_keys_map_to_their_number() {
        assert_eq!(
            reserved(&ctrl_alt(), Keysym::XF86_Switch_VT_1, false),
            Some(Reserved::SwitchVt(1))
        );
        assert_eq!(
            reserved(&ctrl_alt(), Keysym::XF86_Switch_VT_12, true),
            Some(Reserved::SwitchVt(12))
        );
    }

    #[test]
    fn ctrl_alt_h_goes_home_from_anywhere() {
        assert_eq!(
            reserved(&ctrl_alt(), Keysym::h, false),
            Some(Reserved::GoHome)
        );
        assert_eq!(
            reserved(&ctrl_alt(), Keysym::h, true),
            Some(Reserved::GoHome)
        );
    }

    #[test]
    fn arrows_and_enter_drive_home_only_while_it_has_the_keyboard() {
        let none = ModifiersState::default();
        assert_eq!(
            reserved(&none, Keysym::Right, true),
            Some(Reserved::Home(HomeKey::Next))
        );
        assert_eq!(
            reserved(&none, Keysym::Up, true),
            Some(Reserved::Home(HomeKey::Previous))
        );
        assert_eq!(
            reserved(&none, Keysym::Return, true),
            Some(Reserved::Home(HomeKey::Launch))
        );
        assert_eq!(reserved(&none, Keysym::Return, false), None);
        assert_eq!(reserved(&none, Keysym::a, true), None);
    }

    #[test]
    fn plain_keys_pass_through() {
        assert_eq!(
            reserved(&ModifiersState::default(), Keysym::BackSpace, false),
            None
        );
        assert_eq!(reserved(&ctrl_alt(), Keysym::a, false), None);
    }
}
