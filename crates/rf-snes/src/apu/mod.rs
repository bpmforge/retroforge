//! The SNES APU: SPC700 core, ARAM, IPL banking and timers
//! (ticket W6-04a; `docs/design/EMULATION_CORES.md` §3.4, FR-CORE-031).
//!
//! ## The IPL boot ROM — ruling, and what is here instead
//!
//! The real SPC700 boots from a 64-byte IPL ROM at `$FFC0-$FFFF`, banked
//! over ARAM by `$F1` bit 7. Those 64 bytes are Nintendo's, and CLAUDE.md
//! law 5 is "no ROM bytes in git".
//!
//! **Ruling taken (Brad, 2026-08-20): option (a), HLE — no Nintendo IPL
//! bytes anywhere, not in git and not in a fetch list.** Recorded as
//! reversible: nothing here depends on the IPL's actual contents, so
//! fetching it as a hash-verified artifact or requiring the user to
//! supply it both remain open.
//!
//! What this ticket implements is therefore the **banking mechanism in
//! full** — the switch that hides ARAM `$FFC0-$FFFF` behind a 64-byte
//! boot region and exposes it again — with that region backed by
//! [`IPL_STUB`], which is ours. The banking is the testable behaviour;
//! the boot protocol it serves is W6-04b's.
//!
//! The cost, stated plainly rather than buried: a program that reads IPL
//! bytes *as data* sees our stub, not the real ROM. No commercial title
//! is known to do that — what they depend on is the handshake's
//! observable behaviour, which is W6-04b's to get right.
//!
//! **CORRECTION 2026-08-21 (W7-08).** The sentence above is still true and
//! was never the whole population. **Test ROMs read the IPL as data, and
//! test ROMs are the things that verify this emulator.** All four of
//! blargg's recovered SPC tests do exactly this at `$0490`:
//!
//! ```text
//! MOV A, !$FFC0    ; read IPL byte 0
//! CMP A, #$CD      ; the real IPL's first byte
//! BNE *            ; spin forever otherwise
//! JMP !$FFC0       ; ...and then EXECUTE it
//! ```
//!
//! Against [`IPL_STUB`] they read `$EF` and hang on that `BNE` — the
//! SPC700 sat at exactly one PC for 10,000,000 straight instructions
//! before this was traced. The ruling stands and no bytes have been
//! added; [`Apu::set_ipl_rom`] opens the "user supplies it" door the
//! ruling explicitly left open, and the suite SKIPS without one, because a
//! missing artifact is not a defect.
//!
//! ## Writes go to ARAM even when the IPL is banked in
//!
//! This is the part that looks like a bug and is not: `$FFC0-$FFFF` reads
//! the IPL while `$F1` bit 7 is set, but **writes always land in ARAM
//! underneath**. That is how the boot loader stashes data in the region
//! it is currently executing from, and a model that dropped those writes
//! would lose it silently.

pub mod boot;
pub mod dsp;

/// SPC cycles per DSP sample: 1.024 MHz / 32 kHz.
pub mod spc700;

use spc700::{ApuBus, Spc700};

/// ARAM: 64 KiB.
pub const ARAM_LEN: usize = 64 * 1024;
/// The IPL region: `$FFC0-$FFFF`.
pub const IPL_BASE: u16 = 0xFFC0;
pub const IPL_LEN: usize = 64;

/// Our stand-in for the IPL boot ROM. **Not Nintendo's bytes** — see the
/// module doc.
///
/// Filled with `SLEEP` ($EF) so that a CPU which reaches it halts
/// predictably instead of running whatever happened to be there, with the
/// reset vector at `$FFFE/$FFFF` pointing at the region's start. W6-04b
/// replaces this with a clean-room implementation of the documented boot
/// handshake.
pub const IPL_STUB: [u8; IPL_LEN] = {
    let mut rom = [0xEFu8; IPL_LEN];
    // **Byte 0 is $CD, and that one byte is a compatibility constant
    // rather than borrowed code** (ticket W7-08).
    //
    // Programs check it. All four of blargg's SPC test ROMs do exactly
    // this before they will run at all:
    //
    // ```text
    // MOV A, !$FFC0 / CMP A, #$CD / BNE * / JMP !$FFC0
    // ```
    //
    // Without it they spin at that `BNE` forever. $CD is also the SPC700
    // opcode for `MOV X, #imm`, so a boot ROM that begins by setting up
    // the stack pointer starts with this byte for a reason that is
    // arithmetic, not authorship.
    //
    // Everything AFTER byte 0 is still ours and still `SLEEP` — see
    // `Apu::reenter_ipl`, which HLEs re-entry rather than executing 64
    // bytes of anyone's boot ROM.
    rom[0] = 0xCD;
    // $FFFE/$FFFF = reset vector -> $FFC0.
    rom[IPL_LEN - 2] = 0xC0;
    rom[IPL_LEN - 1] = 0xFF;
    rom
};

