//! Standard NES controller: `$4016`/`$4017` strobe/shift protocol.
//!
//! Reference: [nesdev.org/wiki/Standard_controller](https://www.nesdev.org/wiki/Standard_controller)
//! and [nesdev.org/wiki/Controller_reading](https://www.nesdev.org/wiki/Controller_reading).
//!
//! Hardware model: the controller's 8 buttons are parallel-loaded into a
//! shift register whenever the strobe line is held high, and shifted out
//! one bit per read once strobe goes low:
//!
//! - **Strobe high (`$4016` write, bit 0 = 1):** the shift register is
//!   *continuously reloaded* from the live button state — every read while
//!   strobe is high returns button A's current state (bit 0 of
//!   [`Controller::buttons`]), reflecting whatever the buttons are doing
//!   right now, not a frozen snapshot.
//! - **1→0 transition (`$4016` write, bit 0 goes 1 then 0):** this is the
//!   instant that *latches* — the button state at that exact moment is
//!   copied into the shift register and reading begins shifting it out
//!   from bit 0. Button changes *after* this point (before the next
//!   strobe-high pulse) must not be visible in the ongoing read sequence —
//!   this is the behavior the acceptance tests pin down explicitly.
//! - **Reads while strobed low:** each read returns the next bit in order
//!   **A, B, Select, Start, Up, Down, Left, Right** (bit 0 through bit 7 of
//!   the latched byte), then **1** for every read past the 8th on a
//!   standard controller (nesdev: "All subsequent bits will report 1s").
//!
//! Only the low bit is ever actually driven by this device; the caller
//! ([`crate::system::NesBus`]) is responsible for combining it with
//! whatever the rest of the shared data bus is doing (open bus) — see that
//! module's doc for how it does that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Controller {
    /// Live button state, bit 0..7 = A, B, Select, Start, Up, Down, Left,
    /// Right (the documented `$4016`/`$4017` read order). Set by the host
    /// via [`Controller::set_buttons`]; not itself affected by strobing.
    pub(super) buttons: u8,
    /// Whether the strobe line is currently held high (continuous-reload
    /// mode).
    pub(super) strobe: bool,
    /// The byte latched at the most recent 1→0 strobe transition. Only
    /// meaningful once `strobe` has gone low at least once; until then a
    /// standard controller's shift register content is undefined by
    /// nesdev, and we never read this field while `strobe` is true anyway
    /// (see `read`).
    pub(super) latched: u8,
    /// How many bits of `latched` have been shifted out so far (0..=8);
    /// clamped at 8 so every further read reports 1, matching a standard
    /// pad (no Four Score / expansion shift-through modeled here).
    pub(super) shift_index: u8,
}

impl Default for Controller {
    fn default() -> Self {
        Controller {
            buttons: 0,
            strobe: false,
            latched: 0,
            shift_index: 8,
        }
    }
}

impl Controller {
    pub fn new() -> Self {
        Self::default()
    }

    /// Host-side input hook: set the live button byte (bit layout per the
    /// module doc). Does not itself latch anything — only a 1→0 strobe
    /// write does that.
    pub fn set_buttons(&mut self, buttons: u8) {
        self.buttons = buttons;
    }

    /// Handle a write to this controller's strobe line (`$4016` bit 0;
    /// both controllers see the same write — `NesBus` is responsible for
    /// delivering it to both).
    pub fn write_strobe(&mut self, value: u8) {
        let new_strobe = value & 0x01 != 0;
        if self.strobe && !new_strobe {
            // 1->0 transition: latch *now*, not on the next read.
            self.latched = self.buttons;
            self.shift_index = 0;
        } else if new_strobe {
            // Strobe held/raised high: continuous-reload mode; reads
            // ignore `latched`/`shift_index` entirely while this is true.
            self.shift_index = 0;
        }
        self.strobe = new_strobe;
    }

    /// One shift-register read: returns bit 0 = the next button bit (or 1
    /// past the 8th), bits 1-7 always 0 (this device drives only D0 — see
    /// module doc).
    pub fn read_bit(&mut self) -> u8 {
        if self.strobe {
            return self.buttons & 0x01;
        }
        if self.shift_index >= 8 {
            return 1;
        }
        let bit = (self.latched >> self.shift_index) & 0x01;
        self.shift_index += 1;
        bit
    }

    /// Side-effect-free counterpart of [`Controller::read_bit`] — same
    /// value a real read would return, but the shift register never
    /// advances. Exists only for ticket W1-03's trace-logger disassembly
    /// peek (`crate::trace`), which must never perturb the state it is
    /// describing; a real CPU read must go through `read_bit` so the shift
    /// register actually advances.
    pub fn peek_bit(&self) -> u8 {
        if self.strobe {
            return self.buttons & 0x01;
        }
        if self.shift_index >= 8 {
            return 1;
        }
        (self.latched >> self.shift_index) & 0x01
    }
}
