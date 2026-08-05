//! The host-side per-frame input latch (FR-FE-003, `docs/SRS.md`).
//!
//! **Two different things are both called a "latch" in this project — do
//! not confuse them.** This is the *host* one: the frontend pushes
//! key-down/key-up events into it as they arrive, and once per frame
//! samples whatever is currently held into one [`InputFrame`], which is
//! then handed to the core. That per-frame sample is the entire
//! determinism contract this type exists for: two runs that feed the core
//! the same [`InputFrame`] sequence must behave identically. The
//! controller's own 1->0 strobe latch (`rf_nes::system::controller`) is a
//! completely different thing — it is the *game's* business, driven by
//! `$4016` writes the running program issues, and is correct and out of
//! scope here.
use std::collections::HashSet;

use rf_core_api::InputFrame;

use crate::{Key, KeyMap};

/// Holds the set of currently-held [`Key`]s; produces an [`InputFrame`] on
/// demand via a [`KeyMap`].
#[derive(Debug, Clone, Default)]
pub struct InputLatch {
    held: HashSet<Key>,
}

impl InputLatch {
    /// No keys held.
    #[must_use]
    pub fn new() -> Self {
        InputLatch {
            held: HashSet::new(),
        }
    }

    /// Record that `key` is now held (idempotent — a repeat key-down while
    /// already held is a no-op).
    pub fn key_down(&mut self, key: Key) {
        self.held.insert(key);
    }

    /// Record that `key` is no longer held (idempotent).
    pub fn key_up(&mut self, key: Key) {
        self.held.remove(&key);
    }

    /// Whether `key` is currently held.
    #[must_use]
    pub fn is_held(&self, key: Key) -> bool {
        self.held.contains(&key)
    }

    /// Sample the currently-held keys through `keymap` into one
    /// [`InputFrame`] — FR-FE-003's "deterministic per-frame latch".
    /// Un-bound held keys are silently ignored (no error: an unmapped key
    /// held down is not a fault condition).
    #[must_use]
    pub fn sample(&self, keymap: &KeyMap) -> InputFrame {
        let mut frame = InputFrame::empty();
        for &key in &self.held {
            if let Some((port, button)) = keymap.lookup(key) {
                if let Some(bits) = frame.ports.get_mut(port) {
                    *bits |= 1u16 << button.bit();
                }
            }
        }
        frame
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NesButton;

    #[test]
    fn sample_with_nothing_held_is_empty() {
        let latch = InputLatch::new();
        assert_eq!(latch.sample(&KeyMap::default_nes()), InputFrame::empty());
    }

    #[test]
    fn key_down_then_sample_sets_the_mapped_bit() {
        let mut latch = InputLatch::new();
        latch.key_down(Key::X); // -> port 0, A
        let frame = latch.sample(&KeyMap::default_nes());
        assert_eq!(frame.ports[0], 1 << NesButton::A.bit());
        assert_eq!(frame.ports[1], 0);
    }

    #[test]
    fn key_up_clears_the_bit() {
        let mut latch = InputLatch::new();
        latch.key_down(Key::X);
        latch.key_up(Key::X);
        let frame = latch.sample(&KeyMap::default_nes());
        assert_eq!(frame, InputFrame::empty());
    }

    #[test]
    fn multiple_held_keys_combine_into_one_frame() {
        let mut latch = InputLatch::new();
        latch.key_down(Key::ArrowUp);
        latch.key_down(Key::X);
        let frame = latch.sample(&KeyMap::default_nes());
        assert_eq!(
            frame.ports[0],
            (1 << NesButton::Up.bit()) | (1 << NesButton::A.bit())
        );
    }

    #[test]
    fn is_held_reflects_current_state() {
        let mut latch = InputLatch::new();
        assert!(!latch.is_held(Key::Z));
        latch.key_down(Key::Z);
        assert!(latch.is_held(Key::Z));
        latch.key_up(Key::Z);
        assert!(!latch.is_held(Key::Z));
    }

    #[test]
    fn unbound_key_held_does_not_affect_the_sampled_frame() {
        let mut latch = InputLatch::new();
        let map = KeyMap::new(); // nothing bound
        latch.key_down(Key::Z);
        assert_eq!(latch.sample(&map), InputFrame::empty());
    }
}
