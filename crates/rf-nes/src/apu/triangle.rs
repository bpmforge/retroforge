//! The triangle channel, `$4008-$400B` (ticket W2-01a) —
//! nesdev.org/wiki/APU_Triangle.

use super::units::LengthCounter;

/// "The sequencer sends the following looping 32-step sequence of values to
/// the mixer" (nesdev, verbatim).
const SEQUENCE: [u8; 32] = [
    15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12,
    13, 14, 15,
];

#[derive(Debug, Default)]
pub(super) struct Triangle {
    pub(super) length: LengthCounter,
    /// `C` — the linear counter's control flag, physically the same bit as
    /// the length counter's halt flag.
    pub(super) control: bool,
    pub(super) linear_reload_value: u8,
    pub(super) linear_counter: u8,
    pub(super) linear_reload_flag: bool,
    pub(super) period: u16,
    pub(super) timer: u16,
    pub(super) sequence_step: u8,
}

impl Triangle {
    /// `$4008` — `CRRR.RRRR`, linear counter setup.
    pub(super) fn write_linear(&mut self, value: u8) {
        self.control = value & 0x80 != 0;
        self.length.halt = value & 0x80 != 0;
        self.linear_reload_value = value & 0x7F;
    }

    /// `$400A` — timer low 8 bits.
    pub(super) fn write_timer_low(&mut self, value: u8) {
        self.period = (self.period & 0x0700) | u16::from(value);
    }

    /// `$400B` — length load + timer high; side effect: "Sets the linear
    /// counter reload flag".
    pub(super) fn write_timer_high(&mut self, value: u8) {
        self.period = (self.period & 0x00FF) | (u16::from(value & 0x07) << 8);
        self.length.load(value);
        self.linear_reload_flag = true;
    }

    /// One CPU cycle — "Unlike the pulse channels, this timer ticks at the
    /// rate of the CPU clock rather than the APU (CPU/2) clock."
    ///
    /// "The sequencer is clocked by the timer as long as both the linear
    /// counter and the length counter are nonzero."
    pub(super) fn tick_cpu_cycle(&mut self) {
        if self.timer == 0 {
            self.timer = self.period;
            if self.linear_counter > 0 && self.length.counter() > 0 {
                self.sequence_step = (self.sequence_step + 1) & 31;
            }
        } else {
            self.timer -= 1;
        }
    }

    /// One quarter-frame clock, in the order nesdev gives: "If the linear
    /// counter reload flag is set, the linear counter is reloaded with the
    /// counter reload value, otherwise if the linear counter is non-zero,
    /// it is decremented. If the control flag is clear, the linear counter
    /// reload flag is cleared."
    pub(super) fn clock_quarter_frame(&mut self) {
        if self.linear_reload_flag {
            self.linear_counter = self.linear_reload_value;
        } else if self.linear_counter > 0 {
            self.linear_counter -= 1;
        }
        if !self.control {
            self.linear_reload_flag = false;
        }
    }

    pub(super) fn clock_half_frame(&mut self) {
        self.length.clock_half_frame();
    }

    pub(super) fn output(&self) -> u8 {
        SEQUENCE[self.sequence_step as usize]
    }
}

#[cfg(test)]
impl Triangle {
    pub(super) fn linear_counter_for_test(&self) -> u8 {
        self.linear_counter
    }
}
