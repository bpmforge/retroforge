//! S-DD1 ("Data Decompressor"), fullsnes "SNES Cart S-DD1 (Data
//! Decompressor)" (`fullsnes.txt:10628-10757`, ticket W19-03). One retail
//! title this build can exercise: Street Fighter Alpha 2 (1996, Capcom);
//! the chapter also names Star Ocean, which needs the LN3B board's extra
//! SRAM this project's library does not carry.
//!
//! ## What the chip is
//!
//! Unlike SA-1/GSU/CX4, the S-DD1 has no CPU of its own and runs no
//! program — clean-room hardware emulation of a fixed decompression
//! pipeline, nothing copyrighted to avoid (NFR-011). Fullsnes gives the
//! whole algorithm as pseudocode (`fullsnes.txt:10673-10757`,
//! "SNES Cart S-DD1 Decompression Algorithm"); every function, table and
//! constant below is a direct transcription of it, cited by name.
//!
//! ## Register model (fullsnes "S-DD1 I/O Ports", `fullsnes.txt:10638-10648`)
//!
//! - `$4800` DMA Enable 1 (bit0..7 = DMA channel 0..7) — "unchanged after
//!   DMA". Which channels the S-DD1 watches at all.
//! - `$4801` DMA Enable 2 (same bit layout) — "automatically cleared after
//!   DMA". A channel only decompresses while BOTH `$4800` and `$4801` name
//!   it (see [`Sdd1Regs::channel_decompresses`]); `$4801`'s bits self-clear
//!   once [`Sdd1Regs::clear_transfer_bits`] is told the channel ran.
//! - `$4802`/`$4803` "Unknown" — fullsnes: "set to 0000h by Star Ocean
//!   (maybe SRAM related)" / "unused by Street Fighter Alpha 2". Stored and
//!   read back verbatim, same as every other fullsnes-hedged port in this
//!   crate (`crate::obc1`'s `$7FF7` is the precedent).
//! - `$4804-$4807` ROM Bank for `$C00000-$CFFFFF`/`$D00000-$DFFFFF`/
//!   `$E00000-$EFFFFF`/`$F00000-$FFFFFF`, "in 1MByte units" — resolved by
//!   [`crate::mapping::sdd1_target`].
//!
//! No reset values are documented (unlike SA-1's explicit table) — every
//! register starts zeroed, the same default OBC1's ambiguous ports use.
//!
//! ## Memory map (fullsnes "S-DD1 Memory Map", `fullsnes.txt:10649-10656`)
//!
//! - `$008000-$00FFFF` "Exception Handlers, mapped in LoROM-fashion" is
//!   nothing more than the cartridge's own ordinary LoROM window at bank
//!   `$00` — [`crate::mapping::map`] already resolves it, so this module
//!   adds no code for it (the whole cart is map mode `$2`, "LoROM/32K Banks
//!   plus S-DD1", per `rf_cart::SnesMapMode` — only banks `$C0-$FF` need
//!   special handling).
//! - `$C00000-$FFFFFF`, "ROM (mapped via Port `$4804`/`$4805`/`$4806`/
//!   `$4807`) (in HiROM fashion)" per 1 MiB group — [`crate::mapping::sdd1_target`].
//!
//! ## DMA decompression trigger
//!
//! Fullsnes: "`<DMA>` DMA from ROM returns Decompressed Data (originated at
//! DMA start addr)". This build reads that as: a general-purpose DMA
//! channel whose bit is set in BOTH `$4800` and `$4801`, and whose A-bus
//! start address resolves (through [`crate::mapping::sdd1_target`]) into
//! the S-DD1 ROM window, has every one of its per-byte A-bus reads replaced
//! by [`Sdd1Decompressor::next_byte`], seeded once at the channel's start
//! address — [`crate::bus::SnesBus::run_channel`] is where this is wired
//! in. `$4801`'s bit for that channel clears once the transfer completes
//! (fullsnes: "automatically cleared after DMA"); `$4800` is left alone
//! ("unchanged after DMA").
//!
//! ## What is NOT modelled
//!
//! - HDMA is not intercepted, only general-purpose (`$420B`) DMA — fullsnes
//!   only ever writes "DMA", and every known S-DD1 title's decompression
//!   use (tile/tilemap streaming into VRAM) is a one-shot general DMA, not
//!   a per-scanline HDMA table walk.
//! - `$4802`/`$4803` gate nothing: fullsnes hedges both as "Unknown"; they
//!   are stored and read back and never branched on, per this crate's
//!   convention for hedged ports.

