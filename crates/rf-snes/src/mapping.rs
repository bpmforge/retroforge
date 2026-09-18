//! SNES cartridge address mapping — LoROM and HiROM (ticket W6-02a;
//! FR-CORE-035, `docs/design/EMULATION_CORES.md` §3.1).
//!
//! ## Why this is a pure function
//!
//! Mapping is the one part of the bus with no state at all: a bank, an
//! offset and the cartridge's shape fully determine where an access
//! lands. Keeping it separate from [`crate::bus`] means the mapping can
//! be tested exhaustively — every one of the 16,777,216 addresses, in
//! both modes — without constructing a machine, and it means a mapping
//! bug reports itself as a mapping bug rather than as a mislocated byte
//! three layers up.
//!
//! ## The two maps
//!
//! **LoROM** puts a 32 KiB ROM slice in the upper half of every bank:
//! bank `$00:8000` and bank `$80:8000` are the same byte, and the ROM
//! offset is `(bank & $7F) * $8000 + (offset - $8000)`.
//!
//! **HiROM** maps ROM linearly: `$C0:0000` onward is the ROM from byte
//! zero, and the upper half of each low bank shows the corresponding
//! slice — so `$40:8000` and `$C0:8000` are the same byte.
//!
//! That pair of aliases is exactly what `fixtures/snes/mirror-map` checks
//! from inside the machine, and the reason its FORMAT.md says "same
//! question, asked at the address each mapping actually uses".
//!
//! ## Mirroring undersized ROMs
//!
//! A 32 KiB ROM occupies one LoROM bank, but the address space offers
//! 128. Hardware repeats the ROM rather than reading nothing, so the
//! computed offset is taken modulo the ROM length. This is not a
//! convenience: the mirror-map fixture is 32 KiB and its own reset vector
//! is fetched through bank `$00`, which only resolves because of it.

use rf_cart::{DspWindow, Sa1Board, SnesMapMode};

/// Where an access lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Cartridge ROM, at an offset already reduced modulo the ROM size.
    Rom(usize),
    /// Work RAM, at an offset into the full 128 KiB.
    Wram(usize),
    /// Cartridge save RAM, at an offset already reduced modulo its size.
    Sram(usize),
    /// A hardware register; carries the 16-bit offset, since every
    /// register block is identified by its offset alone.
    Register(u16),
    /// The DSP-1's DR (data/command) register (ticket W14-19). No offset
    /// is carried: the whole `dr` sub-range of a [`rf_cart::DspWindow`]
    /// is one mirrored register, and which byte a read/write means is
    /// decided by the chip's own internal protocol state, not by which
    /// address in the window was used (snesdev/fullsnes document no
    /// per-address behaviour inside the window).
    Dsp1Dr,
    /// The DSP-1's SR (status) register — see [`Target::Dsp1Dr`].
    Dsp1Sr,
    /// SA-1 on-chip I-RAM (2 KiB), at an offset already reduced modulo its
    /// size (ticket W17-01). Banks `$00-$3F`/`$80-$BF`:`$3000-$37FF`,
    /// fullsnes "SNES Cart SA-1" memory-map overview.
    Sa1IRam(usize),
    /// SA-1 BW-RAM, at an offset already reduced modulo its size (ticket
    /// W17-01) — either the full `$40-$4F` window or the mappable
    /// `$6000-$7FFF` 8 KiB block, both resolved by [`sa1_target`].
    Sa1BwRam(usize),
    /// The SA-1 SNES-side register window `$2200-$23FF` (ticket W17-01,
    /// fullsnes "SNES Cart SA-1 I/O Map"). Carries the raw offset; the
    /// register file (`crate::sa1::Sa1Regs`) sorts out which register it
    /// is.
    Sa1Register(u16),
    /// Nothing is mapped here. Reads see open bus; writes are dropped.
    Open,
}

