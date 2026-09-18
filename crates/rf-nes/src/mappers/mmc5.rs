//! Mapper 5 — Nintendo MMC5, slice 1 (ticket W14-16).
//!
//! **11 archives** in the census of 1281 real NES archives (2026-09-17):
//! `Castlevania III` (two), `Gemfire`, `Uncharted Waters`, `Nobunaga's
//! Ambition II`, `Romance of the Three Kingdoms II`, `L'Empereur`,
//! `Bandit Kings of Ancient China`, `Laser Invasion`, a `SimCity`
//! prototype and the `Demo Vision` BIOS.
//!
//! Everything below is cited to nesdev, "MMC5". The registers this slice
//! implements:
//!
//! ```text
//! $5100  PRG mode 0-3          $5101  CHR mode 0-3 (8/4/2/1 KiB banks)
//! $5104  extended RAM mode     $5105  nametable mapping, 2 bits per table:
//!                                     0/1 CIRAM A/B, 2 extra RAM, 3 fill
//! $5106  fill tile             $5107  fill attribute (2 bits, replicated)
//! $5113  $6000 RAM bank        $5114-$5117  PRG banks (bit 7: 1 ROM, 0 RAM)
//! $5120-$5127  CHR set A       $5128-$512B  CHR set B     $5130  CHR high bits
//! $5200  split enable/side/tile $5201  split scroll       $5202  split CHR bank
//! $5203  IRQ scanline          $5204  W: bit 7 IRQ enable
//!                                     R: bit 7 pending (reading acks), bit 6 in frame
//! $5205/$5206  multiplier operands; reading gives the 16-bit product
//! $5C00-$5FFF  extended RAM (owned by the PPU here, see below)
//! ```
//!
//! # What the PPU does for this board
//!
//! Four things no other mapper needs, each a small seam added by this
//! ticket: nametables backed by the 1 KiB extended RAM or by the fill
//! tile (`Mirroring::PerTable` kinds 2 and 3); a second CHR window used
//! only for 8x16 sprite fetches (set A, while set B draws the background
//! — "when sprites are 8x16, set B is used for background fetches and set
//! A for sprites; when 8x8, the last-written set is used for both"); a
//! per-scanline signal for the scanline counter and in-frame flag; and
//! CPU reads answered from `$5000-$5FFF`.
//!
//! # PRG modes (nesdev)
//!
//! ```text
//! mode 0: $8000-$FFFF  32 KiB  from $5117 (bits 2-6)
//! mode 1: $8000  16 KiB from $5115 (bits 1-6)    $C000  16 KiB from $5117
//! mode 2: $8000  16 KiB from $5115               $C000  8 KiB $5116, $E000  8 KiB $5117
//! mode 3: 8 KiB each from $5114, $5115, $5116, $5117
//! ```
//!
//! `$5117` always selects ROM. A window whose register has bit 7 clear
//! selects PRG RAM on the board (ticket W14-17; see "PRG RAM windows"
//! below for how that reaches the bus's own chip).
//!
//! # PRG RAM windows (tickets W14-17, W14-22)
//!
//! [`Mapper::prg_ram_window`] answers which byte offset into
//! [`crate::system::NesBus`]'s own PRG RAM (its `$6000-$7FFF` window is
//! [`Mapper::wram_offset`]'s job instead, see below) a RAM-selected
//! `$8000-$FFFF` window addresses. **The bus keeps owning the bytes; the
//! mapper never gets a copy or a borrow of them.** Two reasons, not one:
//!
//! - **Aliasing.** Lending the bus's buffer to `Mmc5` (a `&mut [u8]` held
//!   alongside the bus's own reference to the same bytes, or a `Box<dyn
//!   Mapper>` field that borrows from its owner) is the shape Rust's
//!   aliasing rules exist to forbid; the only way around it without
//!   `unsafe` is for the *bus* to hold the byte offset and index its own
//!   buffer with it, which is exactly what `prg_ram_window`'s `Option<usize>`
//!   return does.
//! - **One copy, one save-state chunk.** If the mapper instead owned a
//!   second PRG RAM buffer, `$6000-$7FFF` and a RAM-selected `$8000+`
//!   window could read two different values for what a real MMC5 board
//!   treats as the same chip, and `StateRegion::Cart`
//!   (`NesBus::prg_ram`) would need a second, mapper-owned copy of the
//!   same bytes to stay in sync across a save/load — silently doubling
//!   what "the" PRG RAM chunk means. Keeping the bus as the single owner
//!   means `prg_ram_window` is pure address arithmetic (see
//!   [`Mmc5::prg_ram_window`] below): no new state to save at all.
//!
//! **Real chip/page banking (ticket W14-22), replacing W14-17's
//! documented "everything aliases one 8 KiB chip modulo 8 KiB" gap.**
//! Real MMC5 boards can address up to 64 KiB of PRG RAM across multiple
//! chips; this crate now sizes [`crate::system::NesBus`]'s PRG RAM from
//! the cartridge's own NES 2.0/iNES header (see that module's doc) and
//! pushes the total size into the mapper via
//! [`Mapper::set_prg_ram_len`]/[`Mmc5::chip_bank_offset`] right after
//! construction. `$5113` (for `$6000-$7FFF`) and `$5114`-`$5116` (for a
//! RAM-selected `$8000+` window, via [`Mmc5::window_reg`]) each
//! contribute a 3-bit bank value; nesdev.org/wiki/MMC5's board table —
//! "8K and 32K games have a single SRAM chip ... 16K games instead have
//! two chips, but only the first is battery backed" — is exactly the
//! rule [`Mmc5::chip_bank_offset`] implements: a 16 KiB board (ETROM;
//! nesdev's board list names Uncharted Waters and Romance of the Three
//! Kingdoms II) treats bit 2 of the bank value as a genuine chip select
//! (values 0-3 land on chip 0's one page, 4-7 on chip 1's), while every
//! other size this crate builds (8 KiB EKROM, 32 KiB EWROM) has exactly
//! one chip, so the bank value's low bits pick a page within it and wrap
//! (mirror) once they run past however many pages that one chip has —
//! nesdev also notes a single-chip board's SRAM is real hardware only
//! *active* when bit 2 is clear (an unselected value goes to no chip at
//! all, not a mirrored one); this crate mirrors instead, matching this
//! ticket's own EKROM acceptance criterion, an honest simplification
//! rather than a silent one.
//!
//! `$5102`/`$5103` (ticket W14-22, nesdev.org/wiki/MMC5) gate every PRG
//! RAM write — `$6000-$7FFF` and a RAM-selected `$8000+` window alike —
//! through [`Mapper::prg_ram_write_enabled`]: writes land only once
//! `$5102` reads `%10` AND `$5103` reads `%01`. Both registers reset to
//! a non-enabling value (`xxxx xx01` / `xxxx xx10`), so a cartridge must
//! explicitly unlock PRG RAM before it can be written, matching real
//! MMC5 boot behavior.
//!
//! # Slice 2 (ticket W14-17): extended attributes and the vertical split
//!
//! `$5104` mode 1 (ExGrafix, per-tile CHR bank and palette from extended
//! RAM) and the vertical split (`$5200-$5202`) both change the
//! background fetch itself, which this mapper cannot express through
//! [`Mapper::chr_window`]'s single materialized 8 KiB view — a tile's CHR
//! bank in either mode can be ANY 4 KiB page of the whole CHR ROM, not
//! one of the eight 1 KiB slots `chr_window`/`chr_window_sprites`
//! juggle. Instead [`Mapper::chr_rom_full`] exposes the raw CHR ROM once
//! (the bytes never change after cart load), and
//! [`Mapper::ext_attribute_mode`]/[`Mapper::vertical_split`] push the
//! small per-mode config the PPU needs to pick a 4 KiB bank per tile
//! itself — the same "materialize the mapper's current view, push it,
//! let the PPU's existing fetch math do the addressing" convention
//! `ppu/mem.rs`'s nametable-kind-2/3 routing and this mapper's own fill
//! tile already use, just extended to the pattern-table half of the
//! fetch. See `crate::ppu::background` module doc for the fetch-side
//! implementation and `crate::ppu::mem` for where the pushed views land.