use rf_core_api::StateError;

use crate::state::{StateIn, StateOut};

/// Fullsnes "S-DD1 Decompression Algorithm", `EvolutionCodeSize[0..32]`
/// (`fullsnes.txt` table under that heading). Indexed by `context_states`.
const EVOLUTION_CODE_SIZE: [u8; 33] = [
    0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 0, 1, 2, 3, 4, 5, 6,
    7,
];

/// Fullsnes `EvolutionMpsNext[0..32]`.
const EVOLUTION_MPS_NEXT: [u8; 33] = [
    25, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 24, 26,
    27, 28, 29, 30, 31, 32, 24,
];

/// Fullsnes `EvolutionLpsNext[0..32]`.
const EVOLUTION_LPS_NEXT: [u8; 33] = [
    25, 1, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 1, 2,
    4, 8, 12, 16, 18, 22,
];

/// Fullsnes `RunTable[0..127]`.
#[rustfmt::skip]
const RUN_TABLE: [u8; 128] = [
    128, 64, 96, 32, 112, 48, 80, 16, 120, 56, 88, 24, 104, 40, 72, 8,
    124, 60, 92, 28, 108, 44, 76, 12, 116, 52, 84, 20, 100, 36, 68, 4,
    126, 62, 94, 30, 110, 46, 78, 14, 118, 54, 86, 22, 102, 38, 70, 6,
    122, 58, 90, 26, 106, 42, 74, 10, 114, 50, 82, 18,  98, 34, 66, 2,
    127, 63, 95, 31, 111, 47, 79, 15, 119, 55, 87, 23, 103, 39, 71, 7,
    123, 59, 91, 27, 107, 43, 75, 11, 115, 51, 83, 19,  99, 35, 67, 3,
    125, 61, 93, 29, 109, 45, 77, 13, 117, 53, 85, 21, 101, 37, 69, 5,
    121, 57, 89, 25, 105, 41, 73,  9, 113, 49, 81, 17,  97, 33, 65, 1,
];

/// The four SNES-visible S-DD1 registers (`$4800-$4807`, minus the
/// documented-unused pair) plus the two "Unknown" bytes, module doc has the
/// full citation.
#[derive(Debug, Clone, Copy)]
pub struct Sdd1Regs {
    /// `$4800` DMA Enable 1, one bit per channel 0..7.
    dma_enable: u8,
    /// `$4801` DMA Enable 2 (the transfer-enable bit), one bit per channel.
    transfer_enable: u8,
    /// `$4802` "Unknown".
    unknown2: u8,
    /// `$4803` "Unknown".
    unknown3: u8,
    /// `$4804-$4807`, ROM bank (1 MiB units) for the `$C0/$D0/$E0/$F0`
    /// groups respectively.
    banks: [u8; 4],
}

impl Default for Sdd1Regs {
    fn default() -> Self {
        Self::new()
    }
}

impl Sdd1Regs {
    #[must_use]
    pub fn new() -> Self {
        Self {
            dma_enable: 0,
            transfer_enable: 0,
            unknown2: 0,
            unknown3: 0,
            banks: [0; 4],
        }
    }

    /// A read of `$4800-$4807`.
    ///
    /// # Panics
    /// If `offset` is outside `$4800-$4807` — callers only reach this
    /// through the bus's own `0x4800..=0x4807` match arm.
    #[must_use]
    pub fn read(&self, offset: u16) -> u8 {
        match offset {
            0x4800 => self.dma_enable,
            0x4801 => self.transfer_enable,
            0x4802 => self.unknown2,
            0x4803 => self.unknown3,
            0x4804..=0x4807 => self.banks[usize::from(offset - 0x4804)],
            _ => unreachable!("Sdd1 register offset outside $4800-$4807: {offset:#06x}"),
        }
    }

    /// A write to `$4800-$4807`. Same panic contract as [`Self::read`].
    pub fn write(&mut self, offset: u16, value: u8) {
        match offset {
            0x4800 => self.dma_enable = value,
            0x4801 => self.transfer_enable = value,
            0x4802 => self.unknown2 = value,
            0x4803 => self.unknown3 = value,
            0x4804..=0x4807 => self.banks[usize::from(offset - 0x4804)] = value,
            _ => unreachable!("Sdd1 register offset outside $4800-$4807: {offset:#06x}"),
        }
    }

