//! The SNES machine: CPU plus bus (ticket W6-02a; FR-CORE-035).
//!
//! ## Loading a cartridge, and refusing one
//!
//! [`SnesSystem::load`] goes through `rf-cart`, which already detects
//! enhancement chips and unsupported map modes and reports them as
//! [`rf_cart::CartError::UnsupportedChip`] — the FR-CORE-013 diagnostic.
//! This crate does not re-implement that detection; it propagates it. The
//! requirement is "fail with a diagnostic naming the chip, **never a
//! crash**", so the failure has to arrive as a `Result` from the entry
//! point a front-end actually calls, which is here rather than three
//! layers down.

use rf_cart::{CartError, Cartridge, SnesMapMode};

use crate::bus::SnesBus;
use crate::cpu::{access_cycles, Cpu, CpuBus};

/// A whole SNES.
pub struct SnesSystem {
    pub cpu: Cpu,
    pub bus: SnesBus,
    /// Master cycles elapsed since reset.
    pub master_cycles: u64,
    /// An NMI edge seen but not yet dispatched.
    pub(crate) pending_nmi: bool,
    /// Diagnostic only, not part of save state (ticket W14-24): the most
    /// recently executed instruction's bus-access count and the master
    /// cycles charged for it. Exists so a probe can measure exactly what
    /// [`Self::step`] feeds `SnesBus::tick_math`, rather than guessing —
    /// see `MathUnit::tick`'s doc for why that distinction matters.
    pub last_instr_accesses: u64,
    /// See [`Self::last_instr_accesses`].
    pub last_instr_master_cycles: u64,
}

impl SnesSystem {
    /// Load a cartridge image.
    ///
    /// # Errors
    /// Returns the `rf-cart` diagnostic for an unparseable header, an
    /// unsupported map mode, or an enhancement chip this core does not
    /// implement (FR-CORE-013 / FR-CORE-035).
    pub fn load(raw: &[u8]) -> Result<Self, CartError> {
        let cart = Cartridge::load(raw)?;
        let Cartridge::Snes { header, .. } = cart else {
            return Err(CartError::InvalidHeader(
                "not a SNES cartridge (an iNES header was found)".to_string(),
            ));
        };

        // A copier header shifts every offset by 512 bytes. rf-cart
        // reports whether it stripped one; the ROM image the bus maps
        // must be stripped to match, or every address is 512 bytes wrong
        // — which would look like a mapping bug rather than a header one.
        let rom = if header.had_copier_header {
            raw[512..].to_vec()
        } else {
            raw.to_vec()
        };

        // An SA-1 cart's `ram_size` is BW-RAM, which `install_sa1` below
        // gives its own buffer (`Sa1State::bwram`) sized from the same
        // header field — the bus's generic `sram` is unreachable for such
        // a cart (`SnesBus::target` routes through `sa1_target` first,
        // and `map`'s own `SnesMapMode::Sa1` arm never returns `Sram`), so
        // it is left empty rather than duplicating the allocation.
        // Ticket W18-01: a GSU cart's `ram_size` is the STANDARD header
        // field, always `$00` per fullsnes's own note ("[FFD8h]=00h Normal
        // SRAM Size (None) (always use the Expansion entry)") — its real
        // RAM comes from `Coprocessor::SuperFx`'s own `ram_kib`, given to
        // `install_gsu` below, so `sram_len` is left `0` here the same way
        // it is for SA-1.
        let sram_len = if matches!(
            header.coprocessor,
            rf_cart::Coprocessor::Sa1(_) | rf_cart::Coprocessor::SuperFx { .. }
        ) {
            0
        } else {
            header.ram_size
        };
        let mut system = Self {
            cpu: Cpu::new(),
            bus: SnesBus::new(rom, sram_len, header.map_mode),
            master_cycles: 0,
            pending_nmi: false,
            last_instr_accesses: 0,
            last_instr_master_cycles: 0,
        };
        system.bus.fast_rom = header.fast_rom;
        // DSP-1 HLE (ticket W14-19; D-010, FR-CORE-038): `rf-cart`
        // (W14-18) already parsed the chip's bus window from the header;
        // this is the one place that turns it into a running chip. Every
        // other cartridge's `header.dsp_window` is `None`, so
        // `install_dsp1` is never called and `SnesBus::target` never
        // routes through the DSP arm — existing goldens are unaffected.
        if let (rf_cart::Coprocessor::Dsp1, Some(window)) = (header.coprocessor, header.dsp_window)
        {
            system.bus.install_dsp1(window);
        }
        // SA-1 slice 1 (ticket W17-01; D-013): `rf-cart` already parsed
        // the board's sizes from the header; this wires them into a live
        // I-RAM/BW-RAM/register-window state. The SA-1 CPU itself does
        // not exist yet (W17-02) — the SNES CPU runs the cart's SNES-side
        // code alone, which is what makes the census's SA-1 titles move
        // from refused to "uniform" rather than to "renders": nothing
        // drives the second CPU that owns the interesting work yet.
        if let rf_cart::Coprocessor::Sa1(board) = header.coprocessor {
            system.bus.install_sa1(board);
        }
        // Super FX slice 1 (ticket W18-01, D-014): `rf-cart` already
        // parsed the chip version and RAM size from the header; this
        // wires them into a live register-window/RAM state. The GSU CPU
        // itself does not exist yet (W18-02+) — the SNES CPU runs the
        // cart's SNES-side code alone, exactly as SA-1 slice 1 did.
        if let rf_cart::Coprocessor::SuperFx { version, ram_kib } = header.coprocessor {
            system.bus.install_gsu(version, header.rom_size, ram_kib);
        }
        // CX4 (ticket W19-02): `rf-cart` already identified the chip from
        // the header's chipset byte + extended-header sub-type; this wires
        // up the register window/CX4RAM/CX4ROM state. The CX4's own
        // program never executes (see `crate::cx4`'s module doc for why
        // fullsnes's chapter does not document enough to run it) — the
        // SNES CPU runs the cart's SNES-side code alone, same starting
        // point SA-1/GSU slice 1 gave those chips.
        if matches!(header.coprocessor, rf_cart::Coprocessor::Cx4) {
            system.bus.install_cx4();
        }
        system.reset();
        Ok(system)
    }

