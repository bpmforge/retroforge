//! Super FX (GSU) SNES-side register window and on-cartridge RAM (ticket
//! W18-01, D-014, slice 1 of 5).
//!
//! ## What this slice models
//!
//! [`Gsu`] holds every register fullsnes's "SNES Cart GSU-n" chapter
//! documents: R0-R15, SFR, PBR, ROMBR, RAMBR, CBR, SCBR, SCMR, COLR, POR,
//! CLSR, VCR and BRAMR, at their documented reset values, with the
//! SNES-side read/write semantics ("General I/O Ports" and "Bitmap I/O
//! Ports") for each. [`GsuState`] wraps it with the cartridge's own RAM
//! buffer, the same "register file plus board memory" split
//! [`crate::sa1::Sa1State`] uses for SA-1.
//!
//! **The GSU does not execute this slice.** Writing R15's most significant
//! byte sets the GO flag exactly as real hardware does (fullsnes: "Writes
//! to 301Fh (R15.MSB) do also set GO=1 (and start GSU code execution)"),
//! and the SNES side can read that flag back, but nothing steps a GSU CPU
//! — there is no opcode core yet (W18-02). The point of this slice is the
//! window's SNES-visible behaviour: what a title's boot code sees when it
//! pokes the chip before this project can run any of its programs.
//!
//! ## Register-window mirrors
//!
//! fullsnes documents the whole `$3000-$3FFF` window (and its bank
//! mirrors `$80-$BF`) as one repeating shape ("Full I/O Map with Mirrors
//! for GSU2", `VCR=04h`):
//! ```text
//! 3000h..301Fh  R0-R15
//! 3020h..302Fh  mirror of 3030h..303Fh
//! 3030h..303Fh  status regs (unused or write-only ones return 00h)
//! 3040h..30FFh  mirrors of 3000h..303Fh
//! 3100h..32FFh  cache
//! 3300h..34FFh  mirrors of 3000h..303Fh
//! 3500h..3FFFh  open-bus
//! ```
//! [`Gsu::canonical_block_offset`] folds every mirror of the `3000-303F`
//! block onto that one 64-byte range; [`Gsu::read`]/[`Gsu::write`] resolve
//! the cache and open-bus ranges directly. This project models only the
//! GSU2 table — GSU1/MC1's older, differently-mirrored table (`VCR=01h`,
//! the "Black Blob" layout) is a strict subset of addresses real titles in
//! this project's library are not observed to depend on for mirror
//! behaviour, so implementing one table for both chip versions is this
//! slice's stated simplification, not an oversight.

use rf_core_api::StateError;

use crate::state::{StateIn, StateOut};

/// Offsets within the canonical `$3000-$303F` block (relative to `$3000`)
/// that this slice's SNES-side handlers give real, per-register meaning.
/// Cited to fullsnes "SNES Cart GSU-n General I/O Ports"/"...Bitmap I/O
/// Ports".
const SFR_LO: u16 = 0x30;
const SFR_HI: u16 = 0x31;
const BRAMR: u16 = 0x33;
const PBR: u16 = 0x34;
const ROMBR: u16 = 0x36;
const CFGR: u16 = 0x37;
const SCBR: u16 = 0x38;
const CLSR: u16 = 0x39;
const SCMR: u16 = 0x3A;
const VCR: u16 = 0x3B;
const RAMBR: u16 = 0x3C;
const CBR_LO: u16 = 0x3E;
const CBR_HI: u16 = 0x3F;

/// `$3000-$301F` R0-R15's byte count.
const REG_BLOCK_LEN: u16 = 0x20;
/// `$3100-$32FF` cache RAM's byte count.
const CACHE_LEN: usize = 0x200;

