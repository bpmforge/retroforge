//! SA-1 SNES-side register file, board memories, and the second 65C816
//! (tickets W17-01, W17-02, D-013, SA-1 slices 1-2).
//!
//! ## Ownership (ticket W17-02 acceptance #1)
//!
//! [`Sa1State`] owns everything the second CPU needs: its own
//! [`crate::cpu::Cpu`] register file, the shared [`Sa1Regs`] register
//! window, and the board's I-RAM/BW-RAM buffers. [`crate::bus::SnesBus`]
//! owns one `Sa1State` (in `sa1: Option<Sa1State>`) alongside the ROM —
//! there is exactly one copy of I-RAM, BW-RAM, ROM and the register file
//! in the whole machine.
//!
//! The two CPUs reach that one copy through two different views:
//! - The SNES CPU reaches it through [`crate::bus::SnesBus`]'s own
//!   `CpuBus` impl, via [`crate::mapping::sa1_target`] (already built by
//!   W17-01).
//! - The SA-1 CPU reaches it through [`Sa1Bus`], a *borrowing* wrapper
//!   built fresh every [`Sa1State::step`] call from `&self.rom` (owned by
//!   `SnesBus`, passed in) and `&mut self.regs`/`&mut self.iram`/
//!   `&mut self.bwram` (three disjoint fields of the very `Sa1State` whose
//!   method is running) via [`crate::mapping::sa1_side_target`].
//!
//! `Sa1Bus` never allocates or copies a byte: it is three borrowed slices
//! and a `Copy` struct of bank-register values, so the two CPUs sharing
//! memory through one owner is enforced by the borrow checker rather than
//! by convention. `Sa1State::cpu` is a fourth, disjoint field — never part
//! of `Sa1Bus` — which is what lets [`Sa1State::step`] call
//! `self.cpu.interrupt_to_vector(&mut bus, ..)` without the aliasing a
//! bus-owns-the-CPU design would hit (a `CpuBus` impl cannot borrow the
//! very `Cpu` that is about to borrow it mutably).

use rf_core_api::StateError;

use crate::cpu::CpuBus;
use crate::mapping::Target;
use crate::state::{StateIn, StateOut};

/// Offset of the write-only register block's first byte ($2200).
const BASE: u16 = 0x2200;
/// The write-only block spans exactly one page ($2200-$22FF); the
/// read-only block ($2300-$230E) is handled separately by [`Sa1Regs::read`]
/// and is never written here.
const RAW_LEN: usize = 0x100;

/// Register offsets used by this slice's mapping (fullsnes "SNES Cart
/// SA-1 Memory Control").
const CXB: usize = 0x2220 - BASE as usize;
const DXB: usize = 0x2221 - BASE as usize;
const EXB: usize = 0x2222 - BASE as usize;
const FXB: usize = 0x2223 - BASE as usize;
const BMAPS: usize = 0x2224 - BASE as usize;
const BMAP: usize = 0x2225 - BASE as usize;

/// Register offsets used by ticket W17-02's control/message/interrupt
/// wiring (fullsnes "SNES Cart SA-1 Interrupt/Control on SNES/SA-1 Side").
const CCNT: usize = 0x2200 - BASE as usize;
const SIE: usize = 0x2201 - BASE as usize;
const SIC: usize = 0x2202 - BASE as usize;
const CRV_LO: usize = 0x2203 - BASE as usize;
const CNV_LO: usize = 0x2205 - BASE as usize;
const CIV_LO: usize = 0x2207 - BASE as usize;
const SCNT: usize = 0x2209 - BASE as usize;
const CIE: usize = 0x220A - BASE as usize;
const CIC: usize = 0x220B - BASE as usize;
const SNV_LO: usize = 0x220C - BASE as usize;
const SIV_LO: usize = 0x220E - BASE as usize;

/// Register offsets used by this ticket (W17-03), fullsnes "SNES Cart
/// SA-1 Timer", "...Memory Control", "...DMA Transfers", "...Character
/// Conversion", "...Arithmetic Maths" and "...Variable-Length Bit
/// Processing".
const TMC: usize = 0x2210 - BASE as usize;
const CTR: usize = 0x2211 - BASE as usize;
const HCNT_LO: usize = 0x2212 - BASE as usize;
const VCNT_LO: usize = 0x2214 - BASE as usize;
const SBWE_SNES: usize = 0x2226 - BASE as usize;
const SBWE_SA1: usize = 0x2227 - BASE as usize;
const SIWP: usize = 0x2229 - BASE as usize;
const CIWP: usize = 0x222A - BASE as usize;
const BBF: usize = 0x223F - BASE as usize;
const DCNT: usize = 0x2230 - BASE as usize;
const CDMA: usize = 0x2231 - BASE as usize;
const SDA_LO: usize = 0x2232 - BASE as usize;
const DDA_LO: usize = 0x2235 - BASE as usize;
const DDA_MID: usize = 0x2236 - BASE as usize;
const DDA_MSB: usize = 0x2237 - BASE as usize;
const DTC_LO: usize = 0x2238 - BASE as usize;
const BRF_BASE: usize = 0x2240 - BASE as usize;
const MCNT: usize = 0x2250 - BASE as usize;
const MA_LO: usize = 0x2251 - BASE as usize;
const MB_LO: usize = 0x2253 - BASE as usize;
const MB_HI: usize = 0x2254 - BASE as usize;
const VBD: usize = 0x2258 - BASE as usize;
const VDA_LO: usize = 0x2259 - BASE as usize;
const VDA_HI: usize = 0x225B - BASE as usize;

/// The SA-1 SNES-side register window's live state.
///
/// ## Why the write-only block is one raw byte array
///
/// The whole $2200-$22FF write-only block is stored verbatim, offset by
/// address, rather than decoded field-by-field for every register. The
/// bank-select and vector registers this crate needs are read back out of
/// it (`cxb`, `sa1_reset_vector`, ...) rather than duplicated into named
/// fields, so a value written once has exactly one place it lives.
///
/// The control/message/interrupt registers ($2200-$2202, $2209-$220B) are
/// the exception: writing them has side effects beyond "remember this
/// byte" (an edge sets a pending interrupt, a clear-strobe resets one), so
/// [`Sa1Regs::write`] updates a handful of extra latch fields alongside
/// storing the raw byte. Everything else this slice does not interpret
/// (DMA $2230-$2239/$223F, arithmetic $2250-$2254, the variable-length bit
/// reader $2258-$225B, and the undocumented offsets fullsnes notes some
/// titles write anyway) is still stored raw for W17-03/04 to read back.
#[derive(Debug, Clone)]
pub struct Sa1Regs {
    /// Every write-only register, $2200-$22FF, indexed by `offset - $2200`.
    raw: [u8; RAW_LEN],
    /// CFR (`$2301`) bit 4 — NMI-from-SNES status. Persists until acked via
    /// `$220B` bit 4; set (alongside [`Self::nmi_edge_sa1`]) whenever `$2200`
    /// bit 4 is written as 1.
    nmi_status_sa1: bool,
    /// Single-shot: true for exactly one [`Sa1State::step`] call after the
    /// NMI-from-SNES edge, so the SA-1 CPU takes the interrupt once per
    /// edge rather than once per step while the status bit is still
    /// waiting to be acked (mirrors `SnesSystem`'s own `pending_nmi`).
    nmi_edge_sa1: bool,
    /// CFR bit 7 — IRQ-from-SNES status, level, gated by CIE bit 7 at
    /// delivery time. Persists until acked via `$220B` bit 7.
    irq_status_sa1: bool,
    /// CFR bit 5 — IRQ-from-DMA status. Always false this slice (no DMA
    /// unit yet, W17-03); the field exists so `$220B` bit 5's ack and
    /// `$220A` bit 5's enable have somewhere real to act once it does.
    dma_irq_status_sa1: bool,
    /// CFR bit 6 — IRQ-from-Timer status. Always false this slice (no
    /// timer yet, W17-03); see [`Self::dma_irq_status_sa1`].
    timer_irq_status_sa1: bool,
    /// SFR (`$2300`) bit 7 — IRQ-from-SA-1 status. Persists until acked
    /// via `$2202` bit 7.
    irq_status_snes: bool,
    /// SFR bit 5 — character-conversion DMA IRQ status. Always false this
    /// slice (no character conversion yet, W17-03); see
    /// [`Self::dma_irq_status_sa1`].
    chardma_irq_status_snes: bool,

    // --- ticket W17-03 additions ---
    /// Timer: current H/V position. In HV mode this mirrors the real PPU
    /// dot/line (fed in by [`Self::tick_timer`]); in Linear mode it is
    /// this chip's own free-running pair. Also what `$2302-$2305`
    /// HCR/VCR latch from (fullsnes "SNES Cart SA-1 Timer").
    timer_h: u16,
    timer_v: u16,
    /// Linear-mode sub-dot credit: master cycles banked toward the next
    /// tick, at the documented "four 21MHz master cycles per dot" rate
    /// this slice also assumes for Linear mode (fullsnes's own "Notes":
    /// "probably same as in HV-mode").
    timer_credit: u64,
    /// `$2302` HCR read's latch snapshot, read back by `$2303-$2305`.
    latch_h: u16,
    latch_v: u16,
    /// Arithmetic: the 40-bit result register ($2306-$230A), stored in
    /// the low 40 bits. Multiply/divide overwrite it; cumulative sum
    /// accumulates into it. Fullsnes "SNES Cart SA-1 Arithmetic Maths".
    mr: u64,
    /// `$230B` bit 7 — Sum overflow. Fullsnes documents no clear rule for
    /// when this clears; this slice clears it only on a Sum-mode reset
    /// (`$2250` bit 1 write) — see [`Self::write`]'s `MCNT` arm.
    overflow: bool,
    /// Variable-length bit reader: the current bit position into the ROM
    /// image (fullsnes "SNES Cart SA-1 Variable-Length Bit Processing").
    vbr_bitpos: u32,
    /// The high byte of the 16-bit window last computed by a `$230C`
    /// read, so `$230D` (read separately, since the CPU bus is 8-bit at
    /// a time) reports the other half of the SAME window rather than
    /// silently recomputing it — see [`Self::read_mut`]'s doc for why
    /// $230C is the trigger and this slice's reading of "may trigger on
    /// reading 230Ch? or 230Dh?".
    vbr_hi_cache: u8,
    /// The low byte of that same cached window, for `read`/`peek`.
    vbr_lo_cache: u8,
    /// Character-conversion type 1: active once armed by writing `$2236`
    /// with charconv enabled and type=1 (fullsnes "SNES Cart SA-1
    /// Character Conversion"); cleared by `$2231` bit 7 (terminate).
    charconv1_active: bool,
    /// Type 1's current BW-RAM source row offset — advances by the row
    /// width every row, so consecutive rows do not reread the first one.
    charconv1_src: u32,
    /// Character-conversion type 2: armed once `$2236` is written with
    /// charconv enabled, type=2 and DMA-enable set; cleared by `$2230`
    /// bit 7 going low.
    charconv2_armed: bool,
    /// Type 2's current tile row (0-7), advanced one row per `$224F`
    /// write — see [`apply_register_side_effect`]'s doc.
    charconv2_row: u8,
    /// Type 1's current tile row (0-7), advanced once per converted row.
    charconv1_row_ctr: u8,
    /// Ticket W17-04: a write into a `$22xx` offset this project has no
    /// register for — fullsnes's own I/O map table lists the gaps
    /// explicitly (`$2216-$221F`, `$222B-$222F`, `$223A-$223E`,
    /// `$2255-$2257`, `$225C` and up) and separately calls out two of
    /// them as real, observed traffic: "`$2261h` Unknown/Undocumented
    /// (Jumpin Derby writes 00h)" and "`$2262h`... (Super Bomberman
    /// writes 00h)". Diagnostic only (see [`Self::is_known_write_offset`]
    /// and `title_probe`'s `PROBE_SA1REGS`): it does not gate or alter
    /// any write — an unknown offset still lands in `raw` exactly as
    /// every other one does, unchanged from before this ticket — and it
    /// is not part of save state, the same way `Dsp1DrTrace` is a
    /// diagnostic the debugger reads rather than gameplay state.
    /// Offset -> times written, so a probe can tell "one write at boot"
    /// from "every frame" without an unbounded log; bounded by
    /// construction (at most 256 possible `u16` keys, one per `$22xx`
    /// byte).
    pub unknown_write_offsets: std::collections::BTreeMap<u16, u32>,
}

impl Default for Sa1Regs {
    fn default() -> Self {
        Self::new()
    }
}

