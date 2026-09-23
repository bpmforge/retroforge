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
    /// The SA-1-side bitmap projection of BW-RAM, banks `$60-$6F`
    /// (ticket W17-03, fullsnes "SNES Cart SA-1 Memory Control" `$223F`
    /// BBF: "BW-RAM bitmap logical space format... from perspective of
    /// the SA-1 CPU"). Carries a flat 0-based pixel index across the
    /// whole 24-bit `$600000-$6FFFFF` space — `(bank - 0x60) * 0x10000 +
    /// offset` — so [`crate::sa1::Sa1Bus`] only has to divide by the
    /// current format's pixels-per-byte to find the underlying BW-RAM
    /// byte and sub-field; it never needs a second address translation.
    /// SA-1-side only: fullsnes's own heading names it "from perspective
    /// of the SA-1 CPU", and [`sa1_target`] (the SNES side) has no arm
    /// for these banks at all, so an SNES-side access there is open bus,
    /// per that reading of the ambiguity (see this module's report
    /// note).
    Sa1Bitmap(usize),
    /// GSU (Super FX) on-cartridge RAM, at an offset already reduced
    /// modulo its size (ticket W18-01, fullsnes "SNES Cart GSU-n Memory
    /// Map"). Covers both the `$70-$71` full-bank window and its
    /// `$6000-$7FFF` mirror — [`gsu_target`] resolves both onto the same
    /// index space.
    GsuRam(usize),
    /// The GSU register window (`$3000-$3FFF` and its documented mirrors,
    /// ticket W18-01, fullsnes "SNES Cart GSU-n I/O Map"). Carries the raw
    /// offset; [`crate::gsu::Gsu`] sorts out which register/mirror it is.
    GsuRegister(u16),
    /// CX4 on-chip data RAM (ticket W19-02, fullsnes "CX4 I/O Map":
    /// `"6000h..6BFFh R/W CX4RAM (3Kbytes)"`), banks `$00-$3F`/`$80-$BF`.
    /// Carries the offset already reduced into `0..0xC00` (the window is
    /// exactly the RAM's size, so no modulo is needed at the mapping
    /// layer — see [`cx4_target`]).
    Cx4Ram(usize),
    /// The CX4 register/DMA/status window (`$7F40-$7F52`, `$7F5E`,
    /// `$7F6A-$7F6B`, `$7F6E-$7F6F`, `$7F80-$7FAF` — ticket W19-02,
    /// fullsnes "CX4 I/O Map"). Carries the raw offset; [`crate::cx4::Cx4`]
    /// sorts out which register it is. Every OTHER offset in `$6C00-$7FFF`
    /// is fullsnes-"Unknown/unused" and is left unclaimed here, falling
    /// through to the generic [`map`] (open bus on a LoROM cart, which
    /// every Cx4 cartridge is).
    Cx4Register(u16),

    /// One of the OBC1's three true registers — `$7FF5` (base select),
    /// `$7FF6` (index) or `$7FF7` (unknown) — ticket W19-01, fullsnes
    /// "SNES Cart OBC1 I/O Ports". Carries the raw offset;
    /// [`crate::obc1::Obc1Regs`] sorts out which one it is. Unlike those
    /// three, `$7FF0-$7FF4` are NOT registers of their own — fullsnes
    /// calls them "totally useless" ports that just redirect to a computed
    /// SRAM address — so [`obc1_target`] resolves those straight to
    /// [`Target::Sram`]/[`Target::Obc1Bits`] instead of a fourth variant
    /// here.
    Obc1Register(u16),
    /// The OBC1's `$7FF4` "OAM Bits" port, carrying the SRAM byte offset
    /// it redirects to (ticket W19-01). Split from the plain
    /// [`Target::Sram`] redirection the other three OAM ports
    /// (`$7FF0-$7FF3`) use because fullsnes documents asymmetric R/W
    /// semantics here — write does a 2-bit read-modify-write, read returns
    /// the whole undecoded byte — that a plain SRAM cell cannot express;
    /// [`crate::bus::SnesBus`] resolves the bit position from the OBC1's
    /// current index at access time.
    Obc1Bits(usize),
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
    /// `$2225` BMAP — 8 KiB BW-RAM block (0..127) mapped to the SA-1-side
    /// `$6000-$7FFF` window (ticket W17-02); bit 7 selects the bitmap
    /// (2/4bpp pixel-buffer) projection, recorded by [`Self::bitmap_mode`]
    /// but not yet applied — see [`sa1_side_target`]'s doc.
    pub bmap: u8,
    pub board: Sa1Board,
}

