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
#[must_use]
pub fn map_key(key: egui::Key) -> Option<rf_input::Key> {
    match key {
        egui::Key::ArrowUp => Some(rf_input::Key::ArrowUp),
        egui::Key::ArrowDown => Some(rf_input::Key::ArrowDown),
        egui::Key::ArrowLeft => Some(rf_input::Key::ArrowLeft),
        egui::Key::ArrowRight => Some(rf_input::Key::ArrowRight),
        egui::Key::Z => Some(rf_input::Key::Z),
        egui::Key::X => Some(rf_input::Key::X),
        egui::Key::Enter => Some(rf_input::Key::Enter),
        egui::Key::ShiftRight => Some(rf_input::Key::RightShift),
        _ => None,
    }
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

    #[test]
    fn unbound_keys_map_to_none() {
        assert_eq!(map_key(egui::Key::Q), None);
        assert_eq!(map_key(egui::Key::F1), None);
        assert_eq!(map_key(egui::Key::Escape), None);
        assert_eq!(map_key(egui::Key::ShiftLeft), None);
    }
}