impl Sa1Regs {
    /// Reset values fullsnes documents ("Reset", under "SNES Cart SA-1
    /// I/O Map"): `$2200`=$20, `$2220-$2223`=$00,$01,$02,$03, `$2228`=$FF;
    /// every other write-only register resets to $00 or is "N/A" (this
    /// slice zeroes those too — a read of them is open bus regardless,
    /// per [`Self::read`]). `$2200`=$20 sets bit 5 (Reset) asserted, which
    /// is what holds the SA-1 CPU out of reset until software clears it —
    /// see [`Self::sa1_reset_asserted`] and `Sa1State::step`.
    #[must_use]
    pub fn new() -> Self {
        let mut raw = [0u8; RAW_LEN];
        raw[CCNT] = 0x20;
        raw[CXB] = 0x00;
        raw[DXB] = 0x01;
        raw[EXB] = 0x02;
        raw[FXB] = 0x03;
        raw[(0x2228 - BASE) as usize] = 0xFF;
        Self {
            raw,
            nmi_status_sa1: false,
            nmi_edge_sa1: false,
            irq_status_sa1: false,
            dma_irq_status_sa1: false,
            timer_irq_status_sa1: false,
            irq_status_snes: false,
            chardma_irq_status_snes: false,
            timer_h: 0,
            timer_v: 0,
            timer_credit: 0,
            latch_h: 0,
            latch_v: 0,
            mr: 0,
            overflow: false,
            vbr_bitpos: 0,
            vbr_hi_cache: 0,
            vbr_lo_cache: 0,
            charconv1_active: false,
            charconv1_src: 0,
            charconv2_armed: false,
            charconv2_row: 0,
            charconv1_row_ctr: 0,
            unknown_write_offsets: std::collections::BTreeMap::new(),
        }
    }

    /// Whether fullsnes's I/O map table names a register at `$22xx`
    /// offset `idx` (`offset - $2200`) — see
    /// [`Self::unknown_write_offsets`]'s doc for the citation and the
    /// exact gaps this leaves out.
    #[must_use]
    fn is_known_write_offset(idx: usize) -> bool {
        matches!(idx,
            0x00..=0x15   // $2200-$2215: control + timer
            | 0x20..=0x2A // $2220-$222A: bank/BW-RAM/I-RAM mapping and protection
            | 0x30..=0x39 // $2230-$2239: DMA control/params
            | 0x3F        // $223F: BBF
            | 0x40..=0x4F // $2240-$224F: BRF
            | 0x50..=0x54 // $2250-$2254: arithmetic
            | 0x58..=0x5B // $2258-$225B: variable-length bit processing
        )
    }

    /// A write into `$2200-$22FF`. Panics if `offset` is outside that
    /// range — callers only reach this through [`crate::mapping::Target::Sa1Register`],
    /// which `bus.rs`/[`Sa1Bus`] restrict to `$2200-$23FF`, and `$2300-$23FF`
    /// writes are dropped by the caller before this is reached.
    ///
    /// Cited to fullsnes "SNES Cart SA-1 Interrupt/Control on SNES Side"
    /// and "...on SA-1 Side" for the four addresses with side effects:
    /// - `$2200` CCNT bit 7 (IRQ) / bit 4 (NMI) — edge "0=No Change?,
    ///   1=Interrupt": a 1 sets the corresponding SA-1-side pending flag;
    ///   a 0 leaves it untouched (never clears it — only `$220B` does).
    /// - `$220B` CIC bits 7/6/5/4 — "0=No change, 1=Clear": acknowledges
    ///   the matching pending flag.
    /// - `$2209` SCNT bit 7 (IRQ) — same edge rule as `$2200` bit 7, for
    ///   the SNES-side pending flag.
    /// - `$2202` SIC bits 7/5 — same ack rule as `$220B`, for the
    ///   SNES-side flags.
    pub fn write(&mut self, offset: u16, value: u8) {
        let idx = usize::from(offset - BASE);
        if !Self::is_known_write_offset(idx) {
            *self.unknown_write_offsets.entry(offset).or_insert(0) += 1;
        }
        self.raw[idx] = value;
        match idx {
            CCNT => {
                if value & 0x80 != 0 {
                    self.irq_status_sa1 = true;
                }
                if value & 0x10 != 0 {
                    self.nmi_status_sa1 = true;
                    self.nmi_edge_sa1 = true;
                }
            }
            CIC => {
                if value & 0x80 != 0 {
                    self.irq_status_sa1 = false;
                }
                if value & 0x10 != 0 {
                    self.nmi_status_sa1 = false;
                    self.nmi_edge_sa1 = false;
                }
                if value & 0x20 != 0 {
                    self.dma_irq_status_sa1 = false;
                }
                if value & 0x40 != 0 {
                    self.timer_irq_status_sa1 = false;
                }
            }
            SCNT => {
                if value & 0x80 != 0 {
                    self.irq_status_snes = true;
                }
            }
            SIC => {
                if value & 0x80 != 0 {
                    self.irq_status_snes = false;
                }
                if value & 0x20 != 0 {
                    self.chardma_irq_status_snes = false;
                }
            }
            // `$2211` CTR — "writing any value restarts the timer at 0"
            // (fullsnes "SNES Cart SA-1 Timer").
            CTR => {
                self.timer_h = 0;
                self.timer_v = 0;
                self.timer_credit = 0;
            }
            // `$2250` MCNT bit 1 — "Writing Bit1=1 does reset the Sum...
            // to zero" (fullsnes "...Arithmetic Maths"). Bit 1 is set for
            // mode values 2 (MultiplySum) and 3 (Reserved); this slice
            // applies the rule literally, to both.
            MCNT => {
                if value & 0x02 != 0 {
                    self.mr = 0;
                    self.overflow = false;
                }
            }
            // `$2254` MB Msb — "Writing to 2254h starts the operation"
            // (fullsnes). Executed immediately; see
            // [`Self::run_arithmetic`]'s doc for the timing approximation.
            MB_HI => self.run_arithmetic(),
            // `$225B` VDA Msb & Kick — "Reading starts on writing to
            // 225Bh"; preload is `bitpos = address * 8` (fullsnes's own
            // pseudocode under "...Variable-Length Bit Processing").
            VDA_HI => {
                let addr = u32::from(self.raw[VDA_LO])
                    | u32::from(self.raw[VDA_LO + 1]) << 8
                    | u32::from(value) << 16;
                self.vbr_bitpos = addr.wrapping_mul(8);
            }
            // `$2231` CDMA bit 7 — "Terminate Character Conversion 1"
            // (fullsnes "...DMA Transfers"): stops the type-1 handshake
            // this slice drives from `$2236`/`$2202` — see
            // [`apply_register_side_effect`].
            CDMA => {
                if value & 0x80 != 0 {
                    self.charconv1_active = false;
                }
            }
            // `$2230` DCNT bit 7 clearing — "disable DMA" also disarms
            // type 2's `$224F` handshake (fullsnes Character Conversion
            // 2's own last step: "Set DCNT.Bit7=0 - disable DMA").
            DCNT => {
                if value & 0x80 == 0 {
                    self.charconv2_armed = false;
                }
            }
            _ => {}
        }
    }

    /// A read of `$2200-$23FF`. `None` means open bus: the write-only
    /// block itself ($2200-$22FF, real hardware never reads it back
    /// either — every port in fullsnes's table is marked "(W)" only) and
    /// any offset this slice's read-only table doesn't name.
    ///
    /// `$2300` SFR and `$2301` CFR are computed live from the control and
    /// interrupt state (ticket W17-02). `$2303-$230B` (the latched H/V
    /// counter, and the arithmetic result/overflow) have no read side
    /// effect and are served here; `$2302` (HCR, which latches) and
    /// `$230C`/`$230D` (VDP, the bit reader, which auto-increments and
    /// needs the ROM) are **not** — see [`Self::read_mut`], which every
    /// caller must use instead for the full `$2300-$230E` range. This
    /// `read` stays side-effect-free by construction so `peek`
    /// (debugging) can call it directly.
    #[must_use]
    pub fn read(&self, offset: u16) -> Option<u8> {
        match offset {
            0x2300 => Some(self.sfr()),
            0x2301 => Some(self.cfr()),
            // `$2302` HCR — mutating (latches); a non-mutating `peek`
            // reports the last-latched value instead, since re-fetching
            // it is what the mutation exists to prevent an observer from
            // doing accidentally.
            0x2302 => Some(self.latch_h as u8),
            0x2303 => Some((self.latch_h >> 8) as u8),
            0x2304 => Some(self.latch_v as u8),
            0x2305 => Some((self.latch_v >> 8) as u8),
            // `$230C`/`$230D` VDP — see [`Self::read_mut`]; the plain,
            // non-mutating `read` reports the last-computed window
            // without recomputing or incrementing (used by `peek`).
            0x230C => Some(self.vbr_lo_cache),
            0x230D => Some(self.vbr_hi_cache),
            0x2306..=0x230A => Some((self.mr >> (8 * (offset - 0x2306))) as u8),
            0x230B => Some(u8::from(self.overflow) << 7),
            0x230E => Some(0), // chip version; undocumented value, reads 0
            _ => None,
        }
    }

    /// The full `$2300-$230E` read, including `$2302`'s HCR latch and
    /// `$230C`'s VDP auto-increment. `rom` backs the variable-length bit
    /// reader (fullsnes "SNES Cart SA-1 Variable-Length Bit Processing":
    /// the address is "Game Pak ROM" space, originated at `$000000`, per
    /// this slice's reading of that section's own uncertainty).
    #[must_use]
    pub fn read_mut(&mut self, offset: u16, rom: &[u8]) -> Option<u8> {
        match offset {
            0x2302 => {
                self.latch_h = self.timer_h;
                self.latch_v = self.timer_v;
                Some(self.latch_h as u8)
            }
            0x230C => Some(self.vbr_read_lo(rom)),
            _ => self.read(offset),
        }
    }

    /// `$230C`'s low byte, sourcing the window fresh from `rom` and
    /// caching the high half for `$230D` — see [`Self::read_mut`]'s doc
    /// for the ROM-address citation. Auto-increment (`$2258` bit 7) then
    /// advances `vbr_bitpos` by the configured data length (bits 0-3,
    /// `0` meaning 16) — fullsnes's own "INCREMENT" pseudocode, applied
    /// per read of `$230C` rather than per read of either half (chosen
    /// reading of the documented "may trigger on reading 230Ch? or
    /// 230Dh?" ambiguity, so a 16-bit `LDA $230C` reads one window
    /// without a second, unwanted increment from the high half).
    fn vbr_read_lo(&mut self, rom: &[u8]) -> u8 {
        let word = self.vbr_window(rom);
        self.vbr_lo_cache = word as u8;
        self.vbr_hi_cache = (word >> 8) as u8;
        if self.raw[VBD] & 0x80 != 0 {
            let len = match self.raw[VBD] & 0x0F {
                0 => 16,
                n => u32::from(n),
            };
            self.vbr_bitpos = self.vbr_bitpos.wrapping_add(len);
        }
        self.vbr_lo_cache
    }

    /// The 16-bit sliding window at the current bit position: a
    /// byte-granular generalisation of fullsnes's dword-aligned
    /// pseudocode (`[230Ch] = dword[bitpos/16*2] shr (bitpos and 15)`),
    /// which only has to hold for word-aligned starts; this shifts a
    /// 24-bit read (three ROM bytes) by the sub-byte bit offset instead,
    /// so an unaligned `vbr_bitpos` (any of the 1-15 bit lengths this
    /// register supports) still produces a well-defined window rather
    /// than only ever being tested at 16-bit boundaries.
    fn vbr_window(&self, rom: &[u8]) -> u16 {
        let byte_pos = (self.vbr_bitpos / 8) as usize;
        let shift = self.vbr_bitpos % 8;
        let b = |i: usize| rom.get(byte_pos + i).copied().unwrap_or(0);
        let triple = u32::from(b(0)) | u32::from(b(1)) << 8 | u32::from(b(2)) << 16;
        ((triple >> shift) & 0xFFFF) as u16
    }

    /// `$2300` SFR — SNES CPU Flag Read. Cited to fullsnes "SNES Cart SA-1
    /// Interrupt/Control on SNES Side": bits 0-3/4/6 mirror `$2209`
    /// directly (message, and the two vector-select bits, which are plain
    /// level state rather than latched — see [`Self::write`]'s doc); bit 5
    /// (character-conversion DMA IRQ) and bit 7 (IRQ from SA-1) are the
    /// latched flags `$2202` acknowledges.
    fn sfr(&self) -> u8 {
        let scnt = self.raw[SCNT];
        (scnt & 0x0F) // bits 0-3: message from SA-1
            | (scnt & 0x10) // bit 4: NMI vector select
            | (scnt & 0x40) // bit 6: IRQ vector select
            | u8::from(self.chardma_irq_status_snes) << 5
            | u8::from(self.irq_status_snes) << 7
    }

    /// `$2301` CFR — SA-1 CPU Flag Read. Cited to fullsnes "SNES Cart SA-1
    /// Interrupt/Control on SA-1 Side": bits 0-3 mirror `$2200` directly
    /// (message from SNES); bits 4-7 are the latched flags `$220B`
    /// acknowledges (NMI/DMA/Timer/IRQ from SNES).
    fn cfr(&self) -> u8 {
        let ccnt = self.raw[CCNT];
        (ccnt & 0x0F) // bits 0-3: message from SNES
            | u8::from(self.nmi_status_sa1) << 4
            | u8::from(self.dma_irq_status_sa1) << 5
            | u8::from(self.timer_irq_status_sa1) << 6
            | u8::from(self.irq_status_sa1) << 7
    }

