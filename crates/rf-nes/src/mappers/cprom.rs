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
//! doc warns about
//!
//! That doc explains why the crate's push/materialize design normally
//! can't bank CHR RAM: a PPU-side `$2007` write lands in
//! [`crate::ppu::Ppu`]'s pushed buffer, and the next *unrelated* push
//! would silently overwrite it with the mapper's own stale copy. This
//! mapper closes that gap with [`super::Mapper::chr_window_writeback`] —
//! ticket W14-58's hook, shared with `TQROM` ([`super::Mmc3::new_tqrom`])
//! — plus [`super::Mapper::chr_ram_page_mask`] (`0xFF`: the whole window
//! is RAM), so [`crate::ppu::mem`] doesn't drop PPU-side writes. See
//! [`super::Mmc3`]'s own TQROM implementation of this same pair for the
//! reference shape this mapper follows.
//!
//! `NesBus::push_mapper_view` calls the write-back hook first (fed the
//! PPU's CURRENT bytes — whatever was pushed at the LAST push, whether or
//! not this mapper's own register changed in between) and only then asks
//! [`Cprom::chr_window`] for a fresh view to push. So the hook cannot fold
//! against `self.selected` — by the time it runs, a `$8000-$FFFF` write
//! may already have moved `selected` on to the NEXT page — it must fold
//! against [`Cprom::materialized_page`], the page that was actually in the
//! window as of that last push. `materialized_page` is set to `selected`
//! at the END of [`Cprom::chr_window_writeback`] itself, once the fold
//! against the OLD value is done and a fresh materialize is about to make
//! it current — never inside [`Cprom::cpu_write`], which only stages the
//! new selection for the write-back hook to pick up whenever it next runs
//! (the register write and the write-back are two independent NesBus call
//! sites; nothing but `push_mapper_view` calling both in order links
//! them).
//!
//! Both halves must fold, not just the upper one: the fixed lower half is
//! CHR RAM too. And when `materialized_page == 0`, both halves of the
//! window are two independently-writable *views* of that one physical
//! page (this crate's flat PPU CHR buffer keeps them as separate bytes,
//! so a write through either half does not, by itself, update the other)
//! — a plain "last write wins" copy in either fixed order silently drops
//! whichever half didn't write. [`Cprom::chr_window_writeback`] instead
//! merges the two halves byte-by-byte against the value each byte held
//! before this fold, so a write through *either* half survives.
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
    /// The page bits 0-1 of the last `$8000-$FFFF` write named. This is a
    /// staged register value, not necessarily what's in `self.window` yet
    /// — see module doc for why [`Cprom::materialized_page`] is the field
    /// that tracks the window's actual current content.
    selected: u8,
    /// The page that was actually in `self.window`'s upper half as of the
    /// last [`Cprom::chr_window_writeback`] call (equivalently: the last
    /// time `self.window` was rebuilt). See module doc for why the
    /// write-back hook must fold against this, not `selected`.
    materialized_page: u8,
    /// Scratch 8 KiB buffer [`Cprom::chr_window`] hands back — page 0
    /// (fixed) followed by `pages[materialized_page]`. Kept in sync by
    /// [`Cprom::rematerialize`].
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
            materialized_page: 0,
            window: vec![0u8; WINDOW_SIZE],
        }
    }

    /// Rebuilds `self.window` from `self.pages`: page 0 (fixed) then
    /// `pages[materialized_page]`. Disjoint-field slicing (not a
    /// `self.page()` helper call) so this compiles as a `&mut self`
    /// method touching two fields at once.
    fn rematerialize(&mut self) {
        let sel = usize::from(self.materialized_page);
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
    /// 0-1). Only stages `selected` — see module doc for why materializing
    /// happens in [`Cprom::chr_window_writeback`] instead, once
    /// `NesBus::push_mapper_view` reaches it.
    fn cpu_write(&mut self, _addr: u16, value: u8, _cycle: u64) {
        self.selected = value & 0x03;
    }

    fn mirroring(&self) -> Mirroring {
        self.mirroring
    }

    /// Always `Some` — CPROM's CHR is never ROM, and [`Cprom::
    /// chr_window_writeback`] (see module doc) is what makes returning a
    /// live view here safe despite [`Mapper::chr_window`]'s general
    /// "CHR RAM is an honest gap" doc.
    fn chr_window(&self) -> Option<&[u8]> {
        Some(&self.window)
    }

    /// See module doc: folds `ppu_chr` back into `self.pages`, then
    /// advances `materialized_page` to whatever [`Cprom::cpu_write`] most
    /// recently staged in `selected` and rematerializes — in that order,
    /// so the fold still targets the page the window showed BEFORE this
    /// call, exactly matching what `ppu_chr`'s bytes are.
    fn chr_window_writeback(&mut self, ppu_chr: &[u8]) {
        debug_assert_eq!(ppu_chr.len(), WINDOW_SIZE);
        let sel = usize::from(self.materialized_page);
        if sel == 0 {
            // Both halves are views of the SAME physical page (module
            // doc) — a write through either one must survive, so merge
            // byte-by-byte against what each byte held before this fold
            // rather than letting one half's copy blindly clobber the
            // other's edit.
            for i in 0..PAGE_SIZE {
                let old = self.pages[i];
                let lo = ppu_chr[i];
                let hi = ppu_chr[PAGE_SIZE + i];
                self.pages[i] = if lo != old {
                    lo
                } else if hi != old {
                    hi
                } else {
                    old
                };
            }
        } else {
            self.pages[..PAGE_SIZE].copy_from_slice(&ppu_chr[..PAGE_SIZE]);
            self.pages[sel * PAGE_SIZE..(sel + 1) * PAGE_SIZE]
                .copy_from_slice(&ppu_chr[PAGE_SIZE..]);
        }
        self.materialized_page = self.selected;
        self.rematerialize();
    }

    /// The whole window is CHR RAM (ticket W14-5859 merge) — without this,
    /// [`crate::ppu::mem`]'s mask rule would silently drop every PPU-side
    /// write once any window has been pushed.
    fn chr_ram_page_mask(&self) -> u8 {
        0xFF
    }

    /// `MAPR` (ticket W2-04): the page-select register plus the 16 KiB
    /// CHR-RAM contents (this mapper — unlike CNROM's CHR ROM — owns
    /// bytes that are genuine mutable state, not cartridge data the
    /// loaded ROM restores). `materialized_page` is appended at the end
    /// (ticket W14-5859 merge).
    fn save_state(&self, out: &mut StateOut<'_>) -> Result<(), StateError> {
        out.u8(self.selected)?;
        out.bytes(&self.pages)?;
        out.u8(self.materialized_page)
    }

    fn load_state(&mut self, inp: &mut StateIn<'_>) -> Result<(), StateError> {
        self.selected = inp.u8()?;
        inp.bytes(&mut self.pages)?;
        self.materialized_page = inp.u8()?;
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

    /// Simulates one `NesBus` write cycle through `$8000-$FFFF`: the
    /// mapper's own handler runs first, then `NesBus::push_mapper_view`
    /// calls `chr_window_writeback` with whatever the PPU buffer held
    /// from the PREVIOUS push, then re-materializes. Returns that PPU
    /// buffer's value so callers can mutate it before a later `write_bus`
    /// to simulate a PPU-side ($2007) write landing in between.
    fn write_bus(m: &mut Cprom, value: u8, ppu_buf: &[u8]) -> Vec<u8> {
        m.cpu_write(0x8000, value, 0);
        m.chr_window_writeback(ppu_buf);
        m.chr_window().unwrap().to_vec()
    }

    #[test]
    fn bank_select_swaps_the_upper_4kib_only() {
        let mut m = cprom();
        let w0 = m.chr_window().unwrap().to_vec();
        assert_eq!(w0[0], 0, "page 0 starts zeroed");
        assert_eq!(w0[PAGE_SIZE], 0, "page 0 selected at power-on too");

        let ppu_buf = w0;
        let w1 = write_bus(&mut m, 1, &ppu_buf); // select page 1
        assert_eq!(
            w1[0], 0,
            "lower half (page 0) must stay fixed across a bank switch"
        );
        assert_eq!(w1[PAGE_SIZE], 0, "page 1 is freshly selected, unwritten");
    }

    /// Reproduces `NesBus`'s real order across two bus writes with a
    /// simulated PPU write in between them (ticket W14-5859 merge): the
    /// mapper's handler stages the new selection, `chr_window_writeback`
    /// folds the PRE-write PPU buffer back into the page that was
    /// actually current, then re-materializes for the new selection.
    #[test]
    fn write_to_chr_ram_survives_switching_away_and_back() {
        let mut m = cprom();
        let initial = m.chr_window().unwrap().to_vec();
        let ppu_buf = write_bus(&mut m, 2, &initial); // select page 2

        // A PPU write lands in the (page 2) upper half.
        let mut ppu_buf = ppu_buf;
        ppu_buf[PAGE_SIZE] = 0x22;

        // Switch away to page 3.
        let w3 = write_bus(&mut m, 3, &ppu_buf);
        assert_eq!(w3[PAGE_SIZE], 0, "page 3 is untouched, still zero");

        // Switch back: NesBus would push page 3's (unwritten) view first.
        let w2 = write_bus(&mut m, 2, &w3);
        assert_eq!(
            w2[PAGE_SIZE], 0x22,
            "page 2's earlier write must survive the round trip through page 3"
        );
    }

    /// The lower 4 KiB is fixed to page 0 but is still CHR RAM — a write
    /// there must survive a bank switch of the (unrelated) upper half.
    /// W14-59's original port only folded the upper half back, which
    /// silently dropped this case; this is the merge's regression test
    /// for that fix.
    #[test]
    fn lower_half_write_survives_a_bank_switch_away_and_back() {
        let mut m = cprom();
        let mut ppu_buf = m.chr_window().unwrap().to_vec();
        ppu_buf[0] = 0x99; // write into the fixed lower half (page 0)

        let w1 = write_bus(&mut m, 1, &ppu_buf); // switch upper half to page 1
        assert_eq!(
            w1[0], 0x99,
            "lower-half write must survive even though only the upper half's bank changed"
        );

        let w2 = write_bus(&mut m, 2, &w1); // switch upper half again, to page 2
        assert_eq!(
            w2[0], 0x99,
            "lower-half write must still be there after a second, unrelated bank switch"
        );
    }

    /// `materialized_page == 0`: BOTH halves alias the same physical page
    /// (module doc). A write through the upper half while aliased must
    /// survive too, not just the lower-half case above — this is the
    /// scenario a naive "copy lower then copy upper" or "copy upper then
    /// copy lower" fold gets wrong in one direction or the other.
    #[test]
    fn upper_half_write_survives_while_aliased_to_the_fixed_page() {
        let mut m = cprom(); // power-on: selected == materialized_page == 0
        let mut ppu_buf = m.chr_window().unwrap().to_vec();
        ppu_buf[PAGE_SIZE] = 0x55; // write through the ALIASED upper half

        let w1 = write_bus(&mut m, 1, &ppu_buf); // switch upper half away to page 1
        assert_eq!(
            w1[0], 0x55,
            "the upper-half write must have landed in page 0, since it was aliased there"
        );

        let w0 = write_bus(&mut m, 0, &w1); // switch back to page 0
        assert_eq!(
            w0[0], 0x55,
            "page 0's contents (written through either alias) must survive"
        );
        assert_eq!(
            w0[PAGE_SIZE], 0x55,
            "re-aliased upper half shows the same page"
        );
    }

    /// `push_mapper_view` calls `chr_window_writeback` at four different
    /// write sites, not just `$8000-$FFFF` — a `$2000-$3FFF` (PPUCTRL)
    /// write, for instance, triggers a push with NO intervening
    /// `cpu_write`. Two write-backs in a row without a bank-select
    /// between them must not corrupt anything.
    #[test]
    fn two_writebacks_in_a_row_without_an_intervening_cpu_write_are_stable() {
        let mut m = cprom();
        let initial = m.chr_window().unwrap().to_vec();
        let ppu_buf = write_bus(&mut m, 2, &initial); // select page 2

        let mut ppu_buf = ppu_buf;
        ppu_buf[PAGE_SIZE] = 0x77;
        m.chr_window_writeback(&ppu_buf); // e.g. triggered by a PPUCTRL write
        let after_first = m.chr_window().unwrap().to_vec();
        assert_eq!(after_first[PAGE_SIZE], 0x77);

        // A second push with no new bank select and no further PPU edit.
        m.chr_window_writeback(&after_first);
        assert_eq!(
            m.chr_window().unwrap()[PAGE_SIZE],
            0x77,
            "a repeat writeback against unchanged bytes must not lose the earlier write"
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

    #[test]
    fn chr_ram_page_mask_is_all_writable() {
        let m = cprom();
        assert_eq!(m.chr_ram_page_mask(), 0xFF);
    }
}
