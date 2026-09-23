//! UNROM 512 (mapper 30) —
//! [nesdev.org/wiki/UNROM_512](https://www.nesdev.org/wiki/UNROM_512): a
//! modern (2016+, homebrew-era) board built around a flash chip, used here
//! only for its non-flash "plays like UxROM but with CHR RAM and register
//! -controlled mirroring" personality. Any `$8000-$FFFF` write sets three
//! independent fields in one register (nesdev's diagram: `MCCP PPPP`):
//!
//! - bits 0-4 (`PPPPP`... this crate keeps all 5 low bits, `0x1F`) select
//!   the 16 KiB PRG bank at `$8000-$BFFF`; `$C000-$FFFF` is fixed to the
//!   LAST bank, same shape as [`super::UxRom`] (32 banks x 16 KiB = the
//!   512 KiB the board name promises).
//! - bits 5-6 select one of four 8 KiB CHR-RAM banks (32 KiB total).
//! - bit 7 selects the one-screen nametable page (nesdev: "if the game
//!   has one-screen mirroring hardwired, this bit selects which
//!   nametable"), honoured only when the header itself declares this
//!   board as one-screen-hardwired.
//!
//! ## Mirroring: honest gap around the header signalling
//!
//! Per the ticket brief (no cached nesdev copy of this specific page
//! exists in `docs/research/` to independently verify against, so this is
//! sourced to the ticket description, not confirmed against a fetched
//! nesdev page — see `crate::mappers` module doc's citation convention):
//! NES 2.0 headers for a one-screen-hardwired UNROM 512 board set BOTH the
//! four-screen bit (flags6 bit 3) and a mirroring bit (bit 0) together —
//! a combination that can't arise from an ordinary four-screen board (its
//! bit 0 is meaningless once bit 3 is set) — to say "one-screen, register
//! -controlled" rather than "real four-screen VRAM". `rf_cart::nes::
//! parse_nes_header` already collapses that combination down to
//! `Mirroring::FourScreen` (bit 3 unconditionally wins), which is the only
//! bit pattern this mapper can distinguish from ordinary H/V at all — so
//! [`Unrom512::new`] treats a header mirroring of `FourScreen` as "this
//! board's one-screen select is live" and anything else (`Horizontal`/
//! `Vertical`) as a hardwired mirroring bit 7 never touches. A real
//! four-screen-VRAM UNROM 512 board is not known to exist and is not
//! represented in this crate's mapper 30 library title (per the ticket:
//! one archive) — if one ever surfaces, `has_ext_nametable_ram`-style
//! extra VRAM would need to be added, which this ticket does not build.
//!
//! ## CHR-RAM banking closes the same round-trip gap [`super::Cprom`]
//! does
//!
//! See that module's doc for the full mechanism —
//! [`super::Mapper::chr_writeback`] is the same self-contained,
//! ticket-W14-59-added hook, used here identically except UNROM 512's
//! whole 8 KiB window swaps (no fixed half, unlike CPROM's fixed lower 4
//! KiB).
//!
//! ## Submapper / bus conflicts / flash
//!
//! NES 2.0 submapper 1 declares no bus conflicts; submapper 0 has them on
//! real hardware but, per this crate's blanket position (`crate::mappers`
//! module doc's "MapperBus/BusValue" section, already applied to UxROM/
//! CNROM/AxROM), "the relevant games all work around this in software", so
//! neither submapper changes this implementation's behavior — bus
//! conflicts are never modeled here regardless of which one a header
//! declares. Flash self-programming (nesdev's "Flash Memory Writes"
//! section — the board's actual *namesake* feature, used by its
//! self-flashing bootloader/multicarts) is out of scope: no game this
//! ticket's library serves flashes itself at runtime, and building it
//! would mean modeling the flash chip's command/status state machine for
//! a capability nothing here exercises — an honest, named gap, not a
//! silent one.
use rf_core_api::StateError;

use crate::state::{StateIn, StateOut};
use rf_cart::Mirroring;

use super::Mapper;

const PRG_BANK_SIZE: usize = 16 * 1024;
const CHR_PAGE_SIZE: usize = 8 * 1024;
const CHR_PAGE_COUNT: usize = 4;
const PRG_BANK_MASK: u8 = 0x1F; // bits 0-4
const CHR_BANK_SHIFT: u8 = 5; // bits 5-6
const CHR_BANK_MASK: u8 = 0x03;
const MIRROR_BIT: u8 = 0x80; // bit 7