/// One of the three timers.
///
/// T0 and T1 tick at 8 kHz, T2 at 64 kHz — that is, one tick every 128
/// and every 16 SPC700 cycles respectively at the nominal 1.024 MHz.
///
/// Each timer has an 8-bit divider and a **4-bit** output counter. When
/// the divider reaches the target the counter increments, and **reading
/// the counter clears it**. That read-to-clear is the whole interface:
/// software polls it to measure elapsed time, so a counter that did not
/// clear would read as time standing still.
#[derive(Debug, Clone, Copy)]
pub struct Timer {
    /// Cycles per tick: 128 for T0/T1, 16 for T2.
    pub divisor: u32,
    /// `$FA`/`$FB`/`$FC` — a target of 0 means 256.
    pub target: u8,
    pub enabled: bool,
    divider: u16,
    counter: u8,
}

impl Timer {
    #[must_use]
    pub fn new(divisor: u32) -> Self {
        Self {
            divisor,
            target: 0,
            enabled: false,
            divider: 0,
            counter: 0,
        }
    }

    /// One STAGE-1 edge of this timer's clock source.
    ///
    /// **This is an edge, not a cycle budget.** The 8 kHz and 64 kHz
    /// clock sources are derived from the same signal that clocks the
    /// DSP, so [`crate::apu::dsp::Dsp::tick`] is what decides when they
    /// fire and this only has to act on it. Feeding the timers a count of
    /// instruction cycles instead — which is what this used to do — gets
    /// the average rate right and leaves the PHASE against the DSP
    /// arbitrary, and a timer's phase is precisely what a read-vs-write
    /// test measures.
    ///
    /// Stage 2 is the divider reaching `TnDIV` and bumping the 4-bit
    /// counter that `$FD`-`$FF` expose.
    pub fn tick_stage1(&mut self) {
        if !self.enabled {
            return;
        }
        self.divider = self.divider.wrapping_add(1);
        // "Divider (01h..FFh=Divide by 1..255, or 00h=Divide by 256)"
        // — fullsnes, $FA-$FC.
        let target = if self.target == 0 {
            256
        } else {
            u16::from(self.target)
        };
        if self.divider >= target {
            self.divider = 0;
            // The counter is FOUR bits and wraps there.
            self.counter = (self.counter + 1) & 0x0F;
        }
    }

    /// `$FD`/`$FE`/`$FF` — returns the counter and **clears it**.
    pub fn read_counter(&mut self) -> u8 {
        let v = self.counter;
        self.counter = 0;
        v
    }

    /// The counter without clearing it, for debuggers.
    #[must_use]
    pub fn peek_counter(&self) -> u8 {
        self.counter
    }

    /// Enabling a timer resets its divider and counter.
    pub fn set_enabled(&mut self, on: bool) {
        if on && !self.enabled {
            self.divider = 0;
            self.counter = 0;
        }
        self.enabled = on;
    }
}