/// The Super FX (GSU) SNES-side register file (ticket W18-01).
///
/// Field-for-field the register list fullsnes's "SNES Cart GSU-n I/O Map"
/// gives, at the reset values the same chapter documents (all `$00`/`$0000`
/// — fullsnes states no non-zero GSU reset value anywhere in this section,
/// unlike SA-1's `$2200=$20`).
#[derive(Debug, Clone)]
pub struct Gsu {
    /// R0-R15 ($3000-$301F). Index 15 is the Program Counter; index 14 is
    /// the GETxx opcodes' Game Pak ROM address pointer; index 13/12 are
    /// LOOP's address/counter.
    regs: [u16; 16],
    /// The even-offset write latch feeding the odd-offset commit — fullsnes:
    /// "Writes to 3000h-301Eh (even addresses) do set LATCH=data. Writes to
    /// 3001h-301Fh (odd addresses) do apply LSB=LATCH and MSB=data."
    latch: u8,
    /// SFR ($3030/$3031), the full 16-bit layout. Only bits 1-5 (Z, CY, S,
    /// OV, GO) are SNES-writable (fullsnes: "(R) (Bit1-5: R/W)"); bit 15
    /// (IRQ) is reset on read; every other bit (R, ALT1, ALT2, IL, IH, B)
    /// is an opcode side effect this slice never sets, so it reads `0`
    /// until W18-02 gives the GSU something to run.
    sfr: u16,
    pbr: u8,
    /// ROMBR ($3036, R) — set only by the GSU's own `SEX`/bank opcodes
    /// (none run this slice), so this never leaves its reset value here.
    rombr: u8,
    /// RAMBR ($303C, R) — see [`Self::rombr`]'s doc; same reasoning.
    rambr: u8,
    /// CBR ($303E/303F, R, upper 12 bits) — fullsnes: "the SNES can set
    /// CBR=0000h by writing GO=0"; [`Self::write_sfr_lo`] applies that one
    /// SNES-side effect this slice models.
    cbr: u16,
    scbr: u8,
    scmr: u8,
    clsr: u8,
    bramr: u8,
    /// CFGR ($3037, W) — Config Register (MS0 multiplier-speed select,
    /// IRQ mask). No SNES-readable address (fullsnes's own table: "unused
    /// or write-only ones return 00h"); stored so a future opcode slice
    /// can read it back internally.
    cfgr: u8,
    /// VCR ($303B, R) — fixed per detected chip version at construction
    /// (fullsnes "Known versions: 1=MC1/Blob... 4=GSU2"): `1` for GSU-1,
    /// `4` for GSU-2. Real hardware never changes this.
    vcr: u8,
    /// COLR (N/A — no SNES-side address; used by the COLOR/GETC/PLOT
    /// opcodes, W18-0x). Held here now because [`rf_cart::Coprocessor`]'s
    /// board data outlives any one opcode slice's state.
    colr: u8,
    /// POR (N/A — CMODE opcode's Plot Option Register). See
    /// [`Self::colr`]'s doc.
    por: u8,
    /// `$3100-$32FF` Cache RAM: plain read/write memory this slice does not
    /// interpret (no code-cache execution — W18-02+), stored so a title
    /// that pre-loads it before setting GO reads back exactly what it
    /// wrote.
    cache: Box<[u8; CACHE_LEN]>,
    /// A write into a `$30xx-$3Fxx`-window offset this slice's canonical
    /// table has no real register at (the documented open-bus tail,
    /// `$3500-$3FFF` and its own mirrors) — diagnostic only, mirrors
    /// `Sa1Regs::unknown_write_offsets`'s doc and purpose; never gates or
    /// alters a write, and is not part of save state.
    pub unknown_write_offsets: std::collections::BTreeMap<u16, u32>,
}

impl Gsu {
    /// A fresh register file for the detected chip version (ticket
    /// W18-01). `version` sets [`Self::vcr`]; every other register starts
    /// at fullsnes's documented all-zero reset state.
    #[must_use]
    pub fn new(version: rf_cart::SuperFxVersion) -> Self {
        Self {
            regs: [0; 16],
            latch: 0,
            sfr: 0,
            pbr: 0,
            rombr: 0,
            rambr: 0,
            cbr: 0,
            scbr: 0,
            scmr: 0,
            clsr: 0,
            bramr: 0,
            cfgr: 0,
            vcr: match version {
                rf_cart::SuperFxVersion::Gsu1 => 0x01,
                rf_cart::SuperFxVersion::Gsu2 => 0x04,
            },
            colr: 0,
            por: 0,
            cache: Box::new([0; CACHE_LEN]),
            unknown_write_offsets: std::collections::BTreeMap::new(),
        }
    }

    /// `$303Ah` SCMR bit 4 — RON, "Game Pak ROM bus access (0=SNES,
    /// 1=GSU)". Read by [`crate::bus::SnesBus`] to decide whether an
    /// SNES-side ROM read sees the cartridge or open bus (see
    /// [`crate::mapping::gsu_target`]'s doc for why that decision is not
    /// made in mapping itself).
    #[must_use]
    pub fn ron(&self) -> bool {
        self.scmr & 0x10 != 0
    }
    /// `$303Ah` SCMR bit 3 — RAN, same rule for the RAM bus.
    #[must_use]
    pub fn ran(&self) -> bool {
        self.scmr & 0x08 != 0
    }

