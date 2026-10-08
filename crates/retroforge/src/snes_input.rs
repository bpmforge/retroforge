//! SNES controller bindings for live play (ticket W23-01).
//!
//! Until W23-01 a running SNES game received no controller input at all:
//! the live key and pad maps (`rf_input::KeyMap`/`PadMap`) bind only the
//! eight NES buttons, and `Machine::set_controller` fed only the NES bus.
//! This module is the SNES half — all twelve buttons, port 1, from the
//! keyboard and a pad — kept beside the NES maps rather than inside them so
//! the NES binding file format (`rf_input::bindings`) is untouched.
//!
//! The word it produces is in the `$4218` layout (fullsnes "Joypad
//! Auto-Read": B Y Select Start Up Down Left Right A X L R, bit 15 first),
//! which is `rf_input::SnesButton::bit`'s numbering.

use std::path::{Path, PathBuf};

use rf_input::{Key, PadButton, SnesButton};

const MAGIC: &str = "RFSNESBIND 1";
const FILE_NAME: &str = "snes-bindings.txt";

/// Keyboard and pad bindings for SNES port 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnesBindings {
    pub keys: Vec<(Key, SnesButton)>,
    pub pads: Vec<(PadButton, SnesButton)>,
}

impl Default for SnesBindings {
    /// Brad, 2026-10-08: arrows for the D-pad, Z/X for B/A as on the NES,
    /// A/S for Y/X beside them, Q/W for the shoulders, Enter/Right Shift
    /// for Start/Select. The pad follows the SNES's own layout (B bottom,
    /// A right, Y left, X top).
    fn default() -> Self {
        use SnesButton as S;
        Self {
            keys: vec![
                (Key::ArrowUp, S::Up),
                (Key::ArrowDown, S::Down),
                (Key::ArrowLeft, S::Left),
                (Key::ArrowRight, S::Right),
                (Key::Z, S::B),
                (Key::X, S::A),
                (Key::A, S::Y),
                (Key::S, S::X),
                (Key::Q, S::L),
                (Key::W, S::R),
                (Key::Enter, S::Start),
                (Key::RightShift, S::Select),
            ],
            pads: vec![
                (PadButton::DpadUp, S::Up),
                (PadButton::DpadDown, S::Down),
                (PadButton::DpadLeft, S::Left),
                (PadButton::DpadRight, S::Right),
                (PadButton::LeftStickUp, S::Up),
                (PadButton::LeftStickDown, S::Down),
                (PadButton::LeftStickLeft, S::Left),
                (PadButton::LeftStickRight, S::Right),
                (PadButton::South, S::B),
                (PadButton::East, S::A),
                (PadButton::West, S::Y),
                (PadButton::North, S::X),
                (PadButton::LeftShoulder, S::L),
                (PadButton::RightShoulder, S::R),
                (PadButton::Start, S::Start),
                (PadButton::Select, S::Select),
            ],
        }
    }
}

impl SnesBindings {
    /// The port-1 word for what is held now.
    #[must_use]
    pub fn sample(
        &self,
        key_held: impl Fn(Key) -> bool,
        pad_held: impl Fn(PadButton) -> bool,
    ) -> u16 {
        let keys = self
            .keys
            .iter()
            .filter(|(k, _)| key_held(*k))
            .map(|(_, b)| *b);
        let pads = self
            .pads
            .iter()
            .filter(|(p, _)| pad_held(*p))
            .map(|(_, b)| *b);
        keys.chain(pads).fold(0u16, |w, b| w | (1u16 << b.bit()))
    }

    /// Bind `key` to `button`: the key leaves any other button and the
    /// button any other key (one key, one button).
    pub fn bind_key(&mut self, key: Key, button: SnesButton) {
        self.keys.retain(|(k, b)| *k != key && *b != button);
        self.keys.push((key, button));
    }

    /// Bind `pad` to `button`, keeping the left stick as an extra D-pad.
    pub fn bind_pad(&mut self, pad: PadButton, button: SnesButton) {
        self.pads
            .retain(|(p, b)| *p != pad && (*b != button || is_stick(*p)));
        self.pads.push((pad, button));
    }

