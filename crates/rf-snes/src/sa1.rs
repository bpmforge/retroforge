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
        }
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
            _ => {}
        }
    }

    /// A read of `$2200-$23FF`. `None` means open bus: the write-only
    /// block itself ($2200-$22FF, real hardware never reads it back
    /// either — every port in fullsnes's table is marked "(W)" only) and
    /// any offset this slice's read-only table doesn't name.
    ///
    /// `$2300` SFR and `$2301` CFR are computed live from the control and
    /// interrupt state (ticket W17-02); `$2302-$230E` (H/V counter,
    /// arithmetic, variable-length bit reader, chip version) still answer
    /// their documented reset value of `$00` — nothing exists yet to move
    /// them (W17-03).
    #[must_use]
    pub fn read(&self, offset: u16) -> Option<u8> {
        match offset {
            0x2300 => Some(self.sfr()),
            0x2301 => Some(self.cfr()),
            0x2302..=0x230E => Some(0),
            _ => None,
        }
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
        o.bool(self.chardma_irq_status_snes)
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
        Ok(())
    }
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
    /// # Errors
    /// Returns the opcode if the CPU does not implement it.
    pub fn step(&mut self, rom: &[u8]) -> Result<u64, u8> {
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
}

impl<'a> Sa1Bus<'a> {
    pub fn new(
        rom: &'a [u8],
        regs: &'a mut Sa1Regs,
        iram: &'a mut [u8],
        bwram: &'a mut [u8],
        board: rf_cart::Sa1Board,
    ) -> Self {
        Self {
            rom,
            regs,
            iram,
            bwram,
            board,
            cycles: 0,
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

    /// Master cycles one SA-1 access at `addr` costs (ticket W17-02's
    /// clocking rule).
    ///
    /// The SA-1 runs its own I-RAM and the register window at its full
    /// 10.74MHz rate — master clock / 2, i.e. 2 master cycles per access
    /// (fullsnes "Misc": "The SA-1 CPU can access memory at 10.74MHz rate
    /// (or less, if the SNES does simultaneously access cartridge
    /// memory)"). ROM and BW-RAM are shared with the SNES side, which can
    /// contend for them; fullsnes's own DMA speed table ("SNES Cart SA-1
    /// DMA Transfers") gives ROM->BW-RAM and BW-RAM->I-RAM transfers half
    /// the SA-1's own rate (5.37MHz) whenever BW-RAM is involved. This
    /// slice approximates the "documented wait states... when it touches
    /// BW-RAM/ROM while the SNES CPU holds the bus" rule the ticket allows
    /// deferring, with a flat doubled cost (4 master cycles) for every
    /// ROM/BW-RAM access, rather than modelling the SNES CPU's actual bus
    /// occupancy cycle by cycle. Stated exactly as implemented, per the
    /// ticket's own requirement.
    fn access_cost(&self, target: Target) -> u64 {
        match target {
            Target::Sa1BwRam(_) | Target::Rom(_) => 4,
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
            Target::Sa1Register(offset) => self.regs.read(offset).unwrap_or(0),
            _ => 0,
        }
    }

    fn write(&mut self, addr: u32, value: u8) {
        let target = self.target(addr);
        self.cycles += self.access_cost(target);
        match target {
            Target::Sa1IRam(i) => self.iram[i] = value,
            Target::Sa1BwRam(i) => self.bwram[i] = value,
            // `$2300-$23FF` (the read-only block) is dropped, not stored —
            // real hardware has nowhere to put a write there either (same
            // rule `crate::bus::SnesBus::write` applies on the SNES side).
            Target::Sa1Register(offset) => {
                if offset < 0x2300 {
                    self.regs.write(offset, value);
                }
            }
            _ => {}
        }
    }

    fn peek(&self, addr: u32) -> u8 {
        match self.target(addr) {
            Target::Rom(i) => self.rom.get(i).copied().unwrap_or(0xFF),
            Target::Sa1IRam(i) => self.iram[i],
            Target::Sa1BwRam(i) => self.bwram[i],
            Target::Sa1Register(offset) => self.regs.read(offset).unwrap_or(0),
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