use rf_core_api::StateError;

use crate::state::{StateIn, StateOut};
use rf_cart::Mirroring;

use super::Mapper;

const PRG_BANK_8K: usize = 8 * 1024;
const CHR_BANK_1K: usize = 1024;
const CHR_VIEW_SIZE: usize = 8 * 1024;

pub struct Mmc5 {
    prg_rom: Vec<u8>,
    chr_rom: Vec<u8>,
    chr_is_ram: bool,
    prg_mode: u8,
    chr_mode: u8,
    ext_ram_mode: u8,
    nametable_map: u8,
    fill_tile: u8,
    fill_attr: u8,
    /// `$5114-$5117` as written.
    prg_regs: [u8; 4],
    /// `$5120-$512B` as written; `$5130` supplies bits 8-9.
    chr_regs: [u8; 12],
    chr_high: u8,
    /// Which set was written last: `false` = A, `true` = B.
    last_chr_set_b: bool,
    sprites_8x16: bool,
    irq_scanline: u8,
    irq_enabled: bool,
    irq_pending: bool,
    in_frame: bool,
    scanline_counter: u8,
    mul_a: u8,
    mul_b: u8,
    chr_view_bg: [u8; CHR_VIEW_SIZE],
    chr_view_sprites: [u8; CHR_VIEW_SIZE],
    /// `$5200` as written: bit 7 enable, bit 6 side, bits 0-4 split tile.
    split_ctrl: u8,
    /// `$5201`: the split region's vertical scroll.
    split_scroll: u8,
    /// `$5202`: the split region's CHR bank (plain 4 KiB index).
    split_chr_bank: u8,
    /// `$5113` as written: which 8 KiB PRG RAM chip/page backs
    /// `$6000-$7FFF` (ticket W14-22). Bits 3-7 are ignored (nesdev: "bits
    /// 7, 6, 5, and 4 are always ignored"; this crate's boards never
    /// exceed 32 KiB of PRG RAM, so bit 3 never matters either).
    wram_bank: u8,
    /// `$5102` as written (PRG RAM protect 1). Reset value `xxxx xx01`
    /// per nesdev.org/wiki/MMC5 -- writes are enabled only once this
    /// reads `%10` AND [`Self::wram_protect_b`] reads `%01`.
    wram_protect_a: u8,
    /// `$5103` as written (PRG RAM protect 2). Reset value `xxxx xx10`.
    wram_protect_b: u8,
    /// The cartridge's total PRG RAM size in bytes, pushed by
    /// [`Mapper::set_prg_ram_len`] right after construction (ticket
    /// W14-22) -- how many 8 KiB chips/pages `wram_bank`'s low bits
    /// select among.
    prg_ram_len: usize,
}

