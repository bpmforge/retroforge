//! The APU frame counter / frame sequencer (ticket W2-01a) —
//! nesdev.org/wiki/APU_Frame_Counter.
//!
//! ## Why this counts CPU cycles, not APU cycles
//!
//! The wiki's step table is written in APU cycles with a `PUT`/`GET`
//! annotation and the caveat "with an additional delay of one CPU cycle for
//! the quarter and half frame signals" — i.e. the real event positions are
//! at CPU-cycle resolution and land between APU cycles. Doubling the table
//! and folding that one-cycle delay in gives the CPU-cycle positions this
//! module uses directly, which is also the form blargg's
//! `apu_test/5-len_timing` and `6-irq_flag_timing` measure in ("Frame
//! interrupt flag is set three times in a row 29831 clocks after writing
//! $00 to $4017").

/// What one CPU cycle of the sequencer asks the channels to do. Both flags
/// can be set at once (the half-frame steps also carry a quarter-frame
/// clock, per the wiki table's two "Clock" columns).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct FrameClocks {
    /// Envelopes and the triangle's linear counter.
    pub(super) quarter: bool,
    /// Length counters and sweep units.
    pub(super) half: bool,
}

impl FrameClocks {
    const NONE: Self = Self {
        quarter: false,
        half: false,
    };
    const QUARTER: Self = Self {
        quarter: true,
        half: false,
    };
    const BOTH: Self = Self {
        quarter: true,
        half: true,
    };

    pub(super) fn any(&self) -> bool {
        self.quarter || self.half
    }
}

/// Mode-0 (4-step) sequence length in CPU cycles: the counter is reset to 0
/// on cycle 29830, so the sequence repeats every 29830 cycles, matching the
/// wiki's "the interrupt flag is set every 29830 CPU cycles".
const MODE0_LAST: u32 = 29830;
/// Mode-1 (5-step) sequence length in CPU cycles (18641 APU cycles x 2).
const MODE1_LAST: u32 = 37282;

#[derive(Debug)]
pub(super) struct FrameCounter {
    /// CPU cycles since the sequence last restarted.
    pub(super) cycle: u32,
    /// `M` — false = 4-step (mode 0), true = 5-step (mode 1).
    pub(super) mode_five_step: bool,
    /// `I` — the interrupt inhibit flag.
    pub(super) inhibit_irq: bool,
    /// The frame interrupt flag itself, wired to the CPU's IRQ line.
    pub(super) irq_flag: bool,
    /// Countdown for a pending `$4017` write: "After 3 or 4 CPU clock
    /// cycles, the timer is reset" — 3 if the write occurred during an APU
    /// cycle, 4 if between them. `None` when no write is pending.
    pub(super) pending_reset: Option<u8>,
    /// The mode bit the pending write carried, applied when the countdown
    /// expires (not at write time).
    pub(super) pending_mode_five_step: bool,
}

impl Default for FrameCounter {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameCounter {
    pub(super) fn new() -> Self {
        Self {
            cycle: 0,
            mode_five_step: false,
            inhibit_irq: false,
            irq_flag: false,
            pending_reset: None,
            pending_mode_five_step: false,
        }
    }

    /// A `$4017` write. The mode change and the sequencer reset are both
    /// deferred by 3-4 CPU cycles (see `pending_reset`); the interrupt
    /// inhibit flag, by contrast, takes effect immediately — nesdev: "If
    /// set, the frame interrupt flag is cleared", which
    /// `apu_test/3-irq_flag` sub-test 7 ("Writing $40 or $C0 to $4017
    /// should clear flag") measures without waiting.
    ///
    /// `on_apu_cycle` is the APU's single parity signal (see
    /// [`super::Apu::on_apu_cycle`]), sampled at write time. nesdev: "If the
    /// write occurs during an APU cycle, the effects occur 3 CPU cycles
    /// after the $4017 write cycle, and if the write occurs between APU
    /// cycles, the effects occurs 4 CPU cycles after the write cycle."
    ///
    /// **Which polarity is which is measured, not assumed.** A register
    /// write in this engine runs BEFORE its own cycle's tick, so the
    /// parity flag still holds the *previous* tick's value here; the
    /// mapping below (`true` -> 4, `false` -> 3) is the one blargg's
    /// `apu_test/4-jitter` accepts, and it is the only fact in this module
    /// that no wiki sentence can settle on its own.
    pub(super) fn write(&mut self, value: u8, on_apu_cycle: bool) {
        self.pending_mode_five_step = value & 0x80 != 0;
        self.inhibit_irq = value & 0x40 != 0;
        if self.inhibit_irq {
            self.irq_flag = false;
        }
        self.pending_reset = Some(if on_apu_cycle { 4 } else { 3 });
    }

