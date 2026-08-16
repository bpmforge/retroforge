//! The 2A03 APU: five channels plus the frame counter (ticket W2-01a) —
//! nesdev.org/wiki/APU.
//!
//! ## Scope: what this ticket builds
//!
//! Everything the CPU can observe through registers, which is exactly what
//! blargg's `apu_test` measures: the two pulse channels, triangle, noise
//! and DMC ([`pulse`], [`triangle`], [`noise`], [`dmc`]), the shared
//! envelope / length-counter / sweep units ([`units`]), and the frame
//! counter with both sequence modes, its `$4017` write delay and its IRQ
//! ([`frame_counter`]).
//!
//! Two things are deliberately left to **W2-01b**, and neither is stubbed
//! or guessed at:
//!
//! - **The DMC's CPU stall.** nesdev's memory-reader step 1 is "The CPU is
//!   stalled for 1-4 CPU cycles to read a sample byte"; the resulting
//!   `$2007`/`$4016`/`$4017` double-read glitch is `dmc_dma_during_read4`'s
//!   subject, which is W2-01b's acceptance criterion. Here the fetch is
//!   instantaneous from the CPU's point of view (see [`dmc`]'s module doc).
//! - **The non-linear mixer.** Each channel exposes its raw output through
//!   [`Apu::channel_outputs`] — pulse/noise as 4-bit envelope volumes,
//!   triangle as its 4-bit sequence step, DMC as its 7-bit level — but no
//!   mixing, no LUT, and no [`rf_core_api::CoreSink`] audio emission
//!   happens here. W2-01b owns the nesdev APU_Mixer formulas and the
//!   `apu_mixer` RMS gate.
//!
//! ## Clocking contract with `crate::system::NesBus`
//!
//! [`Apu::tick`] is called **exactly once per CPU cycle**, from inside
//! `NesBus::tick_master`, alongside the PPU's three dots — the same
//! lock-step arrangement `crate::ppu`'s module doc describes, and for the
//! same reason (there is no catch-up scheduler in this crate yet). The APU
//! itself derives its own APU-cycle (CPU/2) parity from that call; nothing
//! outside `crate::system` may advance it, which is what keeps the
//! determinism invariant (ARCHITECTURE §3) intact.
//!
//! Register writes land **before** the cycle's tick, matching the PPU's
//! convention (`NesBus::write` runs `write_untimed` then `tick_master(1)`).
//!
//! ## What "the test ROM wins" cost here
//!
//! `apu_test/4-jitter` measures which CPU-cycle parity counts as an "APU
//! cycle" for the `$4017` write delay ("3 or 4 CPU clock cycles"), a fact
//! no wiki page states in a form an implementation can adopt directly —
//! see [`frame_counter::FrameCounter::write`]. The parity chosen here is
//! the one that ROM accepts, per MASTER_PROMPT's "if a ticket's test ROM
//! disagrees with your reading of a wiki, trust the test ROM" rule.

mod dmc;
mod frame_counter;
mod mixer;
mod noise;
mod pulse;
mod state;
#[cfg(test)]
mod tests;
mod triangle;
mod units;

use dmc::Dmc;
use frame_counter::FrameCounter;
use noise::Noise;
use pulse::Pulse;
use triangle::Triangle;

/// The raw per-channel outputs, for W2-01b's mixer and for tests. Pulse and
/// noise are 4-bit envelope volumes, triangle a 4-bit sequence step, DMC a
/// 7-bit level — the four inputs nesdev's APU_Mixer formulas take.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ChannelOutputs {
    pub pulse1: u8,
    pub pulse2: u8,
    pub triangle: u8,
    pub noise: u8,
    pub dmc: u8,
}

