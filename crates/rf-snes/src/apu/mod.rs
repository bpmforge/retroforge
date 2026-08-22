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
const DSP_CYCLES_PER_SAMPLE: u32 = 32;
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
    stage: u32,
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
            stage: 0,
            divider: 0,
            counter: 0,
        }
    }

    /// Advance by `cycles` SPC700 cycles.
    pub fn tick(&mut self, cycles: u32) {
        if !self.enabled {
            return;
        }
        self.stage += cycles;
        while self.stage >= self.divisor {
            self.stage -= self.divisor;
            self.divider = self.divider.wrapping_add(1);
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
            self.stage = 0;
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
    pub dsp_addr: u8,
    /// SPC cycles accumulated toward the next DSP sample.
    dsp_cycles: u32,
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
            dsp_addr: 0,
            dsp_cycles: 0,
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
        self.tick_timers(u32::from(cycles));
        self.tick_dsp(u32::from(cycles));
        self.poll_boot();
        r
    }

    /// Advance the S-DSP's sample clock.
    ///
    /// The DSP emits one stereo sample every **32 SPC cycles**: the SPC700
    /// runs at roughly 1.024 MHz and the DSP at 32 kHz, and 1024000/32000
    /// is exactly 32. Anything else makes envelopes, the echo delay line
    /// and the noise LFSR all run at the wrong rate together, which sounds
    /// like a pitch bug rather than a clock bug.
    ///
    /// **Being programmable is not the same as running.** `$F2`/`$F3` let
    /// a program reach the registers; without this, nothing would ever
    /// advance an envelope, set ENDX, or move the echo buffer, so a
    /// program polling ENVX would still spin forever (ticket W7-08).
    pub(crate) fn tick_dsp(&mut self, cycles: u32) {
        self.dsp_cycles += cycles;
        while self.dsp_cycles >= DSP_CYCLES_PER_SAMPLE {
            self.dsp_cycles -= DSP_CYCLES_PER_SAMPLE;
            // `dsp_cycles` decreases by a positive constant on every pass,
            // so this terminates (law 8).
            self.last_sample = self.dsp.mix(&mut self.aram);
        }
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

    pub fn tick_timers(&mut self, cycles: u32) {
        for t in &mut self.timers {
            t.tick(cycles);
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
            return IPL_STUB[usize::from(addr - IPL_BASE)];
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
            return IPL_STUB[usize::from(addr - IPL_BASE)];
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
            o.u32(t.stage)?;
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
            t.stage = i.u32()?;
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