impl Mmc5 {
    pub fn new(prg_rom: Vec<u8>, chr_rom: Vec<u8>, chr_is_ram: bool) -> Self {
        debug_assert!(
            !prg_rom.is_empty() && prg_rom.len().is_multiple_of(PRG_BANK_8K),
            "MMC5 PRG ROM must be a nonzero multiple of 8 KiB"
        );
        let mut m = Self {
            prg_rom,
            chr_rom,
            chr_is_ram,
            prg_mode: 3,
            chr_mode: 3,
            ext_ram_mode: 0,
            nametable_map: 0,
            fill_tile: 0,
            fill_attr: 0,
            // Power-on: every PRG window at the last bank (nesdev: "$5117
            // is $FF at power-on and reset").
            prg_regs: [0xFF; 4],
            chr_regs: [0; 12],
            chr_high: 0,
            last_chr_set_b: false,
            sprites_8x16: false,
            irq_scanline: 0,
            irq_enabled: false,
            irq_pending: false,
            in_frame: false,
            scanline_counter: 0,
            mul_a: 0xFF,
            mul_b: 0xFF,
            chr_view_bg: [0; CHR_VIEW_SIZE],
            chr_view_sprites: [0; CHR_VIEW_SIZE],
            split_ctrl: 0,
            split_scroll: 0,
            split_chr_bank: 0,
            wram_bank: 0,
            // nesdev.org/wiki/MMC5: reset value `xxxx xx01` for $5102,
            // `xxxx xx10` for $5103 -- neither equals the enabling
            // combination ($5102=%10, $5103=%01), so PRG RAM writes are
            // disabled at power-on.
            wram_protect_a: 0b01,
            wram_protect_b: 0b10,
            prg_ram_len: PRG_BANK_8K,
        };
        m.recompute_chr_views();
        m
    }

    /// Which 8 KiB unit of the bus's PRG RAM `bank` (a raw register's low
    /// bits, `$5113`/`$5114`-`$5116`) selects, per nesdev.org/wiki/MMC5's
    /// board table (ticket W14-22): "8K and 32K games have a single SRAM
    /// chip ... 16K games instead have two chips". Two chips (ETROM, 16
    /// KiB total) is the one shape bit 2 genuinely selects a *different*
    /// chip for -- every other size this crate ever builds (8 KiB EKROM,
    /// 32 KiB EWROM, or anything in between/beyond from a header this
    /// crate hasn't seen a real cartridge use) has exactly one chip, so
    /// `bank`'s low bits pick a page within it and simply wrap (mirror)
    /// once they run past how many pages actually exist -- the ticket's
    /// own acceptance criterion for EKROM, not a guess.
    fn chip_bank_offset(&self, bank: u8) -> usize {
        let total_banks = (self.prg_ram_len / PRG_BANK_8K).max(1);
        if total_banks == 2 {
            usize::from((bank >> 2) & 0x01) * PRG_BANK_8K
        } else {
            // `total_banks` is always a power of two here (PRG RAM sizes
            // are always powers of two times 8 KiB in this crate), so
            // masking by `total_banks - 1` is exactly "wrap within the
            // one chip's pages".
            let mask = (total_banks - 1) as u8;
            usize::from(bank & mask) * PRG_BANK_8K
        }
    }

    /// The raw, unshifted `$5114-$5117` byte governing PRG window
    /// `window` (0-3) under the current `$5100` PRG mode -- the same
    /// per-mode routing [`Self::prg_windows`] uses for ROM bank numbers,
    /// but RAM banking (ticket W14-22) always reads a register's low
    /// bits directly with no per-mode shift: cartridge RAM comes in flat
    /// 8 KiB units regardless of how finely ROM is banked in this mode.
    fn window_reg(&self, window: usize) -> u8 {
        let [r4, r5, r6, r7] = self.prg_regs;
        let r7 = r7 | 0x80; // $5117 is always ROM
        match self.prg_mode & 0x03 {
            0 => r7,
            1 => {
                if window < 2 {
                    r5
                } else {
                    r7
                }
            }
            2 => match window {
                0 | 1 => r5,
                2 => r6,
                _ => r7,
            },
            _ => [r4, r5, r6, r7][window],
        }
    }

    fn prg_bank_count(&self) -> usize {
        (self.prg_rom.len() / PRG_BANK_8K).max(1)
    }

