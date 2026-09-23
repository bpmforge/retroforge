//! CPROM (mapper 13) —
//! [nesdev.org/wiki/CPROM](https://www.nesdev.org/wiki/CPROM): PRG ROM is
//! a single fixed 32 KiB bank at `$8000-$FFFF` with no mapper registers on
//! the PRG side at all (identical shape to [`super::Nrom`]'s 32 KiB case).
//! CHR is 16 KiB of CHR **RAM** organized as four 4 KiB pages: `$0000-
//! $0FFF` is permanently wired to page 0; `$1000-$1FFF` shows whichever of
//! the four pages bits 0-1 of the last `$8000-$FFFF` write named (nesdev's
//! register diagram: `xxxx xxPP`, `PP` selects the page — page 0 can also
//! be named for the upper half, in which case both windows simply alias
//! the same physical page; nesdev doesn't call this out as special because
//! CPROM's hardware has no reason to forbid it). Games known to use this
//! board: Videomation (both known dumps).
//!
//! ## Closing the CHR-RAM round-trip gap [`super::Mapper::chr_window`]'s
//! doc warns about — self-contained, ticket W14-59
//!
//! That doc explains why the crate's push/materialize design normally
//! can't bank CHR RAM: a PPU-side `$2007` write lands in
//! [`crate::ppu::Ppu`]'s pushed buffer, and the next *unrelated* push
//! would silently overwrite it with the mapper's own stale copy. This
//! mapper closes that gap with [`super::Mapper::chr_writeback`] — a
//! narrow, self-contained hook this ticket adds to the trait (written
//! without sight of the parallel W14-58 lane, which was warned it might
//! add a differently-shaped CHR write-back mechanism of its own for
//! TQROM's MMC3 variant; see that trait method's doc for the merge note).
//!
//! [`crate::system::NesBus`] calls `chr_writeback(self.ppu.chr())` right
//! before it lets the mapper's own write handler run for every write that
//! can trigger a push (`ppu_ctrl_written`, `cpu_write_expansion`,
//! `cpu_write_wram`, `cpu_write`) — i.e. while `self.selected` still names
//! whatever page the PPU is *currently* showing, one write earlier than
//! whatever this write is about to select. [`Cprom::chr_writeback`] folds
//! the PPU's current upper-4-KiB bytes back into `self.pages[selected]`
//! before anything can change `selected` out from under it, then
//! rematerializes `self.window` (harmless no-op when nothing changed).
//! Only after that does `NesBus` call [`Cprom::cpu_write`], which updates
//! `selected` and rematerializes again for the *new* page — so
//! [`Cprom::chr_window`] (a plain, mutation-free `&self` — no interior
//! mutability needed) always has an up-to-date 8 KiB view ready to push.
//!
//! Bus conflicts are not modeled (no CPROM-specific nesdev note claims
//! them, and CNROM/UxROM's precedent in this crate already treats the
//! family as conflict-free) — see `crate::mappers` module doc's "MapperBus/
//! BusValue" section.
use rf_core_api::StateError;

use crate::state::{StateIn, StateOut};
use rf_cart::Mirroring;

use super::Mapper;

const PAGE_SIZE: usize = 4 * 1024;
const PAGE_COUNT: usize = 4;
const WINDOW_SIZE: usize = 2 * PAGE_SIZE;

pub struct Cprom {
    prg_rom: Vec<u8>,
    mirroring: Mirroring,
    /// Four 4 KiB CHR-RAM pages, flattened (`page * PAGE_SIZE + offset`).
    /// Power-on state is zeroed (no oracle claims otherwise for this
    /// board).
    pages: Vec<u8>,
    /// The page bits 0-1 of the last `$8000-$FFFF` write named — which
    /// page `$1000-$1FFF` (and hence `self.window`'s upper half) shows.
    selected: u8,
    /// Scratch 8 KiB buffer [`Cprom::chr_window`] hands back — page 0
    /// (fixed) followed by `pages[selected]`. Kept in sync by
    /// [`Cprom::rematerialize`], called from both [`Cprom::cpu_write`]
    /// (after `selected` changes) and [`Cprom::chr_writeback`] (after a
    /// fold-back that didn't change `selected` but may have changed the
    /// bytes it's built from).
    window: Vec<u8>,
}

impl Cprom {
    /// CPROM's CHR is always 16 KiB of RAM the board itself provides —
    /// never taken from the cartridge image (nesdev names no CHR-ROM
    /// variant of this board), so this constructor takes only `prg_rom`.
    pub fn new(prg_rom: Vec<u8>, mirroring: Mirroring) -> Self {
        debug_assert!(
            prg_rom.len() == 32 * 1024,
            "CPROM PRG ROM is a fixed 32 KiB bank"
        );
        Cprom {
            prg_rom,
            mirroring,
            pages: vec![0u8; PAGE_SIZE * PAGE_COUNT],
            selected: 0,
            window: vec![0u8; WINDOW_SIZE],
        }
    }

    /// Rebuilds `self.window` from `self.pages`: page 0 (fixed) then
    /// `pages[selected]`. Disjoint-field slicing (not a `self.page()`
    /// helper call) so this compiles as a `&mut self` method touching two
    /// fields at once.
    fn rematerialize(&mut self) {
        let sel = usize::from(self.selected);
        let (window, pages) = (&mut self.window, &self.pages);
        window[..PAGE_SIZE].copy_from_slice(&pages[..PAGE_SIZE]);
        window[PAGE_SIZE..].copy_from_slice(&pages[sel * PAGE_SIZE..(sel + 1) * PAGE_SIZE]);
    }
}