    /// Runs the arithmetic unit ($2251-$2254 -> $2306-$230B), fullsnes
    /// "SNES Cart SA-1 Arithmetic Maths". Triggered by writing `$2254`
    /// (see [`Self::write`]'s `MB_HI` arm); this slice implements the
    /// documented "5 cycles (Multiply/Divide), 6 cycles (Multiply/Sum)"
    /// timing as an immediate result rather than a clocked one (ticket's
    /// own allowance) — [`Sa1Bus`]/[`apply_register_side_effect`] never
    /// see the cost, which is this slice's stated approximation.
    ///
    /// `$2251/$2252` MA is the SIGNED multiplicand/dividend; `$2253/$2254`
    /// MB is the SIGNED multiplier or the UNSIGNED divisor. Division by
    /// zero is documented to return `0000h`/`0000h` with no overflow.
    fn run_arithmetic(&mut self) {
        let a = i16::from_le_bytes([self.raw[MA_LO], self.raw[MA_LO + 1]]);
        let b_bits = u16::from_le_bytes([self.raw[MB_LO], self.raw[MB_LO + 1]]);
        match self.raw[MCNT] & 0x03 {
            // Divide: dividend SIGNED, divisor UNSIGNED. Quotient SIGNED,
            // remainder UNSIGNED — this slice picks `rem_euclid` (always
            // non-negative) as the reading of "remainder unsigned",
            // adjusting the quotient to match (`a = q*d + r`), since
            // fullsnes states the polarity but not the rounding rule.
            1 => {
                let (q, r) = if b_bits == 0 {
                    (0i32, 0i32)
                } else {
                    let d = i32::from(b_bits);
                    let a32 = i32::from(a);
                    let r = a32.rem_euclid(d);
                    ((a32 - r) / d, r)
                };
                self.mr = (q as u16 as u64) | (r as u16 as u64) << 16;
                // MA is destroyed after division (fullsnes); MB always.
                self.raw[MA_LO] = 0;
                self.raw[MA_LO + 1] = 0;
                self.raw[MB_LO] = 0;
                self.raw[MB_LO + 1] = 0;
            }
            // MultiplySum: signed 16x16 product accumulated into the
            // 40-bit signed sum; overflow flag set past the 40-bit
            // signed range (fullsnes: "reportedly set on 40bit
            // multiply/addition overflows").
            2 => {
                let product = i64::from(a) * i64::from(b_bits as i16);
                let prior = sign_extend_40(self.mr);
                let sum = prior.wrapping_add(product);
                if !(-(1i64 << 39)..(1i64 << 39)).contains(&sum) {
                    self.overflow = true;
                }
                self.mr = (sum as u64) & 0xFF_FFFF_FFFF;
                self.raw[MB_LO] = 0;
                self.raw[MB_LO + 1] = 0;
            }
            // Multiply (mode 0) and Reserved (mode 3, undocumented —
            // treated the same as Multiply rather than guessed at
            // further): signed 16x16 -> 32, MA kept, MB destroyed.
            _ => {
                let product = i32::from(a) * i32::from(b_bits as i16);
                self.mr = (product as u32 as u64) & 0xFF_FFFF_FFFF;
                self.raw[MB_LO] = 0;
                self.raw[MB_LO + 1] = 0;
            }
        }
    }

    /// Advance the timer (fullsnes "SNES Cart SA-1 Timer"). `ppu_dot`/
    /// `ppu_line` are the real PPU beam position, used directly in HV
    /// mode (`$2210` TMC bit 7 clear) rather than a second simulated
    /// dot-clock — the ticket's own "the timer clock is... equivalent to
    /// the dotclock" note means the two cannot legitimately drift, so
    /// there is nothing a separate model would add. Linear mode (TMC bit
    /// 7 set) has no such external clock to mirror, so it keeps its own
    /// free-running H/V pair here (each 0 to 511), ticked at the same
    /// four-master-cycles-per-dot rate fullsnes guesses also applies
    /// (its own "Notes": "probably same as in HV-mode").
    ///
    /// Bounded per law 8: the `while` only runs while `timer_credit`
    /// (strictly decreasing by 4 each pass) covers one more tick.
    pub fn tick_timer(&mut self, master_cycles: u64, ppu_dot: u16, ppu_line: u16) {
        if self.raw[TMC] & 0x80 != 0 {
            self.timer_credit += master_cycles;
            while self.timer_credit >= 4 {
                self.timer_credit -= 4;
                self.timer_h += 1;
                if self.timer_h > 511 {
                    self.timer_h = 0;
                    self.timer_v = (self.timer_v + 1) & 0x1FF;
                }
                self.check_timer_match();
            }
        } else if self.timer_h != ppu_dot || self.timer_v != ppu_line {
            self.timer_h = ppu_dot;
            self.timer_v = ppu_line;
            self.check_timer_match();
        }
    }

    /// `$2212-$2215` HCNT/VCNT compare against the live H/V position;
    /// `$2210` TMC bits 0/1 (HEN/VEN) gate which axis matters — both
    /// gated fires only on a match on both, per fullsnes's own guess
    /// ("similar as for the normal SNES timers"). Sets the CFR bit 6
    /// (IRQ-from-Timer) status latch [`Self::timer_irq_status_sa1`],
    /// acked by `$220B` bit 6 (already wired, ticket W17-02).
    fn check_timer_match(&mut self) {
        let tmc = self.raw[TMC];
        let hen = tmc & 0x01 != 0;
        let ven = tmc & 0x02 != 0;
        if !hen && !ven {
            return;
        }
        let hcnt = u16::from(self.raw[HCNT_LO]) | u16::from(self.raw[HCNT_LO + 1]) << 8;
        let vcnt = u16::from(self.raw[VCNT_LO]) | u16::from(self.raw[VCNT_LO + 1]) << 8;
        let h_match = self.timer_h == (hcnt & 0x1FF);
        let v_match = self.timer_v == (vcnt & 0x1FF);
        let fire = if hen && ven {
            h_match && v_match
        } else if hen {
            h_match
        } else {
            v_match
        };
        if fire {
            self.timer_irq_status_sa1 = true;
        }
    }

    /// `$2226`/`$2227` SBWE bit 7 (fullsnes "SNES Cart SA-1 Memory
    /// Control") — a SHARED gate, **either** register's bit 7 enables
    /// writes from **both** CPUs, rather than each register gating only
    /// its own side. `$2228` BWPA is stored (and cited) but not enforced
    /// at all: see the ticket report for both corrections and the titles
    /// that forced them.
    ///
    /// This slice went through three readings before this one, each
    /// falsified by a real title:
    /// 1. Per-side AND, with a BWPA floor under it — Kirby Super Star
    ///    writes `$2228=$05` (a floor equal to its whole 8 KiB BW-RAM)
    ///    then `$2226=$80`, expecting the WHOLE buffer writable.
    /// 2. Per-side AND, no floor — Kirby's Dream Land 3 sets `$2226=$80`
    ///    (SNES side) but leaves `$2227=$00` (SA-1 side) at its reset
    ///    value across a reset/reboot, then the SA-1 CPU itself writes
    ///    thousands of BW-RAM bytes (a startup buffer clear at
    ///    `$7000`-ish) that a strict per-side reading blocks — the SA-1
    ///    spins retrying a write that never lands.
    /// 3. This one: either register's enable bit unlocks BOTH sides,
    ///    which is consistent with fullsnes's own "one can probably
    ///    completely disable the protection via ports 2226h/2227h?" (the
    ///    plural, either-register phrasing) and fixes both titles.
    fn bwram_writable(&self, idx: usize) -> bool {
        let _ = idx;
        self.raw[SBWE_SNES] & 0x80 != 0 || self.raw[SBWE_SA1] & 0x80 != 0
    }
    /// `$2226`/`$2227` SBWE — gates BW-RAM writes from the SNES side.
    /// Same shared gate as [`Self::bwram_writable_sa1`] — see
    /// [`Self::bwram_writable`]'s doc.
    #[must_use]
    pub fn bwram_writable_snes(&self, idx: usize) -> bool {
        self.bwram_writable(idx)
    }
    /// `$2226`/`$2227` SBWE — gates BW-RAM writes from the SA-1 side.
    #[must_use]
    pub fn bwram_writable_sa1(&self, idx: usize) -> bool {
        self.bwram_writable(idx)
    }

    /// `$2229`/`$222A` — "Write enable flags for eight 256-byte chunks"
    /// (fullsnes), keyed on the I-RAM *index* (`idx >> 8`) rather than
    /// the raw address, since both SA-1-side I-RAM windows
    /// (`$0000-$07FF` and `$3000-$37FF`) alias onto the same index range
    /// — see [`crate::mapping::sa1_side_target`]'s doc.
    fn iram_chunk_writable(&self, idx: usize, siwp_offset: usize) -> bool {
        let chunk = (idx >> 8) & 0x07;
        self.raw[siwp_offset] & (1 << chunk) != 0
    }
    /// `$2229` SIWP — gates the SNES side's I-RAM writes.
    #[must_use]
    pub fn iram_chunk_writable_snes(&self, idx: usize) -> bool {
        self.iram_chunk_writable(idx, SIWP)
    }
    /// `$222A` CIWP — gates the SA-1 side's I-RAM writes.
    #[must_use]
    pub fn iram_chunk_writable_sa1(&self, idx: usize) -> bool {
        self.iram_chunk_writable(idx, CIWP)
    }

    /// `$223F` BBF bit 7 — **inverted from the obvious guess**:
    /// "0=4bit, 1=2bit" (fullsnes "SNES Cart SA-1 Memory Control").
    #[must_use]
    pub fn bitmap_is_2bpp(&self) -> bool {
        self.raw[BBF] & 0x80 != 0
    }

    /// `$2230` DCNT — whether a Normal-DMA write to the destination's
    /// start-trigger register (`$2236` for I-RAM, `$2237` for BW-RAM;
    /// fullsnes "...DMA Transfers": "transfer starts after writing
    /// 2237h"/"2236h") should actually run: DMA enabled (bit 7), not
    /// character-conversion (bit 5 clear), and the destination bit (2)
    /// matches which trigger register this is. Source/destination device
    /// legality (no I-RAM<->I-RAM or BW-RAM<->BW-RAM) is
    /// [`execute_normal_dma`]'s job, since only it can report "did
    /// nothing" by returning zero cycles.
    fn normal_dma_armed(&self, bwram_dest: bool) -> bool {
        let dcnt = self.raw[DCNT];
        let enabled = dcnt & 0x80 != 0;
        let charconv = dcnt & 0x20 != 0;
        let dest_is_bwram = dcnt & 0x04 != 0;
        enabled && !charconv && dest_is_bwram == bwram_dest
    }

    /// `$2230` DCNT — character-conversion type 1 arms on the same
    /// `$2236` trigger fullsnes's own walkthrough uses ("Set DCNT...Type
    /// 1 (...and no DMA-enable?)" then "Set DDA...=I-RAM offset"): bit 5
    /// (charconv enable) set, bit 4 (type) = 1. Unlike Normal DMA, bit 7
    /// (DMA Enable) is explicitly NOT required per that walkthrough's own
    /// question mark, so it is not checked here.
    fn charconv1_armed(&self) -> bool {
        let dcnt = self.raw[DCNT];
        dcnt & 0x20 != 0 && dcnt & 0x10 != 0
    }

    /// `$2230` DCNT — character-conversion type 2 arms on `$2236` with
    /// bit 5 set, bit 4 clear (type 2), AND bit 7 set (fullsnes: "Set
    /// DCNT...Type 2 AND set DMA-enable").
    fn charconv2_armed_now(&self) -> bool {
        let dcnt = self.raw[DCNT];
        dcnt & 0x80 != 0 && dcnt & 0x20 != 0 && dcnt & 0x10 == 0
    }

    /// `$2231` CDMA bits 0-1 — Color Depth: 0=8bit, 1=4bit, 2=2bit
    /// (3=Reserved, treated as 8bit rather than guessed at further).
    fn charconv_bpp(&self) -> u8 {
        match self.raw[CDMA] & 0x03 {
            1 => 4,
            2 => 2,
            _ => 8,
        }
    }
    /// `$2231` CDMA bits 2-4 — Virtual VRAM Width: 0..5 -> 1,2,4,8,16,32
    /// characters (6/7 Reserved, treated as 32).
    fn charconv_chars_per_line(&self) -> u32 {
        match (self.raw[CDMA] >> 2) & 0x07 {
            0 => 1,
            1 => 2,
            2 => 4,
            3 => 8,
            4 => 16,
            _ => 32,
        }
    }
    /// Bytes per converted tile: `8 * bpp / 8 = bpp` bytes per row, times
    /// 8 rows — matches the documented 16/32/64-byte buffer per tile
    /// (32/64/128 for the documented TWO tiles fullsnes's BRF section
    /// gives).
    fn charconv_tile_bytes(&self) -> usize {
        usize::from(self.charconv_bpp()) * 8
    }

