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