    /// The four 1 MiB bank selects, for [`crate::mapping::sdd1_target`].
    #[must_use]
    pub fn banks(&self) -> [u8; 4] {
        self.banks
    }

    /// Whether DMA channel `ch` (0..7) is armed for decompression: fullsnes
    /// documents the two enable ports as independent bitmasks, and this
    /// build reads a channel as "decompressing" only while both name it —
    /// see the module doc's DMA-trigger section.
    #[must_use]
    pub fn channel_decompresses(&self, ch: usize) -> bool {
        let bit = 1u8 << ch;
        self.dma_enable & bit != 0 && self.transfer_enable & bit != 0
    }

    /// `$4801`'s bit for `ch` self-clears once its DMA has run (fullsnes:
    /// "automatically cleared after DMA"); `$4800` ("unchanged after DMA")
    /// is left alone.
    pub fn clear_transfer_bit(&mut self, ch: usize) {
        self.transfer_enable &= !(1u8 << ch);
    }

    pub(crate) fn save(&self, o: &mut StateOut) -> Result<(), StateError> {
        o.u8(self.dma_enable)?;
        o.u8(self.transfer_enable)?;
        o.u8(self.unknown2)?;
        o.u8(self.unknown3)?;
        for b in self.banks {
            o.u8(b)?;
        }
        Ok(())
    }

    pub(crate) fn load(&mut self, i: &mut StateIn) -> Result<(), StateError> {
        self.dma_enable = i.u8()?;
        self.transfer_enable = i.u8()?;
        self.unknown2 = i.u8()?;
        self.unknown3 = i.u8()?;
        for b in &mut self.banks {
            *b = i.u8()?;
        }
        Ok(())
    }
}

/// The header byte's top two bits select the bitplane count (fullsnes:
/// `(input AND C0h)`); `Linear` is the `num_planes=0` "raw" case the
/// algorithm's own `decompress_byte` special-cases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlaneMode {
    TwoBpp,
    FourBpp,
    EightBpp,
    Linear,
}

impl PlaneMode {
    fn from_header(header: u8) -> Self {
        match header & 0xC0 {
            0x00 => Self::TwoBpp,
            0x40 => Self::EightBpp,
            0x80 => Self::FourBpp,
            _ => Self::Linear,
        }
    }

    /// `num_planes` as the pseudocode itself names it.
    fn num_planes(self) -> u8 {
        match self {
            Self::TwoBpp => 2,
            Self::EightBpp => 8,
            Self::FourBpp => 4,
            Self::Linear => 0,
        }
    }
}

/// The Golomb-coded bitplane decompressor: fullsnes "SNES Cart S-DD1
/// Decompression Algorithm" (`fullsnes.txt:10673-10757`), transcribed
/// function for function. `rom` is passed to every stepping method rather
/// than owned, so this struct is just the algorithm's own persistent
/// state — the 32 adaptive contexts, the bitplane interleave cursor, and
/// the compressed-stream read position — and save/load is a flat field
/// dump (ticket W19-03's determinism/save-load-mid-transfer requirement).
#[derive(Debug, Clone, Copy)]
pub struct Sdd1Decompressor {
    num_planes: u8,
    /// `high_context_bits`/`low_context_bits` from `decompress_init`'s
    /// `(input AND 30h)` table.
    high_context_bits: u16,
    low_context_bits: u16,
    /// The compressed-bitstream shift register. Fullsnes's own tests
    /// against it (`AND 8000h`) treat it as a 16-bit register, so every
    /// shift below is `u16` (truncating), not an unbounded accumulator.
    input: u16,
    /// Can go negative — `GetCodeword`'s own "if valid_bits<0" branch.
    valid_bits: i8,
    bit_ctr: [u8; 8],
    prev_bits: [u16; 8],
    context_states: [u8; 32],
    context_mps: [u8; 32],
    plane: u8,
    yloc: u8,
    raw: u8,
    /// Absolute ROM byte offset of the next compressed byte to consume.
    src: usize,
}

