//! The delta modulation channel, `$4010-$4013` (ticket W2-01a) —
//! nesdev.org/wiki/APU_DMC.
//!
//! ## What this ticket builds, and what W2-01b owns
//!
//! Everything the CPU can observe *through registers* is here: the memory
//! reader, the sample buffer, the output unit, the interrupt flag, and the
//! 16 rates. What is deliberately NOT here is the **CPU stall**: nesdev's
//! reader step 1 is "The CPU is stalled for 1-4 CPU cycles to read a sample
//! byte", and the resulting `$2007`/`$4016`/`$4017` double-read glitch is
//! the subject of `dmc_dma_during_read4`, which is **W2-01b's** acceptance
//! criterion, not this ticket's. Here the fetch is instantaneous from the
//! CPU's point of view: [`Dmc::fetch_address`] exposes the pending read and
//! [`Dmc::supply_byte`] completes it, driven by `crate::system::NesBus`
//! (the only place that can legally touch the bus).

/// NTSC rate table, in CPU cycles between output-level changes (nesdev's
/// `$4010` "Rate" table). PAL is a different table and is not built here.
const RATES: [u16; 16] = [
    428, 380, 340, 320, 286, 254, 226, 214, 190, 160, 142, 128, 106, 84, 72, 54,
];

#[derive(Debug)]
pub(super) struct Dmc {
    /// `I` — IRQ enabled flag.
    irq_enabled: bool,
    /// `L` — loop flag.
    loop_flag: bool,
    /// The interrupt flag itself, wired to the CPU's IRQ line alongside the
    /// frame counter's.
    pub(super) irq_flag: bool,

    period: u16,
    timer: u16,

    /// `$4012`, decoded: "Sample address = %11AAAAAA.AA000000 = $C000 +
    /// (A * 64)".
    sample_address: u16,
    /// `$4013`, decoded: "Sample length = %LLLL.LLLL0001 = (L * 16) + 1
    /// bytes".
    sample_length: u16,

    current_address: u16,
    bytes_remaining: u16,
    sample_buffer: Option<u8>,
    /// Set while a read has been requested but not yet supplied by the bus.
    fetch_pending: bool,
    /// Which kind of DMA the outstanding request is (ticket W2-01b) —
    /// nesdev.org/wiki/DMA: "Load DMAs occur after $4015 D4 is set, but only
    /// if the sample buffer is empty... Reload DMAs occur in response to the
    /// sample buffer being emptied." They schedule their halt on opposite
    /// cycle types, which is why the kind is carried to the bus.
    fetch_is_load: bool,

    shift_register: u8,
    bits_remaining: u8,
    silence: bool,
    /// The 7-bit output level, "sent to the mixer whether the channel is
    /// enabled or not".
    output_level: u8,
}

impl Default for Dmc {
    fn default() -> Self {
        Self {
            irq_enabled: false,
            loop_flag: false,
            irq_flag: false,
            period: RATES[0],
            timer: RATES[0],
            sample_address: 0xC000,
            sample_length: 1,
            current_address: 0xC000,
            bytes_remaining: 0,
            sample_buffer: None,
            fetch_pending: false,
            fetch_is_load: false,
            shift_register: 0,
            bits_remaining: 8,
            silence: true,
            output_level: 0,
        }
    }
}

impl Dmc {
    /// `$4010` — `IL--.RRRR`. "IRQ enabled flag. If clear, the interrupt
    /// flag is cleared" (`apu_test/7-dmc_basics` sub-test 12).
    pub(super) fn write_control(&mut self, value: u8) {
        self.irq_enabled = value & 0x80 != 0;
        self.loop_flag = value & 0x40 != 0;
        self.period = RATES[(value & 0x0F) as usize];
        if !self.irq_enabled {
            self.irq_flag = false;
        }
    }

    /// `$4011` — direct load of the 7-bit output level.
    pub(super) fn write_direct_load(&mut self, value: u8) {
        self.output_level = value & 0x7F;
    }

    /// `$4012` — sample address.
    pub(super) fn write_sample_address(&mut self, value: u8) {
        self.sample_address = 0xC000 | (u16::from(value) << 6);
    }

    /// `$4013` — sample length.
    pub(super) fn write_sample_length(&mut self, value: u8) {
        self.sample_length = (u16::from(value) << 4) + 1;
    }