    /// `$3030/$3031` SFR bit 5 — GO. Set by writing R15's MSB (fullsnes),
    /// or directly via an SFR write (bit 1-5 are R/W); cleared the same
    /// way, or by a future slice's STOP opcode. This slice's tests pin
    /// exactly this: GO reads back what was written, with no CPU ever
    /// consuming it.
    #[must_use]
    pub fn go(&self) -> bool {
        self.sfr & 0x0020 != 0
    }

    /// `$3030/$3031` SFR bit 15 — IRQ, ORed into the 65C816's IRQ input
    /// (ticket W18-01 acceptance #2; the same OR pattern
    /// `SnesSystem::step` already applies for SA-1's `$2209` bit 7).
    /// fullsnes: "IRQ Interrupt Flag (reset on read, set on STOP)"; no
    /// STOP opcode runs this slice, so nothing sets this yet outside a
    /// test exercising the plumbing directly via [`Self::set_irq_for_test`].
    #[must_use]
    pub fn irq_pending(&self) -> bool {
        self.sfr & 0x8000 != 0
    }

    /// Test-only: set or clear the IRQ flag without an opcode core to set
    /// it. Exists because W18-02+ (the opcode core, specifically its STOP
    /// instruction) is the real setter and does not exist yet; this
    /// slice's acceptance criterion is the OR-into-the-CPU plumbing, which
    /// has to be exercised somehow before there is a STOP to execute.
    pub fn set_irq_for_test(&mut self, value: bool) {
        if value {
            self.sfr |= 0x8000;
        } else {
            self.sfr &= !0x8000;
        }
    }

    /// Fold a raw `$3000-$3FFF`-relative offset onto the canonical
    /// `$0000-$003F` block, or report which non-register range it falls
    /// in instead. Cited to fullsnes's "Full I/O Map with Mirrors for
    /// GSU2" table (this module's doc has the full layout).
    fn locate(rel: u16) -> Location {
        match rel {
            0x000..=0x0FF => Location::Block(Self::canonical_block_offset(rel & 0x3F)),
            0x100..=0x2FF => Location::Cache((rel - 0x100) as usize),
            0x300..=0x4FF => Location::Block(Self::canonical_block_offset(rel & 0x3F)),
            _ => Location::OpenBus,
        }
    }

    /// `$3020-$302F` mirrors `$3030-$303F` (fullsnes); every other offset
    /// in the 64-byte block is itself.
    fn canonical_block_offset(b: u16) -> u16 {
        if (0x20..0x30).contains(&b) {
            b + 0x10
        } else {
            b
        }
    }

    /// A side-effect-free read, for [`Self::peek`] and the non-mutating
    /// half of [`Self::read`]. `$3033`/`$3037`/`$3038`/`$3039`/`$303A`
    /// (BRAMR/CFGR/SCBR/CLSR/SCMR) are all write-only per fullsnes's own
    /// table note — "unused or write-only ones return 00h" — so they read
    /// `0` here regardless of the value last written.
    fn read_canonical(&self, b: u16) -> u8 {
        if b < REG_BLOCK_LEN {
            let idx = usize::from(b / 2);
            return if b % 2 == 0 {
                self.regs[idx] as u8
            } else {
                (self.regs[idx] >> 8) as u8
            };
        }
        match b {
            SFR_LO => self.sfr as u8,
            SFR_HI => (self.sfr >> 8) as u8,
            PBR => self.pbr,
            ROMBR => self.rombr,
            VCR => self.vcr,
            RAMBR => self.rambr,
            CBR_LO => self.cbr as u8,
            CBR_HI => (self.cbr >> 8) as u8,
            _ => 0,
        }
    }

    /// A read with no side effects: `$3030`'s IRQ-reset-on-read behaviour
    /// does not fire. Used by debuggers/peeks and by [`Self::save`]'s
    /// callers, never by [`crate::bus::SnesBus::read`] (which must use
    /// [`Self::read`] instead, so a real CPU read does clear IRQ).
    #[must_use]
    pub fn peek(&self, offset: u16) -> Option<u8> {
        let rel = offset.checked_sub(0x3000)?;
        match Self::locate(rel) {
            Location::Block(b) => Some(self.read_canonical(b)),
            Location::Cache(i) => Some(self.cache[i]),
            Location::OpenBus => None,
        }
    }

    /// A read of `$3000-$3FFF`. `$3031` (SFR's high byte, which carries
    /// the IRQ flag) resets that flag on read (fullsnes: "IRQ Interrupt
    /// Flag (reset on read...)"). Every other offset is side-effect-free.
    pub fn read(&mut self, offset: u16) -> Option<u8> {
        let rel = offset.checked_sub(0x3000)?;
        let loc = Self::locate(rel);
        if let Location::Block(b) = loc {
            if b == SFR_HI {
                let v = (self.sfr >> 8) as u8;
                self.sfr &= !0x8000;
                return Some(v);
            }
        }
        match loc {
            Location::Block(b) => Some(self.read_canonical(b)),
            Location::Cache(i) => Some(self.cache[i]),
            Location::OpenBus => None,
        }
    }