    /// `$2232-$2234` SDA — DMA Source Device Start Address (24 bits).
    fn sda(&self) -> u32 {
        u32::from(self.raw[SDA_LO])
            | u32::from(self.raw[SDA_LO + 1]) << 8
            | u32::from(self.raw[SDA_LO + 2]) << 16
    }
    /// `$2235/$2236` (I-RAM dest) or `$2235-$2237` (BW-RAM dest) DDA.
    fn dda(&self) -> u32 {
        u32::from(self.raw[DDA_LO])
            | u32::from(self.raw[DDA_MID]) << 8
            | u32::from(self.raw[DDA_MSB]) << 16
    }
    /// `$2238/$2239` DTC — DMA Transfer Length in bytes (`0` is
    /// documented "Reserved/unknown"; this slice treats it as "nothing to
    /// do" rather than as 65536).
    fn dtc(&self) -> usize {
        usize::from(self.raw[DTC_LO]) | usize::from(self.raw[DTC_LO + 1]) << 8
    }

    /// Type 1's current tile row (0-7).
    fn charconv1_row(&self) -> u8 {
        self.charconv1_row_ctr
    }
    fn advance_charconv1_row(&mut self) {
        self.charconv1_row_ctr = (self.charconv1_row_ctr + 1) % 8;
    }
    fn reset_charconv1_row(&mut self) {
        self.charconv1_row_ctr = 0;
    }

    /// `$2240-$224F` BRF, masked to the current color depth (fullsnes
    /// "...DMA Transfers": "0-1 2bit pixel (bit 2-7=unused)" etc — the
    /// unused high bits are dropped here rather than trusted from
    /// whatever the CPU happened to write).
    fn brf(&self, offset: usize) -> u8 {
        self.raw[BRF_BASE + offset] & bpp_mask(self.charconv_bpp())
    }

    /// `$2220` CXB — HiROM `$C0-$CF` / LoROM `$00-$1F`.
    #[must_use]
    pub fn cxb(&self) -> u8 {
        self.raw[CXB]
    }
    /// `$2221` DXB — HiROM `$D0-$DF` / LoROM `$20-$3F`.
    #[must_use]
    pub fn dxb(&self) -> u8 {
        self.raw[DXB]
    }
    /// `$2222` EXB — HiROM `$E0-$EF` / LoROM `$80-$9F`.
    #[must_use]
    pub fn exb(&self) -> u8 {
        self.raw[EXB]
    }
    /// `$2223` FXB — HiROM `$F0-$FF` / LoROM `$A0-$BF`.
    #[must_use]
    pub fn fxb(&self) -> u8 {
        self.raw[FXB]
    }
    /// `$2224` BMAPS — the SNES-side `$6000-$7FFF` BW-RAM block select.
    #[must_use]
    pub fn bmaps(&self) -> u8 {
        self.raw[BMAPS]
    }
    /// `$2225` BMAP — the SA-1-side `$6000-$7FFF` BW-RAM block select
    /// (ticket W17-02).
    #[must_use]
    pub fn bmap(&self) -> u8 {
        self.raw[BMAP]
    }

    /// `$2200` CCNT bit 5 — "Reset from SNES to SA-1". Level: the SA-1
    /// core stays held (and re-fetches its reset vector the instant this
    /// clears) for as long as it reads asserted.
    #[must_use]
    pub fn sa1_reset_asserted(&self) -> bool {
        self.raw[CCNT] & 0x20 != 0
    }
    /// `$2200` CCNT bit 6 — "Wait from SNES to SA-1". This slice's chosen
    /// reading of fullsnes's "Unknown if Wait freezes the whole SA1" note:
    /// the CPU simply does not step while it reads asserted, keeping its
    /// register file (unlike Reset, which reinitialises it).
    #[must_use]
    pub fn sa1_wait_asserted(&self) -> bool {
        self.raw[CCNT] & 0x40 != 0
    }

    /// `$220A` CIE bit 4 — NMI-from-SNES enable.
    #[must_use]
    pub fn sa1_nmi_enabled(&self) -> bool {
        self.raw[CIE] & 0x10 != 0
    }
    /// `$220A` CIE bit 7 — IRQ-from-SNES enable, combined with the latched
    /// status bit CFR bit 7 reads: an IRQ is "pending for delivery" only
    /// while both are true, per fullsnes's own note that a masked
    /// interrupt still sets its flag but is only actually taken once
    /// enabled.
    #[must_use]
    pub fn sa1_irq_pending(&self) -> bool {
        self.irq_status_sa1 && self.raw[CIE] & 0x80 != 0
    }
    /// Consume the NMI-from-SNES edge: true at most once per `$2200` bit-4
    /// write, regardless of how many steps pass before the SA-1 gets
    /// around to running (it may be held in Wait). Does not touch the CFR
    /// status bit — that is [`Self::write`]'s (`$220B`) job.
    pub fn take_sa1_nmi_edge(&mut self) -> bool {
        std::mem::take(&mut self.nmi_edge_sa1)
    }

    /// `$2201` SIE bit 7 — IRQ-from-SA-1 enable, combined with SFR bit 7's
    /// latched status the same way [`Self::sa1_irq_pending`] combines
    /// CIE/CFR.
    #[must_use]
    pub fn snes_irq_pending(&self) -> bool {
        self.irq_status_snes && self.raw[SIE] & 0x80 != 0
    }

    /// `$2203`/`$2204` CRV — SA-1 CPU Reset Vector. Fetched by
    /// `Sa1State::step` on every reset-to-running transition; per fullsnes
    /// this ALWAYS overrides the ROM vector, unconditionally (unlike the
    /// SNES-side overrides below, which are optional).
    #[must_use]
    pub fn sa1_reset_vector(&self) -> u16 {
        u16::from(self.raw[CRV_LO]) | u16::from(self.raw[CRV_LO + 1]) << 8
    }
    /// `$2205`/`$2206` CNV — SA-1 CPU NMI Vector (always active).
    #[must_use]
    pub fn sa1_nmi_vector(&self) -> u16 {
        u16::from(self.raw[CNV_LO]) | u16::from(self.raw[CNV_LO + 1]) << 8
    }
    /// `$2207`/`$2208` CIV — SA-1 CPU IRQ Vector (always active).
    #[must_use]
    pub fn sa1_irq_vector(&self) -> u16 {
        u16::from(self.raw[CIV_LO]) | u16::from(self.raw[CIV_LO + 1]) << 8
    }

    /// `$2209` SCNT bit 4 — "NMI Vector for SNES (0=ROM FFEAh/FFFAh,
    /// 1=Port 220Ch)". Unlike the SA-1 side's vectors, this is a
    /// selection bit: the SNES's own NMI (from vblank) still causes the
    /// interrupt, this only redirects where it lands.
    #[must_use]
    pub fn snes_nmi_vector_override(&self) -> Option<u16> {
        if self.raw[SCNT] & 0x10 != 0 {
            Some(u16::from(self.raw[SNV_LO]) | u16::from(self.raw[SNV_LO + 1]) << 8)
        } else {
            None
        }
    }
    /// `$2209` SCNT bit 6 — "IRQ Vector for SNES (0=ROM FFEEh/FFFEh,
    /// 1=Port 220Eh)". See [`Self::snes_nmi_vector_override`].
    #[must_use]
    pub fn snes_irq_vector_override(&self) -> Option<u16> {
        if self.raw[SCNT] & 0x40 != 0 {
            Some(u16::from(self.raw[SIV_LO]) | u16::from(self.raw[SIV_LO + 1]) << 8)
        } else {
            None
        }
    }

    pub(crate) fn save(&self, o: &mut StateOut) -> Result<(), StateError> {
        o.bytes(&self.raw)?;
        o.bool(self.nmi_status_sa1)?;
        o.bool(self.nmi_edge_sa1)?;
        o.bool(self.irq_status_sa1)?;
        o.bool(self.dma_irq_status_sa1)?;
        o.bool(self.timer_irq_status_sa1)?;
        o.bool(self.irq_status_snes)?;
        o.bool(self.chardma_irq_status_snes)?;
        // Ticket W17-03 additions.
        o.u16(self.timer_h)?;
        o.u16(self.timer_v)?;
        o.u64(self.timer_credit)?;
        o.u16(self.latch_h)?;
        o.u16(self.latch_v)?;
        o.u64(self.mr)?;
        o.bool(self.overflow)?;
        o.u32(self.vbr_bitpos)?;
        o.u8(self.vbr_hi_cache)?;
        o.u8(self.vbr_lo_cache)?;
        o.bool(self.charconv1_active)?;
        o.u32(self.charconv1_src)?;
        o.bool(self.charconv2_armed)?;
        o.u8(self.charconv2_row)?;
        o.u8(self.charconv1_row_ctr)
    }

    pub(crate) fn load(&mut self, i: &mut StateIn) -> Result<(), StateError> {
        i.fill(&mut self.raw)?;
        self.nmi_status_sa1 = i.bool()?;
        self.nmi_edge_sa1 = i.bool()?;
        self.irq_status_sa1 = i.bool()?;
        self.dma_irq_status_sa1 = i.bool()?;
        self.timer_irq_status_sa1 = i.bool()?;
        self.irq_status_snes = i.bool()?;
        self.chardma_irq_status_snes = i.bool()?;
        self.timer_h = i.u16()?;
        self.timer_v = i.u16()?;
        self.timer_credit = i.u64()?;
        self.latch_h = i.u16()?;
        self.latch_v = i.u16()?;
        self.mr = i.u64()?;
        self.overflow = i.bool()?;
        self.vbr_bitpos = i.u32()?;
        self.vbr_hi_cache = i.u8()?;
        self.vbr_lo_cache = i.u8()?;
        self.charconv1_active = i.bool()?;
        self.charconv1_src = i.u32()?;
        self.charconv2_armed = i.bool()?;
        self.charconv2_row = i.u8()?;
        self.charconv1_row_ctr = i.u8()?;
        Ok(())
    }
}

/// Sign-extend the low 40 bits of `v` to a full `i64`, for the
/// cumulative-sum accumulator ($2306-$230A, fullsnes "SNES Cart SA-1
/// Arithmetic Maths").
fn sign_extend_40(v: u64) -> i64 {
    let v = v & 0xFF_FFFF_FFFF;
    if v & (1 << 39) != 0 {
        (v | !0xFF_FFFF_FFFFu64) as i64
    } else {
        v as i64
    }
}

/// Writes one pixel into a tile buffer already in SNES bitplane layout
/// (fullsnes's documented 16/32/64-byte-per-tile sizes for 2/4/8bpp: `p`
/// bits, planes stored as 16-byte pairs — `(bp0,bp1)` for rows 0-7, then
/// `(bp2,bp3)`, then `(bp4,bp5)`, then `(bp6,bp7)`). Shared by both
/// character-conversion types (type 1: packed BW-RAM source; type 2:
/// unpacked BRF source) since the destination format is identical.
fn write_tile_pixel(tile: &mut [u8], bpp: u8, x: u8, y: u8, pixel: u8) {
    for p in 0..bpp {
        let bit = (pixel >> p) & 1;
        let half = usize::from(p / 2) * 16;
        let byte_in_pair = usize::from(p % 2);
        let idx = half + usize::from(y) * 2 + byte_in_pair;
        let Some(slot) = tile.get_mut(idx) else {
            continue;
        };
        let mask = 0x80u8 >> x;
        if bit != 0 {
            *slot |= mask;
        } else {
            *slot &= !mask;
        }
    }
}

/// Unpack one `bpp`-bit pixel at pixel-index `n` from a packed BW-RAM
/// bitmap row (fullsnes "SNES Cart SA-1 Character Conversion", Type 1:
/// "Packed Pixels, Bitmap Pixel Array" — LSBs hold the left-most pixel,
/// same packing [`crate::mapping::Target::Sa1Bitmap`]'s projection uses).
fn unpack_bitmap_pixel(bwram: &[u8], byte_offset: usize, bpp: u8, n: u32) -> u8 {
    let per_byte = 8 / u32::from(bpp);
    let byte_idx = byte_offset + (n / per_byte) as usize;
    let sub = n % per_byte;
    let byte = bwram.get(byte_idx).copied().unwrap_or(0);
    (byte >> (sub * u32::from(bpp))) & bpp_mask(bpp)
}

/// The low-`bpp`-bits mask (`bpp` is 2, 4 or 8) — a plain `(1u8 << bpp) -
/// 1` overflows for 8bpp, since `1u8 << 8` is out of range.
fn bpp_mask(bpp: u8) -> u8 {
    if bpp >= 8 {
        0xFF
    } else {
        (1u8 << bpp) - 1
    }
}