    /// Which 8 KiB ROM bank (or `None` for a RAM-selecting register)
    /// each of the four CPU windows reads.
    fn prg_windows(&self) -> [Option<usize>; 4] {
        let count = self.prg_bank_count();
        let rom = |reg: u8, shift: u8, size_8k: usize, window: usize| -> Option<usize> {
            if reg & 0x80 == 0 {
                return None;
            }
            let bank = usize::from((reg & 0x7F) >> shift) * size_8k + window;
            Some(bank % count)
        };
        let [r4, r5, r6, r7] = self.prg_regs;
        let r7 = r7 | 0x80; // $5117 is always ROM
        match self.prg_mode & 0x03 {
            0 => std::array::from_fn(|w| rom(r7, 2, 4, w)),
            1 => [
                rom(r5, 1, 2, 0),
                rom(r5, 1, 2, 1),
                rom(r7, 1, 2, 0),
                rom(r7, 1, 2, 1),
            ],
            2 => [
                rom(r5, 1, 2, 0),
                rom(r5, 1, 2, 1),
                rom(r6, 0, 1, 0),
                rom(r7, 0, 1, 0),
            ],
            _ => [
                rom(r4, 0, 1, 0),
                rom(r5, 0, 1, 0),
                rom(r6, 0, 1, 0),
                rom(r7, 0, 1, 0),
            ],
        }
    }

    fn chr_bank(&self, reg: usize) -> usize {
        usize::from(self.chr_regs[reg]) | (usize::from(self.chr_high & 0x03) << 8)
    }

    /// The eight 1 KiB slots a bank set fills under the current CHR mode
    /// (nesdev's tables for `$5101` modes 0-3). `a` selects set A
    /// (registers 0-7) or set B (8-11, mirrored over both halves).
    fn slots_for_set(&self, a: bool) -> [usize; 8] {
        let r = |i: usize| self.chr_bank(if a { i } else { 8 + (i & 3) });
        match self.chr_mode & 0x03 {
            0 => {
                let base = r(7) * 8;
                std::array::from_fn(|s| base + s)
            }
            1 => {
                let (lo, hi) = (r(3) * 4, r(7) * 4);
                std::array::from_fn(|s| if s < 4 { lo + s } else { hi + (s - 4) })
            }
            2 => std::array::from_fn(|s| r([1, 1, 3, 3, 5, 5, 7, 7][s]) * 2 + (s & 1)),
            _ => std::array::from_fn(r),
        }
    }

    fn materialise(&self, slots: [usize; 8], view: &mut [u8; CHR_VIEW_SIZE]) {
        let count = (self.chr_rom.len() / CHR_BANK_1K).max(1);
        for (slot, &bank) in slots.iter().enumerate() {
            let src = (bank % count) * CHR_BANK_1K;
            let dst = slot * CHR_BANK_1K;
            view[dst..dst + CHR_BANK_1K].copy_from_slice(&self.chr_rom[src..src + CHR_BANK_1K]);
        }
    }

    fn recompute_chr_views(&mut self) {
        if self.chr_rom.is_empty() {
            return;
        }
        // nesdev: 8x16 sprites -> set B draws the background, set A the
        // sprites; 8x8 -> the last-written set draws both.
        let bg_set_a = if self.sprites_8x16 {
            false
        } else {
            !self.last_chr_set_b
        };
        let bg = self.slots_for_set(bg_set_a);
        let sp = self.slots_for_set(true);
        let (mut bg_view, mut sp_view) = ([0u8; CHR_VIEW_SIZE], [0u8; CHR_VIEW_SIZE]);
        self.materialise(bg, &mut bg_view);
        self.materialise(sp, &mut sp_view);
        self.chr_view_bg = bg_view;
        self.chr_view_sprites = sp_view;
    }
}

impl Mapper for Mmc5 {
    fn cpu_read(&self, addr: u16) -> u8 {
        let window = usize::from((addr - 0x8000) / PRG_BANK_8K as u16);
        match self.prg_windows()[window] {
            Some(bank) => {
                self.prg_rom[bank * PRG_BANK_8K + usize::from(addr - 0x8000) % PRG_BANK_8K]
            }
            // A RAM-selecting register: the bus routes these through
            // `prg_ram_window` before ever calling this method (module
            // doc, "PRG RAM windows"), so this arm is unreachable via
            // `NesBus` and only answers direct calls (this file's own
            // tests). Open-bus-shaped rather than a panic.
            None => 0,
        }
    }

    fn cpu_write(&mut self, _addr: u16, _value: u8, _cycle: u64) {}

    fn cpu_write_expansion(&mut self, addr: u16, value: u8) {
        match addr {
            0x5100 => {
                self.prg_mode = value & 0x03;
            }
            0x5101 => {
                self.chr_mode = value & 0x03;
                self.recompute_chr_views();
            }
            0x5104 => self.ext_ram_mode = value & 0x03,
            0x5105 => self.nametable_map = value,
            0x5106 => self.fill_tile = value,
            0x5107 => self.fill_attr = value & 0x03,
            // Ticket W14-22: PRG RAM write protect (nesdev.org/wiki/
            // MMC5). Not a bank register -- just latched for
            // `Mapper::prg_ram_write_enabled` to consult.
            0x5102 => self.wram_protect_a = value & 0x03,
            0x5103 => self.wram_protect_b = value & 0x03,
            0x5113 => self.wram_bank = value & 0x07,
            0x5114..=0x5117 => self.prg_regs[usize::from(addr - 0x5114)] = value,
            0x5120..=0x5127 => {
                self.chr_regs[usize::from(addr - 0x5120)] = value;
                self.last_chr_set_b = false;
                self.recompute_chr_views();
            }
            0x5128..=0x512B => {
                self.chr_regs[8 + usize::from(addr - 0x5128)] = value;
                self.last_chr_set_b = true;
                self.recompute_chr_views();
            }
            0x5130 => {
                self.chr_high = value & 0x03;
                self.recompute_chr_views();
            }
            0x5200 => self.split_ctrl = value,
            0x5201 => self.split_scroll = value,
            0x5202 => self.split_chr_bank = value,
            0x5203 => self.irq_scanline = value,
            0x5204 => self.irq_enabled = value & 0x80 != 0,
            0x5205 => self.mul_a = value,
            0x5206 => self.mul_b = value,
            _ => {}
        }
    }