    /// A write into `$3000-$3FFF`. Offsets this slice's canonical table
    /// does not name as a real register are counted in
    /// [`Self::unknown_write_offsets`] (diagnostic only, mirrors
    /// `Sa1Regs::write`'s pattern) — everywhere else, a mirror lands on
    /// the same register its canonical address would.
    pub fn write(&mut self, offset: u16, value: u8) {
        let Some(rel) = offset.checked_sub(0x3000) else {
            return;
        };
        match Self::locate(rel) {
            Location::Block(b) => self.write_canonical(offset, b, value),
            Location::Cache(i) => self.cache[i] = value,
            Location::OpenBus => {
                *self.unknown_write_offsets.entry(offset).or_insert(0) += 1;
            }
        }
    }

    /// Apply a write already folded onto the canonical `$0000-$003F`
    /// block. `offset` (the raw, unfolded address) is only needed for the
    /// R0-R15 latch protocol's parity check, since a mirror's parity can
    /// differ from the canonical address's own (`$3040` mirrors `$3000`,
    /// both even, so in practice it never does for this project's chosen
    /// mirror set — kept explicit rather than assumed).
    fn write_canonical(&mut self, offset: u16, b: u16, value: u8) {
        if b < REG_BLOCK_LEN {
            self.write_register_word(offset, b, value);
            return;
        }
        match b {
            // Bits 1-5 (Z,CY,S,OV,GO) are R/W; every other bit in the low
            // byte is ignored (fullsnes: "(R) (Bit1-5: R/W)"). The high
            // byte (ALT1/ALT2/IL/IH/B/IRQ) is documented read-only from
            // the SNES side, so a write there is dropped.
            SFR_LO => {
                self.sfr = (self.sfr & !0x003E) | (u16::from(value) & 0x003E);
                // fullsnes "303Eh/303Fh CBR": "the SNES can set CBR=0000h
                // by writing GO=0".
                if value & 0x20 == 0 {
                    self.cbr = 0;
                }
            }
            SFR_HI => {}
            BRAMR => self.bramr = value & 0x01,
            PBR => self.pbr = value,
            CFGR => self.cfgr = value,
            SCBR => self.scbr = value,
            CLSR => self.clsr = value & 0x01,
            SCMR => self.scmr = value,
            // ROMBR/VCR/RAMBR/CBR are documented "(R)" — the SNES cannot
            // write them; a write is dropped rather than guessed at.
            ROMBR | VCR | RAMBR | CBR_LO | CBR_HI => {}
            _ => {
                *self.unknown_write_offsets.entry(offset).or_insert(0) += 1;
            }
        }
    }

    /// R0-R15's write protocol (fullsnes "SNES Cart GSU-n General I/O
    /// Ports"): an even raw offset latches its byte; the matching odd
    /// offset commits `u16::from_le_bytes([latch, value])`, and — only for
    /// R15 (`b == 0x1E`/`0x1F`) — also sets GO.
    fn write_register_word(&mut self, offset: u16, b: u16, value: u8) {
        let idx = usize::from(b / 2);
        if offset % 2 == 0 {
            self.latch = value;
        } else {
            self.regs[idx] = u16::from_le_bytes([self.latch, value]);
            if idx == 15 {
                self.sfr |= 0x0020; // GO=1 (fullsnes: "does also set GO=1").
            }
        }
    }

