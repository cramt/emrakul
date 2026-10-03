//! The keymap a remote keyboard types with.
//!
//! An EI sender is handed a keymap and can only send keycodes from it.
//! kdeconnect turns each character it is sent into a keysym, looks for the
//! first keycode carrying that keysym on any level, and sends that keycode
//! with no modifiers (kdeconnect-kde 26.08,
//! `plugins/mousepad/waylandremoteinput.cpp`, `Xkb::keycodeFromKeysym`).
//! Against the seat's own keymap, `A` would arrive as `a` and `!` as `1`.
//!
//! So the remote keymap has one level per key: the seat's keys with only
//! their unshifted keysym, at the same keycodes, plus one key past the
//! seat's last for every keysym that needs Shift. emrakul presses Shift
//! around those.

use std::{collections::HashMap, fmt::Write};

use smithay::input::keyboard::{Keycode, Keysym, xkb};

/// What a remote keycode is on the seat keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeatKey {
    /// The same key.
    Plain(Keycode),
    /// That key with Shift held around it.
    Shifted(Keycode),
}

pub struct RemoteKeymap {
    /// xkb's text format, as the EI keyboard's keymap.
    text: String,
    /// Every keycode up to here is the seat's own.
    seat_max: Keycode,
    /// The keys past `seat_max`.
    shifted: HashMap<Keycode, Keycode>,
}