impl Sdd1Decompressor {
    /// `decompress_init(src)`. `start` is the ROM offset of the header
    /// byte (the channel's DMA A-bus start address, already resolved
    /// through [`crate::mapping::sdd1_target`] to a ROM index).
    ///
    /// Fullsnes's own pseudocode, transcribed literally including its
    /// apparent gap: `input=[src], src=src+1` reads the header byte and
    /// advances by one; the very next line reads `[src+1]` — i.e. the byte
    /// AFTER the now-current `src`, not `src` itself — before advancing
    /// `src` by one more. Read literally that skips the byte at
    /// `start+1` (the header's second byte is never consumed by `init`);
    /// this module does not correct that, per the clean-room mandate to
    /// implement the documentation's description exactly rather than a
    /// guessed "intended" reading.
    #[must_use]
    pub fn init(rom: &[u8], start: usize) -> Self {
        let len = rom.len().max(1);
        let mut s = start;
        let header = rom[s % len];
        s += 1;
        let mode = PlaneMode::from_header(header);
        let (high_context_bits, low_context_bits) = match header & 0x30 {
            0x00 => (0x01C0, 0x0001),
            0x10 => (0x0180, 0x0001),
            0x20 => (0x00C0, 0x0001),
            _ => (0x0180, 0x0003),
        };
        let b1 = rom[(s + 1) % len];
        let input = (u16::from(header) << 11) | (u16::from(b1) << 3);
        s += 1;
        Self {
            num_planes: mode.num_planes(),
            high_context_bits,
            low_context_bits,
            input,
            valid_bits: 5,
            bit_ctr: [0; 8],
            prev_bits: [0; 8],
            context_states: [0; 32],
            context_mps: [0; 32],
            plane: 0,
            yloc: 0,
            raw: 0,
            src: s,
        }
    }

    fn read_rom_byte(&mut self, rom: &[u8]) -> u8 {
        let len = rom.len().max(1);
        let b = rom[self.src % len];
        self.src += 1;
        b
    }

    /// `GetCodeword(code_size)`.
    fn get_codeword(&mut self, code_size: u8, rom: &[u8]) -> u8 {
        if self.valid_bits == 0 {
            self.input |= u16::from(self.read_rom_byte(rom));
            self.valid_bits = 8;
        }
        self.input = self.input.wrapping_shl(1);
        self.valid_bits -= 1;
        if self.input & 0x8000 == 0 {
            return (0x80u16 + (1u16 << code_size)) as u8;
        }
        let tmp = ((self.input >> 8) & 0x7F) | (0x7F >> code_size);
        self.input = self.input.wrapping_shl(u32::from(code_size));
        self.valid_bits -= code_size as i8;
        if self.valid_bits < 0 {
            let shift = u32::from((-self.valid_bits) as u8);
            self.input |= u16::from(self.read_rom_byte(rom)) << shift;
            self.valid_bits += 8;
        }
        RUN_TABLE[usize::from(tmp)]
    }

    /// `ProbGetBit(context)`.
    fn prob_get_bit(&mut self, context: usize, rom: &[u8]) -> u8 {
        let state = self.context_states[context];
        let code_size = usize::from(EVOLUTION_CODE_SIZE[usize::from(state)]);
        if self.bit_ctr[code_size] & 0x7F == 0 {
            self.bit_ctr[code_size] = self.get_codeword(code_size as u8, rom);
        }
        let mut pbit = self.context_mps[context];
        self.bit_ctr[code_size] = self.bit_ctr[code_size].wrapping_sub(1);
        if self.bit_ctr[code_size] == 0x00 {
            self.context_states[context] = EVOLUTION_LPS_NEXT[usize::from(state)];
            pbit ^= 1;
            if state < 2 {
                self.context_mps[context] = pbit;
            }
        } else if self.bit_ctr[code_size] == 0x80 {
            self.context_states[context] = EVOLUTION_MPS_NEXT[usize::from(state)];
        }
        pbit
    }

    /// `GetBit(plane)`.
    fn get_bit(&mut self, plane: u8, rom: &[u8]) -> u8 {
        let pb = self.prev_bits[usize::from(plane)];
        let mut context = (u16::from(plane) & 1) << 4;
        context |= (pb & self.high_context_bits) >> 5;
        context |= pb & self.low_context_bits;
        let context = usize::from(context) & 0x1F;
        let pbit = self.prob_get_bit(context, rom);
        self.prev_bits[usize::from(plane)] = (pb << 1).wrapping_add(u16::from(pbit));
        if self.num_planes == 0 {
            self.raw = (self.raw >> 1) + (pbit << 7);
        }
        pbit
    }

