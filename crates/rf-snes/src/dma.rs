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

/// One DMA channel's registers (`$43x0`-`$43xB`).
#[derive(Debug, Clone, Copy, Default)]
pub struct Channel {
    /// `$43x0` DMAPn — direction (bit 7), addressing mode, transfer
    /// pattern (bits 0-2).
    pub control: u8,
    /// `$43x1` BBADn — the `$21xx` register this channel talks to.
    pub b_address: u8,
    /// `$43x2`-`$43x4` A1Tn/A1Bn — the 24-bit CPU-side address.
    pub a_address: u32,
    /// `$43x5`/`$43x6` — byte count for MDMA, and the **indirect
    /// address** for HDMA. The two uses share one register pair on
    /// hardware, which is why a channel cannot do both at once.
    pub count: u16,
    /// `$43x7` DASBn — bank of the HDMA indirect address.
    pub indirect_bank: u8,
    /// `$43x8`/`$43x9` A2An — the HDMA table pointer, reloaded from
    /// `a_address` at the start of each frame.
    pub table_addr: u16,
    /// `$43xA` NLTRn — the line counter. Bit 7 is the repeat flag; the
    /// low seven bits are the count.
    pub line_counter: u8,
    /// Set when this channel's table has terminated for the frame.
    pub hdma_done: bool,
    /// Whether the current line should transfer (the repeat flag,
    /// resolved).
    pub do_transfer: bool,
}

impl Channel {
    /// Bit 7: 1 = B-bus to A-bus (read from the PPU side).
    #[must_use]
    pub fn reverse(&self) -> bool {
        self.control & 0x80 != 0
    }
    /// Bit 6: HDMA indirect addressing — the table holds POINTERS rather
    /// than data.
    #[must_use]
    pub fn indirect(&self) -> bool {
        self.control & 0x40 != 0
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
            // Pattern 5 alternates between TWO registers — b, b+1, b, b+1
            // — it does not write b once and b+1 three times.
            _ => &[0, 1, 0, 1],
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