impl Sa1RomBanks {
    /// `$2225` BMAP bit 7 — "Select source (0=Normal/Bank 40h..43h,
    /// 1=Bitmap/Bank 60h..6Fh)". This slice records the bit but always
    /// resolves the `$6000-$7FFF` window as plain linear BW-RAM (W17-03
    /// does the bitmap projection).
    #[must_use]
    pub fn bitmap_mode(&self) -> bool {
        self.bmap & 0x80 != 0
    }
}

/// The `$40-$4F` BW-RAM mirror, identical on both sides of the chip
/// (fullsnes "SNES Cart SA-1" memory-map overview and "Memory Map (SA-1
/// Side)": "Same as on SNES Side"). Factored out so [`sa1_target`] and
/// [`sa1_side_target`] cannot drift apart on it.
fn bwram_full_bank_target(regs: &Sa1RomBanks, bank: u8, offset: u16) -> Option<Target> {
    if (0x40..=0x4F).contains(&bank) && regs.board.bwram_len > 0 {
        // "Entire 256Kbyte BW-RAM (mirrors in 44h-4Fh)": four banks of
        // 64 KiB, repeating every 4 banks across the 16-bank window.
        let idx = (usize::from(bank - 0x40) & 0x03) << 16 | usize::from(offset);
        Some(Target::Sa1BwRam(idx % regs.board.bwram_len))
    } else {
        None
    }
}

/// The `$C0-$FF` HiROM banks, identical on both sides of the chip (same
/// citation as [`bwram_full_bank_target`]).
fn hirom_bank_target(regs: &Sa1RomBanks, bank: u8, offset: u16) -> Option<Target> {
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
        Some(Target::Rom(idx % regs.board.rom_len))
    } else {
        None
    }
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
    if let Some(t) = bwram_full_bank_target(regs, bank, offset) {
        return Some(t);
    }
    hirom_bank_target(regs, bank, offset)
}

