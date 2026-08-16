//! The two pulse (square) channels, `$4000-$4003` and `$4004-$4007`
//! (ticket W2-01a) — nesdev.org/wiki/APU_Pulse.

use super::units::{Envelope, LengthCounter, Sweep};

/// "Duty / Sequence lookup table" from the nesdev page's implementation-
/// details box, in output-waveform order (the hardware reads it backwards
/// from a downward counter; this table is the resulting output sequence,
/// which is what a per-step index needs).
const DUTY_SEQUENCES: [[u8; 8]; 4] = [
    [0, 1, 0, 0, 0, 0, 0, 0], // 12.5%
    [0, 1, 1, 0, 0, 0, 0, 0], // 25%
    [0, 1, 1, 1, 1, 0, 0, 0], // 50%
    [1, 0, 0, 1, 1, 1, 1, 1], // 25% negated
];

#[derive(Debug)]
pub(super) struct Pulse {
    pub(super) envelope: Envelope,
    pub(super) length: LengthCounter,
    pub(super) sweep: Sweep,
    pub(super) duty: u8,
    /// 11-bit raw timer period `t`; the waveform period is `8 * (t + 1)`
    /// APU cycles.
    pub(super) period: u16,
    pub(super) timer: u16,
    pub(super) sequence_step: u8,
}

impl Pulse {
    /// `ones_complement` selects pulse 1's sweep-negate wiring; see
    /// [`Sweep`]'s doc — it is the two channels' only difference.
    pub(super) fn new(ones_complement: bool) -> Self {
        Self {
            envelope: Envelope::default(),
            length: LengthCounter::default(),
            sweep: Sweep::new(ones_complement),
            duty: 0,
            period: 0,
            timer: 0,
            sequence_step: 0,
        }
    }

    /// `$4000`/`$4004` — `DDLC.VVVV`. "The duty cycle is changed, but the
    /// sequencer's current position isn't affected."
    pub(super) fn write_control(&mut self, value: u8) {
        self.duty = value >> 6;
        self.length.halt = value & 0x20 != 0;
        self.envelope.loop_flag = value & 0x20 != 0;
        self.envelope.constant_volume = value & 0x10 != 0;
        self.envelope.volume = value & 0x0F;
    }

    /// `$4001`/`$4005` — the sweep setup register.
    pub(super) fn write_sweep(&mut self, value: u8) {
        self.sweep.write(value);
    }

    /// `$4002`/`$4006` — timer low 8 bits.
    pub(super) fn write_timer_low(&mut self, value: u8) {
        self.period = (self.period & 0x0700) | u16::from(value);
    }

    /// `$4003`/`$4007` — length load + timer high 3 bits. Side effects
    /// (nesdev): "The sequencer is immediately restarted at the first value
    /// of the current sequence. The envelope is also restarted. The period
    /// divider is not reset."
    pub(super) fn write_timer_high(&mut self, value: u8) {
        self.period = (self.period & 0x00FF) | (u16::from(value & 0x07) << 8);
        self.length.load(value);
        self.sequence_step = 0;
        self.envelope.restart();
    }

    /// One APU cycle (every second CPU cycle): "this timer is updated every
    /// APU cycle... and counts t, t-1, ..., 0, t, t-1, ..., clocking the
    /// waveform generator when it goes from 0 to t."
    pub(super) fn tick_apu_cycle(&mut self) {
        if self.timer == 0 {
            self.timer = self.period;
            self.sequence_step = (self.sequence_step + 1) & 7;
        } else {
            self.timer -= 1;
        }
    }

    pub(super) fn clock_quarter_frame(&mut self) {
        self.envelope.clock_quarter_frame();
    }

    pub(super) fn clock_half_frame(&mut self) {
        self.length.clock_half_frame();
        self.sweep.clock_half_frame(&mut self.period);
    }

    /// "The mixer receives the pulse channel's current envelope volume
    /// except when: the sequencer output is zero, or overflow from the
    /// sweep unit's adder is silencing the channel, or the length counter
    /// is zero, or the timer has a value less than eight."
    pub(super) fn output(&self) -> u8 {
        if DUTY_SEQUENCES[self.duty as usize][self.sequence_step as usize] == 0
            || self.length.counter() == 0
            || self.sweep.muting(self.period)
        {
            0
        } else {
            self.envelope.output()
        }
    }
}

#[cfg(test)]
impl Pulse {
    /// The sweep unit's current target period — the quantity
    /// nesdev.org/wiki/APU_Sweep's worked example is stated in, and the
    /// only place the two pulse channels differ.
    pub(super) fn sweep_target_for_test(&self) -> u16 {
        self.sweep.target_period(self.period)
    }
}
