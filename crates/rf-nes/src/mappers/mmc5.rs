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
//! selects PRG RAM on the board; this crate's bus owns the one 8 KiB PRG
//! RAM at `$6000` and the trait cannot lend it out, so such a window
//! reads as open bus here. **Documented gap:** none of the titles above
//! maps RAM into `$8000-$DFFF` during play.
//!
//! # Slice 2, not here
//!
//! Extended attributes (`$5104` mode 1, per-tile CHR bank and palette
//! from extended RAM) and the vertical split (`$5200-$5202`) change the
//! background fetch itself; they are their own ticket.

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
        };
        m.recompute_chr_views();
        m
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
            // A RAM-selecting register: not reachable through the trait
            // (module doc). Open-bus-shaped rather than a panic.
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
        out.u8(self.mul_b)
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
