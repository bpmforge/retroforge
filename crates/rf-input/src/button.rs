//! NES controller buttons and their `$4016`/`$4017` shift-register bit
//! order.
//!
//! Reference: [nesdev.org/wiki/Standard_controller](https://www.nesdev.org/wiki/Standard_controller)
//! — "the buttons ... are provided in this order: A, B, Select, Start, Up,
//! Down, Left, Right", shifted out low-bit-first, i.e. A = bit 0 ...
//! Right = bit 7. This is also the exact bit layout
//! `rf_nes::NesBus::set_controller_buttons`'s doc comment documents on the
//! other side of the host/core boundary — the two must never drift apart,
//! which is why [`NesButton::bit`] is the single source of truth this
//! crate uses to build the byte handed across that boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NesButton {
    A,
    B,
    Select,
    Start,
    Up,
    Down,
    Left,
    Right,
}

impl NesButton {
    /// `$4016`/`$4017` shift-register bit index (module doc).
    #[must_use]
    pub const fn bit(self) -> u8 {
        match self {
            NesButton::A => 0,
            NesButton::B => 1,
            NesButton::Select => 2,
            NesButton::Start => 3,
            NesButton::Up => 4,
            NesButton::Down => 5,
            NesButton::Left => 6,
            NesButton::Right => 7,
        }
    }

    /// BizHawk-compatible single-char mnemonic used in a `.rfreplay`
    /// `[Input]` line (`docs/design/SAVE_STATES.md` §3): unpressed is
    /// always `.`, pressed is this char.
    #[must_use]
    pub const fn mnemonic(self) -> char {
        match self {
            NesButton::A => 'A',
            NesButton::B => 'B',
            NesButton::Select => 's',
            NesButton::Start => 'S',
            NesButton::Up => 'U',
            NesButton::Down => 'D',
            NesButton::Left => 'L',
            NesButton::Right => 'R',
        }
    }

    /// Full name as written in a `.rfreplay` `[LogKey]` line (distinct
    /// from [`NesButton::mnemonic`] — the log key spells buttons out,
    /// `[Input]` lines use the one-char form).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            NesButton::A => "A",
            NesButton::B => "B",
            NesButton::Select => "Select",
            NesButton::Start => "Start",
            NesButton::Up => "Up",
            NesButton::Down => "Down",
            NesButton::Left => "Left",
            NesButton::Right => "Right",
        }
    }

    /// Parse a `[LogKey]`-style name (module doc's [`NesButton::name`]) —
    /// `None` for anything else.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "A" => NesButton::A,
            "B" => NesButton::B,
            "Select" => NesButton::Select,
            "Start" => NesButton::Start,
            "Up" => NesButton::Up,
            "Down" => NesButton::Down,
            "Left" => NesButton::Left,
            "Right" => NesButton::Right,
            _ => return None,
        })
    }

    /// Canonical `$4016` read order — what [`crate::KeyMap::default_nes`]
    /// and the canonical `.rfreplay` `[LogKey]` table both declare per
    /// port.
    pub const ALL: [NesButton; 8] = [
        NesButton::A,
        NesButton::B,
        NesButton::Select,
        NesButton::Start,
        NesButton::Up,
        NesButton::Down,
        NesButton::Left,
        NesButton::Right,
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_layout_matches_nesdev_read_order() {
        assert_eq!(NesButton::A.bit(), 0);
        assert_eq!(NesButton::B.bit(), 1);
        assert_eq!(NesButton::Select.bit(), 2);
        assert_eq!(NesButton::Start.bit(), 3);
        assert_eq!(NesButton::Up.bit(), 4);
        assert_eq!(NesButton::Down.bit(), 5);
        assert_eq!(NesButton::Left.bit(), 6);
        assert_eq!(NesButton::Right.bit(), 7);
    }

    #[test]
    fn mnemonic_encode_decode_round_trip_via_name() {
        for button in NesButton::ALL {
            let parsed = NesButton::from_name(button.name());
            assert_eq!(parsed, Some(button));
        }
    }

    #[test]
    fn from_name_rejects_unknown() {
        assert_eq!(NesButton::from_name("Turbo"), None);
        assert_eq!(NesButton::from_name(""), None);
    }

    #[test]
    fn mnemonics_are_all_distinct() {
        let mut seen = std::collections::HashSet::new();
        for button in NesButton::ALL {
            assert!(seen.insert(button.mnemonic()), "duplicate mnemonic char");
        }
    }
}

/// One button of *either* supported console, for the parts of the
/// `.rfreplay` format that must work for both (ticket W7-02).
///
/// A `.rfreplay` `[LogKey]` line declares a port's buttons by name, and
/// `[Input]` lines encode them by mnemonic — neither of which cares which
/// console it is, as long as `bit()` says where the button lives in the
/// 16-bit [`rf_core_api::InputFrame`] port word.
///
/// **Names collide across consoles and mean different bits.** `A`, `B`,
/// `Select`, `Start` and the four directions exist on both, at different
/// positions. So there is deliberately no `Button::from_name` — parsing
/// is console-directed, driven by the `[Header]`'s `console` field. A
/// name-only lookup that tried both tables would silently mis-decode
/// every NES log ever recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Button {
    Nes(NesButton),
    Snes(crate::SnesButton),
}

impl Button {
    /// Bit index within the port's 16-bit word.
    #[must_use]
    pub const fn bit(self) -> u8 {
        match self {
            Button::Nes(b) => b.bit(),
            Button::Snes(b) => b.bit(),
        }
    }

    /// Single-char `[Input]` mnemonic.
    #[must_use]
    pub const fn mnemonic(self) -> char {
        match self {
            Button::Nes(b) => b.mnemonic(),
            Button::Snes(b) => b.mnemonic(),
        }
    }

    /// Full `[LogKey]` name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Button::Nes(b) => b.name(),
            Button::Snes(b) => b.name(),
        }
    }
}

impl From<NesButton> for Button {
    fn from(b: NesButton) -> Self {
        Button::Nes(b)
    }
}

impl From<crate::SnesButton> for Button {
    fn from(b: crate::SnesButton) -> Self {
        Button::Snes(b)
    }
}
