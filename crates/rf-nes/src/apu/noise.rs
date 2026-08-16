//! The noise channel, `$400C-$400F` (ticket W2-01a) —
//! nesdev.org/wiki/APU_Noise.

use super::units::{Envelope, LengthCounter};

/// NTSC period table, in CPU cycles between shift-register clocks (nesdev's
/// "Rate" table). PAL is a different table and is not built here — this
/// crate is NTSC-only so far (see `crate::ppu`'s frame-timing constants).
const PERIODS: [u16; 16] = [
    4, 8, 16, 32, 64, 96, 128, 160, 202, 254, 380, 508, 762, 1016, 2034, 4068,
];

#[derive(Debug)]
pub(super) struct Noise {
    pub(super) envelope: Envelope,
    pub(super) length: LengthCounter,
    /// `M` — "Feedback is calculated as the exclusive-OR of bit 0 and one
    /// other bit: bit 6 if Mode flag is set, otherwise bit 1."
    pub(super) mode: bool,
    pub(super) period: u16,
    pub(super) timer: u16,
    /// 15-bit LFSR. "On power-up, the shift register is loaded with the
    /// value 1."
    pub(super) shift_register: u16,
}

impl Default for Noise {
    fn default() -> Self {
        Self {
            envelope: Envelope::default(),
            length: LengthCounter::default(),
            mode: false,
            period: PERIODS[0],
            timer: PERIODS[0],
            shift_register: 1,
        }
    }
}

impl Noise {
    /// `$400C` — `--LC.VVVV`.
    pub(super) fn write_control(&mut self, value: u8) {
        self.length.halt = value & 0x20 != 0;
        self.envelope.loop_flag = value & 0x20 != 0;
        self.envelope.constant_volume = value & 0x10 != 0;
        self.envelope.volume = value & 0x0F;
    }

    /// `$400E` — `M---.PPPP`.
    pub(super) fn write_period(&mut self, value: u8) {
        self.mode = value & 0x80 != 0;
        self.period = PERIODS[(value & 0x0F) as usize];
    }

    /// `$400F` — length load; also restarts the envelope.
    pub(super) fn write_length(&mut self, value: u8) {
        self.length.load(value);
        self.envelope.restart();
    }

    /// One CPU cycle. The period table is expressed in CPU cycles ("The
    /// period determines how many CPU cycles happen between shift register
    /// clocks"), so the timer is driven per CPU cycle rather than per APU
    /// cycle; the table's entries are all even, which is what keeps the
    /// channel on APU-cycle boundaries in practice.
    pub(super) fn tick_cpu_cycle(&mut self) {
        if self.timer == 0 {
            self.timer = self.period - 1;
            self.clock_shift_register();
        } else {
            self.timer -= 1;
        }
    }

    fn clock_shift_register(&mut self) {
        let other_bit = if self.mode {
            (self.shift_register >> 6) & 1
        } else {
            (self.shift_register >> 1) & 1
        };
        let feedback = (self.shift_register & 1) ^ other_bit;
        self.shift_register >>= 1;
        self.shift_register |= feedback << 14;
    }

    pub(super) fn clock_quarter_frame(&mut self) {
        self.envelope.clock_quarter_frame();
    }

    pub(super) fn clock_half_frame(&mut self) {
        self.length.clock_half_frame();
    }

    /// "The mixer receives the current envelope volume except when: bit 0
    /// of the shift register is set, or the length counter is zero."
    pub(super) fn output(&self) -> u8 {
        if self.shift_register & 1 != 0 || self.length.counter() == 0 {
            0
        } else {
            self.envelope.output()
        }
    }
}

#[cfg(test)]
impl Noise {
    pub(super) fn shift_register_for_test(&self) -> u16 {
        self.shift_register
    }
}