/// The 2A03 audio processing unit. See the module doc for scope and for the
/// clocking contract with [`crate::system::NesBus`].
#[derive(Debug)]
pub struct Apu {
    pulse1: Pulse,
    pulse2: Pulse,
    triangle: Triangle,
    noise: Noise,
    dmc: Dmc,
    frame_counter: FrameCounter,
    /// The APU's single CPU/2 parity signal: `true` on the CPU cycles that
    /// are also APU cycles. Toggled once at the top of [`Apu::tick`], and
    /// used by two consumers whose validation status is deliberately NOT
    /// the same:
    ///
    /// - The `$4017` write delay reads it and is **ROM-pinned**: blargg's
    ///   `apu_test/4-jitter` measures the 3-vs-4-cycle choice directly, so
    ///   the polarity in [`FrameCounter::write`] is measured.
    /// - Both pulse timers tick on it, and that is **unmeasured** — nothing
    ///   in this ticket's gate is sensitive to which absolute phase the
    ///   pulse timers run on (`apu_test` tests nothing audible). W2-01b's
    ///   `apu_mixer` RMS gate is the first thing that will be. One signal is
    ///   used rather than two so that when W2-01b does measure it, there is
    ///   a single fact to correct, not a hidden relative offset.
    on_apu_cycle: bool,
}

impl Default for Apu {
    fn default() -> Self {
        Self::new()
    }
}

impl Apu {
    /// Power-on state. nesdev: "Power-up and reset have the effect of
    /// writing $00 [to `$4015`], silencing all channels" — which is what
    /// every unit's `Default` already encodes (all length counters
    /// disabled, DMC idle, noise LFSR = 1).
    pub fn new() -> Self {
        Self {
            pulse1: Pulse::new(true),
            pulse2: Pulse::new(false),
            triangle: Triangle::default(),
            noise: Noise::default(),
            dmc: Dmc::default(),
            frame_counter: FrameCounter::new(),
            on_apu_cycle: false,
        }
    }

    /// Dispatch a CPU write to `$4000-$4013`, `$4015` or `$4017`. Every
    /// other address in the APU's range is unmapped and dropped, matching
    /// nesdev's register table ("Unused registers aren't listed").
    pub fn write_register(&mut self, addr: u16, value: u8) {
        match addr {
            0x4000 => self.pulse1.write_control(value),
            0x4001 => self.pulse1.write_sweep(value),
            0x4002 => self.pulse1.write_timer_low(value),
            0x4003 => self.pulse1.write_timer_high(value),
            0x4004 => self.pulse2.write_control(value),
            0x4005 => self.pulse2.write_sweep(value),
            0x4006 => self.pulse2.write_timer_low(value),
            0x4007 => self.pulse2.write_timer_high(value),
            0x4008 => self.triangle.write_linear(value),
            0x400A => self.triangle.write_timer_low(value),
            0x400B => self.triangle.write_timer_high(value),
            0x400C => self.noise.write_control(value),
            0x400E => self.noise.write_period(value),
            0x400F => self.noise.write_length(value),
            0x4010 => self.dmc.write_control(value),
            0x4011 => self.dmc.write_direct_load(value),
            0x4012 => self.dmc.write_sample_address(value),
            0x4013 => self.dmc.write_sample_length(value),
            0x4015 => self.write_status(value),
            0x4017 => self.frame_counter.write(value, self.on_apu_cycle),
            _ => {}
        }
    }

    /// `$4015` write — `---D.NT21`. "Writing to this register clears the
    /// DMC interrupt flag."
    fn write_status(&mut self, value: u8) {
        self.pulse1.length.set_enabled(value & 0x01 != 0);
        self.pulse2.length.set_enabled(value & 0x02 != 0);
        self.triangle.length.set_enabled(value & 0x04 != 0);
        self.noise.length.set_enabled(value & 0x08 != 0);
        self.dmc.irq_flag = false;
        self.dmc.set_enabled(value & 0x10 != 0);
    }

