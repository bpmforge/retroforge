//! Host-agnostic key identifiers (ticket W1-07).
//!
//! Deliberately **not** a full keyboard enumeration — this crate stays
//! host-agnostic (no `egui`/`eframe` dependency; `scripts/validate-arch.sh`
//! and `crates/retroforge/src/input_map.rs`'s doc both hold this line) and
//! MVP keyboard input only needs enough keys for [`crate::KeyMap::default_nes`]:
//! the four D-pad directions plus the four buttons a default NES pad binds.
//! Gamepads and a remap UI (which would need a much larger key surface) are
//! ticket W2-06's job, not this one's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Z,
    X,
    Enter,
    RightShift,
}