/// Total work RAM: 128 KiB, at banks `$7E`-`$7F`.
pub const WRAM_LEN: usize = 128 * 1024;

/// Resolve `(bank, offset)` against a cartridge's DSP-1 bus window, if it
/// has one and the address falls inside it. Checked by [`crate::bus`]
/// BEFORE [`map`], because the window's `dr`/`sr` sub-ranges cover the
/// same bank/offset space the plain LoROM/HiROM ROM and SRAM windows
/// would otherwise claim there (D-010, ticket W14-19) — a cart with no
/// DSP-1 never calls this, so every other cart's mapping is unchanged.
#[must_use]
pub fn dsp1_target(window: &DspWindow, bank: u8, offset: u16) -> Option<Target> {
    if !window.banks[0].contains(&bank) && !window.banks[1].contains(&bank) {
        return None;
    }
    if window.dr.contains(&offset) {
        Some(Target::Dsp1Dr)
    } else if window.sr.contains(&offset) {
        Some(Target::Dsp1Sr)
    } else {
        None
    }
}

/// The live state [`sa1_target`] needs to resolve an address: the four ROM
/// bank-select registers and the SNES-side BW-RAM window select, plus the
/// board's fixed sizes. Everything else about SA-1 mapping is a pure
/// function of these — see [`sa1_target`]'s doc for the cited rules.
#[derive(Debug, Clone, Copy)]
pub struct Sa1RomBanks {
    /// `$2220` CXB — governs HiROM banks `$C0-$CF` / LoROM banks `$00-$1F`.
    pub cxb: u8,
    /// `$2221` DXB — governs HiROM banks `$D0-$DF` / LoROM banks `$20-$3F`.
    pub dxb: u8,
    /// `$2222` EXB — governs HiROM banks `$E0-$EF` / LoROM banks `$80-$9F`.
    pub exb: u8,
    /// `$2223` FXB — governs HiROM banks `$F0-$FF` / LoROM banks `$A0-$BF`.
    pub fxb: u8,
    /// `$2224` BMAPS — 8 KiB BW-RAM block (0..31) mapped to the SNES-side
    /// `$6000-$7FFF` window.
    pub bmaps: u8,
    pub board: Sa1Board,
}