    /// Advance one CPU cycle and report what the channels must be clocked
    /// with. Ordering inside the cycle: the pending `$4017` reset resolves
    /// first (it can itself generate a quarter+half clock in 5-step mode),
    /// and only if no reset resolved does the ordinary sequence step run —
    /// a reset makes this cycle cycle 0 of a fresh sequence, which by
    /// definition has no step on it.
    pub(super) fn tick(&mut self) -> FrameClocks {
        if let Some(delay) = self.pending_reset {
            let delay = delay - 1;
            if delay == 0 {
                self.pending_reset = None;
                self.mode_five_step = self.pending_mode_five_step;
                self.cycle = 0;
                // "Writing to $4017 with bit 7 set ($80) will immediately
                // clock all of its controlled units at the beginning of the
                // 5-step sequence; with bit 7 clear, only the sequence is
                // reset without clocking any of its units."
                return if self.mode_five_step {
                    FrameClocks::BOTH
                } else {
                    FrameClocks::NONE
                };
            }
            self.pending_reset = Some(delay);
        }

        self.cycle += 1;
        if self.mode_five_step {
            self.step_mode_one()
        } else {
            self.step_mode_zero()
        }
    }

    /// Mode 0 (4-step): quarter frames at 7457, 14913, 22371 and 29829;
    /// half frames at 14913 and 29829; the frame interrupt flag set on each
    /// of 29828, 29829 and 29830 (the wiki's three consecutive "Set if
    /// interrupt inhibit is clear" rows), the last of which wraps.
    fn step_mode_zero(&mut self) -> FrameClocks {
        match self.cycle {
            7457 | 22371 => FrameClocks::QUARTER,
            14913 => FrameClocks::BOTH,
            29828 => {
                self.set_irq();
                FrameClocks::NONE
            }
            29829 => {
                self.set_irq();
                FrameClocks::BOTH
            }
            MODE0_LAST => {
                self.set_irq();
                self.cycle = 0;
                FrameClocks::NONE
            }
            _ => FrameClocks::NONE,
        }
    }

    /// Mode 1 (5-step): the same first three steps, nothing at 29829, a
    /// quarter+half at 37281, and the wrap at 37282. "In this mode, the
    /// frame interrupt flag is never set."
    fn step_mode_one(&mut self) -> FrameClocks {
        match self.cycle {
            7457 | 22371 => FrameClocks::QUARTER,
            14913 | 37281 => FrameClocks::BOTH,
            MODE1_LAST => {
                self.cycle = 0;
                FrameClocks::NONE
            }
            _ => FrameClocks::NONE,
        }
    }

    fn set_irq(&mut self) {
        if !self.inhibit_irq {
            self.irq_flag = true;
        }
    }

    /// Whether the *next* [`FrameCounter::tick`] will raise the frame IRQ.
    ///
    /// This is nesdev's `$4015` read rule — "If an interrupt flag was set
    /// at the same moment of the read, it will read back as 1 but it will
    /// not be cleared" — expressed in this engine's ordering, where a
    /// register access runs BEFORE its own CPU cycle's tick
    /// (`NesBus::read` is `read_untimed` then `tick_master(1)`). So the set
    /// that is simultaneous with a read is the one the *upcoming* tick
    /// performs, not the one the previous tick already performed.
    ///
    /// **This alignment is measured, not assumed**: with the rule keyed to
    /// the previous tick instead, `apu_test/6-irq_flag_timing` fails
    /// sub-test 5 ("Flag last set too late") while 2-4 pass. Its source
    /// (`apu_test/source/6-irq_flag_timing.s`) reads `$4015` at write + 29833,
    /// the last of the three consecutive set cycles, and requires the
    /// second read four cycles later to come back CLEAR — i.e. that read
    /// must clear, so the protection must not cover it.
    pub(super) fn set_lands_on_next_tick(&self) -> bool {
        if self.mode_five_step || self.inhibit_irq {
            return false;
        }
        if self.pending_reset == Some(1) {
            // The next tick is the sequencer reset, which has no step.
            return false;
        }
        matches!(self.cycle + 1, 29828 | 29829 | MODE0_LAST)
    }
}
