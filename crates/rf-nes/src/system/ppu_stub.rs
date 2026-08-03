//! Deliberately minimal PPU register stub.
//!
//! There is no PPU yet — that's ticket W1-04a/b — so `$2000-$3FFF` (mirrored
//! every 8 bytes, [nesdev.org/wiki/PPU_registers](https://www.nesdev.org/wiki/PPU_registers))
//! is almost entirely unimplemented here on purpose: `PPUCTRL`/`PPUMASK`/
//! `PPUSTATUS`/`PPUSCROLL`/`PPUADDR`/`PPUDATA` writes are dropped and reads
//! return open bus (via [`NesBus`](super::NesBus)'s shared latch), which is
//! an obviously-wrong placeholder — that's the point: it's an unambiguous
//! seam for W1-04a to replace, not a partial PPU implementation to build on
//! top of.
//!
//! The two exceptions are `OAMADDR` (`$2003`) and `OAMDATA` (`$2004`),
//! which this ticket *does* implement for real: OAM DMA (`$4014`) needs
//! somewhere to land its 256 bytes, and on real hardware OAM DMA is
//! defined in terms of these two registers (it behaves as if the CPU wrote
//! `OAMDATA` 256 times in a row, which is why it honors and advances
//! whatever `OAMADDR` already holds — nesdev.org/wiki/DMA: "the transfer
//! begins at the current OAM write address").
pub struct PpuStub {
    oam: [u8; 256],
    oam_addr: u8,
}

impl Default for PpuStub {
    fn default() -> Self {
        PpuStub {
            oam: [0; 256],
            oam_addr: 0,
        }
    }
}

impl PpuStub {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read register `index` (`addr & 0x0007`). Only `OAMDATA` (index 4)
    /// has real behavior; everything else defers to the bus's open-bus
    /// latch (`open_bus`, passed in by the caller) since this stub drives
    /// nothing for those registers.
    pub fn read(&self, index: u8, open_bus: u8) -> u8 {
        match index {
            4 => self.oam[self.oam_addr as usize],
            _ => open_bus,
        }
    }

    /// Write register `index`. Only `OAMADDR` (index 3) and `OAMDATA`
    /// (index 4) do anything; every other write is silently dropped
    /// (documented stub — see module doc).
    pub fn write(&mut self, index: u8, value: u8) {
        match index {
            3 => self.oam_addr = value,
            4 => self.write_oam_data(value),
            _ => {}
        }
    }

    /// The exact side effect of one `OAMDATA` write, factored out because
    /// OAM DMA (`$4014`) drives it 256 times without going through the
    /// normal register-write path (nesdev: DMA behaves *as if* the CPU
    /// wrote `OAMDATA` repeatedly, but does not actually issue CPU bus
    /// writes to `$2004` — see `NesBus::run_oam_dma`).
    pub fn write_oam_data(&mut self, value: u8) {
        self.oam[self.oam_addr as usize] = value;
        self.oam_addr = self.oam_addr.wrapping_add(1);
    }

    /// The full 256-byte OAM, for the future PPU/renderer (sprite
    /// evaluation) and for this ticket's own DMA-ordering tests.
    pub fn oam(&self) -> &[u8; 256] {
        &self.oam
    }

    /// Current `OAMADDR` value (test/debug visibility).
    pub fn oam_addr(&self) -> u8 {
        self.oam_addr
    }
}
