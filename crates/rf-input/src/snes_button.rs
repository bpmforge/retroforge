//! SNES controller buttons and their `$4218`/`$4219` bit order
//! (ticket W7-02).
//!
//! Reference: fullsnes "Controllers" — the standard pad shifts out
//! **B, Y, Select, Start, Up, Down, Left, Right, A, X, L, R**, which the
//! auto-joypad registers present as a 16-bit word with B at bit 15 and R
//! at bit 4. The low four bits are unused.
//!
//! That layout is the single source of truth shared with the core side:
//! `rf_snes::bus::SnesBus` returns the low byte at `$4218` and the high
//! byte at `$4219`, so `A` really is bit 7 and `Right` really is bit 8.
//! The two must never drift apart — the same contract [`crate::NesButton`]
//! documents for `$4016`.
//!
//! ## Why this exists at all
//!
//! Until W7-02 the `.rfreplay` format had **no SNES button type**, and
//! its log key was a `Vec<NesButton>`. A SNES button outside the NES
//! 8-bit set therefore had no entry in the key and was **silently
//! dropped** on serialisation — a replay recorded holding Right played
//! back holding nothing. That was found by W6-05, whose fixture diverged
//! at the first checkpoint of an otherwise perfectly deterministic run.

/// The twelve buttons of a standard SNES controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SnesButton {
    B,
    Y,
    Select,
    Start,
    Up,
    Down,
    Left,
    Right,
    A,
    X,
    /// The left shoulder button.
    L,
    /// The right shoulder button.
    R,
}

impl SnesButton {
    /// Bit index in the `$4218`/`$4219` 16-bit word (module doc).
    #[must_use]
    pub const fn bit(self) -> u8 {
        match self {
            SnesButton::B => 15,
            SnesButton::Y => 14,
            SnesButton::Select => 13,
            SnesButton::Start => 12,
            SnesButton::Up => 11,
            SnesButton::Down => 10,
            SnesButton::Left => 9,
            SnesButton::Right => 8,
            SnesButton::A => 7,
            SnesButton::X => 6,
            SnesButton::L => 5,
            SnesButton::R => 4,
        }
    }

    /// Single-char mnemonic for a `.rfreplay` `[Input]` line. Unpressed is
    /// always `.`.
    ///
    /// The shoulder buttons take lowercase `l`/`r` because `L` and `R`
    /// are already Left and Right — a collision that would make an input
    /// line ambiguous, which is why [`SnesButton::ALL`]'s mnemonics are
    /// asserted distinct in this module's tests.
    #[must_use]
    pub const fn mnemonic(self) -> char {
        match self {
            SnesButton::B => 'B',
            SnesButton::Y => 'Y',
            SnesButton::Select => 's',
            SnesButton::Start => 'S',
            SnesButton::Up => 'U',
            SnesButton::Down => 'D',
            SnesButton::Left => 'L',
            SnesButton::Right => 'R',
            SnesButton::A => 'A',
            SnesButton::X => 'X',
            SnesButton::L => 'l',
            SnesButton::R => 'r',
        }
    }

    /// Full name as written in a `[LogKey]` line.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            SnesButton::B => "B",
            SnesButton::Y => "Y",
            SnesButton::Select => "Select",
            SnesButton::Start => "Start",
            SnesButton::Up => "Up",
            SnesButton::Down => "Down",
            SnesButton::Left => "Left",
            SnesButton::Right => "Right",
            SnesButton::A => "A",
            SnesButton::X => "X",
            SnesButton::L => "L",
            SnesButton::R => "R",
        }
    }

    /// Parse a `[LogKey]` name.
    ///
    /// Several of these names — `A`, `B`, `Select`, `Start` and the four
    /// directions — are shared with [`crate::NesButton`] but mean a
    /// **different bit**. Parsing is therefore console-directed: the
    /// `[Header]`'s `console` field decides which of the two tables a
    /// name is looked up in. A name-only lookup across both consoles
    /// would silently mis-decode every NES log.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "B" => SnesButton::B,
            "Y" => SnesButton::Y,
            "Select" => SnesButton::Select,
            "Start" => SnesButton::Start,
            "Up" => SnesButton::Up,
            "Down" => SnesButton::Down,
            "Left" => SnesButton::Left,
            "Right" => SnesButton::Right,
            "A" => SnesButton::A,
            "X" => SnesButton::X,
            "L" => SnesButton::L,
            "R" => SnesButton::R,
            _ => return None,
        })
    }

    /// Canonical shift-register read order, which is what a `[LogKey]`
    /// line declares per port.
    pub const ALL: [SnesButton; 12] = [
        SnesButton::B,
        SnesButton::Y,
        SnesButton::Select,
        SnesButton::Start,
        SnesButton::Up,
        SnesButton::Down,
        SnesButton::Left,
        SnesButton::Right,
        SnesButton::A,
        SnesButton::X,
        SnesButton::L,
        SnesButton::R,
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The layout the core side actually uses. `rf-snes` reads `$4218` as
    /// the low byte, so `A` is bit 7 and `Right` is bit 8 — and
    /// `RF-Scroller-S` moves on `$0100`, which is Right.
    #[test]
    fn bit_layout_matches_the_auto_joypad_word() {
        assert_eq!(SnesButton::B.bit(), 15);
        assert_eq!(SnesButton::Right.bit(), 8, "$0100 is Right");
        assert_eq!(SnesButton::A.bit(), 7, "$0080 is A");
        assert_eq!(SnesButton::R.bit(), 4);
        // The low four bits are unused, so nothing may claim them.
        for b in SnesButton::ALL {
            assert!(b.bit() >= 4, "{b:?} claims an unused low bit");
        }
    }

    /// Half these buttons live above bit 7 — the whole reason this type
    /// had to exist.
    #[test]
    fn six_buttons_lie_outside_the_nes_eight_bit_range() {
        let above = SnesButton::ALL.iter().filter(|b| b.bit() > 7).count();
        assert_eq!(above, 8, "B, Y, Select, Start and the four directions");
    }

    #[test]
    fn names_round_trip() {
        for button in SnesButton::ALL {
            assert_eq!(SnesButton::from_name(button.name()), Some(button));
        }
        assert_eq!(SnesButton::from_name("Turbo"), None);
    }

    /// `L`/`R` as names are the shoulders; `L`/`R` as MNEMONICS would
    /// collide with Left/Right, so the shoulders use lowercase.
    #[test]
    fn mnemonics_are_all_distinct() {
        let mut seen = std::collections::HashSet::new();
        for button in SnesButton::ALL {
            assert!(
                seen.insert(button.mnemonic()),
                "duplicate mnemonic {:?} for {button:?}",
                button.mnemonic()
            );
        }
        assert_ne!(SnesButton::L.mnemonic(), SnesButton::Left.mnemonic());
        assert_ne!(SnesButton::R.mnemonic(), SnesButton::Right.mnemonic());
    }

    #[test]
    fn every_bit_is_distinct() {
        let mut seen = std::collections::HashSet::new();
        for button in SnesButton::ALL {
            assert!(seen.insert(button.bit()), "duplicate bit for {button:?}");
        }
    }
}
