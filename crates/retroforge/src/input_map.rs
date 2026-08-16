//! `egui::Key -> rf_input::Key` translation (ticket W1-07).
//!
//! One of exactly two modules in this crate allowed to depend on
//! `egui`/`eframe` (`crate` module doc) — `rf-input` itself stays
//! host-agnostic (`scripts/validate-arch.sh` + that crate's own doc), so
//! this table is the seam between egui's key type and this crate's own.
//!
//! Verified against the pinned `egui-0.35.0` source
//! (`~/.cargo/registry/src/*/egui-0.35.0/src/data/key.rs`), not training
//! data: `egui::Key` has `ArrowUp`/`ArrowDown`/`ArrowLeft`/`ArrowRight`,
//! `Enter`, and (among the modifier-key variants exposed as distinct
//! left/right pairs) `ShiftRight` — `egui::InputState::keys_down` is a
//! `HashSet<egui::Key>` and `egui::InputState::key_down` checks membership
//! in it (`egui-0.35.0/src/input_state/mod.rs:325,766`).
use eframe::egui;

/// Translate one `egui::Key` press to this crate's host-agnostic
/// [`rf_input::Key`], if it's one the default NES keymap uses. Keys egui
/// reports that have no NES-keymap meaning (letters other than Z/X,
/// function keys, punctuation, ...) map to `None` — an unmapped key is not
/// an error, it's simply not bound to anything.
/// Translate one `egui::Key` press to this crate's host-agnostic
/// [`rf_input::Key`], or `None` for a key no binding can use.
///
/// **Widened by ticket W2-06.** W1-07's version mapped only the eight keys
/// the default NES keymap uses, which was right then and wrong the moment a
/// remap UI existed: a user who bound Start to `Q` would have found `Q`
/// silently doing nothing, because the translation dropped it before the
/// keymap ever saw it. The table now covers every [`rf_input::Key`], and
/// [`egui_key_for`] is its inverse — the two are checked against each other
/// in this module's tests, since a one-way table is exactly how the bug
/// above comes back.
#[must_use]
pub fn map_key(key: egui::Key) -> Option<rf_input::Key> {
    use rf_input::Key as K;
    Some(match key {
        egui::Key::ArrowUp => K::ArrowUp,
        egui::Key::ArrowDown => K::ArrowDown,
        egui::Key::ArrowLeft => K::ArrowLeft,
        egui::Key::ArrowRight => K::ArrowRight,
        egui::Key::A => K::A,
        egui::Key::B => K::B,
        egui::Key::C => K::C,
        egui::Key::D => K::D,
        egui::Key::E => K::E,
        egui::Key::F => K::F,
        egui::Key::G => K::G,
        egui::Key::H => K::H,
        egui::Key::I => K::I,
        egui::Key::J => K::J,
        egui::Key::K => K::K,
        egui::Key::L => K::L,
        egui::Key::M => K::M,
        egui::Key::N => K::N,
        egui::Key::O => K::O,
        egui::Key::P => K::P,
        egui::Key::Q => K::Q,
        egui::Key::R => K::R,
        egui::Key::S => K::S,
        egui::Key::T => K::T,
        egui::Key::U => K::U,
        egui::Key::V => K::V,
        egui::Key::W => K::W,
        egui::Key::X => K::X,
        egui::Key::Y => K::Y,
        egui::Key::Z => K::Z,
        egui::Key::Num0 => K::Num0,
        egui::Key::Num1 => K::Num1,
        egui::Key::Num2 => K::Num2,
        egui::Key::Num3 => K::Num3,
        egui::Key::Num4 => K::Num4,
        egui::Key::Num5 => K::Num5,
        egui::Key::Num6 => K::Num6,
        egui::Key::Num7 => K::Num7,
        egui::Key::Num8 => K::Num8,
        egui::Key::Num9 => K::Num9,
        egui::Key::Space => K::Space,
        egui::Key::Enter => K::Enter,
        egui::Key::Tab => K::Tab,
        egui::Key::Backspace => K::Backspace,
        egui::Key::Escape => K::Escape,
        egui::Key::ShiftLeft => K::LeftShift,
        egui::Key::ShiftRight => K::RightShift,
        egui::Key::ControlLeft => K::LeftCtrl,
        egui::Key::ControlRight => K::RightCtrl,
        egui::Key::AltLeft => K::LeftAlt,
        egui::Key::AltRight => K::RightAlt,
        egui::Key::Comma => K::Comma,
        egui::Key::Period => K::Period,
        egui::Key::Slash => K::Slash,
        egui::Key::Semicolon => K::Semicolon,
        egui::Key::Quote => K::Quote,
        egui::Key::OpenBracket => K::LeftBracket,
        egui::Key::CloseBracket => K::RightBracket,
        egui::Key::Minus => K::Minus,
        egui::Key::Equals => K::Equals,
        egui::Key::Backslash => K::Backslash,
        egui::Key::Backtick => K::Backtick,
        _ => return None,
    })
}