pub struct Unrom512 {
    prg_rom: Vec<u8>,
    prg_bank: u8,
    /// Four 8 KiB CHR-RAM pages, flattened. See module doc.
    chr_pages: Vec<u8>,
    chr_selected: u8,
    /// Scratch 8 KiB view [`Unrom512::chr_window`] hands back — always
    /// `chr_pages[chr_selected]`, kept in sync by
    /// [`Unrom512::rematerialize_chr`].
    chr_window: Vec<u8>,
    /// `true` when the header names this board one-screen-hardwired (see
    /// module doc) — only then does [`MIRROR_BIT`] do anything.
    one_screen_hardwired: bool,
    mirroring: Mirroring,
}

impl Unrom512 {
    pub fn new(prg_rom: Vec<u8>, header_mirroring: Mirroring) -> Self {
        debug_assert!(
            !prg_rom.is_empty() && prg_rom.len().is_multiple_of(PRG_BANK_SIZE),
            "UNROM 512 PRG ROM must be a nonzero multiple of 16 KiB"
        );
        let one_screen_hardwired = matches!(header_mirroring, Mirroring::FourScreen);
        Unrom512 {
            prg_rom,
            prg_bank: 0,
            chr_pages: vec![0u8; CHR_PAGE_SIZE * CHR_PAGE_COUNT],
            chr_selected: 0,
            chr_window: vec![0u8; CHR_PAGE_SIZE],
            one_screen_hardwired,
            mirroring: if one_screen_hardwired {
                Mirroring::OneScreenLower
            } else {
                header_mirroring
            },
        }
    }

    fn prg_bank_count(&self) -> usize {
        self.prg_rom.len() / PRG_BANK_SIZE
    }

    fn rematerialize_chr(&mut self) {
        let sel = usize::from(self.chr_selected);
        self.chr_window
            .copy_from_slice(&self.chr_pages[sel * CHR_PAGE_SIZE..(sel + 1) * CHR_PAGE_SIZE]);
    }
}

impl Mapper for Unrom512 {
    /// Same shape as [`super::UxRom::cpu_read`]: a switchable 16 KiB
    /// window at `$8000-$BFFF`, the last bank fixed at `$C000-$FFFF`.
    fn cpu_read(&self, addr: u16) -> u8 {
        let (bank, offset) = match addr {
            0x8000..=0xBFFF => (
                (self.prg_bank as usize) % self.prg_bank_count(),
                addr - 0x8000,
            ),
            0xC000..=0xFFFF => (self.prg_bank_count() - 1, addr - 0xC000),
            _ => unreachable!("cpu_read is only ever called for $8000-$FFFF"),
        };
        self.prg_rom[bank * PRG_BANK_SIZE + offset as usize]
    }

    /// One register, three fields (module doc's `MCCP PPPP`): PRG bank
    /// (bits 0-4), CHR bank (bits 5-6), one-screen page (bit 7, only live
    /// when [`Unrom512::one_screen_hardwired`]). `NesBus` has already
    /// called [`Mapper::chr_writeback`] with the PPU's current window
    /// against the OLD `chr_selected`, same discipline as [`super::Cprom`]
    /// — see that module's doc.
    fn cpu_write(&mut self, _addr: u16, value: u8, _cycle: u64) {
        self.prg_bank = value & PRG_BANK_MASK;
        self.chr_selected = (value >> CHR_BANK_SHIFT) & CHR_BANK_MASK;
        self.rematerialize_chr();
        if self.one_screen_hardwired {
            self.mirroring = if value & MIRROR_BIT != 0 {
                Mirroring::OneScreenUpper
            } else {
                Mirroring::OneScreenLower
            };
        }
    }

    fn mirroring(&self) -> Mirroring {
        self.mirroring
    }

    /// Always `Some` — see [`super::Cprom::chr_window`]'s identical
    /// reasoning; [`Unrom512::chr_writeback`] is what makes it safe.
    fn chr_window(&self) -> Option<&[u8]> {
        Some(&self.chr_window)
    }

    /// See [`super::Cprom::chr_writeback`]'s doc — identical mechanism,
    /// except the WHOLE 8 KiB window is the bank (no fixed half).
    fn chr_writeback(&mut self, current_window: &[u8]) {
        debug_assert_eq!(current_window.len(), CHR_PAGE_SIZE);
        let sel = usize::from(self.chr_selected);
        self.chr_pages[sel * CHR_PAGE_SIZE..(sel + 1) * CHR_PAGE_SIZE]
            .copy_from_slice(current_window);
        self.rematerialize_chr();
    }