    /// Build a system directly from a ROM image and map mode, bypassing
    /// header detection.
    ///
    /// For tests that want a specific mapping regardless of what a header
    /// claims. Not the path a front-end should use — [`Self::load`] is,
    /// because it is the one that produces the FR-CORE-013 diagnostic.
    /// Select NTSC or PAL timing (ticket W7-10).
    ///
    /// **Call before running.** The region changes how many scanlines a
    /// frame has, so switching mid-frame would leave the clock partway
    /// through a frame length it no longer has — harmless for NTSC->PAL
    /// (the frame just gets longer) and a skipped vblank for PAL->NTSC,
    /// which is exactly the kind of one-off glitch that is impossible to
    /// reproduce later.
    ///
    /// `docs/design/EMULATION_CORES.md` §3 is NTSC-first and names PAL a
    /// Phase 7 config; a fresh system is NTSC.
    pub fn set_region(&mut self, region: crate::timing::Region) {
        self.bus.timing.region = region;
    }

    /// The active region.
    #[must_use]
    pub fn region(&self) -> crate::timing::Region {
        self.bus.timing.region
    }

    #[must_use]
    pub fn from_rom(rom: Vec<u8>, mode: SnesMapMode, sram_len: usize) -> Self {
        let mut system = Self {
            cpu: Cpu::new(),
            bus: SnesBus::new(rom, sram_len, mode),
            master_cycles: 0,
            pending_nmi: false,
            last_instr_accesses: 0,
            last_instr_master_cycles: 0,
        };
        system.reset();
        system
    }