impl Mapper for Cprom {
    /// 32 KiB fixed, no bank registers on the PRG side — same arithmetic
    /// as [`super::Nrom::cpu_read`]'s 32 KiB case.
    fn cpu_read(&self, addr: u16) -> u8 {
        self.prg_rom[(addr - 0x8000) as usize]
    }

    /// Any `$8000-$FFFF` write latches the upper-half page select (bits
    /// 0-1) and immediately rematerializes `self.window` for it — see
    /// this module's doc for why `NesBus` guarantees
    /// [`Mapper::chr_writeback`] already ran, against the OLD `selected`,
    /// before this call.
    fn cpu_write(&mut self, _addr: u16, value: u8, _cycle: u64) {
        self.selected = value & 0x03;
        self.rematerialize();
    }

    fn mirroring(&self) -> Mirroring {
        self.mirroring
    }

    /// Always `Some` — CPROM's CHR is never ROM, and [`Cprom::
    /// chr_writeback`] (see module doc) is what makes returning a live
    /// view here safe despite [`Mapper::chr_window`]'s general "CHR RAM
    /// is an honest gap" doc.
    fn chr_window(&self) -> Option<&[u8]> {
        Some(&self.window)
    }

    /// See this module's doc, "Closing the CHR-RAM round-trip gap":
    /// folds `current_window`'s upper 4 KiB back into whichever page is
    /// still `self.selected` at this point (this must run before
    /// [`Cprom::cpu_write`] can change it), then rematerializes so a
    /// caller that polls [`Cprom::chr_window`] without an intervening
    /// `cpu_write` (e.g. a PPUCTRL-triggered push) still sees the fold-back.
    fn chr_writeback(&mut self, current_window: &[u8]) {
        debug_assert_eq!(current_window.len(), WINDOW_SIZE);
        let sel = usize::from(self.selected);
        self.pages[sel * PAGE_SIZE..(sel + 1) * PAGE_SIZE]
            .copy_from_slice(&current_window[PAGE_SIZE..]);
        self.rematerialize();
    }

    /// `MAPR` (ticket W2-04): the page-select register plus the 16 KiB
    /// CHR-RAM contents (this mapper — unlike CNROM's CHR ROM — owns
    /// bytes that are genuine mutable state, not cartridge data the
    /// loaded ROM restores).
    fn save_state(&self, out: &mut StateOut<'_>) -> Result<(), StateError> {
        out.u8(self.selected)?;
        out.bytes(&self.pages)
    }

    fn load_state(&mut self, inp: &mut StateIn<'_>) -> Result<(), StateError> {
        self.selected = inp.u8()?;
        inp.bytes(&mut self.pages)?;
        self.rematerialize();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cprom() -> Cprom {
        Cprom::new(vec![0xAAu8; 32 * 1024], Mirroring::Vertical)
    }

    #[test]
    fn bank_select_swaps_the_upper_4kib_only() {
        let mut m = cprom();
        let w0 = m.chr_window().unwrap().to_vec();
        assert_eq!(w0[0], 0, "page 0 starts zeroed");
        assert_eq!(w0[PAGE_SIZE], 0, "page 0 selected at power-on too");

        // Start from page 1 (NOT page 0 -- page 0 is also the fixed lower
        // half, so writing to it while it's selected upstairs too would
        // legitimately show up in both windows, proving nothing about
        // whether the swap itself is isolated).
        m.cpu_write(0x8000, 1, 0); // select page 1
        let mut w = m.chr_window().unwrap().to_vec();
        w[PAGE_SIZE] = 0x11; // mark page 1's upper-half byte
        m.chr_writeback(&w);
        m.cpu_write(0x8000, 2, 0); // select page 2

        let w2 = m.chr_window().unwrap();
        assert_eq!(
            w2[0], 0,
            "lower half (page 0) must stay fixed across a bank switch"
        );
        assert_eq!(
            w2[PAGE_SIZE], 0,
            "upper half now shows page 2, which was never written -- must not show page 1's marker"
        );
    }

    #[test]
    fn write_to_chr_ram_survives_switching_away_and_back() {
        let mut m = cprom();
        m.cpu_write(0x8000, 2, 0); // select page 2

        // A PPU write lands in the (page 2) upper half; NesBus feeds it
        // back before the NEXT bank-select write reaches `cpu_write`.
        let mut w = m.chr_window().unwrap().to_vec();
        w[PAGE_SIZE] = 0x22;
        m.chr_writeback(&w);

        m.cpu_write(0x8000, 3, 0); // switch away to page 3
        assert_eq!(
            m.chr_window().unwrap()[PAGE_SIZE],
            0,
            "page 3 is untouched, still zero"
        );

        // Switching back: NesBus feeds back page 3's (unwritten) view
        // first, exactly as it would for a real write.
        let w3 = m.chr_window().unwrap().to_vec();
        m.chr_writeback(&w3);
        m.cpu_write(0x8000, 2, 0); // switch back to page 2

        assert_eq!(
            m.chr_window().unwrap()[PAGE_SIZE],
            0x22,
            "page 2's earlier write must survive the round trip through page 3"
        );
    }

    #[test]
    fn prg_is_fixed_32kib_with_no_bank_register() {
        let mut m = cprom();
        let before = m.cpu_read(0xC000);
        m.cpu_write(0x8000, 3, 0);
        assert_eq!(m.cpu_read(0xC000), before);
    }

    #[test]
    fn page_select_wraps_to_two_bits() {
        let mut m = cprom();
        m.cpu_write(0x8000, 0xFC | 1, 0); // only bits 0-1 matter: selects page 1
        assert_eq!(m.selected, 1);
    }
}