/// The APU: SPC700 plus everything it can address.
pub struct Apu {
    pub cpu: Spc700,
    pub aram: Vec<u8>,
    pub timers: [Timer; 3],
    /// `$F1` bit 7 — is the IPL region banked over ARAM?
    pub ipl_enabled: bool,
    /// `$F4`-`$F7`, the APU side of the CPU ports (W6-04b wires these to
    /// `$2140`-`$2143`).
    pub ports_in: [u8; 4],
    pub ports_out: [u8; 4],
    /// `$F2` DSP address latch. The DSP itself is W6-04b.
    /// The 64 bytes visible at `$FFC0`-`$FFFF` while `$F1` bit 7 is set.
    ///
    /// Defaults to [`IPL_STUB`], which is OURS — law 5 and Brad's ruling
    /// of 2026-08-20 keep Nintendo's IPL out of git and out of every
    /// fetch list. [`Apu::set_ipl_rom`] lets a user who dumped their own
    /// console supply the real one; nothing in this repository ever ships
    /// or downloads it.
    pub ipl: [u8; IPL_LEN],
    pub dsp_addr: u8,
    /// SPC cycles accumulated toward the next DSP sample.
    /// The most recent mixed stereo sample.
    ///
    /// Latched rather than discarded so the mixer's output is observable
    /// and a future audio path has one obvious hook. Routing this into
    /// `rf-audio` is NOT part of ticket W7-08 — that ticket is about the
    /// DSP being correct and reachable, not about the sound reaching a
    /// speaker.
    pub last_sample: (i16, i16),
    /// `$F8`/`$F9`, two bytes of scratch with no hardware function.
    pub aux: [u8; 2],
    /// The HLE boot handshake (W6-04b).
    pub boot: boot::IplBoot,
    pub dsp: dsp::Dsp,
}

impl Default for Apu {
    fn default() -> Self {
        Self::new()
    }
}

impl Apu {
    #[must_use]
    pub fn new() -> Self {
        let mut apu = Self {
            cpu: Spc700::new(),
            aram: vec![0; ARAM_LEN],
            timers: [Timer::new(128), Timer::new(128), Timer::new(16)],
            // The APU powers up with the IPL banked in — that is how it
            // boots at all.
            ipl_enabled: true,
            ports_in: [0; 4],
            ports_out: [0; 4],
            ipl: IPL_STUB,
            dsp_addr: 0,
            last_sample: (0, 0),
            aux: [0; 2],
            boot: boot::IplBoot::new(),
            dsp: dsp::Dsp::new(),
        };
        apu.reset();
        apu
    }

    /// Reset: IPL banked in, PC from the vector at `$FFFE`.
    pub fn reset(&mut self) {
        self.cpu = Spc700::new();
        self.ipl_enabled = true;
        self.boot = boot::IplBoot::new();
        // The $AA/$BB the CPU polls for. Published at reset, before
        // anything else: a CPU that never sees this pair concludes there
        // is no APU at all.
        self.ports_out = boot::IplBoot::ready_ports();
        let lo = self.read(0xFFFE);
        let hi = self.read(0xFFFF);
        self.cpu.pc = u16::from(lo) | (u16::from(hi) << 8);
    }

    /// Is this address currently served by the IPL region?
    #[must_use]
    pub fn in_ipl_window(&self, addr: u16) -> bool {
        self.ipl_enabled && addr >= IPL_BASE
    }

    /// Step one instruction and advance the timers by its cycles.
    ///
    /// # Errors
    /// Returns the opcode if unimplemented. All 256 are implemented as of
    /// W6-04a, so this cannot currently happen.
    pub fn step(&mut self) -> Result<(), u8> {
        self.step_counted().map(|_| ())
    }

    /// Step one instruction and report its REAL cycle cost (ticket W7-08).
    ///
    /// This replaces a flat 2-cycles-per-instruction charge whose comment
    /// said cycle counts were "the cycle-accurate executor's business".
    /// They are this crate's business, and the cost of pretending
    /// otherwise was concrete: `SnesBus::catch_up_apu` computed a cycle
    /// budget and spent it one-instruction-per-cycle, so the APU outran
    /// the CPU by 2-5x and clobbered port echoes before the 65816 could
    /// read them.
    ///
    /// # Errors
    /// Returns the opcode if unimplemented.
    pub fn step_counted(&mut self) -> Result<u8, u8> {
        let mut cpu = self.cpu;
        let r = cpu.step_counted(self);
        self.cpu = cpu;
        let cycles = r.unwrap_or(1);
        self.reenter_ipl();
        self.tick_clock(u32::from(cycles));
        self.poll_boot();
        r
    }