    /// Reset: emulation mode, and PC from the vector at `$00:FFFC`.
    ///
    /// The vector is fetched THROUGH THE MAPPING, not read from a file
    /// offset. For a 32 KiB LoROM that only resolves because undersized
    /// ROMs mirror — which makes this the first thing that breaks if the
    /// mirroring is wrong, and a useful canary.
    pub fn reset(&mut self) {
        self.cpu = Cpu::new();
        let lo = self.bus.read(0x00_FFFC);
        let hi = self.bus.read(0x00_FFFD);
        self.cpu.pc = u16::from(lo) | (u16::from(hi) << 8);
        self.cpu.pbr = 0;
        self.master_cycles = 0;
        self.pending_nmi = false;
        self.bus.timing = crate::timing::Timing::new();
    }

    /// Execute one instruction, charging its bus accesses in master
    /// cycles and servicing any DMA it armed.
    ///
    /// # Errors
    /// Returns the opcode if the CPU does not implement it. As of W6-01b
    /// all 256 are implemented, so this cannot currently happen — see the
    /// note on `ops::execute`'s catch-all arm.
    pub fn step(&mut self) -> Result<(), u8> {
        // Ticket W17-04: cleared before the main CPU's share of this step
        // so `sa1.step`'s cost model sees only THIS step's contention —
        // see `SnesBus::sa1_rom_contended`'s doc.
        self.bus.sa1_rom_contended = false;
        self.bus.sa1_bwram_contended = false;
        let fast_rom = self.bus.fast_rom;
        // Captured BEFORE `Cpu::step`, not after: an instruction that
        // itself halts the CPU (`WAI`/`STP`) must still be charged its
        // own real cycles (ticket W14-39's `internal_cycles` among them)
        // — only a step that was ALREADY halted on entry gets the flat
        // one-cycle idle credit below. Checking `self.cpu.stopped` after
        // the call would charge the halt-triggering instruction nothing,
        // since `stopped` is already `true` by the time this line reads
        // it.
        let was_halted = self.cpu.stopped;
        let mut counting = crate::cpu::AccessCost::new(&mut self.bus, fast_rom);
        let result = self.cpu.step(&mut counting);
        // **A HALTED CPU STILL BURNS TIME** (ticket W7-15).
        //
        // `Cpu::step` returns immediately while `stopped` (WAI or STP)
        // without touching the bus, so `AccessCost` counts nothing and
        // this used to add zero. That is a deadlock, not an optimisation:
        // WAI waits for an interrupt, every interrupt this machine can
        // raise comes from the raster, and the raster only moves when
        // master cycles are spent. The CPU waited for a vblank that could
        // never arrive.
        //
        // Measured before the fix: undisbeliever's hdmaen_latch_test ran
        // 3,000,000 instructions and advanced the clock by 6,840 master
        // cycles — still on frame 0, still in forced blank, spinning at
        // $80:8069 forever. That is the whole reason all eighteen of
        // those ROMs looked like they "never leave forced blank", and it
        // was read as a per-dot rendering limitation for two tickets.
        //
        // One internal CPU cycle (`speed::FAST`, 6 master cycles) per
        // halted step: on hardware WAI does not access the bus, but the
        // clock keeps running, and 6 is what an internal cycle costs.
        //
        // Ticket W14-39: a RUNNING instruction now also charges its real
        // internal (no-bus-access) cycles, from `Cpu::internal_cycles` —
        // computed by `cpu::cycles::internal_cycles` from the WDC W65C816S
        // datasheet's per-opcode/addressing-mode penalties and pinned
        // exactly against the SingleStepTests vectors' `cycles` arrays
        // (`cpu/tests/vectors.rs`). This is what W14-24 and W14-28 were
        // each compensating for locally (see their superseded credits,
        // removed below) and what left the CPU's pacing against the
        // raster and the APU roughly 47% too fast — the root of the
        // W14-33/W14-38 APU handshake deadlock family.
        let internal = if was_halted {
            0
        } else {
            u64::from(self.cpu.internal_cycles) * u64::from(crate::cpu::speed::FAST)
        };
        let spent = if was_halted {
            u64::from(crate::cpu::speed::FAST)
        } else {
            counting.master_cycles + internal
        };
        self.master_cycles += spent;
        self.last_instr_accesses = counting.accesses;
        self.last_instr_master_cycles = spent;

        // The math unit is "clocked by the CPU Clock" (fullsnes "SNES
        // Maths Multiply/Divide": "one needs the same amount of 'wait'
        // opcodes no matter if the CPU Clock is 3.5MHz or 2.6MHz") — a
        // real CPU-CYCLE count, not a master-cycle one. `MathUnit::tick`
        // now takes that count directly (ticket W14-39; see its doc for
        // what changed from the master-cycle/6 re-bucketing this
        // supersedes). A CPU cycle is one bus access OR one internal
        // cycle; `counting.accesses` and `self.cpu.internal_cycles` are
        // exactly those two counts for this instruction. The W7-15
        // halted credit and the W14-28 single-access credit are both
        // superseded by real accounting: a halted step still advances
        // the math unit by one CPU cycle (hardware's clock does not stop
        // just because the CPU is waiting), and a running step advances
        // it by its true cycle count, accesses plus internal, with no
        // separate credit needed.
        let math_cycles = if was_halted {
            1
        } else {
            counting.accesses + u64::from(self.cpu.internal_cycles)
        };
        self.bus.tick_math(math_cycles as u32);
        // The APU accrues debt with every master cycle the CPU spends —
        // DMA included, because a transfer stalls the CPU and not the
        // sound chip — and is settled at the end of every instruction
        // (ticket W14-09; EMULATION_CORES.md §1, catch-up to the master
        // cycle). It is never a free-running thread, but it was never
        // meant to run only while the CPU stares at a port either.
        self.bus.apu_debt += spent;
        let dma_cycles = self.bus.service_dma();
        self.master_cycles += dma_cycles;
        self.bus.apu_debt += dma_cycles;
        self.bus.catch_up_apu();

        // Advance the frame clock by everything this instruction spent,
        // DMA included — DMA halts the CPU but the raster keeps going,
        // and a model that froze time during a transfer would let a game
        // DMA through vblank without ever leaving it.
        let irq_mode = self.bus.nmitimen.irq_mode();
        let auto_joypad = self.bus.nmitimen.auto_joypad();
        let timer = self.bus.irq;
        // $2133's overscan bit moves the vblank boundary, and Timing owns
        // that boundary while the PPU owns the register. Pushed here, once
        // per write, rather than read every tick.
        if self.bus.ppu.overscan_changed {
            self.bus.ppu.overscan_changed = false;
            let overscan = self.bus.ppu.setini.overscan;
            self.bus.timing.set_overscan(overscan);
        }
        let events = self
            .bus
            .timing
            .advance(spent + dma_cycles, auto_joypad, |dot, line| {
                timer.matches(irq_mode, dot, line)
            });

        if events.irq {
            self.bus.irq.fired = true;
        }
        // HDMA: re-initialise at the top of each frame, then run one unit
        // per visible scanline. Its cost is charged like MDMA's, because
        // it steals the same bus.
        if events.frame_started {
            // Ticket W13-02c: the lane view shows THIS frame, so the
            // record resets where HDMA itself re-initialises.
            self.bus.clear_hdma_lanes();
            self.bus.hdma_init();
            // Ticket W14-31: swap the just-elapsed frame's per-line
            // records into `completed_*` rather than wiping them — a
            // `Step::Frame`/`render_frame` composer only regains control
            // AFTER this instruction, so a plain clear here destroyed
            // every mid-frame register write's attribution before
            // anything could read it. See `Ppu::advance_line_state`.
            self.bus.ppu.advance_line_state();
            // Ticket W13-02h: the frame boundary, in the order
            // `CoreEvent::FrameEnd`/`FrameStart` document — "after the
            // last scanline" then "before the first" — both landing at
            // this one instant, exactly as rf-nes emits them.
            if self.bus.wants(rf_core_api::EventMask::FRAME_END) {
                self.bus.queue_event(rf_core_api::CoreEvent::FrameEnd);
            }
            if self.bus.wants(rf_core_api::EventMask::FRAME_START) {
                self.bus.queue_event(rf_core_api::CoreEvent::FrameStart);
            }
        }
        for _ in 0..events.visible_lines_crossed {
            // Ticket W13-02h: one `Scanline` per visible line crossed,
            // carrying the line it is about — the same payload contract
            // rf-nes's own `CoreEvent::Scanline` has. Queued before the
            // line's HDMA so the event marks the line's start.
            if self.bus.wants(rf_core_api::EventMask::SCANLINE) {
                let line = self.bus.timing.line;
                self.bus.queue_event(rf_core_api::CoreEvent::Scanline(line));
            }
            self.master_cycles += self.bus.hdma_run_line();
            // Latch AFTER this line's HDMA: the transfer that happens in
            // the preceding hblank is what this line is drawn with.
            let line = self.bus.timing.line;
            self.bus.ppu.latch_line(line);
        }
        if events.auto_joypad_done {
            self.bus.joypads.latch();
        }

        // Delivery. NMI is edge-triggered on the vblank transition and
        // ignores the I flag; IRQ is level-ish and masked by it.
        // Ticket W13-02h: the vblank edge is reported whether or not the
        // ROM enabled NMI — a subscriber is watching the machine, not the
        // program's interrupt configuration.
        if events.vblank_started && self.bus.wants(rf_core_api::EventMask::VBLANK_START) {
            self.bus.queue_event(rf_core_api::CoreEvent::VblankStart);
        }
        // W14-47 follow-up (2026-09-20): NMI dispatches ONLY on the
        // `$4210` vblank flag's own 0-to-1 edge while NMI is already
        // enabled — the pre-W14-47 rule, reinstated after W14-47's
        // broader "either operand" reading of fullsnes's AND-edge
        // sentence was refuted by real, shipped ROMs.
        //
        // fullsnes "SNES Interrupts": "The CPU includes another internal
        // NMI flag, which gets set when '[4200h].7 AND [4210h].7' changes
        // from 0-to-1" reads, taken literally, as licensing a dispatch
        // from the ENABLE operand's rise too (a `$4200` write turning bit
        // 7 on while `$4210` bit 7 is already stale) — W14-35 named this
        // shape and W14-47 shipped it. Three real titles were traced
        // end-to-end against main (no such rule) to check it, and all
        // three refute it:
        //
        // - **The Terminator (USA)**: boots with NMI off, polls `$4210`
        //   in software, then writes `$4200: 00->A1` at n=2,143,181
        //   (line 225 dot 90) while `$4210` bit 7 is stale from dot 7 of
        //   the SAME vblank — its first-ever enable. Main does not
        //   dispatch there; it waits for the next real vblank edge
        //   (n=2,153,414) and renders correctly forever after. Dispatching
        //   immediately (W14-47) OR one instruction later (tried during
        //   this follow-up, to model bsnes's `irqLock`/`nmiTransition`
        //   split in `sfc/cpu/irq.cpp`) both run the NMI handler before
        //   the ROM's own boot sequence is ready for it; its handler
        //   responds by disabling NMI for good, hanging on a
        //   handler-only frame counter (`$00:D67D CMP $00003C`) that
        //   never advances again.
        // - **Super Black Bass (USA)**: the same shape, `$4200: 00->81`
        //   at n=17,504 with `$4210` stale since 2,500+ master cycles
        //   earlier — too old for any plausible instruction-pipeline
        //   delay to excuse. Main waits 51,426 more steps for the real
        //   edge (n=68,930). Firing here (with or without a one-step
        //   defer) is a full extra, unscheduled vblank early; the ROM's
        //   handler disables NMI and never re-enables it.
        // - **Magical Drop II (USA, Europe) (Switch Online)**: refutes
        //   even fullsnes's OWN narrower "disable and re-enable" wording
        //   — this ROM disables and re-enables bit 7 EVERY single frame
        //   as routine practice (`$4200: 81->01` then `01->81` a few
        //   dots apart, every vblank, e.g. n=1,040,797/1,040,812) while
        //   `$4210` stays unread and set the whole time. Main (which has
        //   no enable-edge rule at all, so this toggle is a no-op to it)
        //   dispatches on only 68 of the 121 vblanks it sees — the
        //   ordinary flag edge sometimes lands mid-toggle and is simply
        //   missed, same as real hardware would. Gating a redispatch on
        //   "has enable ever been true before" (tried during this
        //   follow-up, matching fullsnes's sentence literally) fires
        //   EVERY frame for this title instead, once enabled — far more
        //   dispatches than main ever produces, and it goes uniform.
        //
        // Zero of the five titles the original acceptance brief or this
        // follow-up traced were EVER explained or fixed by any version of
        // the enable-edge rule; the census only ever regressed under it.
        // See `docs/TESTING.md`'s dated entry for the full census diff.
        if events.vblank_started && self.bus.nmitimen.nmi_enabled() {
            self.pending_nmi = true;
        }
        // Ticket W17-02 acceptance #3: "the SNES CPU's IRQ line ORed with
        // the SA-1-raised IRQ". `$2209` bit 7 (SCNT) is a second, level
        // IRQ source gated by `$2201` bit 7 (SIE); it is otherwise
        // delivered exactly like the timer IRQ already was. `None` for
        // every non-SA-1 cartridge, so this changes nothing for them.
        let sa1_irq_to_snes = self
            .bus
            .sa1
            .as_ref()
            .is_some_and(|s| s.regs.snes_irq_pending());
        // Ticket W18-01 acceptance #2: the GSU's SFR bit 15 (IRQ) ORed
        // into the same 65C816 IRQ input, the same pattern as SA-1's
        // `$2209` bit 7 above — fullsnes documents no vector-override
        // register for the GSU (unlike SA-1's `$220E`/`$220F`), so a GSU
        // IRQ always dispatches through the ROM's own IRQ vector. `None`
        // for every non-GSU cartridge. Nothing sets this yet this slice
        // (no STOP opcode runs — see `crate::gsu::Gsu::irq_pending`'s
        // doc), so this is always `false` in practice until W18-02+, but
        // the OR plumbing itself is what this ticket's acceptance pins.
        let gsu_irq_to_snes = self.bus.gsu.as_ref().is_some_and(|g| g.regs.irq_pending());
        if self.pending_nmi {
            self.pending_nmi = false;
            // `$2209` bit 4 optionally redirects the SNES's own NMI vector
            // to `$220C`/`$220D` (fullsnes "...on SA-1 Side": this is a
            // vector override, not a second NMI source — the SNES's own
            // vblank NMI is still what fires).
            match self
                .bus
                .sa1
                .as_ref()
                .and_then(|s| s.regs.snes_nmi_vector_override())
            {
                Some(vector) => self.cpu.interrupt_to_vector(&mut self.bus, vector),
                None => self.cpu.interrupt(&mut self.bus, true),
            }
        } else if (self.bus.irq.fired || sa1_irq_to_snes || gsu_irq_to_snes)
            && !self.cpu.flag(crate::cpu::flags::I)
        {
            // Same override rule as NMI above, for `$2209` bit 6 / `$220E`-`$220F`.
            match self
                .bus
                .sa1
                .as_ref()
                .and_then(|s| s.regs.snes_irq_vector_override())
            {
                Some(vector) => self.cpu.interrupt_to_vector(&mut self.bus, vector),
                None => self.cpu.interrupt(&mut self.bus, false),
            }
        } else if (self.bus.irq.fired || sa1_irq_to_snes || gsu_irq_to_snes)
            && self.cpu.flag(crate::cpu::flags::I)
            && self.cpu.wai
        {
            // W14-28: an IRQ that is masked by `I` is never dispatched
            // (the branch above), but per the WDC W65C816S datasheet
            // `WAI` does not wait for a *dispatched* interrupt -- it
            // waits for the interrupt LINE, and resumes "with the next
            // instruction" (not the handler) when that line asserts
            // while `I` is set. Without this, a title that legitimately
            // executes `WAI` with `I` set (common for the H/V-IRQ-only
            // wait idiom, since NMI needs no unmasking) parks forever the
            // first time only the masked IRQ -- never NMI -- fires:
            // `self.cpu.stopped` is the CPU's only "am I running" bit,
            // and nothing upstream of this `else if` ever clears it for
            // that case. Traced in Full Throttle - All-American Racing
            // (USA) (Beta): `$81:CB94` `WAI` with `NMITIMEN`'s H/V mode
            // enabled and `I` set, parked at frame 108 (docs/TESTING.md,
            // W14-28). `STP` (`cpu.wai == false`) is deliberately excluded
            // -- it wakes only on reset, never on an interrupt line.
            self.cpu.stopped = false;
            self.cpu.wai = false;
        }

        // Ticket W17-02 acceptance #2: interleave the SA-1 on the master
        // clock this instruction (DMA included, the same "spent" total the
        // raster above advances by) rather than running it on its own,
        // unsynchronised loop. Credit never accumulates while Reset is
        // held — see `Sa1State::credit`'s doc — so this loop's iteration
        // count is bounded by `(spent + dma_cycles) / 2` (the cheapest
        // possible SA-1 access cost), which proves it terminates without
        // an artificial cap (law 8): `cost` is always at least 2 UNLESS
        // Wait holds the core (`Sa1State::step` then returns 0 having done
        // nothing but the one-time reset-vector fetch), and that case is
        // handled by the explicit `break` below rather than relying on
        // `cost` alone — a `while credit > 0` loop whose body can return 0
        // is exactly the memory-bomb shape law 8 warns about, so the Wait
        // check runs every pass, before the loop would ever see a second
        // zero-cost iteration.
        let master_this_step = spent + dma_cycles;
        // Disjoint field borrows (`bus.rom` alongside `bus.sa1`), not a
        // clone of a multi-megabyte ROM every instruction.
        let dot = self.bus.timing.dot();
        let line = self.bus.timing.line;
        // Ticket W17-04: settled after the CPU instruction, its MDMA and
        // its HDMA line(s) have all had their chance to touch ROM/BW-RAM —
        // see `SnesBus::sa1_rom_contended`'s doc for why this is read here
        // rather than passed piecemeal.
        let rom_contended = self.bus.sa1_rom_contended;
        let bwram_contended = self.bus.sa1_bwram_contended;
        let bus = &mut self.bus;
        if let Some(sa1) = bus.sa1.as_mut() {
            // Ticket W17-03: the timer runs off the master clock
            // regardless of whether the SA-1 core itself is held (Reset
            // still holds, matching the SA-1 CPU's own held state below —
            // fullsnes documents no separate gate for the timer, and a
            // held CPU cannot read `$2302`/`$230x` anyway).
            if !sa1.regs.sa1_reset_asserted() {
                sa1.regs.tick_timer(master_this_step, dot, line);
            }
            if sa1.regs.sa1_reset_asserted() {
                sa1.credit = 0;
            } else {
                sa1.credit += master_this_step;
                while sa1.credit > 0 {
                    match sa1.step(&bus.rom, rom_contended, bwram_contended) {
                        Ok(cost) => sa1.credit = sa1.credit.saturating_sub(cost),
                        Err(_opcode) => break,
                    }
                    // Checked AFTER every step, not just once before the
                    // loop: `step` itself may be what just asserted a
                    // reset-to-running transition (booting), and Wait can
                    // be asserted mid-run by the very code the SA-1 is
                    // executing.
                    if sa1.regs.sa1_reset_asserted() || sa1.regs.sa1_wait_asserted() {
                        sa1.credit = 0;
                        break;
                    }
                }
            }
        }

        // Ticket W18-04 (D-014, slice 4 of 5): interleave the GSU on the
        // master clock exactly like SA-1's credit loop above — same
        // `master_this_step` total (the 65C816 instruction plus its
        // MDMA/HDMA). `bus.rom` is borrowed the same disjoint-field way
        // the SA-1 loop above already borrows it alongside `bus.sa1`.
        if let Some(gsu) = bus.gsu.as_mut() {
            gsu.run_credited(&bus.rom, master_this_step);
        }

        result
    }