/// Resolve `(bank, offset)` against an SA-1 cartridge's SA-1-side memory
/// map — the second CPU's own view (ticket W17-02, D-013). Checked by
/// [`crate::sa1::Sa1Bus::target`], the SA-1 CPU's only bus — unlike
/// [`sa1_target`] (the SNES side) this never falls back to [`map`]:
/// fullsnes "Memory Map (SA-1 Side)" is explicit that the SA-1 has no
/// access to SNES-internal WRAM or I/O ports at all, so an address this
/// function does not name is open bus, never a WRAM/register alias.
///
/// Cited to fullsnes "SNES Cart SA-1", "Memory Map (SA-1 Side)" and
/// "SNES Cart SA-1 Memory Control":
/// - `$0000-$07FF` **and** `$3000-$37FF` both alias the same 2 KiB I-RAM —
///   the SA-1 side's one addition over the SNES-side map ("I-RAM (at both
///   0000h-07FFh and 3000h-37FFh)"). `offset & 0x07FF` collapses both
///   windows to the same index because `$3000` is a multiple of `$0800`.
/// - `$2200-$23FF` — the same register window as the SNES side; it is one
///   shared [`crate::sa1::Sa1Regs`] store, so a value either side writes is
///   visible to a read from the other (only which offsets each side may
///   legally *write* differ, and that is `Sa1Regs::write`'s job, not
///   mapping's).
/// - `$6000-$7FFF` — one mappable 8 KiB BW-RAM block, selected by `$2225`
///   BMAP bits 0-6 (0..127 blocks — twice the range of the SNES side's
///   `$2224` BMAPS, which only uses bits 0-4). See [`Sa1RomBanks::bitmap_mode`]
///   for what bit 7 does and does not do yet.
/// - `$8000-$FFFF` and banks `$C0-$FF` — identical to the SNES side (the
///   same `$2220-$2223` registers; "The registers do affect both SNES and
///   SA-1 mapping").
/// - banks `$40-$4F` — identical to the SNES side.
/// - Everything else in system-area offset space (`$0800-$21FF`,
///   `$2400-$2FFF`, `$3800-$5FFF`) and any address with no board memory
///   behind it: open bus.
#[must_use]
pub fn sa1_side_target(regs: &Sa1RomBanks, bank: u8, offset: u16) -> Target {
    // Bitmap projection, banks $60-$6F — checked before the system-area
    // split below because it is neither: `$60-$6F` falls between the
    // system-area ranges (`<$40` / `$80-$BF`) and the plain BW-RAM/HiROM
    // arms (`$40-$4F` / `$C0-$FF`) that `bwram_full_bank_target`/
    // `hirom_bank_target` claim, so it would otherwise fall through to
    // `Open`. Cited to fullsnes "SNES Cart SA-1 Memory Control" `$223F`.
    if (0x60..=0x6F).contains(&bank) && regs.board.bwram_len > 0 {
        let k = (usize::from(bank - 0x60) << 16) | usize::from(offset);
        return Target::Sa1Bitmap(k);
    }
    let system_area = bank < 0x40 || (0x80..0xC0).contains(&bank);
    if system_area {
        if (0x0000..=0x07FF).contains(&offset) || (0x3000..=0x37FF).contains(&offset) {
            let idx = usize::from(offset & 0x07FF);
            return if idx < regs.board.iram_len {
                Target::Sa1IRam(idx)
            } else {
                Target::Open
            };
        }
        if (0x2200..=0x23FF).contains(&offset) {
            return Target::Sa1Register(offset);
        }
        if (0x6000..=0x7FFF).contains(&offset) && regs.board.bwram_len > 0 {
            let block = usize::from(regs.bmap & 0x7F);
            let idx = block * 0x2000 + usize::from(offset - 0x6000);
            return Target::Sa1BwRam(idx % regs.board.bwram_len);
        }
        if (0x8000..=0xFFFF).contains(&offset) && regs.board.rom_len > 0 {
            return Target::Rom(lorom_quarter_target(regs, bank, offset) % regs.board.rom_len);
        }
        return Target::Open;
    }
    bwram_full_bank_target(regs, bank, offset)
        .or_else(|| hirom_bank_target(regs, bank, offset))
        .unwrap_or(Target::Open)
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

/// The live sizes and bus-ownership bits [`gsu_target`] needs to resolve
/// an address for a Super FX cartridge. Unlike SA-1's [`Sa1RomBanks`],
/// there are no SNES-side ROM/RAM bank-select registers to track — fullsnes
/// "SNES Cart GSU-n Memory Map" gives the GSU a fixed SNES-side shape,
/// only the RON/RAM-bus-ownership bits (`$303Ah` SCMR) change at runtime.
#[derive(Debug, Clone, Copy)]
pub struct GsuBoard {
    /// Cartridge ROM length in bytes.
    pub rom_len: usize,
    /// GSU RAM length in bytes (from the header's expansion-RAM field;
    /// `0` for a cart whose header states none, including the extended-
    /// header-absent case — see `rf_cart::snes::superfx_expansion_ram_kib`'s
    /// doc).
    pub ram_len: usize,
    /// `$303Ah` SCMR bit 4 (RON): `false` = SNES owns the ROM bus,
    /// `true` = the GSU does. Fullsnes "SNES Cart GSU-n Bitmap I/O Ports":
    /// "4 RON Game Pak ROM bus access (0=SNES, 1=GSU)".
    pub ron: bool,
    /// `$303Ah` SCMR bit 3 (RAN): same rule, for the RAM bus.
    pub ran: bool,
}

/// Resolve `(bank, offset)` against a Super FX cartridge's SNES-side
/// memory map. Checked by [`crate::bus::SnesBus::target`] BEFORE [`map`]
/// whenever the cartridge carries [`rf_cart::Coprocessor::SuperFx`] — a
/// cart with none never calls this, so every other cartridge's mapping is
/// unchanged.
///
/// GSU carts keep a plain LoROM (or, per fullsnes, an equally valid HiROM-
/// style) header — fullsnes "SNES Cart GSU-n Cartridge Header": "the
/// header & exception vectors are located at ROM Offset 7Fxxh... the
/// cartridge header declares the cartridge as LoROM" — so this is layered
/// the same way [`sa1_target`]/[`dsp1_target`] are, rather than a new
/// [`rf_cart::SnesMapMode`] variant.
///
/// Cited to fullsnes "SNES Cart GSU-n Memory Map", the GSU2 table (the
/// documented superset this build uses uniformly for GSU1 and GSU2 — GSU1
/// is the same shape at smaller sizes, and fullsnes gives no SNES-side
/// mapping difference between them beyond size):
/// - `$3000-$3FFF` (banks `$00-$3F`/`$80-$BF`) — the register window and
///   its mirrors ("SNES Cart GSU-n I/O Map"'s "Full I/O Map with Mirrors
///   for GSU2"); resolved whole here, [`crate::gsu::Gsu`] decodes the
///   mirror.
/// - `$6000-$7FFF` (same banks) — "Mirror of 70:0000-1FFF (ie. FIRST 8K of
///   Game Pak RAM)".
/// - `$8000-$FFFF` in banks `$00-$3F` ONLY — "Game Pak ROM in LoRom
///   mapping (2Mbyte max)". Banks `$80-$BF` do NOT get this: fullsnes
///   lists `$80-BF:8000-FFFFh` as a separate "Additional 'CPU' ROM LoRom
///   (2Mbyte max, usually none)" chip select no board in this project's
///   library populates — `None` here for that combination falls through
///   to [`map`], whose ordinary LoROM mirror (`bank & 0x7F`) already
///   answers it the same way every other unbanked LoROM cartridge mirrors
///   its FastROM half onto its SlowROM half.
/// - banks `$40-$5F` — "Game Pak ROM in HiRom mapping (mirror of above)".
/// - banks `$70-$71` — "Game Pak RAM (128Kbyte max, usually 32K or 64K)".
/// - Everything else this project's boards don't populate (the additional
///   "Backup" RAM at `$78-$79`, the additional "CPU" ROM at banks
///   `$C0-$FF`): `None` here falls through to [`map`], which also has
///   nothing for those addresses on a GSU cart (`rom_len`/`ram_len` gate
///   every arm above, so an unbacked board still resolves cleanly to
///   `Open`).
///
/// The SCMR RON/RAN ownership rule ("RON/RAN can be temporarily cleared
/// during GSU operation, this causes the GSU to enter WAIT status") is
/// NOT applied here — mapping only says WHERE an address lands; WHETHER
/// the SNES side actually sees ROM/RAM there or open bus while the GSU
/// owns the bus is [`crate::bus::SnesBus::read`]'s job (fullsnes documents
/// the GSU's own WAIT behaviour but not literally what byte the SNES CPU
/// reads meanwhile; this project's own convention — open bus, the same
/// answer every other "nothing here right now" case in this bus already
/// gives — is applied there rather than invented a second time in this
/// module).
#[must_use]
pub fn gsu_target(board: &GsuBoard, bank: u8, offset: u16) -> Option<Target> {
    // The register window and the `$6000-$7FFF` RAM mirror are the SAME
    // in banks `$00-$3F` and `$80-$BF` (fullsnes literally writes both
    // ranges on one row: "00-3F/80-BF:3000-34FFh"/"...:6000-7FFFh"). The
    // PRIMARY ROM window is not: fullsnes gives it only to `$00-$3F`
    // ("00-3F:8000-FFFFh... 2Mbyte max") and lists `$80-BF:8000-FFFFh`
    // separately as "Additional 'CPU' ROM LoRom (2Mbyte max, usually
    // none)" — a second, physically distinct chip select no board in this
    // project's library populates. Returning `None` for it here (rather
    // than claiming it as more GSU ROM) lets it fall through to the
    // generic LoROM `map`, which mirrors `$80-$BF` onto `$00-$3F` the
    // ordinary way every plain LoROM cartridge already mirrors FastROM
    // banks onto SlowROM ones — the correct answer for an unpopulated
    // second chip select on a cartridge whose header still declares plain
    // LoROM.
    let mirrored_system = bank < 0x40 || (0x80..0xC0).contains(&bank);
    if mirrored_system {
        if (0x3000..=0x3FFF).contains(&offset) {
            return Some(Target::GsuRegister(offset));
        }
        if (0x6000..=0x7FFF).contains(&offset) && board.ram_len > 0 {
            let idx = usize::from(offset - 0x6000);
            return Some(Target::GsuRam(idx % board.ram_len));
        }
    }
    if bank < 0x40 && offset >= 0x8000 && board.rom_len > 0 {
        let index = (usize::from(bank) << 15) | usize::from(offset & 0x7FFF);
        return Some(Target::Rom(index % board.rom_len));
    }
    if (0x40..=0x5F).contains(&bank) && board.rom_len > 0 {
        let index = (usize::from(bank - 0x40) << 16) | usize::from(offset);
        return Some(Target::Rom(index % board.rom_len));
    }
    if (0x70..=0x71).contains(&bank) && board.ram_len > 0 {
        let index = (usize::from(bank - 0x70) << 16) | usize::from(offset);
        return Some(Target::GsuRam(index % board.ram_len));
    }
    None
}

/// Resolve `(bank, offset)` against the CX4's fixed SNES-side window, if
/// it falls inside it. Checked BEFORE the generic map by
/// [`crate::bus::SnesBus::target`] whenever the cartridge carries
/// [`rf_cart::Coprocessor::Cx4`] — a cart with none never calls this, so
/// every other cartridge's mapping is unchanged.
///
/// Unlike [`sa1_target`]/[`gsu_target`], there is no bank-select register
/// or board-size input: fullsnes's own "CX4 Memory Map" gives ONE fixed
/// window — `"I/O 00-3F,80-BF:6000-7FFF"` — with no size variant across
/// the two known titles, so this is a pure function of the address alone
/// (ticket W19-02).
///
/// - `$6000-$6BFF` — CX4RAM, 3 KiB (fullsnes "CX4 I/O Map").
/// - `$7F40-$7F52`, `$7F5E`, `$7F6A-$7F6B`, `$7F6E-$7F6F`, `$7F80-$7FAF` —
///   the documented DMA/register/status/vector-shadow ports, same
///   chapter. [`crate::cx4::Cx4::read`]/`write` sort out which register
///   an offset in this set means.
/// - Everything else in `$6C00-$7FFF` (`$6C00-$7F3F`, `$7F53-$7F5D`,
///   `$7F5F-$7F69`, `$7F6C-$7F6D`, `$7F70-$7F7F`, `$7FB0-$7FFF`) is
///   fullsnes's own "Unknown/unused" — left unclaimed here, so it falls
///   through to [`map`], which resolves it as open bus for a LoROM cart
///   (every documented Cx4 board is Slow LoROM, fullsnes "CX4 Cartridge
///   Header": `"[FFD5]=20h ;Slow LoROM"`).
/// - `$8000-$FFFF` (ROM) is intentionally NOT claimed here: fullsnes lists
///   it as part of the same memory map, but it is the cartridge's
///   ordinary LoROM window, not a CX4-specific register — [`map`] already
///   resolves it.
#[must_use]
pub fn cx4_target(bank: u8, offset: u16) -> Option<Target> {
    let mirrored_system = bank < 0x40 || (0x80..0xC0).contains(&bank);
    if !mirrored_system {
        return None;
    }
    match offset {
        0x6000..=0x6BFF => Some(Target::Cx4Ram(usize::from(offset - 0x6000))),
        0x7F40..=0x7F52 | 0x7F5E | 0x7F6A..=0x7F6B | 0x7F6E..=0x7F6F | 0x7F80..=0x7FAF => {
            Some(Target::Cx4Register(offset))
        }
        _ => None,
    }
}

/// The live state [`obc1_target`] needs to resolve an address: the two
/// register values that decide where in SRAM the `$7FF0-$7FF4` ports
/// redirect, plus the cartridge's SRAM size (ticket W19-01).
#[derive(Debug, Clone, Copy)]
pub struct Obc1Board {
    /// SRAM byte offset of the selected `$7C00`/`$7800` base
    /// (`$7C00-$6000` or `$7800-$6000`) — see [`crate::obc1::Obc1Regs::base_offset`].
    pub base_offset: usize,
    /// `$7FF6` Index (OBJ Number), already masked to the documented 0..127
    /// range — see [`crate::obc1::Obc1Regs::index_masked`].
    pub index: u8,
    /// Cartridge SRAM length in bytes (8 KiB on the one real board).
    pub sram_len: usize,
}

/// Resolve `(bank, offset)` against an OBC1 cartridge's SNES-side memory
/// map. Checked by [`crate::bus::SnesBus::target`] BEFORE [`map`], same
/// reasoning as [`sa1_target`]/[`gsu_target`]/[`dsp1_target`]: an OBC1
/// cart's register ports sit inside bank/offset space `map` would
/// otherwise resolve as plain SRAM (or, for these system-area banks under
/// plain LoROM, open bus — see `map`'s own `$6000-$7FFF` comment). `board`
/// is only ever built for a cartridge whose header reports
/// [`rf_cart::Coprocessor::Obc1`], so a non-OBC1 cart never calls this and
/// every other cartridge's mapping is unchanged.
///
/// Cited to fullsnes "SNES Cart OBC1 (OBJ Controller)", "OBC1 I/O Ports"
/// and the paragraph beneath it:
/// - The whole `$6000-$7FFF` window ("Other bytes at 6000h..7FFFh contain
///   8Kbyte battery-backed SRAM") is claimed here, in the system-area
///   banks (`$00-$3F`/`$80-$BF`) that every other bus window in this
///   module uses for a cartridge-resident chip's register space — the
///   chapter gives no explicit bank list of its own (unlike SA-1/GSU),
///   so this follows their convention rather than inventing a new one.
///   The generic LoROM `$70-$7D` SRAM window `map` already provides is
///   left alone: no fullsnes sentence names it for this chip, and Metal
///   Combat's own code addresses the chip through this window, not that
///   one.
/// - `$7FF0h/7FF1h/7FF2h/7FF3h` (OAM Xloc/Yloc/Tile/Attr) redirect to
///   `[Base+Index*4+0..3]` — resolved straight to [`Target::Sram`] here
///   rather than a dedicated register, since fullsnes calls these ports
///   "totally useless": the byte they expose has no existence of its own
///   independent of the table cell it aliases.
/// - `$7FF4h` (OAM Bits) redirects to `[Base+Index/4+200h]`, but with R/W
///   semantics no plain SRAM cell has (2-bit write, whole-byte read) — see
///   [`Target::Obc1Bits`]'s doc.
/// - `$7FF5h/7FF6h/7FF7h` (Base select/Index/Unknown) are true chip
///   registers, not SRAM redirections — [`Target::Obc1Register`].
/// - Every other byte in the window (outside `$7FF0-$7FF7`) is ordinary
///   SRAM, including the two named workspace ranges
///   (`7800h-7A1Fh`/`7C00h-7E1Fh`) the two `Base` values point at.
#[must_use]
pub fn obc1_target(board: &Obc1Board, bank: u8, offset: u16) -> Option<Target> {
    let system_area = bank < 0x40 || (0x80..0xC0).contains(&bank);
    if !system_area || !(0x6000..=0x7FFF).contains(&offset) {
        return None;
    }
    let sram_idx = usize::from(offset - 0x6000);
    if board.sram_len == 0 {
        return Some(Target::Open);
    }
    match offset {
        0x7FF0..=0x7FF3 => {
            let reg = usize::from(offset - 0x7FF0);
            let addr = board.base_offset + usize::from(board.index) * 4 + reg;
            Some(Target::Sram(addr % board.sram_len))
        }
        0x7FF4 => {
            let addr = board.base_offset + usize::from(board.index / 4) + 0x200;
            Some(Target::Obc1Bits(addr % board.sram_len))
        }
        0x7FF5..=0x7FF7 => Some(Target::Obc1Register(offset)),
        _ => Some(Target::Sram(sram_idx % board.sram_len)),
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