/// Resolve `(bank, offset)` against an SA-1 cartridge's SNES-side memory
/// map. Checked by [`crate::bus::SnesBus::target`] BEFORE [`map`] whenever
/// the cart carries [`rf_cart::Coprocessor::Sa1`] — a cart with none never
/// calls this, so every other cartridge's mapping is unchanged.
///
/// Cited to fullsnes "SNES Cart SA-1", "Memory Map (SNES Side)" and
/// "SNES Cart SA-1 Memory Control":
/// - `$2200-$23FF` — the whole SA-1 register window ([`Target::Sa1Register`]).
/// - `$3000-$37FF` — I-RAM, 2 KiB, direct (no modulo needed: the window is
///   exactly the I-RAM's size).
/// - `$6000-$7FFF` — one mappable 8 KiB BW-RAM block, selected by `$2224`
///   BMAPS bits 0-4 (0..31 blocks x 8 KiB = 256 KiB, the documented
///   maximum for this window).
/// - `$8000-$FFFF` — four mappable 1 MiB LoROM-style blocks via
///   `$2220-$2223`; see the note on `bank7_lorom_target` for the bit-7
///   direct-vs-banked rule, which is the one place this project could not
///   find more than the single paragraph fullsnes gives it (see the
///   report's "fullsnes ambiguity" note).
/// - banks `$40-$4F` — the entire BW-RAM, mirrored every 4 banks.
/// - banks `$C0-$FF` — the same four registers, HiROM-style (linear,
///   whole 64 KiB banks, bit 7 irrelevant).
#[must_use]
pub fn sa1_target(regs: &Sa1RomBanks, bank: u8, offset: u16) -> Option<Target> {
    let system_area = bank < 0x40 || (0x80..0xC0).contains(&bank);
    if system_area {
        match offset {
            0x2200..=0x23FF => return Some(Target::Sa1Register(offset)),
            0x3000..=0x37FF => {
                let idx = usize::from(offset - 0x3000);
                if idx < regs.board.iram_len {
                    return Some(Target::Sa1IRam(idx));
                }
            }
            0x6000..=0x7FFF if regs.board.bwram_len > 0 => {
                let block = usize::from(regs.bmaps & 0x1F);
                let idx = block * 0x2000 + usize::from(offset - 0x6000);
                return Some(Target::Sa1BwRam(idx % regs.board.bwram_len));
            }
            0x8000..=0xFFFF if regs.board.rom_len > 0 => {
                return Some(Target::Rom(
                    lorom_quarter_target(regs, bank, offset) % regs.board.rom_len,
                ));
            }
            _ => {}
        }
        return None;
    }
    if (0x40..=0x4F).contains(&bank) && regs.board.bwram_len > 0 {
        // "Entire 256Kbyte BW-RAM (mirrors in 44h-4Fh)": four banks of
        // 64 KiB, repeating every 4 banks across the 16-bank window.
        let idx = (usize::from(bank - 0x40) & 0x03) << 16 | usize::from(offset);
        return Some(Target::Sa1BwRam(idx % regs.board.bwram_len));
    }
    if (0xC0..=0xFF).contains(&bank) && regs.board.rom_len > 0 {
        let (reg, hi_base) = match bank {
            0xC0..=0xCF => (regs.cxb, 0xC0u8),
            0xD0..=0xDF => (regs.dxb, 0xD0),
            0xE0..=0xEF => (regs.exb, 0xE0),
            _ => (regs.fxb, 0xF0),
        };
        let bank_select = usize::from(reg & 0x07);
        let local = usize::from(bank - hi_base); // 0..15, one 64 KiB bank
        let idx = bank_select * 0x10_0000 + local * 0x10000 + usize::from(offset);
        return Some(Target::Rom(idx % regs.board.rom_len));
    }
    None
}

/// The LoROM-style `$8000-$FFFF` quarter lookup for `sa1_target`, before
/// the final `% rom_len` mirror.
///
/// fullsnes "SNES Cart SA-1 Memory Control" ($2220-$2223): bits 0-2 of the
/// governing register select a 1 MiB ROM bank; bit 7 is "Map 1Mbyte
/// ROM-Bank (0=To HiRom, 1=To LoRom and HiRom)" — when set, that 1 MiB
/// bank additionally appears at this LoROM quarter; when clear, "the
/// first 2 MByte of ROM are mapped to $00-$3F, and next 2 MByte to
/// $80-$BF" (a direct, unbanked mapping) instead. That sentence is the
/// full extent of what fullsnes states on the direct/lo-bank rule — it
/// does not spell out whether "direct" is scoped per-register (this
/// cart's implementation) or globally across all four; see the report's
/// ambiguity note. This function applies it per-register/per-quarter,
/// since that is what "each register's bit 7" (the acceptance criterion's
/// own wording) most naturally means and it degrades to the literal
/// 2 MiB+2 MiB split when every register agrees.
fn lorom_quarter_target(regs: &Sa1RomBanks, bank: u8, offset: u16) -> usize {
    let off = usize::from(offset - 0x8000);
    let (reg, quarter_base, direct_bank_base, direct_region_base) = match bank {
        0x00..=0x1F => (regs.cxb, 0x00u8, 0x00u8, 0usize),
        0x20..=0x3F => (regs.dxb, 0x20, 0x00, 0),
        0x80..=0x9F => (regs.exb, 0x80, 0x80, 0x20_0000),
        _ => (regs.fxb, 0xA0, 0x80, 0x20_0000),
    };
    if reg & 0x80 != 0 {
        // Banked: this whole 1 MiB quarter shows the selected bank.
        let bank_select = usize::from(reg & 0x07);
        let local = usize::from(bank - quarter_base); // 0..31
        bank_select * 0x10_0000 + local * 0x8000 + off
    } else {
        // Direct: plain LoROM addressing across the combined $00-$3F (or
        // $80-$BF) range, ignoring the bank-select bits.
        let local = usize::from(bank - direct_bank_base); // 0..63
        local * 0x8000 + off + direct_region_base
    }
}

