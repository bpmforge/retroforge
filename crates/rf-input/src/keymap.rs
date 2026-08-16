//! `Key -> (port, NesButton)` mapping (ticket W1-07).
//!
//! Plain data structure only — no config-file loading or persistence; a
//! remap UI and per-game overrides are ticket W2-06's job (SRS FR-FE-003).
use crate::{Key, NesButton};

/// A host key -> controller-button binding table.
#[derive(Debug, Clone, Default)]
pub struct KeyMap {
    entries: Vec<(Key, usize, NesButton)>,
}

impl KeyMap {
    /// An empty keymap — no keys bound to anything.
    #[must_use]
    pub fn new() -> Self {
        KeyMap {
            entries: Vec::new(),
        }
    }

    /// Bind `key` to `(port, button)`, replacing any prior binding for
    /// that key (a key can only ever drive one button at a time).
    pub fn bind(&mut self, key: Key, port: usize, button: NesButton) {
        self.entries.retain(|(k, _, _)| *k != key);
        self.entries.push((key, port, button));
    }

    /// Bind `key` to `(port, button)` **as a remap**: any other key
    /// previously driving that same `(port, button)` is unbound first.
    ///
    /// This is what a remap UI wants, and [`KeyMap::bind`] is not. `bind`
    /// dedupes by KEY (one key drives one button), which deliberately
    /// allows several keys to drive the same button — useful for binding
    /// both shifts. A capture-style remap needs the opposite guarantee: if
    /// the user says "Start is now Q", `Enter` must stop being Start, or
    /// the old key keeps working while the UI shows only one of the two.
    /// That exact bug is why this method exists rather than the UI doing
    /// it by hand — the next programmatic binder would have reintroduced it.
    pub fn rebind(&mut self, key: Key, port: usize, button: NesButton) {
        self.entries
            .retain(|(k, p, b)| *k != key && !(*p == port && *b == button));
        self.entries.push((key, port, button));
    }

    /// Remove any binding for `key`.
    pub fn unbind(&mut self, key: Key) {
        self.entries.retain(|(k, _, _)| *k != key);
    }

    /// Every binding, in insertion order — for the remap UI and for saving
    /// (ticket W2-06; `crate::bindings` writes these out verbatim, which is
    /// what makes a saved config diff cleanly between runs).
    #[must_use]
    pub fn entries(&self) -> &[(Key, usize, NesButton)] {
        &self.entries
    }

    /// What `key` is bound to, if anything.
    #[must_use]
    pub fn lookup(&self, key: Key) -> Option<(usize, NesButton)> {
        self.entries
            .iter()
            .find(|(k, _, _)| *k == key)
            .map(|&(_, port, button)| (port, button))
    }

    /// The default single-controller (port 0) NES keymap: arrow keys are
    /// the D-pad, Z/X are B/A, Enter is Start, Right Shift is Select — the
    /// classic FCEUX/Mesen-style default binding, port 0 (controller 1)
    /// only. Port 1 has no default binding in this ticket (MVP keyboard is
    /// single-player; multi-controller keyboard binding is a remap-UI
    /// concern, W2-06).
    #[must_use]
    pub fn default_nes() -> Self {
        let mut map = Self::new();
        map.bind(Key::ArrowUp, 0, NesButton::Up);
        map.bind(Key::ArrowDown, 0, NesButton::Down);
        map.bind(Key::ArrowLeft, 0, NesButton::Left);
        map.bind(Key::ArrowRight, 0, NesButton::Right);
        map.bind(Key::Z, 0, NesButton::B);
        map.bind(Key::X, 0, NesButton::A);
        map.bind(Key::Enter, 0, NesButton::Start);
        map.bind(Key::RightShift, 0, NesButton::Select);
        map
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The remap guarantee: rebinding a button moves it, rather than
    /// leaving the old key working alongside the new one (ticket W2-06).
    #[test]
    fn rebinding_a_button_releases_the_key_that_used_to_drive_it() {
        let mut map = KeyMap::default_nes();
        assert_eq!(map.lookup(Key::Enter), Some((0, NesButton::Start)));

        map.rebind(Key::Q, 0, NesButton::Start);

        assert_eq!(map.lookup(Key::Q), Some((0, NesButton::Start)));
        assert_eq!(
            map.lookup(Key::Enter),
            None,
            "the old key must stop driving Start, or it silently keeps working"
        );
        assert_eq!(
            map.entries()
                .iter()
                .filter(|(_, p, b)| *p == 0 && *b == NesButton::Start)
                .count(),
            1,
            "exactly one key drives Start after a remap"
        );
    }

    /// A remap must also drop whatever the NEW key used to do, or pressing
    /// it would drive two buttons at once.
    #[test]
    fn rebinding_also_clears_the_new_keys_previous_job() {
        let mut map = KeyMap::default_nes();
        map.rebind(Key::Z, 0, NesButton::Start);
        assert_eq!(map.lookup(Key::Z), Some((0, NesButton::Start)));
        assert_eq!(
            map.entries()
                .iter()
                .filter(|(k, _, _)| *k == Key::Z)
                .count(),
            1
        );
        assert!(
            map.entries()
                .iter()
                .all(|(_, _, b)| *b != NesButton::B || map.lookup(Key::Z) != Some((0, *b))),
            "Z must not still be B"
        );
    }

    /// `bind` keeps its own, different guarantee — several keys may drive
    /// one button on purpose (both shifts, say). Remap uses `rebind`; this
    /// pins that the two are not the same operation.
    #[test]
    fn bind_still_allows_two_keys_to_drive_the_same_button() {
        let mut map = KeyMap::new();
        map.bind(Key::LeftShift, 0, NesButton::Select);
        map.bind(Key::RightShift, 0, NesButton::Select);
        assert_eq!(map.lookup(Key::LeftShift), Some((0, NesButton::Select)));
        assert_eq!(map.lookup(Key::RightShift), Some((0, NesButton::Select)));
    }

    #[test]
    fn default_nes_binds_all_eight_keys_to_port_zero() {
        let map = KeyMap::default_nes();
        let expected = [
            (Key::ArrowUp, NesButton::Up),
            (Key::ArrowDown, NesButton::Down),
            (Key::ArrowLeft, NesButton::Left),
            (Key::ArrowRight, NesButton::Right),
            (Key::Z, NesButton::B),
            (Key::X, NesButton::A),
            (Key::Enter, NesButton::Start),
            (Key::RightShift, NesButton::Select),
        ];
        for (key, button) in expected {
            assert_eq!(map.lookup(key), Some((0, button)));
        }
    }

    #[test]
    fn unbound_key_looks_up_to_none() {
        let map = KeyMap::new();
        assert_eq!(map.lookup(Key::Z), None);
    }

    #[test]
    fn rebinding_a_key_replaces_the_old_binding() {
        let mut map = KeyMap::new();
        map.bind(Key::Z, 0, NesButton::B);
        map.bind(Key::Z, 1, NesButton::A);
        assert_eq!(map.lookup(Key::Z), Some((1, NesButton::A)));
    }
}