    /// `R14` — GETxx opcodes' Game Pak ROM address pointer.
    #[must_use]
    pub fn r14(&self) -> u16 {
        self.regs[14]
    }
    /// `R15` — Program Counter.
    #[must_use]
    pub fn r15(&self) -> u16 {
        self.regs[15]
    }
    /// `PBR` ($3034, R/W) — the SNES-writable program bank.
    #[must_use]
    pub fn pbr(&self) -> u8 {
        self.pbr
    }
    /// `SCBR` ($3038, W) — Screen Base Register.
    #[must_use]
    pub fn scbr(&self) -> u8 {
        self.scbr
    }
    /// `SCMR` ($303A, W) — Screen Mode Register, raw byte (see
    /// [`Self::ron`]/[`Self::ran`] for the two bits mapping cares about).
    #[must_use]
    pub fn scmr(&self) -> u8 {
        self.scmr
    }
    /// `CLSR` ($3039, W) bit 0 — Clock Select (0=10.7MHz, 1=21.4MHz).
    #[must_use]
    pub fn clsr(&self) -> u8 {
        self.clsr
    }
    /// `BRAMR` ($3033, W) bit 0 — Back-up RAM enable. Unused on every
    /// existing board (fullsnes: "None of the existing PCBs is having that
    /// extra RAM chip"); stored for completeness and save/load only.
    #[must_use]
    pub fn bramr(&self) -> u8 {
        self.bramr
    }
    /// `CFGR` ($3037, W) — see the field's doc.
    #[must_use]
    pub fn cfgr(&self) -> u8 {
        self.cfgr
    }
    /// `COLR` (N/A) — Color Register, for a future opcode slice's
    /// COLOR/GETC/PLOT.
    #[must_use]
    pub fn colr(&self) -> u8 {
        self.colr
    }
    /// `POR` (N/A) — Plot Option Register, for a future opcode slice's
    /// CMODE/PLOT.
    #[must_use]
    pub fn por(&self) -> u8 {
        self.por
    }
    /// `VCR` ($303B, R) — the fixed chip-version byte.
    #[must_use]
    pub fn vcr(&self) -> u8 {
        self.vcr
    }

    pub(crate) fn save(&self, o: &mut StateOut) -> Result<(), StateError> {
        for r in self.regs {
            o.u16(r)?;
        }
        o.u8(self.latch)?;
        o.u16(self.sfr)?;
        o.u8(self.pbr)?;
        o.u8(self.rombr)?;
        o.u8(self.rambr)?;
        o.u16(self.cbr)?;
        o.u8(self.scbr)?;
        o.u8(self.scmr)?;
        o.u8(self.clsr)?;
        o.u8(self.bramr)?;
        o.u8(self.cfgr)?;
        o.u8(self.vcr)?;
        o.u8(self.colr)?;
        o.u8(self.por)?;
        o.bytes(&*self.cache)
    }

    pub(crate) fn load(&mut self, i: &mut StateIn) -> Result<(), StateError> {
        for r in &mut self.regs {
            *r = i.u16()?;
        }
        self.latch = i.u8()?;
        self.sfr = i.u16()?;
        self.pbr = i.u8()?;
        self.rombr = i.u8()?;
        self.rambr = i.u8()?;
        self.cbr = i.u16()?;
        self.scbr = i.u8()?;
        self.scmr = i.u8()?;
        self.clsr = i.u8()?;
        self.bramr = i.u8()?;
        self.cfgr = i.u8()?;
        self.vcr = i.u8()?;
        self.colr = i.u8()?;
        self.por = i.u8()?;
        i.fill(&mut *self.cache)
    }
}

/// Which sub-range of the register window a raw offset resolved to.
enum Location {
    /// The canonical `$3000-$303F` block (post-mirror-fold), carrying the
    /// folded offset.
    Block(u16),
    /// `$3100-$32FF` cache RAM, carrying the 0-based index into it.
    Cache(usize),
    /// The documented open-bus tail (`$3500-$3FFF`) or any offset outside
    /// this window's declared span.
    OpenBus,
}

/// The GSU board's live state: the register file plus the cartridge's own
/// RAM buffer (ticket W18-01) — the same "register file plus board
/// memory" split [`crate::sa1::Sa1State`] uses for SA-1's I-RAM/BW-RAM.
#[derive(Debug, Clone)]
pub struct GsuState {
    pub regs: Gsu,
    pub ram: Vec<u8>,
    /// The parsed cartridge's fixed sizes, kept alongside `ram` so
    /// [`crate::mapping::GsuBoard`] can be rebuilt on every access without
    /// recomputing lengths — same reasoning as `Sa1State::board`.
    pub rom_len: usize,
    pub ram_len: usize,
}

impl GsuState {
    #[must_use]
    pub fn new(version: rf_cart::SuperFxVersion, rom_len: usize, ram_kib: usize) -> Self {
        let ram_len = ram_kib * 1024;
        Self {
            regs: Gsu::new(version),
            ram: vec![0; ram_len],
            rom_len,
            ram_len,
        }
    }

    /// The live [`crate::mapping::GsuBoard`] view [`crate::mapping::gsu_target`]
    /// resolves addresses against.
    #[must_use]
    pub fn board(&self) -> crate::mapping::GsuBoard {
        crate::mapping::GsuBoard {
            rom_len: self.rom_len,
            ram_len: self.ram_len,
            ron: self.regs.ron(),
            ran: self.regs.ran(),
        }
    }
}