/// Normal DMA — fullsnes "SNES Cart SA-1 DMA Transfers": ROM/BW-RAM/I-RAM
/// to I-RAM/BW-RAM, source != destination device. Bounded by `dtc()`
/// (read once into `len` before the loop — law 8): a title that sets a
/// huge count gets a huge but finite copy, never a hang.
///
/// Returns the master cycles the transfer cost, always charged to the
/// SA-1's own credit by the caller ([`apply_register_side_effect`]) —
/// never to the main CPU. Rule: `$2230` DCNT and `$2238/$2239` DTC are
/// documented "(W)" as **SA-1**-only registers, and nothing in fullsnes's
/// DMA section says the main CPU is stalled by a Normal DMA (the "SNES
/// CPU is paused" sentence is about the SNES-side `$43xx` DMA during
/// Character Conversion 1, a different mechanism) — so this slice never
/// charges `master_cycles`. `$2230` bit 6 (DMA Priority, "only valid for
/// Normal DMA between BW-RAM and I-RAM") is read but has no effect in
/// this model: the SA-1 is stalled for the whole transfer either way,
/// since interleaving the SA-1's own instruction stream with an
/// in-flight DMA is not modelled this slice.
///
/// Address translation is a stated simplification: fullsnes's own
/// "Unknown details" note says SDA/DDA's increment behaviour isn't
/// documented, and the 24-bit ROM address is only "translated... via
/// 2220h-2223h" in the sense that titles point it at the HiROM-mapped
/// image (`$C00000` and up); this treats SDA as a plain linear index into
/// each device's buffer (mod its length) rather than re-deriving a
/// bank/offset pair through the CXB/DXB/EXB/FXB registers.
fn execute_normal_dma(regs: &mut Sa1Regs, rom: &[u8], iram: &mut [u8], bwram: &mut [u8]) -> u64 {
    let dcnt = regs.raw[DCNT];
    let source = dcnt & 0x03;
    let dest_is_bwram = dcnt & 0x04 != 0;
    // "Source and Destination may not be the same devices."
    let invalid = matches!((source, dest_is_bwram), (1, true) | (2, false));
    if invalid {
        return 0;
    }
    let len = regs.dtc();
    if len == 0 {
        return 0;
    }
    let src_base = regs.sda() as usize;
    let dst_base = regs.dda() as usize;
    let per_byte_cost: u64 = if source == 0 && !dest_is_bwram { 2 } else { 4 };
    for i in 0..len {
        let byte = match source {
            0 => {
                if rom.is_empty() {
                    0
                } else {
                    rom[(src_base + i) % rom.len()]
                }
            }
            1 => {
                if bwram.is_empty() {
                    0
                } else {
                    bwram[(src_base + i) % bwram.len()]
                }
            }
            _ => {
                if iram.is_empty() {
                    0
                } else {
                    iram[(src_base + i) % iram.len()]
                }
            }
        };
        if dest_is_bwram {
            if !bwram.is_empty() {
                let idx = (dst_base + i) % bwram.len();
                if regs.bwram_writable_sa1(idx) {
                    bwram[idx] = byte;
                }
            }
        } else if !iram.is_empty() {
            let idx = (dst_base + i) % iram.len();
            if regs.iram_chunk_writable_sa1(idx) {
                iram[idx] = byte;
            }
        }
    }
    // CFR bit 5 (IRQ from DMA), fullsnes "...triggered by DMA-finished".
    regs.dma_irq_status_sa1 = true;
    len as u64 * per_byte_cost
}

/// Character Conversion type 1 — converts one 8-pixel-tall tile ROW
/// across `chars_per_line` tiles from a packed BW-RAM bitmap into the
/// I-RAM tile buffer, then raises SFR bit 5 (fullsnes "...Character
/// Conversion", Type 1: "Wait for SFR.Bit5... first character
/// available"). One row per call — driven once from the `$2236` arm
/// trigger and once more per `$2202` SIC ack (see
/// [`apply_register_side_effect`]) — never a loop over the whole
/// "endless" transfer fullsnes describes, per law 8.
fn execute_charconv_type1_row(regs: &mut Sa1Regs, iram: &mut [u8], bwram: &[u8]) -> u64 {
    let bpp = regs.charconv_bpp();
    let chars = regs.charconv_chars_per_line();
    let tile_bytes = regs.charconv_tile_bytes();
    let dst_base = regs.dda() as usize;
    let row_width_pixels = chars * 8;
    let row_bytes = ((row_width_pixels * u32::from(bpp)) / 8) as usize;
    let src_byte = regs.sda() as usize + regs.charconv1_src as usize;
    for tile in 0..chars {
        for x in 0..8u32 {
            let n = tile * 8 + x;
            let pixel = unpack_bitmap_pixel(bwram, src_byte, bpp, n);
            let tile_off = dst_base + (tile as usize) * tile_bytes;
            if tile_off + tile_bytes <= iram.len() {
                write_tile_pixel(
                    &mut iram[tile_off..tile_off + tile_bytes],
                    bpp,
                    x as u8,
                    regs.charconv1_row(),
                    pixel,
                );
            }
        }
    }
    regs.charconv1_src = regs.charconv1_src.wrapping_add(row_bytes as u32);
    regs.advance_charconv1_row();
    regs.charconv1_active = true;
    // SFR bit 5, fullsnes "...triggered by... first character available".
    regs.chardma_irq_status_snes = true;
    u64::from(row_bytes as u32) * 4 // BW-RAM read, 5.37MHz rate
}

/// Character Conversion type 2 — one call converts the current row (0-7,
/// [`Sa1Regs::charconv2_row`]) of `$2240-$224F` (two 8-pixel unpacked
/// rows, one per tile of the documented pair) into the I-RAM tile buffer,
/// fullsnes "...Character Conversion", Type 2: "for y=0 to 7... On SNES
/// side: Transfer DMA from 1st/2nd I-RAM buffer half". Triggered once per
/// `$224F` write ([`apply_register_side_effect`]) — one row per call,
/// never a loop over all 8.
fn execute_charconv_type2_row(regs: &mut Sa1Regs, iram: &mut [u8]) -> u64 {
    let bpp = regs.charconv_bpp();
    let tile_bytes = regs.charconv_tile_bytes();
    let dst_base = regs.dda() as usize;
    let y = regs.charconv2_row;
    for tile in 0..2usize {
        let tile_off = dst_base + tile * tile_bytes;
        if tile_off + tile_bytes > iram.len() {
            continue;
        }
        for x in 0..8usize {
            let brf_offset = tile * 8 + x;
            let pixel = regs.brf(brf_offset);
            write_tile_pixel(
                &mut iram[tile_off..tile_off + tile_bytes],
                bpp,
                x as u8,
                y,
                pixel,
            );
        }
    }
    regs.charconv2_row = (regs.charconv2_row + 1) % 8;
    2 // internal-cycle approximation: BRF is a CPU-fed register, not a bus device
}

/// The side effect of writing a register whose action needs the board's
/// buffers (DMA/character-conversion, ticket W17-03) — split out of
/// [`Sa1Regs::write`] because that method only has `&mut self`, not the
/// I-RAM/BW-RAM/ROM this needs. Returns the master cycles the action
/// cost, `0` for a plain register write with no such side effect.
fn apply_register_side_effect(
    regs: &mut Sa1Regs,
    offset: u16,
    value: u8,
    rom: &[u8],
    iram: &mut [u8],
    bwram: &mut [u8],
) -> u64 {
    match offset {
        // `$2236` — Normal DMA to I-RAM, or Character Conversion's DDA
        // (both types target I-RAM; fullsnes marks $2237 "unused" for an
        // I-RAM destination, so this is the one trigger both mechanisms
        // share, disambiguated by DCNT).
        0x2236 => {
            if regs.normal_dma_armed(false) {
                execute_normal_dma(regs, rom, iram, bwram)
            } else if regs.charconv1_armed() {
                regs.charconv1_src = 0;
                regs.reset_charconv1_row();
                execute_charconv_type1_row(regs, iram, bwram)
            } else if regs.charconv2_armed_now() {
                regs.charconv2_armed = true;
                regs.charconv2_row = 0;
                0
            } else {
                0
            }
        }
        0x2237 if regs.normal_dma_armed(true) => execute_normal_dma(regs, rom, iram, bwram),
        // `$2202` SIC bit 5 ack — advances type 1 to the next row, one
        // row per ack, for as long as it is still active (cleared by
        // `$2231` bit 7 terminate, handled in `Sa1Regs::write`).
        0x2202 if value & 0x20 != 0 && regs.charconv1_active => {
            execute_charconv_type1_row(regs, iram, bwram)
        }
        // `$224F` — the last BRF register: one type-2 row per write.
        0x224F if regs.charconv2_armed => execute_charconv_type2_row(regs, iram),
        _ => 0,
    }
}

/// [`apply_register_side_effect`] for a caller holding a whole
/// [`Sa1State`] rather than its three buffers separately — the shape
/// [`crate::bus::SnesBus::write`] is in (an SNES-side write can legally
/// touch these "Both" registers too). The resulting cycles are charged to
/// [`Sa1State::credit`], never to the caller's own instruction cost — see
/// [`execute_normal_dma`]'s doc for why the main CPU is never charged.
pub(crate) fn handle_register_side_effect(s: &mut Sa1State, offset: u16, value: u8, rom: &[u8]) {
    let cycles =
        apply_register_side_effect(&mut s.regs, offset, value, rom, &mut s.iram, &mut s.bwram);
    s.credit += cycles;
}

/// The SA-1 board's live state: the register file, its two on-board
/// memories, and the second CPU (fullsnes "Misc": 2 KiB I-RAM, up to
/// 2 MiB BW-RAM, 10.74MHz 65C816).
#[derive(Debug, Clone)]
pub struct Sa1State {
    pub regs: Sa1Regs,
    pub iram: Vec<u8>,
    pub bwram: Vec<u8>,
    /// The parsed cartridge's fixed sizes — kept alongside the live
    /// memories so [`crate::mapping::Sa1RomBanks`] can be rebuilt on every
    /// access without recomputing lengths from the `Vec`s (a zero-length
    /// `Vec` and "no BW-RAM" are the same fact stated two ways, but the
    /// board is the one `rf_cart` already computed).
    pub board: rf_cart::Sa1Board,
    /// The second 65C816 (ticket W17-02). A disjoint field from `regs`/
    /// `iram`/`bwram` — see the module doc's ownership section for why
    /// that separation is what makes [`Sa1Bus`] possible at all.
    pub cpu: crate::cpu::Cpu,
    /// Whether the CPU has been (re)initialised since `$2200` bit 5
    /// (Reset) was last asserted. `false` at construction, matching
    /// `Sa1Regs::new`'s reset-asserted default: the CPU is held, not yet
    /// booted, until software clears Reset for the first time.
    pub booted: bool,
    /// Master cycles the SA-1 core is owed and has not yet spent (ticket
    /// W17-02's clocking rule) — see [`crate::system::SnesSystem::step`]'s
    /// catch-up loop. Never allowed to accumulate while the core is held
    /// (Reset or Wait): hardware does not bank cycles it never spent, and
    /// letting the debt grow across many held frames would turn the first
    /// catch-up loop after a long wait into an unbounded one.
    pub credit: u64,
}

impl Sa1State {
    #[must_use]
    pub fn new(board: rf_cart::Sa1Board) -> Self {
        Self {
            regs: Sa1Regs::new(),
            iram: vec![0; board.iram_len],
            bwram: vec![0; board.bwram_len],
            board,
            cpu: crate::cpu::Cpu::new(),
            booted: false,
            credit: 0,
        }
    }

    /// The live [`crate::mapping::Sa1RomBanks`] view both [`crate::mapping::sa1_target`]
    /// (SNES side) and [`crate::mapping::sa1_side_target`] (SA-1 side,
    /// through [`Sa1Bus`]) resolve addresses against.
    #[must_use]
    pub fn banks(&self) -> crate::mapping::Sa1RomBanks {
        crate::mapping::Sa1RomBanks {
            cxb: self.regs.cxb(),
            dxb: self.regs.dxb(),
            exb: self.regs.exb(),
            fxb: self.regs.fxb(),
            bmaps: self.regs.bmaps(),
            bmap: self.regs.bmap(),
            board: self.board,
        }
    }

