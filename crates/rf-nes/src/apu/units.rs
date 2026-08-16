//! The three sub-units the APU's tone channels share: the volume envelope
//! generator, the length counter, and the pulse channels' sweep unit
//! (ticket W2-01a).
//!
//! Each is a direct transcription of its own nesdev.org page — cited per
//! type below — kept in one module because all three are clocked by the
//! frame counter rather than by a channel timer, and because the pulse,
//! triangle and noise channels each need some subset of them.

/// Volume envelope generator — nesdev.org/wiki/APU_Envelope.
///
/// "Each volume envelope unit contains the following: start flag, divider,
/// and decay level counter." Used by both pulse channels (`$4000`/`$4004`)
/// and by noise (`$400C`); the triangle has no envelope (it has no volume
/// control at all).
#[derive(Debug, Default, Clone, Copy)]
pub(super) struct Envelope {
    /// `V` — doubles as the constant volume level and, in envelope mode, as
    /// the divider's reload value ("the period becomes V + 1 quarter
    /// frames").
    pub(super) volume: u8,
    /// `C` — "0: use volume from envelope; 1: use constant volume".
    pub(super) constant_volume: bool,
    /// `L` — the envelope loop flag, which is physically the same bit as
    /// the channel's length-counter halt flag (see [`LengthCounter::halt`]).
    pub(super) loop_flag: bool,
    start: bool,
    divider: u8,
    decay_level: u8,
}

impl Envelope {
    /// `$4003`/`$4007`/`$400F` side effect: "Sets start flag".
    pub(super) fn restart(&mut self) {
        self.start = true;
    }

    /// One quarter-frame clock, verbatim from the nesdev page: "if the
    /// start flag is clear, the divider is clocked, otherwise the start
    /// flag is cleared, the decay level counter is loaded with 15, and the
    /// divider's period is immediately reloaded."
    pub(super) fn clock_quarter_frame(&mut self) {
        if self.start {
            self.start = false;
            self.decay_level = 15;
            self.divider = self.volume;
            return;
        }
        if self.divider == 0 {
            self.divider = self.volume;
            // "If the counter is non-zero, it is decremented, otherwise if
            // the loop flag is set, the decay level counter is loaded
            // with 15."
            if self.decay_level > 0 {
                self.decay_level -= 1;
            } else if self.loop_flag {
                self.decay_level = 15;
            }
        } else {
            self.divider -= 1;
        }
    }

    /// "if set, the envelope parameter directly sets the volume, otherwise
    /// the decay level is the current volume."
    pub(super) fn output(&self) -> u8 {
        if self.constant_volume {
            self.volume
        } else {
            self.decay_level
        }
    }
}

/// nesdev.org/wiki/APU_Length_Counter's table, indexed by the `LLLLL` field
/// of `$4003`/`$4007`/`$400B`/`$400F`.
pub(super) const LENGTH_TABLE: [u8; 32] = [
    10, 254, 20, 2, 40, 4, 80, 6, 160, 8, 60, 10, 14, 12, 26, 14, 12, 16, 24, 18, 48, 20, 96, 22,
    192, 24, 72, 26, 16, 28, 32, 30,
];

/// Automatic duration control — nesdev.org/wiki/APU_Length_Counter.
///
/// The `enabled` flag is the channel's `$4015` bit and is deliberately part
/// of this unit rather than of the channel: "When the enabled bit is
/// cleared (via `$4015`), the length counter is forced to 0 and cannot be
/// changed until enabled is set again."
#[derive(Debug, Default, Clone, Copy)]
pub(super) struct LengthCounter {
    pub(super) enabled: bool,
    /// The halt flag — the same physical bit as the envelope's loop flag on
    /// pulse/noise, and as the triangle's linear-counter control flag.
    pub(super) halt: bool,
    counter: u8,
}

impl LengthCounter {
    pub(super) fn counter(&self) -> u8 {
        self.counter
    }

    /// `$4015` write: "Writing a zero to any of the channel enable bits
    /// will silence that channel and halt its length counter."
    pub(super) fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.counter = 0;
        }
    }

    /// Length-counter load from the top five bits of the channel's `$400x`
    /// high-timer register. "If the enabled flag is set, the length counter
    /// is loaded with entry L of the length table" — and, per
    /// `apu_test/1-len_ctr` sub-test 7 ("When disabled via $4015, length
    /// shouldn't allow reloading"), NOT loaded at all when it is clear.
    pub(super) fn load(&mut self, encoded: u8) {
        if self.enabled {
            self.counter = LENGTH_TABLE[(encoded >> 3) as usize];
        }
    }

    /// One half-frame clock: "the length counter is decremented except
    /// when: the length counter is 0, or the halt flag is set."
    pub(super) fn clock_half_frame(&mut self) {
        if self.counter > 0 && !self.halt {
            self.counter -= 1;
        }
    }
}

/// Pulse-channel sweep unit — nesdev.org/wiki/APU_Sweep.
///
/// `ones_complement` distinguishes the two pulse channels, which "have
/// their adders' carry inputs wired differently": pulse 1 "adds the ones'
/// complement (−c − 1)", pulse 2 "adds the two's complement (−c)". This is
/// the *only* behavioral difference between the two channels.
#[derive(Debug, Default, Clone, Copy)]
pub(super) struct Sweep {
    pub(super) enabled: bool,
    pub(super) period: u8,
    pub(super) negate: bool,
    pub(super) shift: u8,
    pub(super) ones_complement: bool,
    reload: bool,
    divider: u8,
}

impl Sweep {
    pub(super) fn new(ones_complement: bool) -> Self {
        Self {
            ones_complement,
            ..Self::default()
        }
    }

    /// `$4001`/`$4005` write side effect: "Sets the reload flag".
    pub(super) fn write(&mut self, value: u8) {
        self.enabled = value & 0x80 != 0;
        self.period = (value >> 4) & 0x07;
        self.negate = value & 0x08 != 0;
        self.shift = value & 0x07;
        self.reload = true;
    }

    /// "A barrel shifter shifts the pulse channel's 11-bit raw timer period
    /// right by the shift count... If the negate flag is true, the change
    /// amount is made negative. The target period is the sum of the current
    /// period and the change amount, clamped to zero if this sum is
    /// negative."
    pub(super) fn target_period(&self, current: u16) -> u16 {
        let change = current >> self.shift;
        if self.negate {
            let change = if self.ones_complement {
                change.wrapping_add(1)
            } else {
                change
            };
            current.saturating_sub(change)
        } else {
            current + change
        }
    }

    /// "Two conditions cause the sweep unit to mute the channel until the
    /// condition ends: if the current period is less than 8... if at any
    /// time the target period is greater than $7FF." Note this "happens
    /// regardless of whether the sweep unit is disabled".
    pub(super) fn muting(&self, current: u16) -> bool {
        current < 8 || self.target_period(current) > 0x7FF
    }

    /// One half-frame clock, in the order the nesdev page's "Updating the
    /// period" section gives: the period update is evaluated first, then
    /// the divider/reload bookkeeping.
    pub(super) fn clock_half_frame(&mut self, current: &mut u16) {
        if self.divider == 0 && self.enabled && self.shift != 0 && !self.muting(*current) {
            *current = self.target_period(*current);
        }
        if self.divider == 0 || self.reload {
            self.divider = self.period;
            self.reload = false;
        } else {
            self.divider -= 1;
        }
    }
}