/// The inverse of [`map_key`]: which `egui::Key` to poll for a given
/// binding. The app polls per binding rather than over a fixed list, so a
/// key is watched exactly when something is bound to it.
#[must_use]
pub fn egui_key_for(key: rf_input::Key) -> Option<egui::Key> {
    egui::Key::ALL
        .iter()
        .copied()
        .find(|candidate| map_key(*candidate) == Some(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_every_default_keymap_key() {
        assert_eq!(map_key(egui::Key::ArrowUp), Some(rf_input::Key::ArrowUp));
        assert_eq!(
            map_key(egui::Key::ArrowDown),
            Some(rf_input::Key::ArrowDown)
        );
        assert_eq!(
            map_key(egui::Key::ArrowLeft),
            Some(rf_input::Key::ArrowLeft)
        );
        assert_eq!(
            map_key(egui::Key::ArrowRight),
            Some(rf_input::Key::ArrowRight)
        );
        assert_eq!(map_key(egui::Key::Z), Some(rf_input::Key::Z));
        assert_eq!(map_key(egui::Key::X), Some(rf_input::Key::X));
        assert_eq!(map_key(egui::Key::Enter), Some(rf_input::Key::Enter));
        assert_eq!(
            map_key(egui::Key::ShiftRight),
            Some(rf_input::Key::RightShift)
        );
    }

    /// AMENDED by ticket W2-06, not re-baselined: `Q`, `Escape` and
    /// `ShiftLeft` used to map to `None` because W1-07's table covered only
    /// the eight keys the default keymap used. Making them bindable is the
    /// entire point of the remap UI — a user who binds Start to `Q` must
    /// have `Q` reach the keymap. What must still map to `None` is a key no
    /// binding can name at all.
    #[test]
    fn keys_outside_the_bindable_set_still_map_to_none() {
        assert_eq!(map_key(egui::Key::F1), None);
        assert_eq!(map_key(egui::Key::Insert), None);
        assert_eq!(map_key(egui::Key::PageUp), None);

        // ...and the three W1-07 called unbound are now bindable, on
        // purpose.
        assert_eq!(map_key(egui::Key::Q), Some(rf_input::Key::Q));
        assert_eq!(map_key(egui::Key::Escape), Some(rf_input::Key::Escape));
        assert_eq!(
            map_key(egui::Key::ShiftLeft),
            Some(rf_input::Key::LeftShift)
        );
    }

    /// The two directions must agree for every key, or a binding the UI
    /// offers is one the app never polls — the W1-07-era bug this widening
    /// exists to prevent.
    #[test]
    fn every_rf_input_key_has_an_egui_key_that_maps_back_to_it() {
        for &key in rf_input::Key::ALL {
            let egui_key = egui_key_for(key)
                .unwrap_or_else(|| panic!("{key:?} has no egui::Key, so it can never be pressed"));
            assert_eq!(map_key(egui_key), Some(key), "round trip for {key:?}");
        }
    }

    #[test]
    fn keys_with_no_binding_meaning_still_map_to_none() {
        assert_eq!(map_key(egui::Key::F1), None);
        assert_eq!(map_key(egui::Key::Insert), None);
    }
}
