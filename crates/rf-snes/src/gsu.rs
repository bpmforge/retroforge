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

    // --- Ticket W18-02 (D-014, slice 2 of 5): the instruction core's own
    // one-instruction-lifetime state. None of this has a `$30xx` address —
    // fullsnes lists Sreg/Dreg and the ALT1/ALT2/B prefix bits as pure CPU
    // internals ("SNES Cart GSU-n General I/O Ports": "N/A - Sreg/Dreg -
    // Memorized TO/FROM Prefix Selections"); SFR's own ALT1/ALT2/IL/IH/B
    // bits ($3031, read-only from the SNES) mirror `alt1`/`alt2`/`b_flag`
    // below for [`Self::read_canonical`]'s SFR-high-byte read, so a title
    // polling SFR while the GSU runs sees real prefix state instead of the
    // all-zero placeholder slice 1 left there.
    /// The source register (Sreg) a TO/WITH/FROM prefix selected for the
    /// next non-prefix, non-branch opcode; `R0` (`0`) is the documented
    /// default (fullsnes "GSU Prefix Opcodes": "other opcodes... Sreg=R0").
    sreg: u8,
    /// The destination register (Dreg), same default and reset rule as
    /// [`Self::sreg`].
    dreg: u8,
    /// ALT1 prefix flag (`3D`, SFR bit 8).
    alt1: bool,
    /// ALT2 prefix flag (`3E`, SFR bit 9).
    alt2: bool,
    /// The `WITH` prefix's B flag (SFR bit 12): fullsnes "Aside from
    /// setting Sreg+Dreg, WITH does additionally set the B-flag, this
    /// causes any following 1nh/Bnh bytes to act as MOVE/MOVES opcodes
    /// (rather than as TO/FROM prefixes)."
    b_flag: bool,
    /// A control-flow redirect (new PBR, new R15) an opcode just executed
    /// but has not yet applied — fullsnes "Jump Notes": "the next BYTE
    /// after the jump opcode is fetched as opcode byte, and is executed
    /// before continuing at the jump-target address." Set by
    /// [`GsuState::commit_dest`] (Dreg=R15), by a taken branch, by
    /// JMP/LJMP, and by a taken LOOP; applied by [`GsuState::step_one`]
    /// immediately after the ONE delay-slot opcode that follows it has
    /// finished executing. `None` means no jump is pending.
    pending_jump: Option<(u8, u16)>,
    /// The RAM-Address-Cache (fullsnes "SNES Cart GSU-n Other Caches":
    /// "This very simple cache memorizes the most recently used RAM
    /// address (from LM/LMS opcodes, and probably also from
    /// LDB/LDW/STB/STW/SM/SMS opcodes)"), used by SBK's writeback.
    last_ram_addr: u16,
    /// The most recently executed opcode's documented clock count
    /// (fullsnes "SNES Cart GSU-n CPU Misc" per-opcode `Clks` columns) —
    /// recorded, per ticket W18-02 acceptance #1, but not yet charged
    /// against the master clock (that is slice 4's job; see
    /// [`GsuState::run`]'s doc for this slice's provisional substitute).
    last_cost: u32,
    /// Total opcodes executed since construction (or since the last
    /// `load`) — diagnostic/telemetry, part of save state so a mid-program
    /// round trip can assert progress resumed rather than restarted.
    instructions_executed: u64,
    /// PLOT call count (ticket W18-02: PLOT is a recording no-op this
    /// slice — the pixel cache is W18-03). Not part of save state, same
    /// reasoning as [`Self::unknown_write_offsets`]: diagnostic only.
    pub plot_calls: u32,
    /// RPIX call count — see [`Self::plot_calls`]'s doc. RPIX this slice
    /// returns `0` and records the call rather than reading any pixel
    /// cache/RAM bitmap (W18-03).
    pub rpix_calls: u32,
    /// The most recently fetched opcode byte — diagnostic only (not part
    /// of save state, same reasoning as [`Self::plot_calls`]), for
    /// `title_probe`'s `PROBE_GSUREGS` dump (ticket W18-02: "extend it
    /// with PC/opcode/instruction count").
    pub last_opcode: u8,
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
            sreg: 0,
            dreg: 0,
            alt1: false,
            alt2: false,
            b_flag: false,
            pending_jump: None,
            last_ram_addr: 0,
            last_cost: 0,
            instructions_executed: 0,
            plot_calls: 0,
            rpix_calls: 0,
            last_opcode: 0,
        }
    }

    /// The most recently executed opcode's documented clock count (ticket
    /// W18-02 acceptance #1) — see [`Self::last_cost`]'s doc.
    #[must_use]
    pub fn last_cost(&self) -> u32 {
        self.last_cost
    }
    /// Total opcodes executed since construction/load.
    #[must_use]
    pub fn instructions_executed(&self) -> u64 {
        self.instructions_executed
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
    /// fullsnes: "IRQ Interrupt Flag (reset on read, set on STOP)". Ticket
    /// W18-02's STOP opcode ([`GsuState::exec_stop`]) always sets the SFR
    /// bit unconditionally — "set on STOP" is unqualified — but the line
    /// this method reports to the 65C816 is additionally gated by
    /// `CFGR.Bit7` ("7 IRQ Interrupt Mask (0=Trigger IRQ on STOP opcode,
    /// 1=Disable IRQ)"): a title that disabled the mask can still poll
    /// `$3031` and see the flag, it just never reaches the CPU's IRQ input.
    /// This is the reading fullsnes's own parenthetical on the SFR bit
    /// ("also set if IRQ masked?") is consistent with: the bit is set
    /// either way, only delivery is masked.
    #[must_use]
    pub fn irq_pending(&self) -> bool {
        self.sfr & 0x8000 != 0 && self.cfgr & 0x80 == 0
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
            return if b.is_multiple_of(2) {
                self.regs[idx] as u8
            } else {
                (self.regs[idx] >> 8) as u8
            };
        }
        match b {
            SFR_LO => self.sfr as u8,
            // High byte carries ALT1 (bit8), ALT2 (bit9), B (bit12) and
            // IRQ (bit15) — ticket W18-02 gives ALT1/ALT2/B their real
            // opcode-driven values here (slice 1 left them permanently
            // `0`, see this field's own doc); IL/IH (bits 10/11) and R
            // (bit6, in the low byte) stay `0` — this slice fetches
            // operand bytes and ROM data directly rather than modelling
            // the immediate-byte pipeline or the ROM-read-in-progress
            // state as separate SFR-visible stages (fullsnes gives no
            // opcode whose documented behaviour depends on a title
            // observing IL/IH/R mid-instruction).
            SFR_HI => {
                (self.sfr >> 8) as u8
                    | if self.alt1 { 0x01 } else { 0 }
                    | if self.alt2 { 0x02 } else { 0 }
                    | if self.b_flag { 0x10 } else { 0 }
            }
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
                let v = self.read_canonical(b);
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
        if offset.is_multiple_of(2) {
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
        o.bytes(&*self.cache)?;
        // Ticket W18-02: the instruction core's own one-instruction-
        // lifetime state, appended after slice 1's fields for the same
        // reason the GSU board itself is appended after SA-1 above — a
        // state saved before this ticket only disagrees in what trails
        // the byte the older format already ends at.
        o.u8(self.sreg)?;
        o.u8(self.dreg)?;
        o.bool(self.alt1)?;
        o.bool(self.alt2)?;
        o.bool(self.b_flag)?;
        match self.pending_jump {
            Some((bank, pc)) => {
                o.bool(true)?;
                o.u8(bank)?;
                o.u16(pc)?;
            }
            None => o.bool(false)?,
        }
        o.u16(self.last_ram_addr)?;
        o.u32(self.last_cost)?;
        o.u64(self.instructions_executed)
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
        i.fill(&mut *self.cache)?;
        self.sreg = i.u8()?;
        self.dreg = i.u8()?;
        self.alt1 = i.bool()?;
        self.alt2 = i.bool()?;
        self.b_flag = i.bool()?;
        self.pending_jump = if i.bool()? {
            let bank = i.u8()?;
            let pc = i.u16()?;
            Some((bank, pc))
        } else {
            None
        };
        self.last_ram_addr = i.u16()?;
        self.last_cost = i.u32()?;
        self.instructions_executed = i.u64()?;
        Ok(())
    }
}

/// Ticket W18-02 (D-014, slice 2 of 5): register-only executor helpers —
/// everything the instruction core needs that touches only `Gsu`'s own
/// fields, never ROM/RAM. [`GsuState`]'s `impl` block below (which owns the
/// cartridge RAM and borrows ROM per call) is where opcode fetch/dispatch
/// and every memory-touching opcode live.
impl Gsu {
    /// The current source register's value (fullsnes "GSU Prefix Opcodes":
    /// Sreg defaults to R0).
    fn src(&self) -> u16 {
        self.regs[usize::from(self.sreg)]
    }
    /// Any register's raw value, `R15` included (fullsnes "Program Counter
    /// (R15) Notes": "R15 can be used as source operand... in all cases
    /// R15 contains the address of the next opcode" — true here because
    /// [`GsuState::fetch_byte`] advances `regs[15]` at fetch time, before
    /// any opcode body runs).
    fn reg(&self, n: u8) -> u16 {
        self.regs[usize::from(n)]
    }
    /// Commit `v` to register `n`, `R15` included. Per fullsnes's "Jump
    /// Notes" ("Jumps can be...implemented by...using R15 as destination
    /// register... the next BYTE after the jump opcode is...executed
    /// before continuing at the jump-target address"), a write to R15 is
    /// never applied immediately — it becomes [`Self::pending_jump`], which
    /// [`GsuState::step_one`] applies only after the ONE opcode that
    /// follows this one has finished. Every other register commits at
    /// once.
    fn commit_to(&mut self, n: u8, v: u16) {
        if n == 15 {
            self.pending_jump = Some((self.pbr, v));
        } else {
            self.regs[usize::from(n)] = v;
        }
    }
    /// Commit `v` to the current destination register (Dreg) — the common
    /// case for MOV/ALU opcodes whose destination the TO/WITH prefix (or
    /// the R0 default) selected.
    fn commit_dest(&mut self, v: u16) {
        let dreg = self.dreg;
        self.commit_to(dreg, v);
    }

    fn z_flag(&self) -> bool {
        self.sfr & 0x0002 != 0
    }
    fn cy_flag(&self) -> bool {
        self.sfr & 0x0004 != 0
    }
    fn s_flag(&self) -> bool {
        self.sfr & 0x0008 != 0
    }
    fn ov_flag(&self) -> bool {
        self.sfr & 0x0010 != 0
    }
    fn set_flag_bit(&mut self, bit: u16, v: bool) {
        if v {
            self.sfr |= bit;
        } else {
            self.sfr &= !bit;
        }
    }
    fn set_z(&mut self, v: u16) {
        self.set_flag_bit(0x0002, v == 0);
    }
    fn set_cy(&mut self, v: bool) {
        self.set_flag_bit(0x0004, v);
    }
    /// Sign flag from bit 15 — correct for every ordinary 16-bit ALU
    /// result. LOB/HIB's own byte-domain sign bit is set directly, not
    /// through this helper — see their opcode bodies.
    fn set_s(&mut self, v: u16) {
        self.set_flag_bit(0x0008, v & 0x8000 != 0);
    }
    fn set_ov(&mut self, v: bool) {
        self.set_flag_bit(0x0010, v);
    }
    /// Z and S together (fullsnes's very common `000-s-z` flags column).
    fn set_zs(&mut self, v: u16) {
        self.set_z(v);
        self.set_s(v);
    }

    /// ALT3 mirrors ALT1+ALT2 both set (fullsnes "GSU Prefix Opcodes"
    /// table: `3F` sets both the ALT1 and ALT2 flag bits).
    fn alt3(&self) -> bool {
        self.alt1 && self.alt2
    }

    /// Reset the one-instruction prefix state. Called at the end of every
    /// "other" opcode — fullsnes "GSU Prefix Opcodes": "Other opcodes do
    /// reset B=0, ALT1=0, ALT2=0, Sreg=R0, Dreg=R0... namely including
    /// JMP/LOOP...NOP...MOVE/MOVES". Bxx (taken or not) and the
    /// TO/WITH/FROM/ALT1/ALT2 prefix opcodes themselves are the only
    /// opcodes that must NOT call this.
    fn reset_prefix_state(&mut self) {
        self.b_flag = false;
        self.alt1 = false;
        self.alt2 = false;
        self.sreg = 0;
        self.dreg = 0;
    }
}

/// Signed 16-bit addition overflow: operands share a sign and the result's
/// sign differs from theirs.
fn add_overflow(a: u16, b: u16, r: u16) -> bool {
    (a ^ b) & 0x8000 == 0 && (a ^ r) & 0x8000 != 0
}
/// Signed 16-bit subtraction overflow: operands have different signs and
/// the result's sign differs from the minuend's.
fn sub_overflow(a: u16, b: u16, r: u16) -> bool {
    (a ^ b) & 0x8000 != 0 && (a ^ r) & 0x8000 != 0
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

// ============================================================================
// Ticket W18-02 (D-014, slice 2 of 5): the instruction core.
// ============================================================================
//
// This slice does not charge cycles against the master clock (that is
// slice 4's job — see [`GsuState::STEP_BUDGET`]'s doc) and does not model
// the code-cache, pixel-cache, or ROM/RAM-data-cache as separate hardware
// stages (fullsnes "SNES Cart GSU-n Code-Cache"/"Pixel-Cache"/"Other
// Caches" — the code-cache is opcode-fetch-from-cache only, which never
// happens here since fetch always goes straight to ROM/RAM; the pixel
// cache belongs to PLOT/RPIX, stubbed below and given a real
// implementation in W18-03; the ROM/RAM-data caches only affect *when* a
// GETxx/store opcode would WAIT, which this slice's uncharged-cycle model
// has no use for). Every fetch and every GETxx/LDx/STx/SBK access reads or
// writes ROM/RAM directly and immediately, exactly as ticket W18-02's brief
// says is acceptable for this slice.
impl GsuState {
    /// Provisional per-`SnesSystem::step` opcode budget: the GSU executes
    /// at most this many opcodes each SNES master-clock step while GO is
    /// set, with **no** cycle cost charged against that master clock yet.
    /// Slice 4 replaces this with the same credit-based interleave
    /// `SnesSystem::step` already runs for SA-1 (`Sa1State::credit`),
    /// charging each opcode's [`Gsu::last_cost`] against the shared master
    /// clock instead of a flat opcode count. `64` is not a hardware
    /// constant — it is simply large enough that a title's boot-time GSU
    /// program (typically dozens of opcodes: clear some registers, CACHE,
    /// jump into a loop) can make visible progress within one SNES
    /// instruction's worth of `SnesSystem::step` calls, without letting a
    /// runaway or infinite-looping GSU program monopolise a single step
    /// indefinitely.
    pub const STEP_BUDGET: u32 = 64;

    /// Run up to [`Self::STEP_BUDGET`] GSU opcodes, stopping early if STOP
    /// (or an SNES-side `SFR` write) clears GO. `rom` is the cartridge's
    /// flat ROM bytes, borrowed for the duration of this call only — the
    /// same short-lived-borrow shape [`crate::sa1::Sa1State::step`] uses
    /// `&SnesBus::rom` for, and for the same reason: `GsuState` needs
    /// `&bus.rom` while `SnesSystem::step` is simultaneously mutating
    /// `bus.gsu`, which the call site resolves with the same
    /// disjoint-field borrow (`let bus = &mut self.bus;` then
    /// `bus.gsu.as_mut()` alongside `&bus.rom`) SA-1 already established.
    ///
    /// Law 8 (`docs/design/...`/`CLAUDE.md`): `budget` is decremented
    /// unconditionally, first, every iteration — before dispatch, not
    /// after — so a hand-assembled test program with a mis-encoded branch
    /// (or one that never executes STOP) cannot spin this loop forever;
    /// the worst case is exactly [`Self::STEP_BUDGET`] opcodes executed
    /// this call, full stop.
    pub fn run(&mut self, rom: &[u8]) {
        let mut budget = Self::STEP_BUDGET;
        while budget > 0 && self.regs.go() {
            budget -= 1;
            self.step_one(rom);
        }
    }

    /// Run exactly one GSU opcode (including applying a control-flow
    /// redirect the PREVIOUS opcode set up but deferred one instruction —
    /// see [`Gsu::pending_jump`]'s doc).
    fn step_one(&mut self, rom: &[u8]) {
        let had_pending = self.regs.pending_jump.take();
        let opcode = self.fetch_byte(rom);
        self.regs.last_opcode = opcode;
        let cost = self.exec_opcode(rom, opcode);
        self.regs.last_cost = cost;
        self.regs.instructions_executed += 1;
        if let Some((bank, pc)) = had_pending {
            self.regs.pbr = bank;
            self.regs.regs[15] = pc;
        }
    }

    // --- Fetch/memory -------------------------------------------------

    /// Fold `(bank, addr)` onto a flat `rom` index. fullsnes "SNES Cart
    /// GSU-n Memory Map (at GSU Side)": banks `$00-$3F` are LoROM
    /// (`$8000-$FFFF`, "Mirror of LoROM at 00-3F:8000-FFFF" folds
    /// `$0000-$7FFF` onto the same bytes — masking either half by
    /// `$7FFF` reproduces that mirror directly); banks `$40-$5F` are the
    /// linear HiROM mirror of the same 2 MiB.
    fn gsu_rom_index(bank: u8, addr: u16) -> usize {
        let b = bank & 0x7F;
        if b < 0x40 {
            (usize::from(b) << 15) | (usize::from(addr) & 0x7FFF)
        } else {
            (usize::from(b.saturating_sub(0x40)) << 16) | usize::from(addr)
        }
    }

    fn rom_byte(rom: &[u8], bank: u8, addr: u16) -> u8 {
        if rom.is_empty() {
            return 0;
        }
        rom[Self::gsu_rom_index(bank, addr) % rom.len()]
    }

    /// Fold `(bank in {$70,$71}, addr)` onto a `self.ram` index, `None` if
    /// there is no GSU RAM at all (fullsnes "GSU2 Memory Map (at GSU
    /// Side)": "70-71:0000-FFFF Game Pak RAM").
    fn gsu_ram_index(&self, bank: u8, addr: u16) -> Option<usize> {
        if self.ram_len == 0 {
            return None;
        }
        let b = usize::from(bank == 0x71);
        Some(((b << 16) | usize::from(addr)) % self.ram_len)
    }

    /// Fetch one opcode/operand byte at `(PBR, R15)`, advancing R15 by one
    /// — fullsnes "301Eh-301Fh R15 Program Counter". PBR pointing at
    /// `$70`/`$71` fetches from GSU RAM (fullsnes allows PBR to address
    /// either ROM or RAM, unlike ROMBR/RAMBR); everything else fetches
    /// from ROM. Real hardware can also execute from the 512-byte code
    /// cache (`$00-$1FF`-relative to CBR) — not modelled this slice, see
    /// this section's banner comment.
    fn fetch_byte(&mut self, rom: &[u8]) -> u8 {
        let pc = self.regs.regs[15];
        self.regs.regs[15] = pc.wrapping_add(1);
        let bank = self.regs.pbr;
        if bank == 0x70 || bank == 0x71 {
            match self.gsu_ram_index(bank, pc) {
                Some(i) => self.ram[i],
                None => 0,
            }
        } else {
            Self::rom_byte(rom, bank, pc)
        }
    }

    /// GETB/GETBH/GETBL/GETBS/GETC's data source: `[ROMBR:R14]` (fullsnes
    /// "GSU MOV Opcodes (Load BYTE from ROM)"). A direct, uncached read —
    /// this section's banner comment explains why that is this slice's
    /// accepted simplification.
    fn rom_data_byte(&self, rom: &[u8]) -> u8 {
        Self::rom_byte(rom, self.regs.rombr, self.regs.r14())
    }

    /// `[RAMBR:addr]` byte, `0` if there is no GSU RAM (fullsnes "GSU MOV
    /// Opcodes (Load/Store Byte/Word to/from RAM)").
    fn ram_read_byte(&self, addr: u16) -> u8 {
        let bank = if self.regs.rambr & 1 != 0 { 0x71 } else { 0x70 };
        match self.gsu_ram_index(bank, addr) {
            Some(i) => self.ram[i],
            None => 0,
        }
    }
    fn ram_write_byte(&mut self, addr: u16, v: u8) {
        let bank = if self.regs.rambr & 1 != 0 { 0x71 } else { 0x70 };
        if let Some(i) = self.gsu_ram_index(bank, addr) {
            self.ram[i] = v;
        }
    }
    /// Word read through RAMBR. fullsnes: "Words at odd addresses are
    /// accessing [addr AND NOT 1], with data LSB/MSB swapped."
    fn ram_read_word(&self, addr: u16) -> u16 {
        let aligned = addr & !1;
        let b0 = self.ram_read_byte(aligned);
        let b1 = self.ram_read_byte(aligned.wrapping_add(1));
        if addr & 1 != 0 {
            u16::from_le_bytes([b1, b0])
        } else {
            u16::from_le_bytes([b0, b1])
        }
    }
    fn ram_write_word(&mut self, addr: u16, v: u16) {
        let aligned = addr & !1;
        let [lo, hi] = v.to_le_bytes();
        if addr & 1 != 0 {
            self.ram_write_byte(aligned, hi);
            self.ram_write_byte(aligned.wrapping_add(1), lo);
        } else {
            self.ram_write_byte(aligned, lo);
            self.ram_write_byte(aligned.wrapping_add(1), hi);
        }
    }

    // --- Opcode dispatch ------------------------------------------------

    /// Execute one already-fetched opcode byte, fetching any of its own
    /// operand bytes from `rom`/RAM as needed, and return its documented
    /// clock count (fullsnes "SNES Cart GSU-n CPU Misc" — recorded per
    /// ticket W18-02 acceptance #1, not charged; see [`Self::STEP_BUDGET`]'s
    /// doc). Cited section for every opcode family is given at its match
    /// arm below.
    #[allow(clippy::too_many_lines)]
    fn exec_opcode(&mut self, rom: &[u8], op: u8) -> u32 {
        let alt1 = self.regs.alt1;
        let alt2 = self.regs.alt2;
        let alt3 = self.regs.alt3();
        match op {
            // --- CPU Misc / Special opcodes ---------------------------
            0x00 => self.exec_stop(rom),
            0x01 => {
                self.regs.reset_prefix_state();
                1
            }
            0x02 => {
                // CACHE: fullsnes "Code-Cache Loading Notes": "CACHE sets
                // CBR to 'R15 AND FFF0h'". Only CBR itself is modelled
                // (the cache contents are the plain `Gsu::cache` bytes
                // slice 1 already gives read/write semantics — no opcode
                // executes out of them this slice).
                self.regs.cbr = self.regs.regs[15] & 0xFFF0;
                self.regs.reset_prefix_state();
                1
            }

            // --- Rotate/Shift/Inc/Dec (no ALT variants) ---------------
            0x03 => {
                // LSR: fullsnes "000-0cz" — S is hard-zeroed (not just
                // "unaffected"), CY/Z from the shift.
                let rs = self.regs.src();
                let cy = rs & 1 != 0;
                let result = rs >> 1;
                self.regs.set_cy(cy);
                self.regs.set_flag_bit(0x0008, false);
                self.regs.set_z(result);
                self.regs.commit_dest(result);
                self.regs.reset_prefix_state();
                1
            }
            0x04 => {
                // ROL: rotate left through carry.
                let rs = self.regs.src();
                let cy_in = self.regs.cy_flag();
                let cy_out = rs & 0x8000 != 0;
                let result = (rs << 1) | u16::from(cy_in);
                self.regs.set_cy(cy_out);
                self.regs.set_zs(result);
                self.regs.commit_dest(result);
                self.regs.reset_prefix_state();
                1
            }

            // --- Branches (0x05-0x0F): flags/prefix state UNCHANGED ---
            // fullsnes "GSU Prefix Opcodes": "Bxx addr...branch opcodes
            // (no change)". `Self::exec_branch` handles fetch, condition,
            // and the pending-jump target; it never touches prefix state.
            0x05..=0x0F => self.exec_branch(rom, op),

            // --- TO Rn (prefix) / MOVE Rd,Rs (if B set) ---------------
            0x10..=0x1F => {
                let n = op & 0x0F;
                if self.regs.b_flag {
                    // fullsnes "2s 1d MOVE Rd,Rs": Rd=n (from this byte),
                    // Rs=Sreg (memorized by the preceding WITH). No flags.
                    let v = self.regs.reg(self.regs.sreg);
                    self.regs.commit_to(n, v);
                    self.regs.reset_prefix_state();
                    2
                } else {
                    self.regs.dreg = n;
                    1
                }
            }
            // --- WITH Rn (prefix): Sreg=Dreg=Rn, B=1 ------------------
            0x20..=0x2F => {
                let n = op & 0x0F;
                self.regs.sreg = n;
                self.regs.dreg = n;
                self.regs.b_flag = true;
                1
            }
            // --- STW (Rn) / ALT1: STB (Rn) -----------------------------
            0x30..=0x3B => {
                let n = op & 0x0F;
                let addr = self.regs.reg(n);
                if alt1 {
                    let v = self.regs.src() as u8;
                    self.ram_write_byte(addr, v);
                    self.regs.last_ram_addr = addr;
                    self.regs.reset_prefix_state();
                    2
                } else {
                    let v = self.regs.src();
                    self.ram_write_word(addr, v);
                    self.regs.last_ram_addr = addr;
                    self.regs.reset_prefix_state();
                    1
                }
            }
            0x3C => {
                // LOOP: fullsnes "3C LOOP loop r12,r13 ;r12=r12-1, if
                // Zf=0 then R15=R13".
                let r12 = self.regs.reg(12).wrapping_sub(1);
                self.regs.commit_to(12, r12);
                self.regs.set_zs(r12);
                if !self.regs.z_flag() {
                    let target = self.regs.reg(13);
                    self.regs.pending_jump = Some((self.regs.pbr, target));
                }
                self.regs.reset_prefix_state();
                1
            }
            0x3D => {
                self.regs.alt1 = true;
                1
            }
            0x3E => {
                self.regs.alt2 = true;
                1
            }
            0x3F => {
                self.regs.alt1 = true;
                self.regs.alt2 = true;
                1
            }

            // --- LDW (Rn) / ALT1: LDB (Rn) -----------------------------
            0x40..=0x4B => {
                let n = op & 0x0F;
                let addr = self.regs.reg(n);
                let (v, cost) = if alt1 {
                    (u16::from(self.ram_read_byte(addr)), 2)
                } else {
                    (self.ram_read_word(addr), 2)
                };
                self.regs.commit_dest(v);
                self.regs.last_ram_addr = addr;
                self.regs.reset_prefix_state();
                cost
            }
            // --- PLOT / ALT1(mirrors to ALT3 too): RPIX ---------------
            0x4C => {
                let cost = if alt1 || alt3 {
                    // RPIX (W18-03 gives this the real pixel-cache
                    // flush+read; this slice records the call and
                    // returns a deterministic 0 — fullsnes "RPIX isn't
                    // cached, it does always read data from RAM").
                    self.regs.rpix_calls += 1;
                    self.regs.commit_dest(0);
                    self.regs.set_zs(0);
                    20
                } else {
                    // PLOT (W18-03 gives this the real pixel cache; this
                    // slice records the call and advances the X
                    // coordinate exactly as fullsnes documents — "Pixel=
                    // COLR, R1=R1+1" — without ever touching RAM).
                    self.regs.plot_calls += 1;
                    let x = self.regs.reg(1).wrapping_add(1);
                    self.regs.commit_to(1, x);
                    2
                };
                self.regs.reset_prefix_state();
                cost
            }
            0x4D => {
                // SWAP: Rd = Rs ROR 8.
                let rs = self.regs.src();
                let result = rs.rotate_right(8);
                self.regs.set_zs(result);
                self.regs.commit_dest(result);
                self.regs.reset_prefix_state();
                1
            }
            // --- COLOR / ALT1(mirrors ALT3): CMODE --------------------
            0x4E => {
                let cost = if alt1 || alt3 {
                    self.regs.por = (self.regs.src() as u8) & 0x1F;
                    2
                } else {
                    self.regs.colr = self.regs.src() as u8;
                    1
                };
                self.regs.reset_prefix_state();
                cost
            }
            0x4F => {
                let result = !self.regs.src();
                self.regs.set_zs(result);
                self.regs.commit_dest(result);
                self.regs.reset_prefix_state();
                1
            }

            // --- ADD/ADC family ----------------------------------------
            0x50..=0x5F => {
                let n = op & 0x0F;
                let rs = self.regs.src();
                let (operand, cin, cost) = if alt3 {
                    (u16::from(n), u16::from(self.regs.cy_flag()), 2)
                } else if alt1 {
                    (self.regs.reg(n), u16::from(self.regs.cy_flag()), 2)
                } else if alt2 {
                    (u16::from(n), 0, 2)
                } else {
                    (self.regs.reg(n), 0, 1)
                };
                let sum = u32::from(rs) + u32::from(operand) + u32::from(cin);
                let result = sum as u16;
                self.regs.set_ov(add_overflow(rs, operand, result));
                self.regs.set_cy(sum > 0xFFFF);
                self.regs.set_zs(result);
                self.regs.commit_dest(result);
                self.regs.reset_prefix_state();
                cost
            }
            // --- SUB/SBC/CMP family --------------------------------------
            0x60..=0x6F => {
                let n = op & 0x0F;
                let rs = self.regs.src();
                let (operand, borrow_in, cost, is_cmp) = if alt3 {
                    (self.regs.reg(n), 0u16, 2, true)
                } else if alt1 {
                    (self.regs.reg(n), u16::from(!self.regs.cy_flag()), 2, false)
                } else if alt2 {
                    (u16::from(n), 0, 2, false)
                } else {
                    (self.regs.reg(n), 0, 1, false)
                };
                let result = rs.wrapping_sub(operand).wrapping_sub(borrow_in);
                let no_borrow = u32::from(rs) >= u32::from(operand) + u32::from(borrow_in);
                self.regs.set_ov(sub_overflow(rs, operand, result));
                self.regs.set_cy(no_borrow);
                self.regs.set_zs(result);
                if !is_cmp {
                    self.regs.commit_dest(result);
                }
                self.regs.reset_prefix_state();
                cost
            }

            // --- MERGE --------------------------------------------------
            0x70 => {
                let r7 = self.regs.reg(7);
                let r8 = self.regs.reg(8);
                let result = (r7 & 0xFF00) | (r8 >> 8);
                self.regs.set_flag_bit(0x0008, result & 0x8080 != 0); // S
                self.regs.set_flag_bit(0x0010, result & 0xC0C0 != 0); // OV
                self.regs.set_flag_bit(0x0004, result & 0xE0E0 != 0); // CY
                self.regs.set_flag_bit(0x0002, result & 0xF0F0 != 0); // Z (sic — set when NONzero)
                self.regs.commit_dest(result);
                self.regs.reset_prefix_state();
                1
            }
            // --- AND/BIC family (n=1..15) -------------------------------
            0x71..=0x7F => {
                let n = op & 0x0F;
                let rs = self.regs.src();
                let (result, cost) = match (alt1, alt2) {
                    (false, false) => (rs & self.regs.reg(n), 1),
                    (true, false) => (rs & !self.regs.reg(n), 2),
                    (false, true) => (rs & u16::from(n), 2),
                    (true, true) => (rs & !u16::from(n), 2),
                };
                self.regs.set_zs(result);
                self.regs.commit_dest(result);
                self.regs.reset_prefix_state();
                cost
            }

            // --- MULT/UMULT family ---------------------------------------
            0x80..=0x8F => {
                let n = op & 0x0F;
                let rs_lsb = (self.regs.src() as u8) as i32;
                let rs_lsb_u = i32::from(self.regs.src() as u8);
                let (result, cost) = match (alt1, alt2) {
                    (false, false) => {
                        let rn = i32::from((self.regs.reg(n) as u8) as i8);
                        (((rs_lsb as i8 as i32) * rn) as i16 as u16, 1)
                    }
                    (true, false) => {
                        let rn = i32::from(self.regs.reg(n) as u8);
                        ((rs_lsb_u * rn) as u16, 2)
                    }
                    (false, true) => {
                        let rn = i32::from(n);
                        (((rs_lsb as i8 as i32) * rn) as i16 as u16, 2)
                    }
                    (true, true) => {
                        let rn = i32::from(n);
                        ((rs_lsb_u * rn) as u16, 2)
                    }
                };
                self.regs.set_zs(result);
                self.regs.commit_dest(result);
                self.regs.reset_prefix_state();
                cost
            }

            // --- SBK / LINK / SEX / ASR / ROR / JMP / LOB / FMULT ------
            0x90 => {
                let addr = self.regs.last_ram_addr;
                let v = self.regs.src();
                self.ram_write_word(addr, v);
                self.regs.reset_prefix_state();
                2
            }
            0x91..=0x94 => {
                // LINK #n: R11 = R15 + n. `regs[15]` already points at the
                // next opcode (fetch advanced it), matching fullsnes's
                // "sets R11 to addr of next opcode after MOV" note for the
                // analogous MOV R13,R15 idiom.
                let n = u16::from(op - 0x90);
                let target = self.regs.regs[15].wrapping_add(n);
                self.regs.commit_to(11, target);
                self.regs.reset_prefix_state();
                1
            }
            0x95 => {
                let v = ((self.regs.src() as u8) as i8) as i16 as u16;
                self.regs.set_zs(v);
                self.regs.commit_dest(v);
                self.regs.reset_prefix_state();
                1
            }
            0x96 => {
                let rs = self.regs.src() as i16;
                let cy = rs & 1 != 0;
                let (result, cost) = if alt1 || alt3 {
                    // DIV2: fullsnes "Rd=Rs SAR 1, Rd=0 if Rs=-1".
                    if rs == -1 {
                        (0u16, 2)
                    } else {
                        ((rs >> 1) as u16, 2)
                    }
                } else {
                    ((rs >> 1) as u16, 1)
                };
                self.regs.set_cy(cy);
                self.regs.set_zs(result);
                self.regs.commit_dest(result);
                self.regs.reset_prefix_state();
                cost
            }
            0x97 => {
                let rs = self.regs.src();
                let cy_in = self.regs.cy_flag();
                let cy_out = rs & 1 != 0;
                let result = (rs >> 1) | if cy_in { 0x8000 } else { 0 };
                self.regs.set_cy(cy_out);
                self.regs.set_zs(result);
                self.regs.commit_dest(result);
                self.regs.reset_prefix_state();
                1
            }
            0x98..=0x9D => {
                let n = op & 0x0F;
                let cost = if alt1 || alt3 {
                    // LJMP Rn: fullsnes "jmp Rn:Rs ;R15=Rs, PBR=Rn".
                    let new_pbr = self.regs.reg(n) as u8;
                    let new_pc = self.regs.src();
                    self.regs.pending_jump = Some((new_pbr, new_pc));
                    2
                } else {
                    let new_pc = self.regs.reg(n);
                    self.regs.pending_jump = Some((self.regs.pbr, new_pc));
                    1
                };
                self.regs.reset_prefix_state();
                cost
            }
            0x9E => {
                // LOB: Rd=Rs AND FFh, but SF is bit7 of the BYTE result
                // (never bit15, since the AND already clears it).
                let result = self.regs.src() & 0x00FF;
                self.regs.set_flag_bit(0x0008, result & 0x0080 != 0);
                self.regs.set_z(result);
                self.regs.commit_dest(result);
                self.regs.reset_prefix_state();
                1
            }
            0x9F => {
                let rs = i32::from(self.regs.src() as i16);
                let r6 = i32::from(self.regs.reg(6) as i16);
                let product = rs * r6;
                let high = (product >> 16) as u16;
                let cost = if alt1 || alt3 {
                    // LMULT: Rd:R4 = full 32-bit product.
                    self.regs.commit_to(4, product as u16);
                    2
                } else {
                    // FMULT: fullsnes warns Dreg=R4 misbehaves on real
                    // hardware ("this will reportedly leave R4
                    // unchanged"); not specially replicated here — no
                    // known title in this project's library is documented
                    // to rely on that erratum, and doing so would make an
                    // ordinary FMULT into R4 silently wrong for everyone
                    // else.
                    1
                };
                self.regs.commit_dest(high);
                self.regs.set_cy(false); // undocumented; see module notes
                self.regs.set_zs(high);
                self.regs.reset_prefix_state();
                cost
            }

            // --- IBT Rn,#pp / ALT1: LMS Rn,(kk) --------------------------
            // Base: IBT Rn,#pp. ALT1 (mirrored by ALT3, no explicit ALT3
            // form documented): LMS Rn,(kk) — a *load*. ALT2: SMS (kk),Rn
            // — a *store*, easy to miss since it shares ALT1's "extra
            // byte" shape but reads Sreg/writes RAM instead of the
            // reverse (fullsnes "GSU MOV Opcodes (Load/Store Byte/Word to/
            // from RAM)": "3D An kk LMS Rn,(yy)" vs "3E An kk SMS (yy),Rn").
            0xA0..=0xAF => {
                let n = op & 0x0F;
                let cost = if alt2 && !alt1 {
                    let kk = self.fetch_byte(rom);
                    let addr = u16::from(kk) * 2;
                    // SMS's data register is `Rn` (the opcode nibble
                    // itself), not Sreg — fullsnes "mov [ramb:kk*2],Rn".
                    self.ram_write_word(addr, self.regs.reg(n));
                    self.regs.last_ram_addr = addr;
                    8
                } else if alt1 || alt3 {
                    let kk = self.fetch_byte(rom);
                    let addr = u16::from(kk) * 2;
                    let v = self.ram_read_word(addr);
                    self.regs.commit_to(n, v);
                    self.regs.last_ram_addr = addr;
                    10
                } else {
                    let pp = self.fetch_byte(rom);
                    let v = (pp as i8) as i16 as u16;
                    self.regs.commit_to(n, v);
                    2
                };
                self.regs.reset_prefix_state();
                cost
            }

            // --- FROM Rn (prefix) / MOVES Rd,Rs (if B set) --------------
            0xB0..=0xBF => {
                let n = op & 0x0F;
                if self.regs.b_flag {
                    let v = self.regs.reg(self.regs.sreg);
                    self.regs.set_ov(v & 0x0080 != 0); // fullsnes "OV=bit7"
                    self.regs.set_zs(v);
                    self.regs.commit_to(n, v);
                    self.regs.reset_prefix_state();
                    2
                } else {
                    self.regs.sreg = n;
                    1
                }
            }
            0xC0 => {
                // HIB: Rd = Rs SHR 8; SF is bit7 of that (already <=0xFF).
                let result = self.regs.src() >> 8;
                self.regs.set_flag_bit(0x0008, result & 0x0080 != 0);
                self.regs.set_z(result);
                self.regs.commit_dest(result);
                self.regs.reset_prefix_state();
                1
            }
            // --- OR/XOR family (n=1..15; XOR is fullsnes's documented-
            // undocumented pair, "GSU Undoc opcodes") ---------------------
            0xC1..=0xCF => {
                let n = op & 0x0F;
                let rs = self.regs.src();
                let (result, cost) = match (alt1, alt2) {
                    (false, false) => (rs | self.regs.reg(n), 1),
                    (true, false) => (rs ^ self.regs.reg(n), 2),
                    (false, true) => (rs | u16::from(n), 2),
                    (true, true) => (rs ^ u16::from(n), 2),
                };
                self.regs.set_zs(result);
                self.regs.commit_dest(result);
                self.regs.reset_prefix_state();
                cost
            }
            // --- INC Rn (n=0..14) / GETC / ALT2: RAMB / ALT3: ROMB ------
            0xD0..=0xDE => {
                let n = op - 0xD0;
                let v = self.regs.reg(n).wrapping_add(1);
                self.regs.set_zs(v);
                self.regs.commit_to(n, v);
                self.regs.reset_prefix_state();
                1
            }
            0xDF => {
                let cost = if alt3 {
                    self.regs.rombr = self.regs.src() as u8;
                    2
                } else if alt2 {
                    self.regs.rambr = (self.regs.src() as u8) & 0x01;
                    2
                } else {
                    // GETC (ALT1 has no documented form, so it falls back
                    // to base per fullsnes's "ignored prefixes" rule).
                    self.regs.colr = self.rom_data_byte(rom);
                    2
                };
                self.regs.reset_prefix_state();
                cost
            }
            // --- DEC Rn (n=0..14) / GETB family --------------------------
            0xE0..=0xEE => {
                let n = op - 0xE0;
                let v = self.regs.reg(n).wrapping_sub(1);
                self.regs.set_zs(v);
                self.regs.commit_to(n, v);
                self.regs.reset_prefix_state();
                1
            }
            0xEF => {
                let byte = self.rom_data_byte(rom);
                let cur = self.regs.reg(self.regs.dreg);
                let v = match (alt1, alt2) {
                    (false, false) => u16::from(byte), // GETB: zero-expand
                    (true, false) => (cur & 0x00FF) | (u16::from(byte) << 8), // GETBH
                    (false, true) => (cur & 0xFF00) | u16::from(byte), // GETBL
                    (true, true) => (byte as i8) as i16 as u16, // GETBS
                };
                self.regs.commit_dest(v);
                self.regs.reset_prefix_state();
                2
            }
            // --- IWT Rn,#yyxx / ALT1: LM Rn,(hilo) / ALT2: SM (hilo),Rn --
            0xF0..=0xFF => {
                let n = op & 0x0F;
                let cost = if alt2 && !alt1 {
                    let lo = self.fetch_byte(rom);
                    let hi = self.fetch_byte(rom);
                    let addr = u16::from_le_bytes([lo, hi]);
                    // SM's data register is `Rn`, not Sreg — same as SMS.
                    self.ram_write_word(addr, self.regs.reg(n));
                    self.regs.last_ram_addr = addr;
                    9
                } else if alt1 || alt3 {
                    let lo = self.fetch_byte(rom);
                    let hi = self.fetch_byte(rom);
                    let addr = u16::from_le_bytes([lo, hi]);
                    let v = self.ram_read_word(addr);
                    self.regs.commit_to(n, v);
                    self.regs.last_ram_addr = addr;
                    11
                } else {
                    let xx = self.fetch_byte(rom);
                    let yy = self.fetch_byte(rom);
                    let v = u16::from_le_bytes([xx, yy]);
                    self.regs.commit_to(n, v);
                    3
                };
                self.regs.reset_prefix_state();
                cost
            }
        }
    }

    /// STOP: fullsnes "GSU Special Opcodes" — "STOP at $+0 does prefetch
    /// another opcode byte at $+1 (but without executing it), and does
    /// then stop with R15=$+2, SFR.GO=0, SFR.IRQ=1 (that, even if IRQ is
    /// disabled in CFGR.IRQ)." `regs[15]` already advanced past STOP's own
    /// byte by the time this runs (fetch does that), so fetching exactly
    /// one more byte here (discarded, never executed) lands R15 at `$+2`
    /// as documented. The MC1/GSU1 "STOP after a RAM write hangs" erratum
    /// is not modelled — no cycle-accurate bus state exists this slice to
    /// detect it.
    fn exec_stop(&mut self, rom: &[u8]) -> u32 {
        let _prefetched = self.fetch_byte(rom);
        self.regs.sfr &= !0x0020; // GO=0
        self.regs.sfr |= 0x8000; // IRQ=1, unconditionally (see `irq_pending`'s doc)
        self.regs.reset_prefix_state();
        1
    }

    /// The eleven `Bxx` branch opcodes (`$05`-`$0F`): fetch the signed
    /// displacement, test the documented condition against the CURRENT
    /// flags (branches never touch prefix/flag state themselves), and —
    /// if taken — set [`Gsu::pending_jump`] rather than writing `R15`
    /// directly, since fullsnes's delay-slot rule ("the next BYTE after
    /// the jump opcode is fetched...and is executed before continuing at
    /// the jump destination") applies to branches exactly like it does to
    /// JMP/LJMP/Dreg=R15. Prefix state (Sreg/Dreg/ALT1/ALT2/B) is
    /// deliberately left untouched — fullsnes "GSU Prefix Opcodes": "Bxx
    /// addr...branch opcodes (no change)".
    fn exec_branch(&mut self, rom: &[u8], op: u8) -> u32 {
        let disp = self.fetch_byte(rom) as i8;
        let taken = match op {
            0x05 => true,                                      // BRA
            0x06 => self.regs.s_flag() == self.regs.ov_flag(), // BGE: (S XOR V)=0
            0x07 => self.regs.s_flag() != self.regs.ov_flag(), // BLT: (S XOR V)=1
            0x08 => !self.regs.z_flag(),                       // BNE
            0x09 => self.regs.z_flag(),                        // BEQ
            0x0A => !self.regs.s_flag(),                       // BPL
            0x0B => self.regs.s_flag(),                        // BMI
            0x0C => !self.regs.cy_flag(),                      // BCC
            0x0D => self.regs.cy_flag(),                       // BCS
            0x0E => !self.regs.ov_flag(),                      // BVC
            0x0F => self.regs.ov_flag(),                       // BVS
            _ => unreachable!("exec_branch called with non-branch opcode {op:#04x}"),
        };
        if taken {
            let target = self.regs.regs[15].wrapping_add_signed(i16::from(disp));
            self.regs.pending_jump = Some((self.regs.pbr, target));
        }
        2
    }
}

#[cfg(test)]
mod exec_tests {
    use super::*;

    /// A 64 KiB fake cart ROM (bank `$00`) holding `program` at offset 0,
    /// plus a fresh [`GsuState`] with 8 KiB of GSU RAM, `PBR=0`, `R15=0`.
    /// Ticket W18-02 acceptance #2: "a hand-assembled GSU program (bytes
    /// written by the test, no game code)".
    fn program(bytes: &[u8]) -> (GsuState, Vec<u8>) {
        let mut rom = vec![0u8; 0x1_0000];
        rom[..bytes.len()].copy_from_slice(bytes);
        let g = GsuState::new(rf_cart::SuperFxVersion::Gsu2, rom.len(), 8);
        (g, rom)
    }

    /// Execute exactly `n` opcodes, bypassing the GO/budget gate
    /// [`GsuState::run`] uses — the per-opcode-group tests below want
    /// precise control over how many instructions have executed, not the
    /// SNES-visible GO dance (which [`stop_sets_go_false_and_raises_irq`]
    /// and [`loop_link_and_jmp_program`] exercise separately).
    fn step_n(g: &mut GsuState, rom: &[u8], n: usize) {
        for _ in 0..n {
            g.step_one(rom);
        }
    }

    // --- MOV opcodes (fullsnes "SNES Cart GSU-n CPU MOV Opcodes") -------

    #[test]
    fn mov_move_moves_ibt_iwt() {
        // WITH R3; MOVE R2,R3 (0x23,0x12); WITH R2; MOVES R5,R2 (0x22,0xB5);
        // IBT R4,#-2 (0xA4,0xFE); IWT R6,#1234h (0xF6,0x34,0x12).
        let (mut g, rom) = program(&[0x23, 0x12, 0x22, 0xB5, 0xA4, 0xFE, 0xF6, 0x34, 0x12]);
        g.regs.regs[3] = 0x1234;
        g.regs.regs[2] = 0x8000; // bit7 set, for MOVES's OV=bit7 check
        step_n(&mut g, &rom, 2); // WITH R3; MOVE R2,R3
        assert_eq!(g.regs.reg(2), 0x1234);
        assert!(!g.regs.b_flag, "MOVE resets prefix state");
        step_n(&mut g, &rom, 2); // WITH R2; MOVES R5,R2
        assert_eq!(g.regs.reg(5), 0x1234); // R2 was overwritten above
        assert!(!g.regs.ov_flag(), "bit7 of 0x1234 is 0");
        step_n(&mut g, &rom, 1); // IBT R4,#-2
        assert_eq!(g.regs.reg(4), 0xFFFE, "sign-extended");
        step_n(&mut g, &rom, 1); // IWT R6,#1234h
        assert_eq!(g.regs.reg(6), 0x1234);
    }

    #[test]
    fn getb_family_reads_rom_via_r14() {
        // IWT R14,#0100h; TO R0; GETB (0xEF); TO R1; GETBH (0x3D,0xEF);
        // TO R2; GETBL (0x3E,0xEF); TO R3; GETBS (0x3F,0xEF).
        let mut bytes = vec![
            0xF0 + 14,
            0x00,
            0x01, // IWT R14,#0100h
            0x10,
            0xEF, // TO R0; GETB
            0x11,
            0x3D,
            0xEF, // TO R1; GETBH
            0x12,
            0x3E,
            0xEF, // TO R2; GETBL
            0x13,
            0x3F,
            0xEF, // TO R3; GETBS
        ];
        bytes.resize(0x101, 0);
        bytes[0x100] = 0x80; // the byte GETxx will read: sign bit set
        let (mut g, rom) = program(&bytes);
        g.regs.regs[1] = 0x00FF; // GETBH must preserve the low byte
        g.regs.regs[2] = 0xFF00; // GETBL must preserve the high byte
        step_n(&mut g, &rom, 1); // IWT R14,#0100h (self-contained: 1 opcode)
        step_n(&mut g, &rom, 2); // TO R0; GETB
        assert_eq!(g.regs.reg(0), 0x0080, "GETB zero-expands");
        step_n(&mut g, &rom, 3); // TO R1; ALT1; GETBH
        assert_eq!(g.regs.reg(1), 0x80FF, "GETBH keeps R1.lo");
        step_n(&mut g, &rom, 3); // TO R2; ALT2; GETBL
        assert_eq!(g.regs.reg(2), 0xFF80, "GETBL keeps R2.hi");
        step_n(&mut g, &rom, 3); // TO R3; ALT3; GETBS
        assert_eq!(g.regs.reg(3), 0xFF80, "GETBS sign-expands");
    }

    #[test]
    fn ram_load_store_and_banks() {
        // STW (R1) with Sreg=R2 (WITH R2 then FROM... actually STW takes
        // Sreg only): WITH R2 (sets Sreg=Dreg=R2) then STW (R1) (0x31);
        // LDW (R1) into R3 (TO R3; LDW (R1)); STB (R1) from R4 (ALT1);
        // LDB (R1) into R5 (ALT1); RAMB #1 then ROMB #2; SBK writes R6 to
        // the last RAM address touched (R1).
        let bytes = [
            0x22, 0x31, // WITH R2; STW (R1)   ;RAM[R1]=R2
            0x13, 0x41, // TO R3; LDW (R1)     ;R3=RAM[R1]
            0x24, 0x3D, 0x31, // WITH R4; STB (R1)   ;RAM[R1].lo=R4.lo
            0x15, 0x3D, 0x41, // TO R5; LDB (R1)     ;R5=zero-ext(RAM[R1])
            0x26, 0x90, // WITH R6; SBK        ;RAM[last]=R6
        ];
        let (mut g, rom) = program(&bytes);
        g.regs.regs[1] = 0x0010; // RAM address (even, in bank $70)
        g.regs.regs[2] = 0xBEEF;
        g.regs.regs[4] = 0x00AA;
        g.regs.regs[6] = 0xCAFE;
        step_n(&mut g, &rom, 2); // WITH R2; STW (R1)
        assert_eq!(g.ram[0x10], 0xEF);
        assert_eq!(g.ram[0x11], 0xBE);
        step_n(&mut g, &rom, 2); // TO R3; LDW (R1)
        assert_eq!(g.regs.reg(3), 0xBEEF);
        step_n(&mut g, &rom, 3); // WITH R4; ALT1; STB (R1)
        assert_eq!(g.ram[0x10], 0xAA, "STB writes only the low byte");
        step_n(&mut g, &rom, 3); // TO R5; ALT1; LDB (R1)
        assert_eq!(g.regs.reg(5), 0x00AA, "LDB zero-expands");
        step_n(&mut g, &rom, 2); // WITH R6; SBK
        assert_eq!(g.ram[0x10], 0xFE);
        assert_eq!(g.ram[0x11], 0xCA, "SBK writes to the last RAM address");
    }

    #[test]
    fn lm_lms_sm_sms_and_ram_banks() {
        // LM R2,(0x0020) (0x3D,0xF2,lo,hi); SM (0x0020),R3 (0x3E,0xF3,lo,hi);
        // LMS R4,(0x10) -> addr 0x20 (0x3D,0xA4,0x10);
        // SMS (0x10),R5 -> addr 0x20 (0x3E,0xA5,0x10); RAMB #1; ROMB #2.
        let bytes = [
            0x3D, 0xF2, 0x20, 0x00, // LM R2,(0020h)
            0x22, 0x3E, 0xF3, 0x20, 0x00, // WITH R3; SM (0020h),R3
            0x3D, 0xA4, 0x10, // LMS R4,(10h) -> addr 20h
            0x25, 0x3E, 0xA5, 0x10, // WITH R5; SMS (10h),R5
            0x26, 0x3E, 0xDF, // WITH R6; RAMB
            0x27, 0x3F, 0xDF, // WITH R7; ROMB
        ];
        let (mut g, rom) = program(&bytes);
        g.ram[0x20] = 0x34;
        g.ram[0x21] = 0x12;
        g.regs.regs[3] = 0x5678;
        g.regs.regs[5] = 0x9ABC;
        g.regs.regs[6] = 0x01; // RAMB argument
        g.regs.regs[7] = 0x02; // ROMB argument
        step_n(&mut g, &rom, 2); // ALT1; LM R2,(0020h)  (2 opcodes: ALT1, then Fn which self-reads lo/hi)
        assert_eq!(g.regs.reg(2), 0x1234, "LM read-back");
        step_n(&mut g, &rom, 3); // WITH R3; ALT2; SM (0020h),R3
        assert_eq!(g.ram[0x20], 0x78);
        assert_eq!(g.ram[0x21], 0x56, "SM wrote R3");
        step_n(&mut g, &rom, 2); // ALT1; LMS R4,(10h) -> addr 0x20
        assert_eq!(g.regs.reg(4), 0x5678, "LMS re-reads whatever SM left");
        step_n(&mut g, &rom, 3); // WITH R5; ALT2; SMS (10h),R5
        assert_eq!(g.ram[0x20], 0xBC);
        assert_eq!(g.ram[0x21], 0x9A, "SMS wrote R5");
        step_n(&mut g, &rom, 3); // WITH R6; ALT2; RAMB
        assert_eq!(g.regs.rambr, 0x01);
        step_n(&mut g, &rom, 3); // WITH R7; ALT3; ROMB
        assert_eq!(g.regs.rombr, 0x02);
    }

    #[test]
    fn cmode_color_getc_plot_rpix_stubs() {
        // WITH R1; COLOR (0x4E); WITH R2; CMODE (0x3D,0x4E); GETC (0xDF);
        // PLOT (0x4C); RPIX into R3 (TO R3; ALT1 RPIX = 0x3D,0x4C).
        let mut bytes = vec![
            0x21, 0x4E, // WITH R1; COLOR
            0x22, 0x3D, 0x4E, // WITH R2; CMODE
            0xDF, // GETC
            0x4C, // PLOT
            0x13, 0x3D, 0x4C, // TO R3; RPIX
        ];
        bytes.resize(0x101, 0);
        bytes[0x100] = 0x77; // GETC's source byte
        let (mut g, rom) = program(&bytes);
        g.regs.regs[1] = 0x00AB;
        g.regs.regs[2] = 0x1F; // CMODE masks to 5 bits
        g.regs.regs[14] = 0x0100; // R14 for GETC
        g.regs.regs[1] = 0x00AB; // (X coordinate, re-set after COLOR reads it)
        step_n(&mut g, &rom, 2); // WITH R1; COLOR
        assert_eq!(g.regs.colr, 0xAB);
        step_n(&mut g, &rom, 3); // WITH R2; ALT1; CMODE
        assert_eq!(g.regs.por, 0x1F);
        step_n(&mut g, &rom, 1); // GETC
        assert_eq!(g.regs.colr, 0x77, "GETC overwrote COLR from ROM");
        let x_before = g.regs.reg(1);
        step_n(&mut g, &rom, 1); // PLOT
        assert_eq!(g.regs.plot_calls, 1);
        assert_eq!(g.regs.reg(1), x_before.wrapping_add(1), "PLOT advances X");
        step_n(&mut g, &rom, 3); // TO R3; ALT1; RPIX
        assert_eq!(g.regs.rpix_calls, 1);
        assert_eq!(g.regs.reg(3), 0, "RPIX is a stub this slice");
    }

    // --- ALU opcodes (fullsnes "SNES Cart GSU-n CPU ALU Opcodes") -------

    #[test]
    fn add_family_flags_and_variants() {
        // WITH R1; ADD R2 (0x50+2); ADC R2 (ALT1); WITH R1; ADD #5 (ALT2);
        // ADC #5 (ALT3).
        let bytes = [
            0x21, 0x52, // WITH R1; ADD R2
            0x21, 0x3D, 0x52, // WITH R1; ADC R2
            0x21, 0x3E, 0x55, // WITH R1; ADD #5
            0x21, 0x3F, 0x55, // WITH R1; ADC #5
        ];
        let (mut g, rom) = program(&bytes);
        g.regs.regs[1] = 0xFFFF; // -1
        g.regs.regs[2] = 0x0001;
        step_n(&mut g, &rom, 2); // ADD R2: -1+1=0, CY set (unsigned carry out)
        assert_eq!(g.regs.reg(1), 0);
        assert!(g.regs.z_flag());
        assert!(g.regs.cy_flag());
        assert!(!g.regs.ov_flag());
        g.regs.regs[1] = 0x7FFF; // max positive
        g.regs.regs[2] = 0x0001;
        step_n(&mut g, &rom, 3); // WITH R1; ADC R2 (Cy is set from above)
        assert_eq!(g.regs.reg(1), 0x8001, "0x7FFF+1+1(carry-in)");
        assert!(g.regs.ov_flag(), "signed overflow into negative");
        g.regs.regs[1] = 0x0010;
        step_n(&mut g, &rom, 3); // WITH R1; ADD #5
        assert_eq!(g.regs.reg(1), 0x0015);
        step_n(&mut g, &rom, 3); // WITH R1; ADC #5 (Cy currently clear)
        assert_eq!(g.regs.reg(1), 0x001A);
    }

    #[test]
    fn sub_family_sbc_cmp() {
        let bytes = [
            0x21, 0x62, // WITH R1; SUB R2
            0x21, 0x3D, 0x62, // WITH R1; SBC R2
            0x21, 0x3E, 0x65, // WITH R1; SUB #5
            0x21, 0x3F, 0x62, // WITH R1; CMP R2
        ];
        let (mut g, rom) = program(&bytes);
        g.regs.regs[1] = 0x0005;
        g.regs.regs[2] = 0x0003;
        step_n(&mut g, &rom, 2); // SUB R2: 5-3=2, no borrow -> CY=1
        assert_eq!(g.regs.reg(1), 2);
        assert!(g.regs.cy_flag());
        step_n(&mut g, &rom, 3); // WITH R1; SBC R2 (Cy=1 -> borrow_in=0)
        assert_eq!(g.regs.reg(1), 2u16.wrapping_sub(3)); // 2-3-0
        assert!(!g.regs.cy_flag(), "2<3: borrow occurred");
        g.regs.regs[1] = 0x0010;
        step_n(&mut g, &rom, 3); // WITH R1; SUB #5
        assert_eq!(g.regs.reg(1), 0x000B);
        let before = g.regs.reg(1);
        step_n(&mut g, &rom, 3); // WITH R1; CMP R2 (R2=3)
        assert_eq!(g.regs.reg(1), before, "CMP never writes Dreg");
    }

    #[test]
    fn and_bic_or_xor_not() {
        let bytes = [
            0x21, 0x71, // WITH R1; AND R1  (n must be 1..15; use FROM+dest below)
        ];
        // AND/BIC/OR/XOR need n in 1..15 as the SOURCE-of-Rn register
        // selector, independent of Sreg/Dreg — build a fresh program per
        // check instead of reusing register 1 for both roles.
        let _ = bytes;
        let bytes2 = [
            0x22, 0x71, // WITH R2; AND R1     ;R2 = R2 AND R1
            0x22, 0x3D, 0x71, // WITH R2; BIC R1     ;R2 = R2 AND NOT R1
            0x22, 0x3E, 0x7F, // WITH R2; AND #15
            0x22, 0x3F, 0x7F, // WITH R2; BIC #15
            0x23, 0xC1, // WITH R3; OR R1
            0x23, 0x3D, 0xC1, // WITH R3; XOR R1
            0x23, 0x3E, 0xCF, // WITH R3; OR #15
            0x23, 0x3F, 0xCF, // WITH R3; XOR #15
            0x24, 0x4F, // WITH R4; NOT
        ];
        let (mut g, rom) = program(&bytes2);
        g.regs.regs[1] = 0x00F0;
        g.regs.regs[2] = 0x00FF;
        step_n(&mut g, &rom, 2); // AND R1
        assert_eq!(g.regs.reg(2), 0x00F0);
        g.regs.regs[2] = 0x00FF;
        step_n(&mut g, &rom, 3); // BIC R1
        assert_eq!(g.regs.reg(2), 0x000F);
        g.regs.regs[2] = 0x00FF;
        step_n(&mut g, &rom, 3); // AND #15
        assert_eq!(g.regs.reg(2), 0x000F);
        g.regs.regs[2] = 0x00FF;
        step_n(&mut g, &rom, 3); // BIC #15
        assert_eq!(g.regs.reg(2), 0x00F0);
        g.regs.regs[3] = 0x0F00;
        step_n(&mut g, &rom, 2); // OR R1
        assert_eq!(g.regs.reg(3), 0x0FF0);
        g.regs.regs[3] = 0x0F00;
        step_n(&mut g, &rom, 3); // XOR R1
        assert_eq!(g.regs.reg(3), 0x0FF0);
        g.regs.regs[3] = 0x0F00;
        step_n(&mut g, &rom, 3); // OR #15
        assert_eq!(g.regs.reg(3), 0x0F0F);
        g.regs.regs[3] = 0x0F00;
        step_n(&mut g, &rom, 3); // XOR #15
        assert_eq!(g.regs.reg(3), 0x0F0F);
        g.regs.regs[4] = 0x00FF;
        step_n(&mut g, &rom, 2); // NOT
        assert_eq!(g.regs.reg(4), 0xFF00);
    }

    #[test]
    fn shift_rotate_and_div2() {
        let bytes = [
            0x21, 0x03, // WITH R1; LSR
            0x21, 0x04, // WITH R1; ROL
            0x21, 0x96, // WITH R1; ASR
            0x21, 0x97, // WITH R1; ROR
            0x21, 0x3D, 0x96, // WITH R1; DIV2
        ];
        let (mut g, rom) = program(&bytes);
        g.regs.regs[1] = 0x8003;
        step_n(&mut g, &rom, 2); // LSR
        assert_eq!(g.regs.reg(1), 0x4001);
        assert!(g.regs.cy_flag(), "bit0 of 0x8003 was 1");
        g.regs.regs[1] = 0x8000;
        step_n(&mut g, &rom, 2); // ROL (Cy currently set from above)
        assert_eq!(g.regs.reg(1), 0x0001, "0x8000<<1 | old Cy(1)");
        assert!(g.regs.cy_flag(), "bit15 of 0x8000 was 1");
        g.regs.regs[1] = 0x8001;
        step_n(&mut g, &rom, 2); // ASR: arithmetic, sign-preserving
        assert_eq!(g.regs.reg(1), 0xC000);
        g.regs.regs[1] = 0x0001;
        g.regs.sfr &= !0x0004; // clear CY explicitly before this check
        step_n(&mut g, &rom, 2); // ROR
        assert_eq!(g.regs.reg(1), 0x0000);
        assert!(g.regs.cy_flag(), "bit0 of 0x0001 was 1");
        g.regs.regs[1] = 0xFFFF; // -1
        step_n(&mut g, &rom, 3); // DIV2: Rd=0 when Rs=-1
        assert_eq!(g.regs.reg(1), 0);
    }

    #[test]
    fn inc_dec_swap_sex_lob_hib_merge() {
        let bytes = [
            0xD3, // INC R3
            0xE3, // DEC R3
            0x21, 0x4D, // WITH R1; SWAP
            0x21, 0x95, // WITH R1; SEX
            0x21, 0x9E, // WITH R1; LOB
            0x21, 0xC0, // WITH R1; HIB
            0x70, // MERGE (fixed R7,R8 -> Dreg=R0 default)
        ];
        let (mut g, rom) = program(&bytes);
        g.regs.regs[3] = 5;
        step_n(&mut g, &rom, 1); // INC R3
        assert_eq!(g.regs.reg(3), 6);
        step_n(&mut g, &rom, 1); // DEC R3
        assert_eq!(g.regs.reg(3), 5);
        g.regs.regs[1] = 0x1234;
        step_n(&mut g, &rom, 2); // SWAP
        assert_eq!(g.regs.reg(1), 0x3412);
        g.regs.regs[1] = 0x00FF;
        step_n(&mut g, &rom, 2); // SEX
        assert_eq!(g.regs.reg(1), 0xFFFF);
        g.regs.regs[1] = 0xAB80;
        step_n(&mut g, &rom, 2); // LOB
        assert_eq!(g.regs.reg(1), 0x0080);
        assert!(g.regs.s_flag(), "LOB's SF is bit7 of the byte result");
        g.regs.regs[1] = 0x80AB;
        step_n(&mut g, &rom, 2); // HIB
        assert_eq!(g.regs.reg(1), 0x0080);
        g.regs.regs[7] = 0xABCD;
        g.regs.regs[8] = 0x1234;
        step_n(&mut g, &rom, 1); // MERGE -> Rd (R0) = R7.hi:R8.hi = ABh,12h
        assert_eq!(g.regs.reg(0), 0xAB12);
    }

    #[test]
    fn multiply_family() {
        let bytes = [
            0x21, 0x82, // WITH R1; MULT R2
            0x21, 0x3D, 0x82, // WITH R1; UMULT R2
            0x21, 0x3E, 0x85, // WITH R1; MULT #5
            0x21, 0x3F, 0x85, // WITH R1; UMULT #5
            0x21, 0x9F, // WITH R1; FMULT (Rd=R1, must not clobber R4)
            0x21, 0x3D, 0x9F, // WITH R1; LMULT
        ];
        let (mut g, rom) = program(&bytes);
        g.regs.regs[1] = 0x00FE; // -2 as i8
        g.regs.regs[2] = 0x0003;
        step_n(&mut g, &rom, 2); // MULT R2: signed(-2*3) = -6
        assert_eq!(g.regs.reg(1), 0xFFFA);
        g.regs.regs[1] = 0x00FE; // 254 unsigned
        step_n(&mut g, &rom, 3); // UMULT R2: unsigned(254*3)=762
        assert_eq!(g.regs.reg(1), 762);
        g.regs.regs[1] = 0x00FE;
        step_n(&mut g, &rom, 3); // MULT #5: signed(-2*5) = -10
        assert_eq!(g.regs.reg(1), 0xFFF6);
        g.regs.regs[1] = 0x00FE;
        step_n(&mut g, &rom, 3); // UMULT #5: 254*5=1270
        assert_eq!(g.regs.reg(1), 1270);
        g.regs.regs[1] = 100;
        g.regs.regs[6] = 200;
        g.regs.regs[4] = 0xDEAD;
        step_n(&mut g, &rom, 2); // FMULT: Rd=high16(100*200)=high16(20000)=0
        assert_eq!(g.regs.reg(1), 0);
        g.regs.regs[1] = 100; // FMULT above overwrote R1(Sreg); restore it
        step_n(&mut g, &rom, 3); // LMULT: Rd:R4 = 20000 = 0x4E20
        assert_eq!(g.regs.reg(1), 0x0000);
        assert_eq!(g.regs.reg(4), 0x4E20);
    }

    // --- JMP/Prefix opcodes (fullsnes "CPU JMP and Prefix Opcodes") -----

    #[test]
    fn branches_take_the_delay_slot_instruction_first() {
        // BEQ +2 (0x09,0x02) lands past a 1-byte NOP fragment onto an
        // INC R5 at the branch target; the byte right after the branch
        // (INC R1) must still execute BEFORE the jump takes effect —
        // fullsnes "Jump Notes": "the next BYTE after the jump opcode is
        // fetched...and is executed before continuing at the jump-target
        // address."
        //   0: BEQ +2   (09 02)
        //   2: INC R1   (D1)   <- delay slot, always executes
        //   3: NOP      (01)   <- skipped when branch is taken
        //   4: INC R5   (D5)   <- branch target
        let bytes = [0x09, 0x02, 0xD1, 0x01, 0xD5];
        let (mut g, rom) = program(&bytes);
        g.regs.sfr |= 0x0002; // Z=1, so BEQ is taken
        step_n(&mut g, &rom, 1); // BEQ: sets pending_jump, doesn't move R15 yet
        assert_eq!(g.regs.reg(15), 2, "R15 already past the branch+disp");
        step_n(&mut g, &rom, 1); // delay slot: INC R1
        assert_eq!(g.regs.reg(1), 1);
        assert_eq!(
            g.regs.reg(15),
            4,
            "pending jump applied after the delay slot"
        );
        step_n(&mut g, &rom, 1); // INC R5 at the branch target
        assert_eq!(g.regs.reg(5), 1);
    }

    #[test]
    fn jmp_and_ljmp() {
        // JMP R8 (0x98); NOP delay slot; INC R2 would be skipped.
        let bytes = [0x98, 0x01, 0xD2, 0xD3, 0xD3]; // target=4: INC R3 twice? just one
        let (mut g, rom) = program(&bytes);
        g.regs.regs[8] = 0x0004;
        step_n(&mut g, &rom, 2); // JMP R8; delay slot NOP
        assert_eq!(g.regs.reg(15), 4);
        step_n(&mut g, &rom, 1);
        assert_eq!(g.regs.reg(3), 1, "landed on the JMP target");

        // LJMP R9: PBR=Rn's value, R15=Sreg's value.
        let bytes2 = [0x21, 0x3D, 0x98, 0x01]; // WITH R1; LJMP R8
        let (mut g2, rom2) = program(&bytes2);
        g2.regs.regs[1] = 0x1234; // Sreg (=R1 via WITH) supplies R15
        g2.regs.regs[8] = 0x02; // Rn supplies the new PBR
        step_n(&mut g2, &rom2, 3); // WITH R1; ALT1; LJMP R8
        step_n(&mut g2, &rom2, 1); // delay slot
        assert_eq!(g2.regs.pbr, 0x02);
        assert_eq!(
            g2.regs.reg(15),
            0x1234,
            "pending jump overwrites R15 with the target verbatim"
        );
    }

    #[test]
    fn loop_link_and_jmp_program() {
        // R11=LINK target; R12=loop counter; R13=loop address.
        //   0: LINK #1         (91)         ; R11 = 2 (addr of next opcode)
        //   1: INC R0          (D0)         ; loop body, address 1 -> LOOP target
        //   2: LOOP            (3C)         ; R12-=1, if !=0 jump to R13(=1)
        //   3: JMP R11         (98+11=A3? no JMP n=8..13 only)
        // Use JMP via a register in range 8..13: put target in R11 isn't
        // reachable by JMP (n=8..13), so read it back through R10 instead.
        let bytes = [
            0x91, // LINK #1        ; R11 = addr of next opcode (1)
            0xD0, // INC R0         ; loop body            <- addr 1
            0x3C, // LOOP           ; addr 2
            0x01, // NOP            ; delay slot for LOOP's jump, addr 3
            0xD5, // INC R5         ; falls through when LOOP stops looping
        ];
        let (mut g, rom) = program(&bytes);
        g.regs.regs[12] = 3; // loop 3 times
        g.regs.regs[13] = 1; // loop back to the INC R0 at address 1
        step_n(&mut g, &rom, 1); // LINK #1
        assert_eq!(g.regs.reg(11), 2, "R11 = (addr of next opcode = 1) + n(1)");
        // Run the loop to completion: 3 iterations * (INC + LOOP + delay
        // slot) plus the final fallthrough INC R5. Budgeted generously;
        // determinism/termination is what's under test, so a bound (not
        // an unconditional loop) is used here too (law 8).
        for _ in 0..20 {
            if g.regs.reg(0) == 3 && g.regs.reg(5) == 1 {
                break;
            }
            step_n(&mut g, &rom, 1);
        }
        assert_eq!(g.regs.reg(0), 3, "INC R0 ran once per loop iteration");
        assert_eq!(g.regs.reg(12), 0, "R12 counted down to 0");
        assert_eq!(
            g.regs.reg(5),
            1,
            "fell through once the loop stopped taking"
        );
    }

    #[test]
    fn stop_sets_go_false_and_raises_irq() {
        // A program that sets GO (via the SNES-side R15-MSB-write rule,
        // exactly like a real title's boot sequence), runs to STOP, and
        // is observed from the "SNES side" (Gsu::read) waiting on SFR —
        // ticket W18-02 acceptance #2.
        let bytes = [0x00, 0x01]; // STOP; (dummy byte STOP prefetches)
        let (mut g, rom) = program(&bytes);
        // SNES side: write R15 (both bytes) to set GO and start execution
        // — mirrors a real title poking $301E/$301F.
        g.regs.write(0x301E, 0x00);
        g.regs.write(0x301F, 0x00);
        assert!(
            g.regs.go(),
            "R15.MSB write sets GO (fullsnes, ticket W18-01)"
        );
        g.run(&rom);
        assert!(!g.regs.go(), "STOP cleared GO");
        assert_eq!(g.regs.reg(15), 2, "R15 = STOP's address + 2");
        // "the SNES side... waits on SFR": the IRQ line is still asserted
        // going into this read (checked first — the read itself is what
        // clears it, per fullsnes's "reset on read").
        assert!(g.regs.irq_pending(), "ORed into the 65C816 IRQ line");
        let sfr_hi = g.regs.read(0x3031).unwrap();
        assert_eq!(sfr_hi & 0x80, 0x80, "IRQ bit visible to the SNES");
        // Reading $3031 resets the IRQ bit (fullsnes: "reset on read").
        assert_eq!(g.regs.read(0x3031).unwrap() & 0x80, 0);
        assert!(!g.regs.irq_pending());
    }

    #[test]
    fn cfgr_irq_mask_suppresses_the_cpu_line_but_not_the_sfr_bit() {
        let bytes = [0x00, 0x01]; // STOP
        let (mut g, rom) = program(&bytes);
        g.regs.write(0x3037, 0x80); // CFGR.IRQ = 1 (disable)
        g.regs.write(0x301E, 0x00);
        g.regs.write(0x301F, 0x00);
        g.run(&rom);
        assert!(!g.regs.irq_pending(), "masked from the CPU line");
        assert_ne!(
            g.regs.peek(0x3031).unwrap() & 0x80,
            0,
            "but the SFR bit itself is still set"
        );
    }

    // --- Cross-cutting acceptance: determinism, save/load ---------------

    /// LINK/LOOP/JMP/ALU/MOV mixed program, used by both the determinism
    /// and save/load tests below.
    fn mixed_program() -> Vec<u8> {
        vec![
            0xA1, 0x05, // IBT R1,#5
            0xA2, 0x03, // IBT R2,#3
            0x21, 0x52, // WITH R1; ADD R2      ; R1 = 8
            0xD3, // INC R3
            0x3C, // LOOP (R12/R13 both 0 this run, harmless no-op path)
            0x01, // NOP
            0x21, 0x71, // WITH R1; AND R... (n=1, uses R1 itself)
        ]
    }

    #[test]
    fn determinism_two_runs_match() {
        let bytes = mixed_program();
        let (mut a, rom_a) = program(&bytes);
        let (mut b, rom_b) = program(&bytes);
        step_n(&mut a, &rom_a, 7);
        step_n(&mut b, &rom_b, 7);
        assert_eq!(a.regs.regs, b.regs.regs);
        assert_eq!(a.regs.sfr, b.regs.sfr);
        assert_eq!(a.regs.instructions_executed, b.regs.instructions_executed);
    }

    struct MemStream {
        buf: Vec<u8>,
        at: usize,
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

    #[test]
    fn save_load_mid_program_round_trip() {
        let bytes = mixed_program();
        let (mut g, rom) = program(&bytes);
        step_n(&mut g, &rom, 4); // stop mid-program, after the ADD

        let mut stream = MemStream {
            buf: Vec::new(),
            at: 0,
        };
        g.regs.save(&mut StateOut::new(&mut stream)).expect("save");
        let mut restored = Gsu::new(rf_cart::SuperFxVersion::Gsu2);
        restored.load(&mut StateIn::new(&mut stream)).expect("load");
        assert_eq!(restored.regs, g.regs.regs);
        assert_eq!(restored.sreg, g.regs.sreg);
        assert_eq!(restored.dreg, g.regs.dreg);
        assert_eq!(restored.instructions_executed, g.regs.instructions_executed);

        // Continue BOTH the original and the restored copy for the same
        // remaining instructions; they must end up identical.
        let mut restored_state = GsuState {
            regs: restored,
            ram: g.ram.clone(),
            rom_len: g.rom_len,
            ram_len: g.ram_len,
        };
        step_n(&mut g, &rom, 3);
        step_n(&mut restored_state, &rom, 3);
        assert_eq!(g.regs.regs, restored_state.regs.regs);
        assert_eq!(g.regs.sfr, restored_state.regs.sfr);
        assert_eq!(g.ram, restored_state.ram);
    }
}
