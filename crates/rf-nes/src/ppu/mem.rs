//! The PPU's own memory bus: pattern tables (CHR), nametables (with mirror
//! mapping), and palette RAM — `$0000-$3FFF` as seen through `v`/`$2007`,
//! entirely separate from the CPU-side `$0000-$FFFF` bus
//! ([`crate::system::NesBus`]).
//!
//! No `Mapper` trait exists yet (see `crate::ppu`'s module doc scope
//! fence): CHR is a flat `Vec<u8>` handed in at construction, addressed
//! directly (mapper 0 has no CHR banking).
use super::Ppu;
use rf_cart::Mirroring;

impl Ppu {
    /// One PPU-bus read at `addr & 0x3FFF`: `$0000-$1FFF` pattern tables
    /// (CHR), `$2000-$3EFF` nametables (mirrored per `mirroring`),
    /// `$3F00-$3FFF` palette RAM.
    pub(super) fn mem_read(&self, addr: u16) -> u8 {
        let addr = addr & 0x3FFF;
        match addr {
            0x0000..=0x1FFF => self.chr_read(addr),
            0x2000..=0x3EFF => self.vram[self.nametable_offset(addr)],
            0x3F00..=0x3FFF => self.palette_read(addr),
            _ => unreachable!("addr masked to 14 bits above"),
        }
    }

    /// One PPU-bus write at `addr & 0x3FFF` — same ranges as [`Ppu::mem_read`].
    pub(super) fn mem_write(&mut self, addr: u16, value: u8) {
        let addr = addr & 0x3FFF;
        match addr {
            0x0000..=0x1FFF => self.chr_write(addr, value),
            0x2000..=0x3EFF => {
                let offset = self.nametable_offset(addr);
                self.vram[offset] = value;
            }
            0x3F00..=0x3FFF => self.palette_write(addr, value),
            _ => unreachable!("addr masked to 14 bits above"),
        }
    }

    fn chr_read(&self, addr: u16) -> u8 {
        if self.chr.is_empty() {
            return 0;
        }
        self.chr[addr as usize % self.chr.len()]
    }

    /// CHR ROM writes have no effect (no mapper registers exist for mapper
    /// 0, and real ROM can't be written); CHR RAM cartridges (`chr_is_ram`)
    /// accept them.
    fn chr_write(&mut self, addr: u16, value: u8) {
        if !self.chr_is_ram || self.chr.is_empty() {
            return;
        }
        let len = self.chr.len();
        self.chr[addr as usize % len] = value;
    }

    /// Physical offset into `vram` (4 KiB, one 1 KiB bank per logical
    /// nametable) for a PPU-bus address in `$2000-$3EFF`. `$3000-$3EFF`
    /// mirrors `$2000-$2EFF` exactly (both fold into the same 4 KiB modulo
    /// below), matching nesdev.org/wiki/PPU_memory_map.
    ///
    /// Horizontal mirroring: nametables 0/1 share physical bank 0, 2/3
    /// share bank 1 (`logical_bank / 2`). Vertical: 0/2 share bank 0, 1/3
    /// share bank 1 (`logical_bank % 2`). Four-screen: each of the 4
    /// logical nametables gets its own physical bank (`vram` is sized for
    /// exactly this case).
    fn nametable_offset(&self, addr: u16) -> usize {
        let logical_offset = (addr - 0x2000) % 0x1000;
        let logical_bank = (logical_offset / 0x400) as usize;
        let within_bank = (logical_offset % 0x400) as usize;
        let physical_bank = match self.mirroring {
            Mirroring::Horizontal => logical_bank / 2,
            Mirroring::Vertical => logical_bank % 2,
            Mirroring::FourScreen => logical_bank,
        };
        physical_bank * 0x400 + within_bank
    }

    /// Palette RAM index (0-31) for a PPU-bus address in `$3F00-$3FFF`:
    /// mirrored every 32 bytes, with `$3F10`/`$3F14`/`$3F18`/`$3F1C`
    /// aliasing `$3F00`/`$3F04`/`$3F08`/`$3F0C` (nesdev.org/wiki/PPU_palettes:
    /// "entry 0 of each palette is also unique in that its color value is
    /// shared between the background and sprite palettes... the backdrop
    /// color can be written through both $3F00 and $3F10" — the same
    /// shared-storage mechanism applies symmetrically to the other three
    /// sprite-palette entry-0s).
    fn palette_index(addr: u16) -> usize {
        let mut index = (addr & 0x1F) as usize;
        if index >= 0x10 && index.is_multiple_of(4) {
            index -= 0x10;
        }
        index
    }

    pub(super) fn palette_read(&self, addr: u16) -> u8 {
        self.palette[Self::palette_index(addr)]
    }