    /// `MAPR` (ticket W2-04): the one combined register plus the 32 KiB
    /// CHR-RAM contents (genuine mutable state, like [`super::Cprom`]'s).
    /// `mirroring` is fully reconstructible from `chr_pages`... no --
    /// from `one_screen_hardwired` (header-fixed, not saved) and the
    /// register's own bit 7, which isn't stored separately from
    /// `mirroring` itself, so `mirroring` is saved directly.
    fn save_state(&self, out: &mut StateOut<'_>) -> Result<(), StateError> {
        out.u8(self.prg_bank)?;
        out.u8(self.chr_selected)?;
        out.bytes(&self.chr_pages)?;
        out.u8(match self.mirroring {
            Mirroring::OneScreenUpper => 1,
            _ => 0,
        })
    }

    fn load_state(&mut self, inp: &mut StateIn<'_>) -> Result<(), StateError> {
        self.prg_bank = inp.u8()?;
        self.chr_selected = inp.u8()?;
        inp.bytes(&mut self.chr_pages)?;
        self.rematerialize_chr();
        if self.one_screen_hardwired {
            self.mirroring = if inp.u8()? == 1 {
                Mirroring::OneScreenUpper
            } else {
                Mirroring::OneScreenLower
            };
        } else {
            let _ = inp.u8()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `banks` 16 KiB PRG banks, bank `n`'s first byte is `n`.
    fn unrom512(banks: u8, mirroring: Mirroring) -> Unrom512 {
        let mut prg_rom = vec![0u8; banks as usize * PRG_BANK_SIZE];
        for n in 0..banks {
            prg_rom[n as usize * PRG_BANK_SIZE] = n;
        }
        Unrom512::new(prg_rom, mirroring)
    }

    #[test]
    fn prg_bank_select_and_fixed_last_bank() {
        let mut m = unrom512(4, Mirroring::Horizontal);
        assert_eq!(m.cpu_read(0xC000), 3, "last bank fixed at $C000");
        m.cpu_write(0x8000, 1, 0);
        assert_eq!(m.cpu_read(0x8000), 1);
        assert_eq!(
            m.cpu_read(0xC000),
            3,
            "selecting bank 1 for $8000 must not move $C000"
        );
    }

    #[test]
    fn chr_bank_select_swaps_the_whole_window() {
        let mut m = unrom512(2, Mirroring::Horizontal);
        // Write a marker into CHR bank 0 (currently selected), feed it
        // back, then switch to bank 1.
        let mut w = m.chr_window().unwrap().to_vec();
        w[0] = 0x55;
        m.chr_writeback(&w);
        m.cpu_write(0x8000, 1 << CHR_BANK_SHIFT, 0); // select CHR bank 1
        assert_eq!(
            m.chr_window().unwrap()[0],
            0,
            "bank 1 is untouched, must not show bank 0's marker"
        );
    }

    #[test]
    fn chr_write_survives_switching_away_and_back() {
        let mut m = unrom512(2, Mirroring::Horizontal);
        m.cpu_write(0x8000, 2 << CHR_BANK_SHIFT, 0); // select CHR bank 2
        let mut w = m.chr_window().unwrap().to_vec();
        w[10] = 0x77;
        m.chr_writeback(&w);

        m.cpu_write(0x8000, 3 << CHR_BANK_SHIFT, 0); // switch to bank 3
        let w3 = m.chr_window().unwrap().to_vec();
        m.chr_writeback(&w3);
        m.cpu_write(0x8000, 2 << CHR_BANK_SHIFT, 0); // back to bank 2

        assert_eq!(m.chr_window().unwrap()[10], 0x77);
    }

    #[test]
    fn mirror_bit_only_acts_when_header_names_one_screen() {
        let mut m = unrom512(2, Mirroring::Horizontal);
        m.cpu_write(0x8000, MIRROR_BIT, 0);
        assert_eq!(
            m.mirroring(),
            Mirroring::Horizontal,
            "header didn't declare one-screen-hardwired; bit 7 must be ignored"
        );

        let mut m2 = unrom512(2, Mirroring::FourScreen);
        assert_eq!(m2.mirroring(), Mirroring::OneScreenLower, "power-on state");
        m2.cpu_write(0x8000, MIRROR_BIT, 0);
        assert_eq!(m2.mirroring(), Mirroring::OneScreenUpper);
        m2.cpu_write(0x8000, 0, 0);
        assert_eq!(m2.mirroring(), Mirroring::OneScreenLower);
    }

    #[test]
    fn prg_bank_number_wraps_modulo_the_real_bank_count() {
        let mut m = unrom512(4, Mirroring::Horizontal);
        m.cpu_write(0x8000, 7, 0); // 7 % 4 == 3
        assert_eq!(m.cpu_read(0x8000), 3);
    }
}
