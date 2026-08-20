//! MDMA — general-purpose DMA (ticket W6-02a;
//! `docs/design/EMULATION_CORES.md` §3.1, §3.2).
//!
//! ## Scope, stated because it is deliberately partial
//!
//! The acceptance is "MDMA basics **sufficient to boot libSFX fixture
//! ROMs**"; full DMA/HDMA edge cases are W7 per §3.2. So this implements
//! the eight channels, the seven transfer patterns, `$420B` triggering
//! and the byte-by-byte cost — and does **not** implement HDMA, DMA/HDMA
//! contention, or mid-transfer register writes.
//!
//! What it will not do is pretend: [`Dma::run`] transfers real bytes
//! through the real bus, so a fixture that DMAs a tilemap gets the
//! tilemap. A stub that cleared `$420B` and moved nothing would boot the
//! same ROMs and report the same success while copying nothing at all.
//!
//! ## Cost
//!
//! 8 master cycles per byte, plus 8 for channel setup (§3.1). Returned
//! rather than applied, because the CPU owns the clock and the bus should
//! not silently advance it.

/// One DMA channel's registers (`$43x0`-`$43x6`).
#[derive(Debug, Clone, Copy, Default)]
pub struct Channel {
    /// `$43x0` DMAPn — direction (bit 7), addressing mode, transfer
    /// pattern (bits 0-2).
    pub control: u8,
    /// `$43x1` BBADn — the `$21xx` register this channel talks to.
    pub b_address: u8,
    /// `$43x2`-`$43x4` A1Tn/A1Bn — the 24-bit CPU-side address.
    pub a_address: u32,
    /// `$43x5`/`$43x6` DASn — byte count; 0 means 65536.
    pub count: u16,
}

impl Channel {
    /// Bit 7: 1 = B-bus to A-bus (read from the PPU side).
    #[must_use]
    pub fn reverse(&self) -> bool {
        self.control & 0x80 != 0
    }
    /// Bit 3: fixed address. Bit 4: decrement instead of increment.
    #[must_use]
    pub fn a_step(&self) -> i32 {
        if self.control & 0x08 != 0 {
            0
        } else if self.control & 0x10 != 0 {
            -1
        } else {
            1
        }
    }
    /// The B-bus offset pattern, bits 0-2. Each entry is added to
    /// `b_address` for successive bytes of the unit.
    #[must_use]
    pub fn pattern(&self) -> &'static [u8] {
        match self.control & 0x07 {
            0 => &[0],
            1 => &[0, 1],
            2 | 6 => &[0, 0],
            3 | 7 => &[0, 0, 1, 1],
            4 => &[0, 1, 2, 3],
            _ => &[0, 1, 1, 1],
        }
    }
}

/// The eight channels plus the `$420B` enable latch.
#[derive(Debug, Clone, Default)]
pub struct Dma {
    pub channels: [Channel; 8],
}

/// Master cycles charged per byte transferred.
pub const CYCLES_PER_BYTE: u64 = 8;
/// Master cycles charged once per active channel.
pub const CYCLES_PER_CHANNEL: u64 = 8;