    /// Run up to `max_instructions`, stopping early if the CPU halts
    /// (`WAI`/`STP`) or reaches `stop_pc` in bank 0.
    ///
    /// Returns how many instructions actually ran. A bounded runner
    /// rather than a loop: a fixture that never reaches its end must fail
    /// the test, not hang it.
    ///
    /// # Errors
    /// Propagates an unimplemented opcode.
    pub fn run_until(&mut self, max_instructions: u64, stop_pc: Option<u16>) -> Result<u64, u8> {
        for n in 0..max_instructions {
            if self.cpu.stopped {
                return Ok(n);
            }
            if stop_pc == Some(self.cpu.pc) && self.cpu.pbr == 0 {
                return Ok(n);
            }
            self.step()?;
        }
        Ok(max_instructions)
    }

    /// Run until the start of the next vblank, then compose the visible
    /// frame.
    ///
    /// Rendering at vblank rather than mid-frame is what makes the result
    /// stable: a ROM sets registers during vblank and expects them to
    /// hold for the frame, so composing at any other moment can catch a
    /// half-updated tilemap.
    ///
    /// # Errors
    /// Propagates an unimplemented opcode.
    pub fn render_frame(&mut self, max_instructions: u64) -> Result<Vec<Vec<u8>>, u8> {
        // Get into vblank...
        let start = self.bus.timing.frame;
        let mut n = 0;
        while !self.bus.timing.in_vblank() && n < max_instructions {
            self.step()?;
            n += 1;
        }
        // ...and out again, so composition happens on a settled frame.
        while (self.bus.timing.in_vblank() || self.bus.timing.frame == start)
            && n < max_instructions
        {
            self.step()?;
            n += 1;
        }

        // 224 or 239 lines, as SETINI asks (ticket W7-06). A frame that
        // always emitted 224 would silently crop the bottom 15 lines of
        // an overscan game rather than letterbox it.
        let visible = self.bus.ppu.setini.visible_lines();
        let mut frame = Vec::with_capacity(usize::from(visible));
        for y in 0..visible {
            frame.push(
                self.bus
                    .ppu
                    .render_scanline(y)
                    .pixels
                    .iter()
                    .map(|p| p.palette_index)
                    .collect(),
            );
        }
        Ok(frame)
    }

    /// CGRAM as RGB888, for tests and tools that need to look at a frame.
    ///
    /// CGRAM is BGR555; the 5-bit channels are scaled to 8 bits by
    /// replicating the high bits (`v << 3 | v >> 2`) rather than shifting
    /// alone, so full-scale input maps to full-scale output instead of
    /// topping out at 248.
    #[must_use]
    pub fn palette_rgb(&self) -> Vec<[u8; 3]> {
        self.bus
            .ppu
            .cgram
            .iter()
            .map(|&c| {
                let ch = |v: u16| ((v & 0x1F) as u8) << 3 | ((v & 0x1F) as u8) >> 2;
                [ch(c), ch(c >> 5), ch(c >> 10)]
            })
            .collect()
    }

    /// Master cycles one access at `addr` would cost right now.
    #[must_use]
    pub fn access_cost(&self, addr: u32) -> u8 {
        access_cycles(addr, self.bus.fast_rom)
    }
}