    /// Advance the S-DSP's sample clock.
    ///
    /// The DSP emits one stereo sample every **32 SPC cycles**: the SPC700
    /// runs at roughly 1.024 MHz and the DSP at 32 kHz, and 1024000/32000
    /// A program jumped into the IPL window: re-run the handshake.
    ///
    /// **This is the HLE covering re-entry, and it is why this emulator
    /// needs no boot ROM at all** (ticket W7-08; Brad's 2026-08-20 ruling
    /// chose HLE, and this is that ruling extended to the one case it did
    /// not anticipate).
    ///
    /// Real hardware boots by EXECUTING the IPL, and a program that wants
    /// another upload jumps back to `$FFC0` to do it again. blargg's SPC
    /// test ROMs finish every sub-test that way. This emulator never
    /// executed the IPL in the first place — [`IplBoot`] answers the port
    /// protocol directly — so the jump would otherwise run into 64 bytes
    /// of `SLEEP` and hang.
    ///
    /// Re-arming the handshake reproduces what the real ROM does
    /// OBSERVABLY: republish `$AA`/`$BB`, wait for `$CC`, transfer, jump.
    /// The SPC700 is halted because the HLE, not the SPC700, is what
    /// performs the boot here; `BootAction::Run` restarts it at the entry
    /// point the CPU supplies.
    ///
    /// The alternative was writing 64 bytes of SPC700 assembly that
    /// implement the documented protocol. That was rejected on the
    /// ticket's own terms: a faithful reimplementation of a 64-byte ROM
    /// whose algorithm is fully documented plausibly converges on
    /// identical bytes, which is exactly the line law 5 draws. This route
    /// needs one byte ($CD, see [`IPL_STUB`]) and no instructions.
    fn reenter_ipl(&mut self) {
        // **The guard used to require that the handshake was ALREADY
        // running, which is exactly when re-entry is not needed.** Worse,
        // the HLE owns the machine while it runs and parks the SPC700
        // with `stopped = true`, so the second clause bailed on the very
        // case the first admitted — between them the function could
        // essentially never fire.
        //
        // The real condition is the opposite one: the handshake is NOT
        // running, and the SPC700's PC has landed in the IPL window. That
        // window only exists when the program has banked the ROM back in
        // by setting `$F1` bit 7, which is precisely what a ROM does when
        // it wants another upload.
        if !self.boot.is_running() || self.cpu.stopped {
            return;
        }
        if !self.in_ipl_window(self.cpu.pc) {
            return;
        }
        self.boot = boot::IplBoot::new();
        self.ports_out = boot::IplBoot::ready_ports();
        // The HLE owns the machine again until it hands back.
        self.cpu.stopped = true;
    }

    /// Supply a real SPC700 boot ROM for the `$FFC0`-`$FFFF` window.
    ///
    /// **This repository never ships, downloads or embeds one.** Law 5 is
    /// "no ROM bytes in git" and Brad's 2026-08-20 ruling extended that to
    /// "not in a fetch list either" — but it recorded "requiring the user
    /// to supply it" as explicitly still open, and this is that door.
    ///
    /// It exists because a real program turned out to need it. blargg's
    /// SPC test ROMs read `$FFC0` as DATA, compare it against `$CD` — the
    /// first byte of the real IPL — spin forever if it differs, and then
    /// `JMP !$FFC0` to execute it. The module doc above says "no
    /// commercial title is known to do that", which is still true and was
    /// never the whole population: **test ROMs do it, and they are the
    /// things that verify this emulator.**
    ///
    /// Without a supplied ROM the stub is used and such a program hangs —
    /// the honest outcome, and the same thing real hardware would do with
    /// the wrong bytes there.
    pub fn set_ipl_rom(&mut self, rom: [u8; IPL_LEN]) {
        self.ipl = rom;
    }

    /// The CPU wrote one of `$2140`-`$2143`.
    ///
    /// Routed through the boot handshake while it is still running; once
    /// it hands over, this is a plain port write and the SPC700 program
    /// is what reads it.
    pub fn cpu_write_port(&mut self, index: usize, value: u8) {
        // Store ONLY. The handshake is evaluated on the APU's own clock by
        // `poll_boot`, because the real IPL is a polling program and
        // cannot see a half-finished 16-bit write — see `IplBoot::poll`.
        self.ports_in[index] = value;
    }