    fn cpu_read_expansion(&mut self, addr: u16) -> Option<u8> {
        match addr {
            0x5204 => {
                let v = (u8::from(self.irq_pending) << 7) | (u8::from(self.in_frame) << 6);
                self.irq_pending = false;
                Some(v)
            }
            0x5205 => Some((u16::from(self.mul_a) * u16::from(self.mul_b)) as u8),
            0x5206 => Some(((u16::from(self.mul_a) * u16::from(self.mul_b)) >> 8) as u8),
            _ => None,
        }
    }

    fn ppu_ctrl_written(&mut self, value: u8) {
        let big = value & 0x20 != 0;
        if big != self.sprites_8x16 {
            self.sprites_8x16 = big;
            self.recompute_chr_views();
        }
    }

    fn mirroring(&self) -> Mirroring {
        let m = self.nametable_map;
        Mirroring::PerTable([m & 3, (m >> 2) & 3, (m >> 4) & 3, (m >> 6) & 3])
    }

    fn chr_window(&self) -> Option<&[u8]> {
        if self.chr_is_ram || self.chr_rom.is_empty() {
            None
        } else {
            Some(&self.chr_view_bg[..])
        }
    }

    fn chr_window_sprites(&self) -> Option<&[u8]> {
        if self.chr_is_ram || self.chr_rom.is_empty() {
            None
        } else {
            Some(&self.chr_view_sprites[..])
        }
    }

    fn fill_tile(&self) -> Option<(u8, u8)> {
        // The 2-bit attribute is replicated into all four quadrants.
        Some((self.fill_tile, self.fill_attr * 0x55))
    }

    fn has_ext_nametable_ram(&self) -> bool {
        true
    }

    /// See this file's module doc, "PRG RAM windows". Ticket W14-22
    /// replaces the old modulo-8-KiB alias with the real rule: the
    /// window's own register (via [`Self::window_reg`], the same
    /// per-mode routing [`Self::prg_windows`] uses for ROM) selects RAM
    /// when its bit 7 is clear, and its low 3 bits pick the chip/page
    /// through [`Self::chip_bank_offset`] exactly like `$5113` does for
    /// `$6000-$7FFF`.
    fn prg_ram_window(&self, addr: u16) -> Option<usize> {
        let window = usize::from((addr - 0x8000) / PRG_BANK_8K as u16);
        let reg = self.window_reg(window);
        if reg & 0x80 != 0 {
            return None;
        }
        let offset_in_bank = usize::from(addr - 0x8000) % PRG_BANK_8K;
        Some(self.chip_bank_offset(reg & 0x07) + offset_in_bank)
    }

    /// Ticket W14-22: `set_prg_ram_len`'s doc on [`Mapper`] -- stored so
    /// [`Self::chip_bank_offset`] can tell an 8/16/32 KiB board apart.
    fn set_prg_ram_len(&mut self, len: usize) {
        self.prg_ram_len = len.max(PRG_BANK_8K);
    }

    /// `$5113`'s low 3 bits select the `$6000-$7FFF` chip/page (ticket
    /// W14-22), through the same [`Self::chip_bank_offset`] rule as an
    /// RAM-selected `$8000+` window.
    fn wram_offset(&self, addr: u16) -> usize {
        self.chip_bank_offset(self.wram_bank) + usize::from(addr - 0x6000)
    }

    /// nesdev.org/wiki/MMC5: PRG RAM writes land only once `$5102` reads
    /// `%10` AND `$5103` reads `%01` (ticket W14-22). Both registers
    /// reset to a non-enabling value, so writes are disabled at
    /// power-on.
    fn prg_ram_write_enabled(&self) -> bool {
        self.wram_protect_a == 0b10 && self.wram_protect_b == 0b01
    }

    fn chr_rom_full(&self) -> Option<&[u8]> {
        if self.chr_is_ram || self.chr_rom.is_empty() {
            None
        } else {
            Some(&self.chr_rom)
        }
    }

    fn ext_attribute_mode(&self) -> Option<u8> {
        if self.ext_ram_mode == 1 {
            Some(self.chr_high)
        } else {
            None
        }
    }

    fn vertical_split(&self) -> Option<(bool, u8, u8, u8)> {
        if self.split_ctrl & 0x80 == 0 {
            return None;
        }
        Some((
            self.split_ctrl & 0x40 != 0,
            self.split_ctrl & 0x1F,
            self.split_scroll,
            self.split_chr_bank,
        ))
    }

    fn scanline_started(&mut self) {
        if !self.in_frame {
            self.in_frame = true;
            self.scanline_counter = 0;
        } else {
            self.scanline_counter = self.scanline_counter.wrapping_add(1);
            if self.scanline_counter == self.irq_scanline && self.irq_scanline != 0 {
                self.irq_pending = true;
            }
        }
    }