/// Resolve a 24-bit address.
///
/// `rom_len` and `sram_len` shape the mirroring; a zero `sram_len` means
/// the cartridge has no save RAM, and those windows read as open bus
/// rather than aliasing onto something else.
#[must_use]
pub fn map(mode: SnesMapMode, bank: u8, offset: u16, rom_len: usize, sram_len: usize) -> Target {
    // Banks $7E-$7F are the full 128 KiB of work RAM, in both maps. This
    // is checked FIRST because those bank numbers fall inside the
    // $40-$7D "cartridge" range that the map-specific arms below would
    // otherwise claim.
    if bank == 0x7E || bank == 0x7F {
        return Target::Wram(((usize::from(bank) - 0x7E) << 16) | usize::from(offset));
    }

    let system_area = bank < 0x40 || (0x80..0xC0).contains(&bank);
    if system_area && offset < 0x8000 {
        return match offset {
            // The low 8 KiB of WRAM, mirrored into every system bank.
            // This is the alias the fixture's check 2 exercises.
            0x0000..=0x1FFF => Target::Wram(usize::from(offset)),
            // PPU, APU, joypad, CPU and DMA registers all live here. The
            // register file sorts out which is which; mapping only has to
            // know it is not memory.
            0x2000..=0x5FFF => Target::Register(offset),
            // $6000-$7FFF: HiROM puts save RAM here; LoROM leaves it open.
            _ => match mode {
                SnesMapMode::HiRom if sram_len > 0 => {
                    let index = ((usize::from(bank) & 0x1F) << 13) | usize::from(offset - 0x6000);
                    Target::Sram(index % sram_len)
                }
                _ => Target::Open,
            },
        };
    }

    match mode {
        SnesMapMode::LoRom => {
            // LoROM save RAM lives in banks $70-$7D (and $F0-$FF) below
            // $8000. Checked before the ROM arm, which would otherwise
            // claim the whole bank.
            if (0x70..0x7E).contains(&bank) && offset < 0x8000 && sram_len > 0 {
                let index = ((usize::from(bank) - 0x70) << 15) | usize::from(offset);
                return Target::Sram(index % sram_len);
            }
            if rom_len == 0 {
                return Target::Open;
            }
            // 32 KiB per bank, taken from the upper half. The `& 0x7FFF`
            // makes the lower half of banks $40-$7D mirror the upper,
            // which is what hardware does and what keeps this a single
            // expression rather than two cases.
            let index = ((usize::from(bank) & 0x7F) << 15) | usize::from(offset & 0x7FFF);
            Target::Rom(index % rom_len)
        }
        SnesMapMode::HiRom => {
            if rom_len == 0 {
                return Target::Open;
            }
            // Linear: the bank's low six bits select a full 64 KiB slice,
            // so $C0:0000 is ROM byte 0 and $40:8000 aliases $C0:8000.
            let index = ((usize::from(bank) & 0x3F) << 16) | usize::from(offset);
            Target::Rom(index % rom_len)
        }
        // [`sa1_target`] is authoritative for every address an SA-1
        // cartridge's board actually wires up; this arm is reached only
        // for a degenerate board (zero-length ROM or BW-RAM) at an
        // address that would otherwise have come from one of those, and
        // fullsnes documents no meaningful behaviour there — open bus,
        // never a plain-LoROM/HiROM guess through data that isn't really
        // shaped that way.
        SnesMapMode::Sa1 => Target::Open,
    }
}