impl RemoteKeymap {
    pub fn new(seat: &xkb::Keymap) -> Self {
        let shift = 1 << seat.mod_get_index(xkb::MOD_NAME_SHIFT);
        let mut plain = Vec::new();
        let mut needs_shift = Vec::new();
        for raw in seat.min_keycode().raw()..=seat.max_keycode().raw() {
            let code = Keycode::new(raw);
            if let [sym, ..] = seat.key_get_syms_by_level(code, 0, 0) {
                plain.push((code, *sym));
            }
        }
        let mut typable: Vec<Keysym> = plain.iter().map(|(_, sym)| *sym).collect();
        for raw in seat.min_keycode().raw()..=seat.max_keycode().raw() {
            let code = Keycode::new(raw);
            for level in 1..seat.num_levels_for_key(code, 0) {
                let Some(&sym) = seat.key_get_syms_by_level(code, 0, level).first() else {
                    continue;
                };
                if typable.contains(&sym) || !level_masks(seat, code, level).contains(&shift) {
                    continue;
                }
                typable.push(sym);
                needs_shift.push((code, sym));
            }
        }

        let seat_max = seat.max_keycode();
        let extra = |i: usize| Keycode::new(seat_max.raw() + 1 + i as u32);
        let shifted = needs_shift
            .iter()
            .enumerate()
            .map(|(i, (code, _))| (extra(i), *code))
            .collect();
        let keys: Vec<(Keycode, Keysym)> = plain
            .into_iter()
            .chain(
                needs_shift
                    .iter()
                    .enumerate()
                    .map(|(i, (_, sym))| (extra(i), *sym)),
            )
            .collect();
        // kdeconnect searches `min..max`, leaving the maximum keycode out, so
        // the last real key is followed by one with nothing on it.
        let padding = extra(needs_shift.len());
        Self {
            text: keymap_text(&keys, padding),
            seat_max,
            shifted,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// The seat key behind a remote keycode, `None` for one the remote
    /// keymap doesn't have.
    pub fn seat_key(&self, remote: Keycode) -> Option<SeatKey> {
        if remote <= self.seat_max {
            return Some(SeatKey::Plain(remote));
        }
        self.shifted.get(&remote).copied().map(SeatKey::Shifted)
    }
}

/// The modifier masks that pick `level` on `code` in layout 0.
fn level_masks(keymap: &xkb::Keymap, code: Keycode, level: u32) -> Vec<u32> {
    let mut masks = [0; 8];
    let n = keymap.key_get_mods_for_level(code, 0, level, &mut masks);
    masks[..n].to_vec()
}

/// A self-contained keymap: nothing in it is looked up in the receiver's
/// xkeyboard-config, so it compiles the same everywhere. It has no
/// modifier actions, because the only thing a sender does with it is find
/// keycodes.
fn keymap_text(keys: &[(Keycode, Keysym)], padding: Keycode) -> String {
    let min = keys.iter().map(|(c, _)| c.raw()).min().unwrap_or(8);
    let mut text = String::from("xkb_keymap {\nxkb_keycodes \"emrakul\" {\n");
    let _ = writeln!(text, "minimum = {min};\nmaximum = {};", padding.raw());
    for (code, _) in keys {
        let _ = writeln!(text, "<K{0}> = {0};", code.raw());
    }
    let _ = writeln!(text, "<K{0}> = {0};", padding.raw());
    text.push_str(concat!(
        "};\n",
        "xkb_types \"emrakul\" {\n",
        "type \"ONE_LEVEL\" { modifiers = none; level_name[Level1] = \"Any\"; };\n",
        "};\n",
        "xkb_compat \"emrakul\" { };\n",
        "xkb_symbols \"emrakul\" {\n",
    ));
    for (code, sym) in keys {
        let _ = writeln!(
            text,
            "key <K{}> {{ type = \"ONE_LEVEL\", [ {} ] }};",
            code.raw(),
            xkb::keysym_get_name(*sym)
        );
    }
    text.push_str("};\n};\n");
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn us() -> xkb::Keymap {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        xkb::Keymap::new_from_names(&context, "evdev", "pc105", "us", "", None, 0)
            .expect("the us layout")
    }

    fn compile(text: &str) -> xkb::Keymap {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        xkb::Keymap::new_from_string(
            &context,
            text.to_owned(),
            xkb::KEYMAP_FORMAT_TEXT_V1,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .expect("the remote keymap compiles")
    }

    /// kdeconnect's `Xkb::keycodeFromKeysym`, as it reads the keymap it
    /// is given.
    fn kdeconnect_keycode(keymap: &xkb::Keymap, sym: Keysym) -> Option<Keycode> {
        for raw in keymap.min_keycode().raw()..keymap.max_keycode().raw() {
            let code = Keycode::new(raw);
            for level in 0..keymap.num_levels_for_key(code, 0) {
                if keymap.key_get_syms_by_level(code, 0, level).contains(&sym) {
                    return Some(code);
                }
            }
        }
        None
    }

    fn typed(remote: &RemoteKeymap, c: char) -> Option<SeatKey> {
        let sent = kdeconnect_keycode(&compile(remote.text()), xkb::utf32_to_keysym(c as u32))?;
        remote.seat_key(sent)
    }

    // xkb keycodes, evdev's plus 8.
    const A: Keycode = Keycode::new(38);
    const ONE: Keycode = Keycode::new(10);
    const SLASH: Keycode = Keycode::new(61);

    #[test]
    fn lowercase_and_digits_are_the_seats_own_keys() {
        let remote = RemoteKeymap::new(&us());
        assert_eq!(typed(&remote, 'a'), Some(SeatKey::Plain(A)));
        assert_eq!(typed(&remote, '1'), Some(SeatKey::Plain(ONE)));
        assert_eq!(typed(&remote, '/'), Some(SeatKey::Plain(SLASH)));
    }

    #[test]
    fn capitals_and_shifted_symbols_hold_shift() {
        let remote = RemoteKeymap::new(&us());
        assert_eq!(typed(&remote, 'A'), Some(SeatKey::Shifted(A)));
        assert_eq!(typed(&remote, '!'), Some(SeatKey::Shifted(ONE)));
        assert_eq!(typed(&remote, '?'), Some(SeatKey::Shifted(SLASH)));
    }

    #[test]
    fn every_printable_ascii_character_can_be_typed() {
        let remote = RemoteKeymap::new(&us());
        let missing: String = (' '..='~')
            .filter(|&c| typed(&remote, c).is_none())
            .collect();
        assert_eq!(missing, "");
    }

    #[test]
    fn keycodes_sent_as_evdev_codes_pass_straight_through() {
        // kdeconnect sends Backspace, the arrows and the modifiers by
        // evdev code, never through the keymap.
        let remote = RemoteKeymap::new(&us());
        let backspace = Keycode::new(14 + 8);
        assert_eq!(remote.seat_key(backspace), Some(SeatKey::Plain(backspace)));
    }

    #[test]
    fn a_keycode_past_the_keymap_is_nothing() {
        let remote = RemoteKeymap::new(&us());
        assert_eq!(remote.seat_key(Keycode::new(100_000)), None);
    }
}