    /// `$4015` read — `IF-D.NT21`. "Reading this register clears the frame
    /// interrupt flag (but not the DMC interrupt flag). If an interrupt
    /// flag was set at the same moment of the read, it will read back as 1
    /// but it will not be cleared."
    ///
    /// Bit 5 is open bus, which this method leaves to the caller: it
    /// returns 0 there, and `crate::system::NesBus` is the layer that knows
    /// the open-bus latch value.
    pub fn read_status(&mut self) -> u8 {
        let mut value = 0;
        if self.pulse1.length.counter() > 0 {
            value |= 0x01;
        }
        if self.pulse2.length.counter() > 0 {
            value |= 0x02;
        }
        if self.triangle.length.counter() > 0 {
            value |= 0x04;
        }
        if self.noise.length.counter() > 0 {
            value |= 0x08;
        }
        if self.dmc.active() {
            value |= 0x10;
        }
        if self.frame_counter.irq_flag {
            value |= 0x40;
        }
        if self.dmc.irq_flag {
            value |= 0x80;
        }
        if !self.frame_counter.set_lands_on_next_tick() {
            self.frame_counter.irq_flag = false;
        }
        value
    }

    /// Advance one CPU cycle. See the module doc's clocking contract.
    pub fn tick(&mut self) {
        self.on_apu_cycle = !self.on_apu_cycle;

        let clocks = self.frame_counter.tick();
        if clocks.any() {
            if clocks.quarter {
                self.pulse1.clock_quarter_frame();
                self.pulse2.clock_quarter_frame();
                self.triangle.clock_quarter_frame();
                self.noise.clock_quarter_frame();
            }
            if clocks.half {
                self.pulse1.clock_half_frame();
                self.pulse2.clock_half_frame();
                self.triangle.clock_half_frame();
                self.noise.clock_half_frame();
            }
        }

        // Triangle, noise and DMC time in CPU cycles; both pulses in APU
        // cycles (see each channel's `tick_*` doc for the nesdev sentence
        // that fixes which).
        self.triangle.tick_cpu_cycle();
        self.noise.tick_cpu_cycle();
        self.dmc.tick_cpu_cycle();
        if self.on_apu_cycle {
            self.pulse1.tick_apu_cycle();
            self.pulse2.tick_apu_cycle();
        }
    }

    /// The DMC memory reader's outstanding fetch — `(address, is_load)`, or
    /// `None`. Polled by `crate::system::NesBus::read`, which halts the CPU
    /// for it (ticket W2-01b) and answers with [`Apu::dmc_supply_byte`].
    /// This handshake exists because the APU may not touch the bus itself
    /// (it would need a `&mut NesBus` it is a field of); the `is_load` flag
    /// distinguishes nesdev's two DMA kinds, which schedule their halt on
    /// opposite cycle types.
    pub fn dmc_fetch_request(&self) -> Option<(u16, bool)> {
        self.dmc.fetch_request()
    }

    /// Completes the fetch [`Apu::dmc_fetch_request`] reported.
    pub fn dmc_supply_byte(&mut self, value: u8) {
        self.dmc.supply_byte(value);
    }

    /// Whether either APU interrupt source currently holds the CPU's IRQ
    /// line — "At any time, if the interrupt flag is set, the CPU's IRQ
    /// line is continuously asserted until the interrupt flag is cleared."
    /// `crate::system::NesBus::irq_line` ORs this with the mapper's.
    pub fn irq_line(&self) -> bool {
        self.frame_counter.irq_flag || self.dmc.irq_flag
    }

    /// The mixed, non-linear output level for the current cycle, in
    /// nesdev.org/wiki/APU_Mixer's "range of 0.0 to 1.0" (ticket W2-01b).
    /// No filtering and no resampling — see [`mixer`]'s module doc for why
    /// both belong to the audio path rather than here.
    pub fn mixed_output(&self) -> f32 {
        let c = self.channel_outputs();
        mixer::mix(c.pulse1, c.pulse2, c.triangle, c.noise, c.dmc)
    }

    /// [`Apu::mixed_output`] scaled to the `i16` domain
    /// [`rf_core_api::CoreSink::audio`] takes.
    pub fn mixed_sample(&self) -> i16 {
        (self.mixed_output() * f32::from(i16::MAX)) as i16
    }

    /// Raw per-channel outputs for the mixer above (see [`ChannelOutputs`]).
    pub fn channel_outputs(&self) -> ChannelOutputs {
        ChannelOutputs {
            pulse1: self.pulse1.output(),
            pulse2: self.pulse2.output(),
            triangle: self.triangle.output(),
            noise: self.noise.output(),
            dmc: self.dmc.output(),
        }
    }
}