    /// Run one SA-1 instruction, or none if the core is held.
    ///
    /// Returns the master cycles this step spent (`0` while held — Reset
    /// or Wait — which is also why [`Self::credit`] must not accumulate
    /// across those calls: see its doc) and the CPU's own `Result` (an
    /// unimplemented opcode propagates exactly the way it does for the
    /// main CPU).
    ///
    /// `rom` is borrowed from [`crate::bus::SnesBus`] for the duration of
    /// this call only — see the module doc's ownership section.
    ///
    /// `rom_contended`/`bwram_contended` are this master-clock step's
    /// SNES-side bus contention flags (ticket W17-04) — see
    /// [`crate::bus::SnesBus::sa1_rom_contended`]'s doc for how they are
    /// derived and [`Sa1Bus::access_cost`] for how they change cost.
    ///
    /// # Errors
    /// Returns the opcode if the CPU does not implement it.
    pub fn step(
        &mut self,
        rom: &[u8],
        rom_contended: bool,
        bwram_contended: bool,
    ) -> Result<u64, u8> {
        if self.regs.sa1_reset_asserted() {
            self.booted = false;
            return Ok(0);
        }
        if !self.booted {
            self.cpu = crate::cpu::Cpu::new();
            self.cpu.pbr = 0;
            self.cpu.pc = self.regs.sa1_reset_vector();
            self.booted = true;
        }
        if self.regs.sa1_wait_asserted() {
            return Ok(0);
        }

        let mut bus = Sa1Bus::new(
            rom,
            &mut self.regs,
            &mut self.iram,
            &mut self.bwram,
            self.board,
            rom_contended,
            bwram_contended,
        );

        // Interrupt delivery, before the next opcode fetch — the same
        // ordering `SnesSystem::step` uses for the main CPU. NMI is
        // edge-triggered and single-shot; IRQ is level and re-checked
        // every step (masked by the CPU's own I flag, same as the SNES
        // CPU's `bus.irq.fired`).
        if bus.regs.take_sa1_nmi_edge() && bus.regs.sa1_nmi_enabled() {
            let vector = bus.regs.sa1_nmi_vector();
            self.cpu.interrupt_to_vector(&mut bus, vector);
        } else if bus.regs.sa1_irq_pending() && !self.cpu.flag(crate::cpu::flags::I) {
            let vector = bus.regs.sa1_irq_vector();
            self.cpu.interrupt_to_vector(&mut bus, vector);
        }

        let result = self.cpu.step(&mut bus);
        let cost = if bus.cycles == 0 && self.cpu.stopped {
            // A halted SA-1 still burns clock, the same reasoning
            // `SnesSystem::step` documents for the main CPU's WAI/STP: one
            // internal cycle at the SA-1's own 10.74MHz rate.
            2
        } else {
            bus.cycles
        };
        result.map(|()| cost)
    }
}

/// The SA-1 CPU's own view of the cartridge (ticket W17-02, D-013).
///
/// A *borrowing* wrapper, not a second copy of anything — see the module
/// doc's ownership section. Built fresh inside [`Sa1State::step`] every
/// call; it cannot outlive that call, so it cannot alias anything the SNES
/// side is using concurrently (there is no concurrency here at all: the
/// two CPUs are interleaved on one thread by `SnesSystem::step`).
pub struct Sa1Bus<'a> {
    rom: &'a [u8],
    regs: &'a mut Sa1Regs,
    iram: &'a mut [u8],
    bwram: &'a mut [u8],
    board: rf_cart::Sa1Board,
    /// Master cycles accumulated across every access this instruction made
    /// — the SA-1 analogue of [`crate::cpu::speed::AccessCost`], folded in
    /// directly rather than as a separate wrapper type since `Sa1Bus`
    /// already owns every field a cost function would need.
    cycles: u64,
    /// This step's SNES-side ROM/BW-RAM contention (ticket W17-04) — see
    /// [`Self::access_cost`].
    rom_contended: bool,
    bwram_contended: bool,
}

impl<'a> Sa1Bus<'a> {
    pub fn new(
        rom: &'a [u8],
        regs: &'a mut Sa1Regs,
        iram: &'a mut [u8],
        bwram: &'a mut [u8],
        board: rf_cart::Sa1Board,
        rom_contended: bool,
        bwram_contended: bool,
    ) -> Self {
        Self {
            rom,
            regs,
            iram,
            bwram,
            board,
            cycles: 0,
            rom_contended,
            bwram_contended,
        }
    }

    fn banks(&self) -> crate::mapping::Sa1RomBanks {
        crate::mapping::Sa1RomBanks {
            cxb: self.regs.cxb(),
            dxb: self.regs.dxb(),
            exb: self.regs.exb(),
            fxb: self.regs.fxb(),
            bmaps: self.regs.bmaps(),
            bmap: self.regs.bmap(),
            board: self.board,
        }
    }

    fn target(&self, addr: u32) -> Target {
        let bank = ((addr >> 16) & 0xFF) as u8;
        let offset = addr as u16;
        crate::mapping::sa1_side_target(&self.banks(), bank, offset)
    }

    /// Master cycles one SA-1 access at `addr` costs (ticket W17-04's
    /// clocking rule, superseding W17-02's flat approximation).
    ///
    /// The SA-1's own resources — I-RAM and its register window — are
    /// always charged its full, uncontended 10.74MHz rate: master clock
    /// / 2, i.e. 2 master cycles per access (fullsnes "Misc": "The SA-1
    /// CPU can access memory at 10.74MHz rate (or less, if the SNES does
    /// simultaneously access cartridge memory)" — nothing on the SNES
    /// side can reach I-RAM's own window or the SA-1's registers, so
    /// "simultaneously" can never apply to them).
    ///
    /// ROM and BW-RAM are shared with the SNES side, so "or less" above
    /// is exactly the case this function has to model: an access costs
    /// the same uncontended 2 UNLESS the SNES side (the main CPU's
    /// instruction, its MDMA, or its HDMA — anything that ran during
    /// this same `SnesSystem::step`, before the SA-1 catch-up loop; see
    /// `crate::bus::SnesBus::sa1_rom_contended`'s doc) also touched that
    /// same device this step, in which case it costs the doubled 4 —
    /// consistent with fullsnes's own DMA speed table ("SNES Cart SA-1
    /// DMA Transfers"), which gives ROM->BW-RAM and BW-RAM->I-RAM
    /// transfers half the SA-1's own rate (5.37MHz) whenever BW-RAM is
    /// involved.
    ///
    /// This is an explicit, deterministic approximation of "the
    /// documented wait states... when it touches BW-RAM/ROM while the
    /// SNES CPU holds the bus" (the rule W17-02's ticket allowed
    /// deferring): contention is decided once per `SnesSystem::step` —
    /// for the WHOLE device, not per byte-range or per bus cycle — rather
    /// than modelling the SNES CPU's actual bus occupancy cycle by cycle.
    /// A main CPU that runs almost entirely from ROM (the common case)
    /// will therefore see its SA-1 co-processor pay the doubled ROM rate
    /// almost every step, which is the expected, hardware-consistent
    /// outcome, not a modelling artifact — see `docs/design/
    /// EMULATION_CORES.md` §3.5 for where this is not yet
    /// cycle-accurate: the SNES CPU's own wait when the SA-1 is mid-DMA
    /// on BW-RAM is not modelled (DMA execution is charged atomically to
    /// the SA-1 instruction that triggers it, not spread across master
    /// cycles the main CPU could contend with), which fullsnes's "BW-RAM
    /// cannot be used during character conversion DMA" note says exists
    /// on hardware.
    fn access_cost(&self, target: Target) -> u64 {
        match target {
            Target::Rom(_) if self.rom_contended => 4,
            Target::Sa1BwRam(_) if self.bwram_contended => 4,
            _ => 2,
        }
    }
}

impl CpuBus for Sa1Bus<'_> {
    fn read(&mut self, addr: u32) -> u8 {
        let target = self.target(addr);
        self.cycles += self.access_cost(target);
        match target {
            Target::Rom(i) => self.rom.get(i).copied().unwrap_or(0xFF),
            Target::Sa1IRam(i) => self.iram[i],
            Target::Sa1BwRam(i) => self.bwram[i],
            // `read_mut` needs the ROM for the variable-length bit reader
            // ($230C) — see `Sa1Regs::read_mut`'s doc.
            Target::Sa1Register(offset) => self.regs.read_mut(offset, self.rom).unwrap_or(0),
            Target::Sa1Bitmap(k) => bitmap_read(self.regs, self.bwram, k),
            Target::Wram(_)
            | Target::Sram(_)
            | Target::Register(_)
            | Target::Dsp1Dr
            | Target::Dsp1Sr
            | Target::GsuRam(_)
            | Target::GsuRegister(_)
            | Target::Open => 0,
        }
    }

    fn write(&mut self, addr: u32, value: u8) {
        let target = self.target(addr);
        self.cycles += self.access_cost(target);
        match target {
            // Ticket W17-03: `$222A` CIWP gates the SA-1 side's own I-RAM
            // writes (fullsnes "...Memory Control"). Reset value `$00`
            // protects every 256-byte chunk until software enables it.
            Target::Sa1IRam(i) => {
                if self.regs.iram_chunk_writable_sa1(i) {
                    self.iram[i] = value;
                }
            }
            // `$2226`/`$2227` SBWE — shared gate, see `Sa1Regs::bwram_writable`'s doc.
            Target::Sa1BwRam(i) => {
                if self.regs.bwram_writable_sa1(i) {
                    self.bwram[i] = value;
                }
            }
            // `$2300-$23FF` (the read-only block) is dropped, not stored —
            // real hardware has nowhere to put a write there either (same
            // rule `crate::bus::SnesBus::write` applies on the SNES side).
            Target::Sa1Register(offset) => {
                if offset < 0x2300 {
                    self.regs.write(offset, value);
                    // DMA/character-conversion cycles a SA-1-side write
                    // triggers are folded into THIS instruction's own
                    // cost, which is exactly how the SA-1's own credit
                    // gets charged for them (`Sa1State::step`'s `cost` is
                    // `bus.cycles`) — see the module-level cycle-charging
                    // rule on [`execute_normal_dma`].
                    self.cycles += apply_register_side_effect(
                        self.regs, offset, value, self.rom, self.iram, self.bwram,
                    );
                }
            }
            // Bitmap projection writes: SA-1-side only, gated by the same
            // `$2227`/`$2228` BW-RAM write protection as the plain
            // window, since it targets the same underlying bytes.
            Target::Sa1Bitmap(k) => bitmap_write(self.regs, self.bwram, k, value),
            Target::Rom(_)
            | Target::Wram(_)
            | Target::Sram(_)
            | Target::Register(_)
            | Target::Dsp1Dr
            | Target::Dsp1Sr
            | Target::GsuRam(_)
            | Target::GsuRegister(_)
            | Target::Open => {}
        }
    }

    fn peek(&self, addr: u32) -> u8 {
        match self.target(addr) {
            Target::Rom(i) => self.rom.get(i).copied().unwrap_or(0xFF),
            Target::Sa1IRam(i) => self.iram[i],
            Target::Sa1BwRam(i) => self.bwram[i],
            Target::Sa1Register(offset) => self.regs.read(offset).unwrap_or(0),
            Target::Sa1Bitmap(k) => bitmap_read(self.regs, self.bwram, k),
            Target::Wram(_)
            | Target::Sram(_)
            | Target::Register(_)
            | Target::Dsp1Dr
            | Target::Dsp1Sr
            | Target::GsuRam(_)
            | Target::GsuRegister(_)
            | Target::Open => 0,
        }
    }
}

/// Bitmap projection read (fullsnes "$223F" BBF) — see
/// [`crate::mapping::Target::Sa1Bitmap`]'s doc for the index scheme.
fn bitmap_read(regs: &Sa1Regs, bwram: &[u8], k: usize) -> u8 {
    let per_byte = bitmap_pixels_per_byte(regs);
    let byte_idx = k / per_byte;
    if bwram.is_empty() {
        return 0;
    }
    let sub = k % per_byte;
    let width = 8 / per_byte;
    let byte = bwram[byte_idx % bwram.len()];
    ((byte as usize >> (sub * width)) & ((1 << width) - 1)) as u8
}

/// Bitmap projection write: read-modify-write of the packed underlying
/// byte, gated by the SA-1 side's own BW-RAM write protection (the same
/// bytes are reachable through the plain `$40-$4F`/`$6000-$7FFF` windows,
/// so this must not bypass it).
fn bitmap_write(regs: &Sa1Regs, bwram: &mut [u8], k: usize, value: u8) {
    let per_byte = bitmap_pixels_per_byte(regs);
    if bwram.is_empty() {
        return;
    }
    let byte_idx = (k / per_byte) % bwram.len();
    if !regs.bwram_writable_sa1(byte_idx) {
        return;
    }
    let sub = k % per_byte;
    let width = 8 / per_byte;
    let mask = ((1u16 << width) - 1) as u8;
    let shift = sub * width;
    bwram[byte_idx] = (bwram[byte_idx] & !(mask << shift)) | ((value & mask) << shift);
}