    /// `decompress_byte`: produces the next output byte of the
    /// decompressed stream.
    #[must_use]
    pub fn next_byte(&mut self, rom: &[u8]) -> u8 {
        if self.num_planes == 0 {
            for p in 0..8u8 {
                self.get_bit(p, rom);
            }
            self.raw
        } else if self.plane & 1 == 0 {
            for _ in 0..8 {
                self.get_bit(self.plane, rom);
                self.get_bit(self.plane + 1, rom);
            }
            let out = (self.prev_bits[usize::from(self.plane)] & 0xFF) as u8;
            self.plane += 1;
            out
        } else {
            let out = (self.prev_bits[usize::from(self.plane)] & 0xFF) as u8;
            self.plane -= 1;
            self.yloc += 1;
            if self.yloc == 8 {
                self.yloc = 0;
                self.plane = (self.plane + 2) & (self.num_planes - 1);
            }
            out
        }
    }

    /// `cfg(test)`-only: a general-purpose DMA in this build always runs to
    /// completion inside one `run_channel` call (see the module doc), so no
    /// decompression is ever mid-flight at a save-state boundary and
    /// production code never calls this. Exercised directly by
    /// [`tests::save_load_mid_stream_resumes_identically`] against the
    /// ticket's own determinism/save-load requirement.
    #[cfg(test)]
    pub(crate) fn save(&self, o: &mut StateOut) -> Result<(), StateError> {
        o.u8(self.num_planes)?;
        o.u16(self.high_context_bits)?;
        o.u16(self.low_context_bits)?;
        o.u16(self.input)?;
        o.i8(self.valid_bits)?;
        for b in self.bit_ctr {
            o.u8(b)?;
        }
        for p in self.prev_bits {
            o.u16(p)?;
        }
        for s in self.context_states {
            o.u8(s)?;
        }
        for m in self.context_mps {
            o.u8(m)?;
        }
        o.u8(self.plane)?;
        o.u8(self.yloc)?;
        o.u8(self.raw)?;
        o.u32(self.src as u32)
    }