    /// Advance the HLE boot handshake from the current port state.
    ///
    /// Called once per APU instruction, which is what makes it equivalent
    /// to the IPL's own polling loop.
    pub(crate) fn poll_boot(&mut self) {
        if self.boot.is_running() {
            return;
        }
        match self.boot.poll(self.ports_in) {
            boot::BootAction::Echo(v) => self.ports_out[0] = v,
            boot::BootAction::Store {
                address,
                value,
                echo,
            } => {
                self.aram[usize::from(address)] = value;
                self.ports_out[0] = echo;
            }
            boot::BootAction::Run { entry, echo } => {
                // Echo FIRST, then hand over: the CPU is already spinning
                // on `CMP $2140 / BNE` waiting for exactly this byte, and
                // it never gets another chance to see it once the SPC700
                // owns the ports.
                self.ports_out[0] = echo;
                // Hand over to the real SPC700 core, and bank the IPL out
                // so the uploaded program owns the whole address space.
                self.cpu.pc = entry;
                self.cpu.stopped = false;
                self.ipl_enabled = false;
            }
            boot::BootAction::None => {}
        }
    }

    /// The CPU read one of `$2140`-`$2143`.
    #[must_use]
    pub fn cpu_read_port(&self, index: usize) -> u8 {
        self.ports_out[index]
    }

    /// Advance the APU's shared clock by `cycles` SPC700 cycles.
    ///
    /// **One entry point, because there is one clock.** fullsnes: "The
    /// SPC and DSP chips are started via same /RESET and clocked via same
    /// 2.048MHz signal". Advancing the DSP and the timers separately let
    /// them drift apart in phase even when both had the right rate.
    ///
    /// Timer edges come OUT of the DSP tick and are applied here, so the
    /// DSP never reaches up into the timers.
    pub fn tick_clock(&mut self, cycles: u32) {
        for _ in 0..cycles {
            // Fixed trip count: terminates by construction (law 8).
            let t = self.dsp.tick(&mut self.aram);
            if let Some(s) = t.sample {
                self.last_sample = s;
            }
            if t.tick_slow {
                self.timers[0].tick_stage1();
                self.timers[1].tick_stage1();
            }
            if t.tick_fast {
                self.timers[2].tick_stage1();
            }
        }
    }

    pub(crate) fn read_register(&mut self, addr: u16) -> u8 {
        match addr {
            0xF0 => 0,
            0xF1 => {
                // $F1 is write-only on hardware; reads see open bus. The
                // banking state is observable through what $FFC0+ returns,
                // not by reading this back.
                0
            }
            0xF2 => self.dsp_addr,
            // DSP data. This used to be a literal `0`, which meant the
            // S-DSP was unreachable from the SPC700 and no game could
            // produce sound at all (ticket W7-08) — blargg's spc_dsp6.sfc
            // stalled forever on "Echo/basics" waiting for a read-back
            // that could never change.
            0xF3 => self.dsp.read_register(self.dsp_addr),
            0xF4..=0xF7 => self.ports_in[usize::from(addr - 0xF4)],
            0xF8 | 0xF9 => self.aux[usize::from(addr - 0xF8)],
            0xFA..=0xFC => 0, // timer targets are write-only
            0xFD..=0xFF => self.timers[usize::from(addr - 0xFD)].read_counter(),
            _ => 0,
        }
    }

    pub(crate) fn write_register(&mut self, addr: u16, value: u8) {
        match addr {
            0xF1 => {
                for (i, t) in self.timers.iter_mut().enumerate() {
                    t.set_enabled(value & (1 << i) != 0);
                }
                // Bits 4/5 clear the incoming port pairs.
                if value & 0x10 != 0 {
                    self.ports_in[0] = 0;
                    self.ports_in[1] = 0;
                }
                if value & 0x20 != 0 {
                    self.ports_in[2] = 0;
                    self.ports_in[3] = 0;
                }
                self.ipl_enabled = value & 0x80 != 0;
            }
            0xF2 => self.dsp_addr = value,
            // The counterpart write arm, which did not exist at all.
            // ARAM goes in because key-on resolves a voice's sample
            // address through the $5D DIR directory that lives there.
            0xF3 => {
                let addr = self.dsp_addr;
                self.dsp.write_register(addr, value, &self.aram);
            }
            0xF4..=0xF7 => self.ports_out[usize::from(addr - 0xF4)] = value,
            0xF8 | 0xF9 => self.aux[usize::from(addr - 0xF8)] = value,
            0xFA..=0xFC => self.timers[usize::from(addr - 0xFA)].target = value,
            _ => {}
        }
    }
}