    /// The key bound to `button`, if any.
    #[must_use]
    pub fn key_for(&self, button: SnesButton) -> Option<Key> {
        self.keys
            .iter()
            .find(|(_, b)| *b == button)
            .map(|(k, _)| *k)
    }

    /// The (non-stick) pad button bound to `button`, if any.
    #[must_use]
    pub fn pad_for(&self, button: SnesButton) -> Option<PadButton> {
        self.pads
            .iter()
            .find(|(p, b)| *b == button && !is_stick(*p))
            .map(|(p, _)| *p)
    }

    /// The bindings as text (`key:Z=B`, `pad:South=B`).
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut out = format!("{MAGIC}\n");
        for (k, b) in &self.keys {
            out.push_str(&format!("key:{}={}\n", k.name(), b.name()));
        }
        for (p, b) in &self.pads {
            out.push_str(&format!("pad:{}={}\n", p.name(), b.name()));
        }
        out
    }

    /// Parse; a file that is not ours gives the defaults, and a line this
    /// build does not understand is skipped, never fatal.
    #[must_use]
    pub fn from_text(text: &str) -> Self {
        let mut lines = text.lines();
        if lines.next().map(str::trim) != Some(MAGIC) {
            return Self::default();
        }
        let mut out = Self {
            keys: Vec::new(),
            pads: Vec::new(),
        };
        for line in lines {
            let Some((lhs, button)) = line.trim().split_once('=') else {
                continue;
            };
            let Some(button) = SnesButton::from_name(button) else {
                continue;
            };
            if let Some(name) = lhs.strip_prefix("key:") {
                if let Some(k) = Key::from_name(name) {
                    out.keys.push((k, button));
                }
            } else if let Some(name) = lhs.strip_prefix("pad:") {
                if let Some(p) = PadButton::from_name(name) {
                    out.pads.push((p, button));
                }
            }
        }
        out
    }
}

fn is_stick(p: PadButton) -> bool {
    matches!(
        p,
        PadButton::LeftStickUp
            | PadButton::LeftStickDown
            | PadButton::LeftStickLeft
            | PadButton::LeftStickRight
    )
}

/// Where the SNES bindings live.
#[must_use]
pub fn path(root: &Path) -> PathBuf {
    root.join(crate::bindings_store::APP_DIR).join(FILE_NAME)
}

/// Load, falling back to the defaults when there is no file.
#[must_use]
pub fn load(root: &Path) -> SnesBindings {
    std::fs::read_to_string(path(root))
        .map_or_else(|_| SnesBindings::default(), |t| SnesBindings::from_text(&t))
}

/// Save.
///
/// # Errors
/// Returns the OS error text.
pub fn save(root: &Path, b: &SnesBindings) -> Result<(), String> {
    let p = path(root);
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&p, b.to_text()).map_err(|e| format!("{}: {e}", p.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_cover_all_twelve_buttons_on_both_devices() {
        let b = SnesBindings::default();
        for button in SnesButton::ALL {
            assert!(b.key_for(button).is_some(), "{button:?} has a key");
            assert!(b.pad_for(button).is_some(), "{button:?} has a pad button");
        }
    }

    #[test]
    fn sample_sets_the_4218_bits() {
        let b = SnesBindings::default();
        let w = b.sample(|k| k == Key::A, |_| false);
        assert_eq!(w, 1 << SnesButton::Y.bit());
        let w = b.sample(|_| false, |p| p == PadButton::RightShoulder);
        assert_eq!(w, 1 << SnesButton::R.bit());
        assert_eq!(b.sample(|_| false, |_| false), 0);
    }

    #[test]
    fn rebind_moves_the_key_and_round_trips() {
        let mut b = SnesBindings::default();
        b.bind_key(Key::Space, SnesButton::Y);
        assert_eq!(b.key_for(SnesButton::Y), Some(Key::Space));
        assert!(
            !b.keys.iter().any(|(k, _)| *k == Key::A),
            "A is now unbound"
        );
        b.bind_pad(PadButton::North, SnesButton::Y);
        assert_eq!(b.pad_for(SnesButton::Y), Some(PadButton::North));
        assert_eq!(b.pad_for(SnesButton::X), None, "North left X");
        let back = SnesBindings::from_text(&b.to_text());
        assert_eq!(back, b);
        assert_eq!(SnesBindings::from_text("junk"), SnesBindings::default());
    }
}