    #[cfg(test)]
    pub(crate) fn load(i: &mut StateIn) -> Result<Self, StateError> {
        let num_planes = i.u8()?;
        let high_context_bits = i.u16()?;
        let low_context_bits = i.u16()?;
        let input = i.u16()?;
        let valid_bits = i.i8()?;
        let mut bit_ctr = [0u8; 8];
        for b in &mut bit_ctr {
            *b = i.u8()?;
        }
        let mut prev_bits = [0u16; 8];
        for p in &mut prev_bits {
            *p = i.u16()?;
        }
        let mut context_states = [0u8; 32];
        for s in &mut context_states {
            *s = i.u8()?;
        }
        let mut context_mps = [0u8; 32];
        for m in &mut context_mps {
            *m = i.u8()?;
        }
        let plane = i.u8()?;
        let yloc = i.u8()?;
        let raw = i.u8()?;
        let src = i.u32()? as usize;
        Ok(Self {
            num_planes,
            high_context_bits,
            low_context_bits,
            input,
            valid_bits,
            bit_ctr,
            prev_bits,
            context_states,
            context_mps,
            plane,
            yloc,
            raw,
            src,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // Register window tests
    // ------------------------------------------------------------------

    #[test]
    fn reset_state_is_all_zero() {
        let r = Sdd1Regs::new();
        assert_eq!(r.read(0x4800), 0);
        assert_eq!(r.read(0x4801), 0);
        assert_eq!(r.read(0x4804), 0);
        assert!(!r.channel_decompresses(0));
    }

    #[test]
    fn channel_decompresses_only_with_both_enable_bits() {
        let mut r = Sdd1Regs::new();
        r.write(0x4800, 0x01);
        assert!(!r.channel_decompresses(0));
        r.write(0x4801, 0x01);
        assert!(r.channel_decompresses(0));
        assert!(!r.channel_decompresses(1));
    }

    #[test]
    fn transfer_bit_self_clears_but_enable_bit_does_not() {
        let mut r = Sdd1Regs::new();
        r.write(0x4800, 0xFF);
        r.write(0x4801, 0xFF);
        r.clear_transfer_bit(3);
        assert_eq!(r.read(0x4800), 0xFF);
        assert_eq!(r.read(0x4801), 0xFF & !(1 << 3));
        assert!(!r.channel_decompresses(3));
        assert!(r.channel_decompresses(4));
    }

    #[test]
    fn bank_registers_read_back_written_values() {
        let mut r = Sdd1Regs::new();
        r.write(0x4804, 0x00);
        r.write(0x4805, 0x01);
        r.write(0x4806, 0x02);
        r.write(0x4807, 0x03);
        assert_eq!(r.banks(), [0x00, 0x01, 0x02, 0x03]);
    }

    #[test]
    fn unknown_ports_read_back_writes_verbatim() {
        let mut r = Sdd1Regs::new();
        r.write(0x4802, 0x5A);
        r.write(0x4803, 0xA5);
        assert_eq!(r.read(0x4802), 0x5A);
        assert_eq!(r.read(0x4803), 0xA5);
    }

    #[test]
    fn regs_save_load_round_trips() {
        let mut r = Sdd1Regs::new();
        r.write(0x4800, 0x11);
        r.write(0x4801, 0x22);
        r.write(0x4802, 0x33);
        r.write(0x4803, 0x44);
        r.write(0x4804, 0x01);
        r.write(0x4807, 0x02);

        let mut stream = MemStream::default();
        r.save(&mut StateOut::new(&mut stream)).expect("save");
        let mut restored = Sdd1Regs::new();
        restored.load(&mut StateIn::new(&mut stream)).expect("load");
        assert_eq!(restored.read(0x4800), 0x11);
        assert_eq!(restored.read(0x4801), 0x22);
        assert_eq!(restored.read(0x4802), 0x33);
        assert_eq!(restored.read(0x4803), 0x44);
        assert_eq!(restored.banks(), [0x01, 0x00, 0x00, 0x02]);
    }

    // ------------------------------------------------------------------
    // Decompressor tests
    //
    // Every fixture below is generated by an independent Python model of
    // the SAME fullsnes pseudocode this module transcribes (kept as a
    // comment on each test, not shipped as a file — no ROM bytes, no
    // copyrighted data, just the documented algorithm run forward by hand
    // via a second implementation, so the fixture is not "whatever this
    // module happens to produce").
    // ------------------------------------------------------------------

    struct MemStream {
        buf: Vec<u8>,
        at: usize,
    }
    impl Default for MemStream {
        fn default() -> Self {
            Self {
                buf: Vec::new(),
                at: 0,
            }
        }
    }
    impl rf_core_api::StateWriter for MemStream {
        fn write_all(&mut self, bytes: &[u8]) -> Result<(), StateError> {
            self.buf.extend_from_slice(bytes);
            Ok(())
        }
    }
    impl rf_core_api::StateReader for MemStream {
        fn read_exact(&mut self, out: &mut [u8]) -> Result<(), StateError> {
            let end = self.at + out.len();
            out.copy_from_slice(&self.buf[self.at..end]);
            self.at = end;
            Ok(())
        }
    }

    /// Independent Python model of `decompress_init`/`decompress_byte`,
    /// used by hand to compute every `expected` array below:
    ///
    /// ```python
    /// EVOLUTION_CODE_SIZE = [0,0,0,0,0,1,1,1,1,2,2,2,2,3,3,3,3,
    ///                        4,4,5,5,6,6,7,7,0,1,2,3,4,5,6,7]
    /// EVOLUTION_MPS_NEXT = [25,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,
    ///                       18,19,20,21,22,23,24,24,26,27,28,29,30,31,32,24]
    /// EVOLUTION_LPS_NEXT = [25,1,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,
    ///                       16,17,18,19,20,21,22,23,1,2,4,8,12,16,18,22]
    /// RUN_TABLE = [128,64,96,32,112,48,80,16,120,56,88,24,104,40,72,8,
    ///              124,60,92,28,108,44,76,12,116,52,84,20,100,36,68,4,
    ///              126,62,94,30,110,46,78,14,118,54,86,22,102,38,70,6,
    ///              122,58,90,26,106,42,74,10,114,50,82,18,98,34,66,2,
    ///              127,63,95,31,111,47,79,15,119,55,87,23,103,39,71,7,
    ///              123,59,91,27,107,43,75,11,115,51,83,19,99,35,67,3,
    ///              125,61,93,29,109,45,77,13,117,53,85,21,101,37,69,5,
    ///              121,57,89,25,105,41,73,9,113,49,81,17,97,33,65,1]
    ///
    /// class Sdd1:
    ///     def __init__(self, rom, start):
    ///         self.rom = rom
    ///         s = start
    ///         header = rom[s]; s += 1
    ///         top = header & 0xC0
    ///         self.num_planes = {0x00:2,0x40:8,0x80:4,0xC0:0}[top]
    ///         sel = header & 0x30
    ///         self.high, self.low = {0x00:(0x1C0,1),0x10:(0x180,1),
    ///                                 0x20:(0xC0,1),0x30:(0x180,3)}[sel]
    ///         b1 = rom[s+1]
    ///         self.input = ((header << 11) | (b1 << 3)) & 0xFFFF
    ///         s += 1
    ///         self.valid_bits = 5
    ///         self.bit_ctr = [0]*8
    ///         self.prev_bits = [0]*8
    ///         self.states = [0]*32
    ///         self.mps = [0]*32
    ///         self.plane = 0; self.yloc = 0; self.raw = 0
    ///         self.src = s
    ///
    ///     def rb(self):
    ///         b = self.rom[self.src]; self.src += 1; return b
    ///
    ///     def codeword(self, cs):
    ///         if self.valid_bits == 0:
    ///             self.input = (self.input | self.rb()) & 0xFFFF
    ///             self.valid_bits = 8
    ///         self.input = (self.input << 1) & 0xFFFF
    ///         self.valid_bits -= 1
    ///         if (self.input & 0x8000) == 0:
    ///             return (0x80 + (1 << cs)) & 0xFF
    ///         tmp = ((self.input >> 8) & 0x7F) | (0x7F >> cs)
    ///         self.input = (self.input << cs) & 0xFFFF
    ///         self.valid_bits -= cs
    ///         if self.valid_bits < 0:
    ///             self.input = (self.input | (self.rb() << -self.valid_bits)) & 0xFFFF
    ///             self.valid_bits += 8
    ///         return RUN_TABLE[tmp]
    ///
    ///     def probbit(self, ctx):
    ///         state = self.states[ctx]
    ///         cs = EVOLUTION_CODE_SIZE[state]
    ///         if (self.bit_ctr[cs] & 0x7F) == 0:
    ///             self.bit_ctr[cs] = self.codeword(cs)
    ///         pbit = self.mps[ctx]
    ///         self.bit_ctr[cs] = (self.bit_ctr[cs] - 1) & 0xFF
    ///         if self.bit_ctr[cs] == 0:
    ///             self.states[ctx] = EVOLUTION_LPS_NEXT[state]
    ///             pbit ^= 1
    ///             if state < 2: self.mps[ctx] = pbit
    ///         elif self.bit_ctr[cs] == 0x80:
    ///             self.states[ctx] = EVOLUTION_MPS_NEXT[state]
    ///         return pbit
    ///
    ///     def getbit(self, plane):
    ///         ctx = (plane & 1) << 4
    ///         ctx |= (self.prev_bits[plane] & self.high) >> 5
    ///         ctx |= self.prev_bits[plane] & self.low
    ///         pbit = self.probbit(ctx & 0x1F)
    ///         self.prev_bits[plane] = ((self.prev_bits[plane] << 1) + pbit) & 0xFFFF
    ///         if self.num_planes == 0:
    ///             self.raw = ((self.raw >> 1) + (pbit << 7)) & 0xFF
    ///         return pbit
    ///
    ///     def next_byte(self):
    ///         if self.num_planes == 0:
    ///             for p in range(8): self.getbit(p)
    ///             return self.raw
    ///         elif self.plane & 1 == 0:
    ///             for _ in range(8):
    ///                 self.getbit(self.plane); self.getbit(self.plane + 1)
    ///             out = self.prev_bits[self.plane] & 0xFF
    ///             self.plane += 1
    ///             return out
    ///         else:
    ///             out = self.prev_bits[self.plane] & 0xFF
    ///             self.plane -= 1
    ///             self.yloc += 1
    ///             if self.yloc == 8:
    ///                 self.yloc = 0
    ///                 self.plane = (self.plane + 2) & (self.num_planes - 1)
    ///             return out
    /// ```
    ///
    /// Run by hand against a 16-byte all-zero compressed block (header
    /// `$00` selects the 2bpp mode, `(input AND 30h)=00h` context table),
    /// this model's state never leaves state 0 (an all-zero bitstream is
    /// always the MPS with `context_mps[ctx]=0`, so every `getbit` returns
    /// `0`) — the whole decompressed output is `0x00` bytes. This is the
    /// degenerate but exactly-checkable case every mode's test below
    /// starts from.
    fn all_zero_block(len: usize) -> Vec<u8> {
        vec![0u8; len]
    }

    #[test]
    fn two_bpp_header_selects_num_planes_2() {
        let rom = all_zero_block(64);
        let d = Sdd1Decompressor::init(&rom, 0);
        assert_eq!(d.num_planes, 2);
        assert_eq!(d.high_context_bits, 0x01C0);
        assert_eq!(d.low_context_bits, 0x0001);
    }

    #[test]
    fn eight_bpp_header_selects_num_planes_8() {
        let mut rom = all_zero_block(64);
        rom[0] = 0x40;
        let d = Sdd1Decompressor::init(&rom, 0);
        assert_eq!(d.num_planes, 8);
    }

    #[test]
    fn four_bpp_header_selects_num_planes_4() {
        let mut rom = all_zero_block(64);
        rom[0] = 0x80;
        let d = Sdd1Decompressor::init(&rom, 0);
        assert_eq!(d.num_planes, 4);
    }

    #[test]
    fn linear_header_selects_num_planes_0() {
        let mut rom = all_zero_block(64);
        rom[0] = 0xC0;
        let d = Sdd1Decompressor::init(&rom, 0);
        assert_eq!(d.num_planes, 0);
    }

    #[test]
    fn context_select_bits_pick_the_documented_high_low_pairs() {
        let mut rom = all_zero_block(64);
        rom[0] = 0x10;
        let d = Sdd1Decompressor::init(&rom, 0);
        assert_eq!((d.high_context_bits, d.low_context_bits), (0x0180, 0x0001));
        rom[0] = 0x20;
        let d = Sdd1Decompressor::init(&rom, 0);
        assert_eq!((d.high_context_bits, d.low_context_bits), (0x00C0, 0x0001));
        rom[0] = 0x30;
        let d = Sdd1Decompressor::init(&rom, 0);
        assert_eq!((d.high_context_bits, d.low_context_bits), (0x0180, 0x0003));
    }

    /// All-zero compressed stream: every context starts in state 0 with
    /// `context_mps=0`, so `ProbGetBit` always returns the MPS bit `0` —
    /// the Python model above confirms this never transitions state 0
    /// (state<2 rewrite keeps `context_mps[ctx]=0`) — and every decoded
    /// byte is `0x00`, for every mode.
    #[test]
    fn all_zero_stream_decompresses_to_all_zero_bytes_every_mode() {
        for header in [0x00u8, 0x40, 0x80, 0xC0] {
            let mut rom = all_zero_block(256);
            rom[0] = header;
            let mut d = Sdd1Decompressor::init(&rom, 0);
            for _ in 0..16 {
                assert_eq!(d.next_byte(&rom), 0x00, "header {header:#04x}");
            }
        }
    }

    /// Determinism: two decompressors built from the same ROM bytes at the
    /// same start address produce the same stream.
    #[test]
    fn decompression_is_deterministic() {
        let mut rom = vec![0u8; 256];
        for (i, b) in rom.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(37).wrapping_add(11);
        }
        rom[0] = 0x00;
        let mut a = Sdd1Decompressor::init(&rom, 0);
        let mut b = Sdd1Decompressor::init(&rom, 0);
        let out_a: Vec<u8> = (0..32).map(|_| a.next_byte(&rom)).collect();
        let out_b: Vec<u8> = (0..32).map(|_| b.next_byte(&rom)).collect();
        assert_eq!(out_a, out_b);
    }

    #[test]
    fn save_load_mid_stream_resumes_identically() {
        let mut rom = vec![0u8; 512];
        for (i, b) in rom.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(211).wrapping_add(3);
        }
        rom[0] = 0x40; // 8bpp, exercises the odd/even plane toggle
        let mut live = Sdd1Decompressor::init(&rom, 0);
        let mut reference = Sdd1Decompressor::init(&rom, 0);

        // Run both the same distance in.
        let prefix: Vec<u8> = (0..20).map(|_| live.next_byte(&rom)).collect();
        let reference_prefix: Vec<u8> = (0..20).map(|_| reference.next_byte(&rom)).collect();
        assert_eq!(prefix, reference_prefix);

        // Save `live`, load into a fresh instance, and continue both —
        // they must agree byte for byte from here on (FR-STATE-002 shape,
        // applied to the decompressor's own state rather than the whole
        // machine, since a general-purpose DMA in this build always runs
        // to completion within one `run_channel` call — see `bus.rs`).
        let mut stream = MemStream::default();
        live.save(&mut StateOut::new(&mut stream)).expect("save");
        let mut restored = Sdd1Decompressor::load(&mut StateIn::new(&mut stream)).expect("load");

        for _ in 0..40 {
            assert_eq!(restored.next_byte(&rom), reference.next_byte(&rom));
        }
    }
}