    fn frame_ended(&mut self) {
        self.in_frame = false;
        self.scanline_counter = 0;
    }

    fn irq_pending(&self) -> bool {
        self.irq_pending && self.irq_enabled
    }

    fn save_state(&self, out: &mut StateOut<'_>) -> Result<(), StateError> {
        out.u8(self.prg_mode)?;
        out.u8(self.chr_mode)?;
        out.u8(self.ext_ram_mode)?;
        out.u8(self.nametable_map)?;
        out.u8(self.fill_tile)?;
        out.u8(self.fill_attr)?;
        out.bytes(&self.prg_regs)?;
        out.bytes(&self.chr_regs)?;
        out.u8(self.chr_high)?;
        out.bool(self.last_chr_set_b)?;
        out.bool(self.sprites_8x16)?;
        out.u8(self.irq_scanline)?;
        out.bool(self.irq_enabled)?;
        out.bool(self.irq_pending)?;
        out.bool(self.in_frame)?;
        out.u8(self.scanline_counter)?;
        out.u8(self.mul_a)?;
        out.u8(self.mul_b)?;
        out.u8(self.split_ctrl)?;
        out.u8(self.split_scroll)?;
        out.u8(self.split_chr_bank)?;
        // Ticket W14-22. `prg_ram_len` is NOT saved here, the same
        // reasoning `Mapper::save_state`'s own doc gives for ROM bytes:
        // it is re-derived from the loaded cartridge's header every time
        // (`NesBus::new` -> `set_prg_ram_len`, before any state load),
        // so saving it would just be a second, redundant copy of a cart
        // fact a save state must never be allowed to override.
        out.u8(self.wram_bank)?;
        out.u8(self.wram_protect_a)?;
        out.u8(self.wram_protect_b)
    }