    /// `$4015` bit 4. Verbatim: "If the DMC bit is clear, the DMC bytes
    /// remaining will be set to 0 and the DMC will silence when it empties.
    /// If the DMC bit is set, the DMC sample will be restarted only if its
    /// bytes remaining is 0." (`7-dmc_basics` sub-tests 4, 5 and 6.)
    pub(super) fn set_enabled(&mut self, enabled: bool) {
        if enabled {
            if self.bytes_remaining == 0 {
                self.restart();
            }
        } else {
            self.bytes_remaining = 0;
        }
        self.maybe_request_fetch(true);
    }

    /// "When a sample is (re)started, the current address is set to the
    /// sample address, and bytes remaining is set to the sample length."
    fn restart(&mut self) {
        self.current_address = self.sample_address;
        self.bytes_remaining = self.sample_length;
    }

    /// `$4015` read bit 4: "D will read as 1 if the DMC bytes remaining is
    /// more than 0."
    pub(super) fn active(&self) -> bool {
        self.bytes_remaining > 0
    }

    /// The outstanding fetch, if any: `(address, is_load)`. The bus halts
    /// the CPU for it and answers with [`Dmc::supply_byte`] — see
    /// `crate::system::NesBus`'s `CpuBus::read` for the stall itself.
    pub(super) fn fetch_request(&self) -> Option<(u16, bool)> {
        self.fetch_pending
            .then_some((self.current_address, self.fetch_is_load))
    }

    /// "Any time the sample buffer is in an empty state and bytes remaining
    /// is not zero (including just after a write to $4015 that enables the
    /// channel...)" — the reader starts a fetch.
    fn maybe_request_fetch(&mut self, is_load: bool) {
        if self.sample_buffer.is_none() && self.bytes_remaining > 0 && !self.fetch_pending {
            self.fetch_pending = true;
            self.fetch_is_load = is_load;
        }
    }

    /// Completes the outstanding fetch: buffer the byte, advance the
    /// address ("if it exceeds $FFFF, it is wrapped around to $8000"), and
    /// decrement the byte counter, restarting on loop or raising the
    /// interrupt flag at the end of a non-looping sample.
    pub(super) fn supply_byte(&mut self, value: u8) {
        self.fetch_pending = false;
        self.sample_buffer = Some(value);
        self.current_address = if self.current_address == 0xFFFF {
            0x8000
        } else {
            self.current_address + 1
        };
        self.bytes_remaining -= 1;
        if self.bytes_remaining == 0 {
            if self.loop_flag {
                self.restart();
                self.maybe_request_fetch(false);
            } else if self.irq_enabled {
                self.irq_flag = true;
            }
        }
    }

    /// One CPU cycle. The rate table is in CPU cycles, so the timer runs at
    /// the CPU clock; "a rate of 428 means the output level changes every
    /// 214 APU cycles."
    pub(super) fn tick_cpu_cycle(&mut self) {
        if self.timer == 0 {
            self.timer = self.period - 1;
            self.clock_output_unit();
        } else {
            self.timer -= 1;
        }
        self.maybe_request_fetch(false);
    }

    /// The output unit, in the order nesdev's "When the timer outputs a
    /// clock" list gives: level change, shift, then the bits-remaining
    /// decrement that may end the output cycle.
    fn clock_output_unit(&mut self) {
        if !self.silence {
            // "If the bit is 1, add 2; otherwise, subtract 2. But if adding
            // or subtracting 2 would cause the output level to leave the
            // 0-127 range, leave the output level unchanged."
            if self.shift_register & 1 != 0 {
                if self.output_level <= 125 {
                    self.output_level += 2;
                }
            } else if self.output_level >= 2 {
                self.output_level -= 2;
            }
        }
        self.shift_register >>= 1;
        self.bits_remaining -= 1;
        if self.bits_remaining == 0 {
            self.start_output_cycle();
        }
    }

    /// "When an output cycle ends, a new cycle is started as follows: the
    /// bits-remaining counter is loaded with 8. If the sample buffer is
    /// empty, then the silence flag is set; otherwise, the silence flag is
    /// cleared and the sample buffer is emptied into the shift register."
    fn start_output_cycle(&mut self) {
        self.bits_remaining = 8;
        match self.sample_buffer.take() {
            Some(byte) => {
                self.silence = false;
                self.shift_register = byte;
            }
            None => self.silence = true,
        }
    }

    pub(super) fn output(&self) -> u8 {
        self.output_level
    }
}
