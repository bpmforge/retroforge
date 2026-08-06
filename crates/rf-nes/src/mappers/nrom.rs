//! NROM (mapper 0) — [nesdev.org/wiki/NROM](https://www.nesdev.org/wiki/NROM):
//! no bank registers at all, PRG ROM either 16 KiB (mirrored into both
//! `$8000-$BFFF` and `$C000-$FFFF`) or 32 KiB (mapped straight through),
//! no CHR banking, fixed mirroring.
//!
//! Extracted verbatim (ticket W2-02) from `system/mod.rs`'s original
//! W1-02 inline `read_prg` — see that ticket's own note for why it was
//! inlined rather than built as a trait impl in the first place (no
//! second mapper existed yet to inform the trait's shape). The PRG-read
//! arithmetic here is byte-for-byte the same as the code it replaces; the
//! six-canary regression check (this ticket's acceptance criterion 3) is
//! what proves that.
use rf_cart::Mirroring;

use super::Mapper;

pub struct Nrom {
    prg_rom: Vec<u8>,
    mirroring: Mirroring,
}

impl Nrom {
    pub fn new(prg_rom: Vec<u8>, mirroring: Mirroring) -> Self {
        Nrom { prg_rom, mirroring }
    }
}

impl Mapper for Nrom {
    /// A 16 KiB image is mirrored into both `$8000-$BFFF` and
    /// `$C000-$FFFF`; a 32 KiB image is mapped straight through. `%
    /// prg_rom.len()` implements both in one line since 16 KiB and 32 KiB
    /// both evenly divide the 32 KiB `$8000-$FFFF` window.
    fn cpu_read(&self, addr: u16) -> u8 {
        debug_assert!(!self.prg_rom.is_empty(), "NROM image with empty PRG ROM");
        let offset = (addr as usize - 0x8000) % self.prg_rom.len();
        self.prg_rom[offset]
    }

    /// NROM has no mapper registers; PRG ROM writes have no effect.
    fn cpu_write(&mut self, _addr: u16, _value: u8, _cycle: u64) {}

    fn mirroring(&self) -> Mirroring {
        self.mirroring
    }

    /// NROM has no CHR banking at all — [`crate::ppu::Ppu`]'s flat CHR
    /// buffer, seeded once from the cartridge's CHR bytes at construction,
    /// is already the permanently-correct view.
    fn chr_window(&self) -> Option<&[u8]> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nrom(prg_banks: usize, fill: impl Fn(usize) -> u8) -> Nrom {
        let prg_rom = (0..prg_banks * 16 * 1024).map(fill).collect();
        Nrom::new(prg_rom, Mirroring::Horizontal)
    }

    #[test]
    fn sixteen_kib_prg_mirrors_into_both_halves() {
        let m = nrom(1, |i| i as u8);
        assert_eq!(m.cpu_read(0x8000), 0x00);
        assert_eq!(
            m.cpu_read(0xC000),
            0x00,
            "second half must mirror the first"
        );
        assert_eq!(m.cpu_read(0x8001), 0x01);
        assert_eq!(m.cpu_read(0xC001), 0x01);
        assert_eq!(m.cpu_read(0xBFFF), 0x3FFFu16 as u8);
        assert_eq!(m.cpu_read(0xFFFF), 0x3FFFu16 as u8);
    }

    #[test]
    fn thirty_two_kib_halves_are_independently_addressable() {
        let m = nrom(2, |i| if i < 16 * 1024 { 0xAA } else { 0xBB });
        assert_eq!(m.cpu_read(0x8000), 0xAA, "first half ($8000) is bank 0");
        assert_eq!(
            m.cpu_read(0xC000),
            0xBB,
            "second half ($C000) is bank 1, not a mirror"
        );
    }

    #[test]
    fn writes_to_prg_space_have_no_effect() {
        let mut m = nrom(1, |i| i as u8);
        let before = m.cpu_read(0x8000);
        m.cpu_write(0x8000, !before, 0);
        assert_eq!(m.cpu_read(0x8000), before, "NROM PRG ROM is not writable");
    }

    #[test]
    fn chr_window_is_always_none() {
        let m = nrom(1, |i| i as u8);
        assert!(m.chr_window().is_none());
    }

    #[test]
    fn mirroring_is_whatever_was_passed_at_construction() {
        let m = Nrom::new(vec![0u8; 16 * 1024], Mirroring::Vertical);
        assert_eq!(m.mirroring(), Mirroring::Vertical);
    }
}