    fn load_state(&mut self, inp: &mut StateIn<'_>) -> Result<(), StateError> {
        self.prg_mode = inp.u8()?;
        self.chr_mode = inp.u8()?;
        self.ext_ram_mode = inp.u8()?;
        self.nametable_map = inp.u8()?;
        self.fill_tile = inp.u8()?;
        self.fill_attr = inp.u8()?;
        inp.bytes(&mut self.prg_regs)?;
        inp.bytes(&mut self.chr_regs)?;
        self.chr_high = inp.u8()?;
        self.last_chr_set_b = inp.bool()?;
        self.sprites_8x16 = inp.bool()?;
        self.irq_scanline = inp.u8()?;
        self.irq_enabled = inp.bool()?;
        self.irq_pending = inp.bool()?;
        self.in_frame = inp.bool()?;
        self.scanline_counter = inp.u8()?;
        self.mul_a = inp.u8()?;
        self.mul_b = inp.u8()?;
        self.split_ctrl = inp.u8()?;
        self.split_scroll = inp.u8()?;
        self.split_chr_bank = inp.u8()?;
        self.wram_bank = inp.u8()?;
        self.wram_protect_a = inp.u8()?;
        self.wram_protect_b = inp.u8()?;
        self.recompute_chr_views();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prg(banks: u8) -> Vec<u8> {
        let mut data = vec![0u8; banks as usize * PRG_BANK_8K];
        for n in 0..banks {
            data[n as usize * PRG_BANK_8K] = 0x40 + n;
        }
        data
    }

    fn chr(banks_1k: u8) -> Vec<u8> {
        let mut data = vec![0u8; banks_1k as usize * CHR_BANK_1K];
        for n in 0..banks_1k {
            data[n as usize * CHR_BANK_1K] = 0x80 + n;
        }
        data
    }

    fn w(m: &mut Mmc5, addr: u16, v: u8) {
        m.cpu_write_expansion(addr, v);
    }

    #[test]
    fn power_on_reads_the_last_bank_everywhere_and_prg_modes_follow_nesdev() {
        let mut m = Mmc5::new(prg(32), chr(128), false);
        assert_eq!(m.cpu_read(0xE000), 0x40 + 31);
        assert_eq!(m.cpu_read(0x8000), 0x40 + 31, "mode 3 with $FF everywhere");
        w(&mut m, 0x5100, 3);
        w(&mut m, 0x5114, 0x80 | 4);
        w(&mut m, 0x5115, 0x80 | 5);
        w(&mut m, 0x5116, 0x80 | 6);
        w(&mut m, 0x5117, 7);
        assert_eq!(
            [
                m.cpu_read(0x8000),
                m.cpu_read(0xA000),
                m.cpu_read(0xC000),
                m.cpu_read(0xE000)
            ],
            [0x44, 0x45, 0x46, 0x47]
        );
        w(&mut m, 0x5100, 1); // 16 KiB: $5115 bits 1-6 -> banks 4,5 ; $5117 -> 6,7
        assert_eq!(
            [
                m.cpu_read(0x8000),
                m.cpu_read(0xA000),
                m.cpu_read(0xC000),
                m.cpu_read(0xE000)
            ],
            [0x44, 0x45, 0x46, 0x47]
        );
        w(&mut m, 0x5100, 0); // 32 KiB: $5117 bits 2-6 = 1 -> banks 4-7
        assert_eq!([m.cpu_read(0x8000), m.cpu_read(0xE000)], [0x44, 0x47]);
        w(&mut m, 0x5100, 2);
        w(&mut m, 0x5116, 0x80 | 9);
        assert_eq!(
            [m.cpu_read(0x8000), m.cpu_read(0xC000), m.cpu_read(0xE000)],
            [0x44, 0x49, 0x47]
        );
        w(&mut m, 0x5116, 0x09); // RAM-selecting: not reachable here
        assert_eq!(m.cpu_read(0xC000), 0);
    }

    #[test]
    fn chr_modes_and_the_sprite_size_pick_the_bank_set() {
        let mut m = Mmc5::new(prg(32), chr(128), false);
        for i in 0..8u16 {
            w(&mut m, 0x5120 + i, 10 + i as u8); // set A
        }
        for i in 0..4u16 {
            w(&mut m, 0x5128 + i, 50 + i as u8); // set B, written last
        }
        w(&mut m, 0x5101, 3); // 1 KiB banks
                              // 8x8 sprites: the last-written set (B) draws everything.
        let bg = m.chr_window().unwrap();
        assert_eq!(
            [bg[0], bg[0x400], bg[0x1000]],
            [0x80 + 50, 0x80 + 51, 0x80 + 50]
        );
        m.ppu_ctrl_written(0x20); // 8x16: B for background, A for sprites
        let bg = m.chr_window().unwrap();
        let sp = m.chr_window_sprites().unwrap();
        assert_eq!(bg[0x400], 0x80 + 51);
        assert_eq!([sp[0], sp[0x1C00]], [0x80 + 10, 0x80 + 17]);
        w(&mut m, 0x5101, 0); // 8 KiB: set A from $5127 = 17 -> banks 136..; masked by count 128
        let sp = m.chr_window_sprites().unwrap();
        assert_eq!(sp[0], 0x80 + ((17 * 8) % 128) as u8);
        w(&mut m, 0x5101, 2); // 2 KiB: A uses $5121,$5123,$5125,$5127
        let sp = m.chr_window_sprites().unwrap();
        assert_eq!(
            [sp[0], sp[0x400], sp[0x800]],
            [0x80 + 22, 0x80 + 23, 0x80 + 26]
        );
    }

    #[test]
    fn nametable_map_fill_and_multiplier() {
        let mut m = Mmc5::new(prg(32), chr(8), false);
        w(&mut m, 0x5105, 0b11_10_01_00);
        assert_eq!(m.mirroring(), Mirroring::PerTable([0, 1, 2, 3]));
        w(&mut m, 0x5106, 0x42);
        w(&mut m, 0x5107, 0b10);
        assert_eq!(m.fill_tile(), Some((0x42, 0xAA)));
        assert!(m.has_ext_nametable_ram());
        w(&mut m, 0x5205, 200);
        w(&mut m, 0x5206, 3);
        assert_eq!(m.cpu_read_expansion(0x5205), Some((600u16 & 0xFF) as u8));
        assert_eq!(m.cpu_read_expansion(0x5206), Some((600u16 >> 8) as u8));
    }

    #[test]
    fn prg_ram_windows_are_reported_only_when_the_registers_bit_7_is_clear() {
        let mut m = Mmc5::new(prg(8), chr(8), false);
        w(&mut m, 0x5100, 3); // mode 3: four independent 8 KiB windows
        w(&mut m, 0x5114, 0x00); // RAM at $8000 (bit 7 clear)
        w(&mut m, 0x5115, 0x80); // ROM at $A000
        assert_eq!(m.prg_ram_window(0x8000), Some(0));
        assert_eq!(
            m.prg_ram_window(0x8FFF),
            Some(0x0FFF),
            "offset within the window, not the bank number"
        );
        assert_eq!(m.prg_ram_window(0xA000), None, "this window selects ROM");
        assert_eq!(m.prg_ram_window(0xE000), None, "$5117 is always ROM");
    }

    /// Ticket W14-22, acceptance: ETROM (2x8 KiB chips) keeps `$6000`
    /// distinct between `$5113=0` (chip 0) and `$5113=4` (chip 1).
    #[test]
    fn etrom_two_chips_are_distinct_through_5113() {
        let mut m = Mmc5::new(prg(8), chr(8), false);
        m.set_prg_ram_len(16 * 1024); // ETROM: 2x8 KiB chips
        w(&mut m, 0x5113, 0);
        assert_eq!(m.wram_offset(0x6000), 0, "chip 0");
        assert_eq!(m.wram_offset(0x7FFF), 0x1FFF, "chip 0, end of window");
        w(&mut m, 0x5113, 4);
        assert_eq!(m.wram_offset(0x6000), 0x2000, "chip 1 starts at 8 KiB");
        assert_eq!(m.wram_offset(0x7FFF), 0x3FFF, "chip 1, end of window");
        // Bits 0-1 (page within chip) don't matter on ETROM: each chip
        // has exactly one page.
        w(&mut m, 0x5113, 1);
        assert_eq!(m.wram_offset(0x6000), 0, "still chip 0");
        w(&mut m, 0x5113, 7);
        assert_eq!(m.wram_offset(0x6000), 0x2000, "still chip 1");
    }

    /// Ticket W14-22, acceptance: EKROM (a single 8 KiB chip) mirrors
    /// every `$5113` value onto that one chip -- `$5113=4` lands on the
    /// same bank as `$5113=0`.
    #[test]
    fn ekrom_single_chip_mirrors_every_bank_value() {
        let mut m = Mmc5::new(prg(8), chr(8), false);
        m.set_prg_ram_len(8 * 1024); // EKROM: 1x8 KiB chip (the default)
        for bank in 0..8u8 {
            w(&mut m, 0x5113, bank);
            assert_eq!(
                m.wram_offset(0x6000),
                0,
                "bank {bank} must mirror onto the only chip"
            );
        }
    }

    /// Ticket W14-22, acceptance: EWROM (32 KiB, one chip, four 8 KiB
    /// pages) picks a distinct page per low-2-bit value of `$5113` and
    /// wraps once every page has been used (bit 2 is part of the same
    /// chip, not a second one).
    #[test]
    fn ewrom_32kib_single_chip_has_four_distinct_pages() {
        let mut m = Mmc5::new(prg(8), chr(8), false);
        m.set_prg_ram_len(32 * 1024);
        for bank in 0..4u8 {
            w(&mut m, 0x5113, bank);
            assert_eq!(m.wram_offset(0x6000), usize::from(bank) * 0x2000);
        }
        w(&mut m, 0x5113, 4);
        assert_eq!(m.wram_offset(0x6000), 0, "bank 4 wraps back to page 0");
    }

    /// Ticket W14-22, acceptance: a RAM-selected `$8000+` window is
    /// chip-aware exactly like `$6000-$7FFF`, through the same
    /// `chip_bank_offset` rule (`$5114`-`$5116`, not `$5113`).
    #[test]
    fn ram_selected_8000_window_is_chip_aware_on_etrom() {
        let mut m = Mmc5::new(prg(8), chr(8), false);
        m.set_prg_ram_len(16 * 1024);
        w(&mut m, 0x5100, 3); // mode 3: four independent 8 KiB windows
        w(&mut m, 0x5114, 0x04); // $8000 window: bit 7 clear -> RAM, chip 1
        assert_eq!(m.prg_ram_window(0x8000), Some(0x2000));
        assert_eq!(m.prg_ram_window(0x9FFF), Some(0x3FFF));
    }

    /// Ticket W14-22, acceptance: `$5102`/`$5103` gate PRG RAM writes
    /// (nesdev.org/wiki/MMC5). Disabled at reset; only the exact
    /// enabling combination (`$5102=%10`, `$5103=%01`) allows writes.
    #[test]
    fn prg_ram_write_protect_requires_the_exact_5102_5103_combination() {
        let mut m = Mmc5::new(prg(8), chr(8), false);
        assert!(
            !m.prg_ram_write_enabled(),
            "reset values (5102=01, 5103=10) must not enable writes"
        );
        w(&mut m, 0x5102, 0b10);
        assert!(!m.prg_ram_write_enabled(), "5103 still wrong");
        w(&mut m, 0x5103, 0b01);
        assert!(m.prg_ram_write_enabled(), "both registers now match");
        w(&mut m, 0x5102, 0b01);
        assert!(!m.prg_ram_write_enabled(), "5102 reverted");
    }

    #[test]
    fn ext_attribute_mode_and_the_vertical_split_report_from_their_own_registers() {
        let mut m = Mmc5::new(prg(8), chr(8), false);
        assert_eq!(m.ext_attribute_mode(), None, "ext RAM mode 0 by default");
        w(&mut m, 0x5104, 1);
        w(&mut m, 0x5130, 2);
        assert_eq!(m.ext_attribute_mode(), Some(2), "mode 1, with $5130's bits");
        assert_eq!(
            m.vertical_split(),
            None,
            "disabled until $5200 bit 7 is set"
        );
        w(&mut m, 0x5200, 0x80 | 0x40 | 5); // enabled, right side, split tile 5
        w(&mut m, 0x5201, 10);
        w(&mut m, 0x5202, 7);
        assert_eq!(m.vertical_split(), Some((true, 5, 10, 7)));
    }

    #[test]
    fn the_scanline_counter_fires_at_the_target_and_5204_reports_and_acks() {
        let mut m = Mmc5::new(prg(32), chr(8), false);
        w(&mut m, 0x5203, 3);
        w(&mut m, 0x5204, 0x80);
        assert_eq!(m.cpu_read_expansion(0x5204), Some(0x00), "not in frame yet");
        m.scanline_started(); // scanline 0: enters the frame
        assert_eq!(m.cpu_read_expansion(0x5204), Some(0x40));
        m.scanline_started(); // 1
        m.scanline_started(); // 2
        assert!(!m.irq_pending());
        m.scanline_started(); // 3: fires
        assert!(m.irq_pending());
        assert_eq!(m.cpu_read_expansion(0x5204), Some(0xC0));
        assert!(!m.irq_pending(), "reading $5204 acknowledges");
        m.frame_ended();
        assert_eq!(m.cpu_read_expansion(0x5204), Some(0x00));
        w(&mut m, 0x5204, 0x00);
        for _ in 0..10 {
            m.scanline_started();
        }
        assert!(!m.irq_pending(), "disabled: pending never reaches the line");
    }
}