    fn palette_write(&mut self, addr: u16, value: u8) {
        let index = Self::palette_index(addr);
        self.palette[index] = value;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ppu::Ppu;

    fn ppu_with_mirroring(mirroring: Mirroring) -> Ppu {
        Ppu::new(vec![0u8; 0x2000], false, mirroring)
    }

    #[test]
    fn horizontal_mirroring_aliases_nt0_with_nt1_and_nt2_with_nt3() {
        let mut ppu = ppu_with_mirroring(Mirroring::Horizontal);
        ppu.mem_write(0x2000, 0xAB); // NT0
        assert_eq!(ppu.mem_read(0x2400), 0xAB, "NT1 aliases NT0");
        ppu.mem_write(0x2800, 0xCD); // NT2
        assert_eq!(ppu.mem_read(0x2C00), 0xCD, "NT3 aliases NT2");
        assert_ne!(
            ppu.mem_read(0x2000),
            ppu.mem_read(0x2800),
            "NT0/NT1 bank must be distinct from NT2/NT3 bank"
        );
    }

    #[test]
    fn vertical_mirroring_aliases_nt0_with_nt2_and_nt1_with_nt3() {
        let mut ppu = ppu_with_mirroring(Mirroring::Vertical);
        ppu.mem_write(0x2000, 0x11); // NT0
        assert_eq!(ppu.mem_read(0x2800), 0x11, "NT2 aliases NT0");
        ppu.mem_write(0x2400, 0x22); // NT1
        assert_eq!(ppu.mem_read(0x2C00), 0x22, "NT3 aliases NT1");
    }

    #[test]
    fn nametable_3000_mirrors_2000() {
        let mut ppu = ppu_with_mirroring(Mirroring::Horizontal);
        ppu.mem_write(0x2001, 0x55);
        assert_eq!(ppu.mem_read(0x3001), 0x55);
    }

    #[test]
    fn four_screen_mirroring_keeps_all_four_nametables_distinct() {
        let mut ppu = ppu_with_mirroring(Mirroring::FourScreen);
        ppu.mem_write(0x2000, 1);
        ppu.mem_write(0x2400, 2);
        ppu.mem_write(0x2800, 3);
        ppu.mem_write(0x2C00, 4);
        assert_eq!(ppu.mem_read(0x2000), 1);
        assert_eq!(ppu.mem_read(0x2400), 2);
        assert_eq!(ppu.mem_read(0x2800), 3);
        assert_eq!(ppu.mem_read(0x2C00), 4);
    }

    #[test]
    fn palette_mirrors_every_32_bytes() {
        let mut ppu = ppu_with_mirroring(Mirroring::Horizontal);
        ppu.mem_write(0x3F05, 0x2A);
        assert_eq!(ppu.mem_read(0x3F25), 0x2A);
        assert_eq!(ppu.mem_read(0x3F45), 0x2A);
    }

    #[test]
    fn sprite_palette_entry_zeros_alias_background_entry_zeros() {
        let mut ppu = ppu_with_mirroring(Mirroring::Horizontal);
        ppu.mem_write(0x3F00, 0x01);
        assert_eq!(ppu.mem_read(0x3F10), 0x01);
        ppu.mem_write(0x3F14, 0x02);
        assert_eq!(ppu.mem_read(0x3F04), 0x02);
        ppu.mem_write(0x3F18, 0x03);
        assert_eq!(ppu.mem_read(0x3F08), 0x03);
        ppu.mem_write(0x3F1C, 0x04);
        assert_eq!(ppu.mem_read(0x3F0C), 0x04);
    }

    #[test]
    fn non_entry_zero_sprite_palette_slots_are_independent() {
        let mut ppu = ppu_with_mirroring(Mirroring::Horizontal);
        ppu.mem_write(0x3F01, 0x11);
        ppu.mem_write(0x3F11, 0x22);
        assert_eq!(ppu.mem_read(0x3F01), 0x11, "not aliased, unlike entry 0");
        assert_eq!(ppu.mem_read(0x3F11), 0x22);
    }

    #[test]
    fn chr_rom_writes_are_ignored() {
        let mut ppu = ppu_with_mirroring(Mirroring::Horizontal);
        let before = ppu.mem_read(0x0000);
        ppu.mem_write(0x0000, !before);
        assert_eq!(ppu.mem_read(0x0000), before, "CHR ROM is not writable");
    }

    #[test]
    fn chr_ram_writes_take_effect() {
        let mut ppu = Ppu::new(vec![0u8; 0x2000], true, Mirroring::Horizontal);
        ppu.mem_write(0x0010, 0x99);
        assert_eq!(ppu.mem_read(0x0010), 0x99);
    }
}