impl ApuBus for Apu {
    fn read(&mut self, addr: u16) -> u8 {
        if (0x00F0..=0x00FF).contains(&addr) {
            return self.read_register(addr);
        }
        if self.in_ipl_window(addr) {
            return self.ipl[usize::from(addr - IPL_BASE)];
        }
        self.aram[usize::from(addr)]
    }

    fn write(&mut self, addr: u16, value: u8) {
        if (0x00F0..=0x00FF).contains(&addr) {
            self.write_register(addr, value);
            return;
        }
        // Note: NOT gated on the IPL window. Writes always reach ARAM,
        // even where the IPL is currently being read from — see the
        // module doc.
        self.aram[usize::from(addr)] = value;
    }

    fn peek(&self, addr: u16) -> u8 {
        if (0x00F0..=0x00FF).contains(&addr) {
            // Only the side-effect-free subset: $FD-$FF clear on read, so
            // a debugger must not go through read_register.
            return match addr {
                0xF2 => self.dsp_addr,
                0xF4..=0xF7 => self.ports_in[usize::from(addr - 0xF4)],
                0xF8 | 0xF9 => self.aux[usize::from(addr - 0xF8)],
                0xFD..=0xFF => self.timers[usize::from(addr - 0xFD)].peek_counter(),
                _ => 0,
            };
        }
        if self.in_ipl_window(addr) {
            return self.ipl[usize::from(addr - IPL_BASE)];
        }
        self.aram[usize::from(addr)]
    }
}

impl Apu {
    /// Serialise the APU, **ARAM included** (ticket W7-09).
    ///
    /// ARAM rides in this region rather than a tag of its own because
    /// `docs/design/SAVE_STATES.md` §2 defines nine core tags and none of
    /// them is an APU-RAM tag. It belongs here on the merits anyway: the
    /// SPC700's program, its samples and its variables all live in ARAM,
    /// so an `APU_` chunk without it restores a processor with no code.
    ///
    /// `ports_in`/`ports_out` are both saved. They are the two directions
    /// of `$2140`-`$2143`, and the famous handshakes are sensitive to
    /// exactly which side has written what.
    pub(crate) fn save(
        &self,
        o: &mut crate::state::StateOut,
    ) -> Result<(), rf_core_api::StateError> {
        self.cpu.save(o)?;
        o.bytes(&self.aram)?;
        for t in &self.timers {
            o.u32(t.divisor)?;
            o.u8(t.target)?;
            o.bool(t.enabled)?;
            o.u16(t.divider)?;
            o.u8(t.counter)?;
        }
        o.bool(self.ipl_enabled)?;
        for v in self.ports_in {
            o.u8(v)?;
        }
        for v in self.ports_out {
            o.u8(v)?;
        }
        o.u8(self.dsp_addr)?;
        for v in self.aux {
            o.u8(v)?;
        }
        self.boot.save(o)?;
        self.dsp.save(o)
    }

    pub(crate) fn load(
        &mut self,
        i: &mut crate::state::StateIn,
    ) -> Result<(), rf_core_api::StateError> {
        self.cpu.load(i)?;
        i.fill(&mut self.aram)?;
        for t in &mut self.timers {
            t.divisor = i.u32()?;
            t.target = i.u8()?;
            t.enabled = i.bool()?;
            t.divider = i.u16()?;
            t.counter = i.u8()?;
        }
        self.ipl_enabled = i.bool()?;
        for v in &mut self.ports_in {
            *v = i.u8()?;
        }
        for v in &mut self.ports_out {
            *v = i.u8()?;
        }
        self.dsp_addr = i.u8()?;
        for v in &mut self.aux {
            *v = i.u8()?;
        }
        self.boot.load(i)?;
        self.dsp.load(i)
    }
}
