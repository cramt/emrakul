use smithay::{
    backend::{
        input::{Event, InputEvent, KeyState, KeyboardKeyEvent},
        libinput::LibinputInputBackend,
    },
    input::keyboard::{FilterResult, Keysym, ModifiersState},
    utils::SERIAL_COUNTER,
};

use crate::{idle::Activity, lifecycle::HomeKey, state::Emrakul};

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
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        let pressed = event.state() == KeyState::Pressed;
        let code = event.key_code();
        let woke = pressed && self.on_activity() == Activity::Woke;
        let action = keyboard.input(
            self,
            code,
            event.state(),
            SERIAL_COUNTER.next_serial(),
            Event::time_msec(&event),
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