/// Pixels packed per underlying byte: 2 (4bpp) or 4 (2bpp) — fullsnes
/// "$223F" BBF bit 7, "0=4bit, 1=2bit" (**inverted** from the obvious
/// guess: set means the narrower, 2-bit format).
fn bitmap_pixels_per_byte(regs: &Sa1Regs) -> usize {
    if regs.bitmap_is_2bpp() {
        4
    } else {
        2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ticket W17-04 acceptance #1: pins `Sa1Bus::access_cost` per access
    /// class, contended and not. I-RAM/registers never change (the SNES
    /// side cannot reach them); ROM/BW-RAM double under contention.
    #[test]
    fn access_cost_is_uncontended_by_default_and_doubles_when_contended() {
        let rom = [0u8; 8];
        let mut regs = Sa1Regs::new();
        let mut iram = vec![0u8; 8];
        let mut bwram = vec![0u8; 8];
        let board = rf_cart::Sa1Board {
            rom_len: rom.len(),
            iram_len: iram.len(),
            bwram_len: bwram.len(),
        };

        let uncontended = Sa1Bus::new(&rom, &mut regs, &mut iram, &mut bwram, board, false, false);
        assert_eq!(
            uncontended.access_cost(Target::Rom(0)),
            2,
            "uncontended ROM: full 10.74MHz rate"
        );
        assert_eq!(
            uncontended.access_cost(Target::Sa1BwRam(0)),
            2,
            "uncontended BW-RAM: full rate"
        );
        assert_eq!(
            uncontended.access_cost(Target::Sa1IRam(0)),
            2,
            "I-RAM: always full rate"
        );
        assert_eq!(
            uncontended.access_cost(Target::Sa1Register(0x2200)),
            2,
            "registers: always full rate"
        );

        let mut regs2 = Sa1Regs::new();
        let mut iram2 = vec![0u8; 8];
        let mut bwram2 = vec![0u8; 8];
        let rom_contended = Sa1Bus::new(
            &rom,
            &mut regs2,
            &mut iram2,
            &mut bwram2,
            board,
            true,
            false,
        );
        assert_eq!(
            rom_contended.access_cost(Target::Rom(0)),
            4,
            "SNES side touched ROM this step: halved (doubled cost)"
        );
        assert_eq!(
            rom_contended.access_cost(Target::Sa1BwRam(0)),
            2,
            "BW-RAM contention is independent of ROM contention"
        );
        assert_eq!(
            rom_contended.access_cost(Target::Sa1IRam(0)),
            2,
            "I-RAM unaffected"
        );

        let mut regs3 = Sa1Regs::new();
        let mut iram3 = vec![0u8; 8];
        let mut bwram3 = vec![0u8; 8];
        let bwram_contended = Sa1Bus::new(
            &rom,
            &mut regs3,
            &mut iram3,
            &mut bwram3,
            board,
            false,
            true,
        );
        assert_eq!(
            bwram_contended.access_cost(Target::Sa1BwRam(0)),
            4,
            "SNES side touched BW-RAM this step: halved (doubled cost)"
        );
        assert_eq!(
            bwram_contended.access_cost(Target::Rom(0)),
            2,
            "ROM contention is independent"
        );
    }

    /// `$2200` CCNT edge -> CFR status, and `$220B` CIC acks it (ticket
    /// W17-02 acceptance #3). Message (bits 0-3) passes straight through
    /// with no latching, matching fullsnes's own note that it is "same as
    /// 2200h.Bit0-3" with no ack semantics.
    #[test]
    fn ccnt_edge_sets_the_cfr_flags_and_cic_acks_them() {
        let mut r = Sa1Regs::new();
        r.write(0x2200, 0b1011_0101); // IRQ + NMI + message 5
        assert_eq!(r.cfr() & 0x0F, 0x05, "message passes through");
        assert_ne!(r.cfr() & 0x10, 0, "NMI status latched");
        assert_ne!(r.cfr() & 0x80, 0, "IRQ status latched");
        assert!(r.take_sa1_nmi_edge(), "edge fires once");
        assert!(!r.take_sa1_nmi_edge(), "and only once per $2200 write");

        // fullsnes: "0=No Change?" — a later write with those bits clear
        // must NOT clear the latched status; only $220B does.
        r.write(0x2200, 0x00);
        assert_ne!(r.cfr() & 0x90, 0, "status persists through a 0-bit write");
        r.write(0x220B, 0b1001_0000); // CIC: ack NMI + IRQ
        assert_eq!(r.cfr() & 0x90, 0, "$220B acks clear the status");
    }

    /// `$2209` SCNT bit 7 edge -> SFR status, and `$2202` SIC acks it.
    #[test]
    fn scnt_edge_sets_the_sfr_flag_and_sic_acks_it() {
        let mut r = Sa1Regs::new();
        r.write(0x2209, 0b1000_0111); // IRQ + message 7
        assert_eq!(r.sfr() & 0x0F, 0x07);
        assert_ne!(r.sfr() & 0x80, 0);
        r.write(0x2202, 0x80); // SIC: ack IRQ from SA-1
        assert_eq!(r.sfr() & 0x80, 0);
    }

    /// `$220A` CIE / `$2201` SIE gate delivery, per fullsnes's note that a
    /// masked interrupt still sets its status flag.
    #[test]
    fn enable_bits_gate_pending_delivery_without_hiding_the_status_flag() {
        let mut r = Sa1Regs::new();
        r.write(0x2200, 0x80); // IRQ edge, CIE not yet enabled
        assert_ne!(
            r.cfr() & 0x80,
            0,
            "status flag sets regardless of the enable bit"
        );
        assert!(
            !r.sa1_irq_pending(),
            "but delivery is masked until CIE bit 7 enables it"
        );
        r.write(0x220A, 0x80); // CIE: enable IRQ-from-SNES
        assert!(r.sa1_irq_pending());

        r.write(0x2209, 0x80); // SCNT IRQ edge, SIE not yet enabled
        assert!(!r.snes_irq_pending());
        r.write(0x2201, 0x80); // SIE: enable IRQ-from-SA-1
        assert!(r.snes_irq_pending());
    }

    /// `$2203-$2208` (SA-1 vectors, always active) and `$220C-$220F` (SNES
    /// vectors, gated by `$2209` bits 4/6).
    #[test]
    fn vectors_read_back_exactly_as_written_and_snes_vectors_are_gated() {
        let mut r = Sa1Regs::new();
        r.write(0x2203, 0x34);
        r.write(0x2204, 0x12);
        assert_eq!(r.sa1_reset_vector(), 0x1234);
        r.write(0x2205, 0x78);
        r.write(0x2206, 0x56);
        assert_eq!(r.sa1_nmi_vector(), 0x5678);
        r.write(0x2207, 0xBC);
        r.write(0x2208, 0x9A);
        assert_eq!(r.sa1_irq_vector(), 0x9ABC);

        r.write(0x220C, 0x34);
        r.write(0x220D, 0x12);
        assert_eq!(
            r.snes_nmi_vector_override(),
            None,
            "not selected until $2209 bit 4"
        );
        r.write(0x2209, 0x10);
        assert_eq!(r.snes_nmi_vector_override(), Some(0x1234));

        r.write(0x220E, 0x78);
        r.write(0x220F, 0x56);
        r.write(0x2209, 0x40); // selects IRQ vector; clears the NMI-select bit too
        assert_eq!(r.snes_irq_vector_override(), Some(0x5678));
        assert_eq!(r.snes_nmi_vector_override(), None);
    }

    /// `$2200` bits 5/6 (Reset/Wait) are level, read straight back.
    #[test]
    fn reset_and_wait_bits_read_back_from_ccnt() {
        let r = Sa1Regs::new();
        assert!(r.sa1_reset_asserted(), "power-on default $20 asserts Reset");
        assert!(!r.sa1_wait_asserted());
        let mut r = r;
        r.write(0x2200, 0x60);
        assert!(r.sa1_reset_asserted());
        assert!(r.sa1_wait_asserted());
    }

    /// `$2300` SFR and `$2301` CFR compute live; the write-only block
    /// never reads back (every port in fullsnes's I/O map is "(W)" only).
    #[test]
    fn sfr_and_cfr_are_computed_reads_and_the_write_block_is_open_bus() {
        let mut r = Sa1Regs::new();
        r.write(0x2209, 0x81);
        r.write(0x220A, 0x80);
        r.write(0x2200, 0x90);
        assert_eq!(r.read(0x2300), Some(r.sfr()));
        assert_eq!(r.read(0x2301), Some(r.cfr()));
        assert_eq!(r.read(0x2200), None);
        assert_eq!(r.read(0x220B), None);
    }

    // ---- ticket W17-03 ----

    fn test_board() -> rf_cart::Sa1Board {
        rf_cart::Sa1Board {
            rom_len: 0x10000,
            bwram_len: 0x2000,
            iram_len: 0x800,
        }
    }

    fn unlocked_state() -> Sa1State {
        let mut s = Sa1State::new(test_board());
        s.regs.write(0x2226, 0x80); // SBWE (SNES)
        s.regs.write(0x2227, 0x80); // SBWE (SA-1)
        s.regs.write(0x2228, 0x00); // BWPA floor: 256 bytes
        s.regs.write(0x2229, 0xFF); // SIWP (SNES)
        s.regs.write(0x222A, 0xFF); // CIWP (SA-1)
        s
    }

    /// A test's local write helper: every register write must go through
    /// [`handle_register_side_effect`], not `s.regs.write` directly, or a
    /// trigger write (`$2236`/`$2237`/`$2202`/`$224F`) would store the
    /// byte but never run the DMA/conversion it is supposed to arm.
    fn w(s: &mut Sa1State, rom: &[u8], offset: u16, value: u8) {
        s.regs.write(offset, value);
        handle_register_side_effect(s, offset, value, rom);
    }

    /// Normal DMA: all four legal source/destination pairs, synthetic
    /// tables, acceptance #1.
    #[test]
    fn normal_dma_covers_every_legal_source_destination_pair() {
        // ROM -> I-RAM (10.74MHz, 2 master cycles/byte).
        let rom: Vec<u8> = (0..16u8).collect();
        let mut s = unlocked_state();
        w(&mut s, &rom, 0x2232, 0x00);
        w(&mut s, &rom, 0x2233, 0x00);
        w(&mut s, &rom, 0x2234, 0x00); // SDA = 0
        w(&mut s, &rom, 0x2238, 0x04);
        w(&mut s, &rom, 0x2239, 0x00); // DTC = 4 bytes
        w(&mut s, &rom, 0x2230, 0b1000_0000); // DCNT: src=ROM, dst=I-RAM, enable
        w(&mut s, &rom, 0x2235, 0x10);
        w(&mut s, &rom, 0x2236, 0x00); // DDA = $0010 -> triggers
        assert_eq!(&s.iram[0x10..0x14], &rom[0..4]);
        assert!(
            s.regs.dma_irq_status_sa1,
            "CFR bit 5 must set on completion"
        );

        // BW-RAM -> I-RAM (5.37MHz, 4 cycles/byte).
        let mut s = unlocked_state();
        s.bwram[0..4].copy_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
        w(&mut s, &[], 0x2232, 0x00);
        w(&mut s, &[], 0x2233, 0x00);
        w(&mut s, &[], 0x2234, 0x00);
        w(&mut s, &[], 0x2238, 0x04);
        w(&mut s, &[], 0x2239, 0x00);
        w(&mut s, &[], 0x2230, 0b1000_0001); // src=BW-RAM, dst=I-RAM
        w(&mut s, &[], 0x2235, 0x20);
        w(&mut s, &[], 0x2236, 0x00); // DDA=$0020, triggers on the I-RAM write
        assert_eq!(&s.iram[0x20..0x24], &[0xAA, 0xBB, 0xCC, 0xDD]);

        // I-RAM -> BW-RAM.
        let mut s = unlocked_state();
        s.iram[0..2].copy_from_slice(&[0x11, 0x22]);
        w(&mut s, &[], 0x2232, 0x00);
        w(&mut s, &[], 0x2233, 0x00);
        w(&mut s, &[], 0x2234, 0x00);
        w(&mut s, &[], 0x2238, 0x02);
        w(&mut s, &[], 0x2239, 0x00);
        w(&mut s, &[], 0x2230, 0b1100_0110); // src=I-RAM(2), dst=BW-RAM(bit2)
        w(&mut s, &[], 0x2235, 0x00);
        w(&mut s, &[], 0x2236, 0x02);
        w(&mut s, &[], 0x2237, 0x00); // DDA=$000200, triggers on the BW-RAM write
        assert_eq!(&s.bwram[0x200..0x202], &[0x11, 0x22]);

        // ROM -> BW-RAM.
        let rom: Vec<u8> = (0..16u8).map(|i| 0x55 + i).collect();
        let mut s = unlocked_state();
        w(&mut s, &rom, 0x2232, 0x00);
        w(&mut s, &rom, 0x2233, 0x00);
        w(&mut s, &rom, 0x2234, 0x00);
        w(&mut s, &rom, 0x2238, 0x03);
        w(&mut s, &rom, 0x2239, 0x00);
        w(&mut s, &rom, 0x2230, 0b1000_0100); // src=ROM(0), dst=BW-RAM
        w(&mut s, &rom, 0x2235, 0x00);
        w(&mut s, &rom, 0x2236, 0x03);
        w(&mut s, &rom, 0x2237, 0x00);
        assert_eq!(&s.bwram[0x300..0x303], &rom[0..3]);
    }

    /// Same-device DMA (I-RAM<->I-RAM, BW-RAM<->BW-RAM) is refused per
    /// fullsnes ("Source and Destination may not be the same devices") —
    /// a no-op, not a panic.
    #[test]
    fn normal_dma_refuses_same_device_transfers() {
        let mut s = unlocked_state();
        s.iram[5] = 0x99;
        w(&mut s, &[], 0x2232, 0x00);
        w(&mut s, &[], 0x2233, 0x00);
        w(&mut s, &[], 0x2234, 0x05); // SDA=5 (I-RAM)
        w(&mut s, &[], 0x2238, 0x01);
        w(&mut s, &[], 0x2239, 0x00);
        w(&mut s, &[], 0x2230, 0b1000_0010); // src=I-RAM(2), dst=I-RAM(bit2=0)
        w(&mut s, &[], 0x2235, 0x00);
        w(&mut s, &[], 0x2236, 0x00); // DDA=0
        assert_eq!(s.iram[0], 0, "I-RAM->I-RAM must not run");
    }

    /// Character conversion type 1: one hand-computed 4bpp tile row.
    #[test]
    fn charconv_type1_produces_the_hand_computed_tile_row() {
        let mut s = unlocked_state();
        // One 4bpp pixel row, 8 pixels, values 0,1,2,...,7 packed 2/byte,
        // LSB = left-most pixel.
        s.bwram[0] = 0x10; // pixels 0,1
        s.bwram[1] = 0x32; // pixels 2,3
        s.bwram[2] = 0x54; // pixels 4,5
        s.bwram[3] = 0x76; // pixels 6,7
        w(&mut s, &[], 0x2231, 0x01); // CDMA: 4bpp, 1 char/line
        w(&mut s, &[], 0x2232, 0x00);
        w(&mut s, &[], 0x2233, 0x00);
        w(&mut s, &[], 0x2234, 0x00); // SDA=0
        w(&mut s, &[], 0x2230, 0b0011_0000); // DCNT: charconv enable (bit5) + type1 (bit4)
        w(&mut s, &[], 0x2235, 0x00);
        w(&mut s, &[], 0x2236, 0x00); // DDA=0 (I-RAM), triggers row 0
        assert!(s.regs.chardma_irq_status_snes, "SFR bit 5 must set");
        // Row 0 lands at bytes [0,1] (bp0,bp1) of the 32-byte tile, x=0..7
        // pixel values 0..7 (a bit at `0x80 >> x` when that plane's bit is
        // set in pixel(x)).
        assert_eq!(
            s.iram[0], 0b0101_0101,
            "bp0 (pixel bit 0): odd x (1,3,5,7) set it"
        );
        assert_eq!(
            s.iram[1], 0b0011_0011,
            "bp1 (pixel bit 1): x=2,3,6,7 set it"
        );
    }

    /// Character conversion type 2: one hand-computed 2bpp tile row from
    /// unpacked BRF pixel writes.
    #[test]
    fn charconv_type2_produces_the_hand_computed_tile_row() {
        let mut s = unlocked_state();
        w(&mut s, &[], 0x2231, 0x02); // CDMA: 2bpp
        w(&mut s, &[], 0x2230, 0b1010_0000); // DCNT: DMA-enable (bit7) + charconv (bit5), type2 (bit4=0)
        w(&mut s, &[], 0x2235, 0x00);
        w(&mut s, &[], 0x2236, 0x00); // DDA=0 (arms type 2, no conversion yet)
        assert_eq!(s.iram[0], 0, "arming alone must not convert anything");
        // Tile A row: pixels 3,3,3,3,0,0,0,0 (x=0..7). Tile B row: zero.
        for x in 0..4u16 {
            w(&mut s, &[], 0x2240 + x, 0x03);
        }
        for x in 4..16u16 {
            w(&mut s, &[], 0x2240 + x, 0x00);
        }
        assert_eq!(
            s.iram[0], 0b1111_0000,
            "bp0: pixels x=0..3 (value 3, bit0 set) light the high nibble"
        );
        assert_eq!(s.iram[1], 0b1111_0000, "bp1: same pixels, bit1 also set");
    }

    /// Arithmetic: multiply, divide (incl. by zero) and cumulative sum
    /// with overflow — fullsnes "SNES Cart SA-1 Arithmetic Maths".
    #[test]
    fn arithmetic_unit_computes_multiply_divide_and_cumulative_sum() {
        let mut r = Sa1Regs::new();
        // Multiply: -3 * 5 = -15.
        r.write(0x2250, 0x00); // mode 0: multiply
        r.write(0x2251, (-3i16).to_le_bytes()[0]);
        r.write(0x2252, (-3i16).to_le_bytes()[1]);
        r.write(0x2253, 5);
        r.write(0x2254, 0); // start
        let result = i32::from(r.read(0x2306).unwrap())
            | i32::from(r.read(0x2307).unwrap()) << 8
            | i32::from(r.read(0x2308).unwrap()) << 16
            | i32::from(r.read(0x2309).unwrap()) << 24;
        assert_eq!(result, -15);

        // Divide: 17 / 5 = 3 remainder 2.
        let mut r = Sa1Regs::new();
        r.write(0x2250, 0x01); // mode 1: divide
        r.write(0x2251, 17);
        r.write(0x2252, 0);
        r.write(0x2253, 5);
        r.write(0x2254, 0);
        assert_eq!(r.read(0x2306).unwrap() as i8, 3, "quotient");
        assert_eq!(r.read(0x2308).unwrap(), 2, "remainder");

        // Division by zero: 0/0 per fullsnes.
        let mut r = Sa1Regs::new();
        r.write(0x2250, 0x01);
        r.write(0x2251, 9);
        r.write(0x2252, 0);
        r.write(0x2253, 0);
        r.write(0x2254, 0);
        assert_eq!(r.read(0x2306).unwrap(), 0);
        assert_eq!(r.read(0x2308).unwrap(), 0);

        // Cumulative sum: two multiplies accumulate; mode-select resets it.
        let mut r = Sa1Regs::new();
        r.write(0x2250, 0x02); // mode 2 (bit1 set): resets sum, selects Sum
        r.write(0x2251, 2);
        r.write(0x2252, 0);
        r.write(0x2253, 3);
        r.write(0x2254, 0); // +6
        r.write(0x2251, 4);
        r.write(0x2252, 0);
        r.write(0x2253, 5);
        r.write(0x2254, 0); // +20 -> 26
        assert_eq!(r.read(0x2306).unwrap(), 26);
        assert!(!r.overflow);
        r.write(0x2250, 0x02); // bit1 write resets the sum
        assert_eq!(r.read(0x2306).unwrap(), 0);
    }

    /// Variable-length bit reader: fixed mode (no auto-increment) and
    /// auto-increment mode.
    #[test]
    fn variable_length_bit_reader_preloads_and_auto_increments() {
        let rom = [0b1010_1100u8, 0b0000_1111, 0xFF];
        let mut r = Sa1Regs::new();
        r.write(0x2258, 0x00); // fixed mode, length field unused by preload
        r.write(0x2259, 0x00);
        r.write(0x225A, 0x00);
        r.write(0x225B, 0x00); // kick: bitpos = 0
        let lo = r.read_mut(0x230C, &rom).unwrap();
        let hi = r.read_mut(0x230D, &rom).unwrap();
        let window = u16::from(lo) | u16::from(hi) << 8;
        assert_eq!(window, u16::from(rom[0]) | u16::from(rom[1]) << 8);
        // Fixed mode: a second read sees the same window.
        let lo2 = r.read_mut(0x230C, &rom).unwrap();
        assert_eq!(lo2, lo, "fixed mode does not advance");

        // Auto-increment, length 8 bits: bitpos advances by 8 each read.
        let mut r = Sa1Regs::new();
        r.write(0x2258, 0x88); // length=8, auto-increment
        r.write(0x2259, 0x00);
        r.write(0x225A, 0x00);
        r.write(0x225B, 0x00);
        let first = r.read_mut(0x230C, &rom).unwrap();
        assert_eq!(first, rom[0]);
        let second = r.read_mut(0x230C, &rom).unwrap();
        assert_eq!(second, rom[1], "advanced by 8 bits = 1 byte");
    }

    /// Timer: HV mode fires on an H/V compare match; Linear mode ticks
    /// its own free-running counter and fires the same way.
    #[test]
    fn timer_fires_on_hv_and_linear_matches() {
        let mut r = Sa1Regs::new();
        r.write(0x2210, 0x01); // TMC: HEN, HV mode
        r.write(0x2212, 100);
        r.write(0x2213, 0); // HCNT compare = 100
        r.tick_timer(0, 99, 5); // not yet
        assert!(!r.timer_irq_status_sa1);
        r.tick_timer(0, 100, 5); // dot advances to the match
        assert!(r.timer_irq_status_sa1, "H match must set CFR bit 6");

        // $2211 CTR restarts the timer.
        r.write(0x2211, 0x00);
        assert_eq!(r.timer_h, 0);

        // Linear mode: ticks off master cycles, 4 per unit.
        let mut r = Sa1Regs::new();
        r.write(0x2210, 0x81); // TMC: HEN + Linear mode
        r.write(0x2212, 2);
        r.write(0x2213, 0); // compare = 2
        r.tick_timer(4, 0, 0); // h: 0->1
        assert!(!r.timer_irq_status_sa1);
        r.tick_timer(4, 0, 0); // h: 1->2, matches
        assert!(r.timer_irq_status_sa1);
    }

    /// BW-RAM/I-RAM write protection: default-protected, then enabled,
    /// with BWPA's floor.
    #[test]
    fn write_protection_gates_bwram_and_iram_by_side() {
        let r = Sa1Regs::new();
        assert!(
            !r.bwram_writable_snes(1000),
            "SBWE resets to protect (fullsnes reset table has no $2226 exception)"
        );
        assert!(
            !r.iram_chunk_writable_snes(0),
            "SIWP resets to $00: every chunk protected"
        );

        let mut r = Sa1Regs::new();
        r.write(0x2226, 0x80); // SBWE enable (SNES side register)
        assert!(
            r.bwram_writable_snes(0),
            "SBWE alone enables the whole buffer (Kirby Super Star, see doc)"
        );
        assert!(
            r.bwram_writable_sa1(300),
            "the gate is shared: $2226 also unlocks the SA-1 side (Kirby's Dream Land 3, see doc)"
        );

        let mut r = Sa1Regs::new();
        r.write(0x2229, 0x01); // SIWP: chunk 0 only
        assert!(r.iram_chunk_writable_snes(0));
        assert!(!r.iram_chunk_writable_snes(256), "chunk 1 still protected");
        assert!(
            !r.iram_chunk_writable_sa1(0),
            "CIWP ($222A) is independent of SIWP"
        );
    }

    /// Bitmap projection: 4bpp and 2bpp packing, read-modify-write.
    #[test]
    fn bitmap_projection_packs_and_unpacks_2bpp_and_4bpp() {
        let mut regs = Sa1Regs::new();
        regs.write(0x2227, 0x80); // SBWE (SA-1)
        regs.write(0x2228, 0x00); // BWPA: minimum 256-byte protected floor
        let mut bwram = vec![0u8; 512];
        // Byte 256 is past the floor (see `write_protection_gates_...`);
        // pixel index 512 (4bpp, 2/byte) or 1024 (2bpp, 4/byte) lands
        // there.
        let byte_idx = 256usize;

        // 4bpp: BBF bit 7 = 0. Two pixels per byte.
        regs.write(0x223F, 0x00);
        let k = byte_idx * 2;
        bitmap_write(&regs, &mut bwram, k, 0x0A); // low nibble
        bitmap_write(&regs, &mut bwram, k + 1, 0x0B); // high nibble
        assert_eq!(bwram[byte_idx], 0xBA);
        assert_eq!(bitmap_read(&regs, &bwram, k), 0x0A);
        assert_eq!(bitmap_read(&regs, &bwram, k + 1), 0x0B);

        // 2bpp: BBF bit 7 = 1 (inverted from the obvious guess). Four
        // pixels per byte.
        let mut bwram = vec![0u8; 512];
        regs.write(0x223F, 0x80);
        let k = byte_idx * 4;
        bitmap_write(&regs, &mut bwram, k, 0b01);
        bitmap_write(&regs, &mut bwram, k + 1, 0b10);
        bitmap_write(&regs, &mut bwram, k + 2, 0b11);
        bitmap_write(&regs, &mut bwram, k + 3, 0b00);
        assert_eq!(bwram[byte_idx], 0b00_11_10_01);
        assert_eq!(bitmap_read(&regs, &bwram, k + 2), 0b11);
    }

    /// Save state: every new field round-trips.
    #[test]
    fn sa1_regs_w17_03_fields_round_trip_through_save_and_load() {
        let mut r = Sa1Regs::new();
        r.write(0x2210, 0x01);
        r.write(0x2212, 50);
        r.tick_timer(0, 50, 10);
        r.write(0x2250, 0x00);
        r.write(0x2251, 6);
        r.write(0x2253, 7);
        r.write(0x2254, 0); // mr = 42
        r.write(0x2258, 0x80);
        r.write(0x225B, 0x00); // vbr_bitpos = 0
        r.write(0x2231, 0x01);
        r.write(0x2230, 0b0001_0000); // charconv1 armed (not yet triggered)

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

        let mut stream = MemStream {
            buf: Vec::new(),
            at: 0,
        };
        r.save(&mut StateOut::new(&mut stream)).expect("save");
        let mut restored = Sa1Regs::new();
        restored.load(&mut StateIn::new(&mut stream)).expect("load");

        assert_eq!(restored.timer_h, r.timer_h);
        assert_eq!(restored.mr, r.mr);
        assert_eq!(restored.vbr_bitpos, r.vbr_bitpos);
        assert_eq!(restored.raw, r.raw);
    }
}
