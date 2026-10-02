use smithay::{
    backend::{
        input::{Event, InputEvent, KeyState, KeyboardKeyEvent},
        libinput::LibinputInputBackend,
    },
    input::keyboard::{FilterResult, Keysym, ModifiersState},
    utils::SERIAL_COUNTER,
};

use crate::state::Emrakul;

/// Keys the compositor keeps for itself. Everything else goes to the
/// foreground client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reserved {
    /// Ctrl+Alt+Backspace. While Plasma is still the fallback session, this is
    /// how a keyboard gets back to it.
    Quit,
    /// Ctrl+Alt+F1..F12, so a console is always reachable.
    SwitchVt(i32),
}

fn reserved(modifiers: &ModifiersState, sym: Keysym) -> Option<Reserved> {
    if !(modifiers.ctrl && modifiers.alt) {
        return None;
    }
    if sym == Keysym::BackSpace {
        return Some(Reserved::Quit);
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
        let action = keyboard.input(
            self,
            event.key_code(),
            event.state(),
            SERIAL_COUNTER.next_serial(),
            Event::time_msec(&event),
            |_, modifiers, handle| match reserved(modifiers, handle.modified_sym()) {
                Some(action) if pressed => FilterResult::Intercept(Some(action)),
                // Swallow the release too, or the client sees half a keypress.
                Some(_) => FilterResult::Intercept(None),
                None => FilterResult::Forward,
            },
        );
        match action.flatten() {
            Some(Reserved::Quit) => self.loop_signal.stop(),
            Some(Reserved::SwitchVt(vt)) => self.backend.change_vt(vt),
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
            reserved(&ctrl_alt(), Keysym::BackSpace),
            Some(Reserved::Quit)
        );
    }

    #[test]
    fn vt_switch_keys_map_to_their_number() {
        assert_eq!(
            reserved(&ctrl_alt(), Keysym::XF86_Switch_VT_1),
            Some(Reserved::SwitchVt(1))
        );
        assert_eq!(
            reserved(&ctrl_alt(), Keysym::XF86_Switch_VT_12),
            Some(Reserved::SwitchVt(12))
        );
    }

    #[test]
    fn plain_keys_pass_through() {
        assert_eq!(
            reserved(&ModifiersState::default(), Keysym::BackSpace),
            None
        );
        assert_eq!(reserved(&ctrl_alt(), Keysym::a), None);
    }
}
