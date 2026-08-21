//! Frame timing, vblank, and the interrupt sources that hang off it
//! (ticket W6-02b; `docs/design/EMULATION_CORES.md` §3.1).
//!
//! ## Counted in master cycles, because everything else here is
//!
//! W6-01b made master cycles the CPU's unit; this module is where that
//! pays off. A scanline is **1364 master cycles** (341 dots × 4) and a
//! frame is **262 lines**, which puts the frame rate at
//! 21477270 / (1364 × 262) ≈ 60.1 Hz — the same arithmetic
//! `speed.rs`'s clock test pins.
//!
//! ## `$4210` RDNMI clears on read, and that is not a detail
//!
//! The vblank flag in bit 7 is **cleared by reading the register**. The
//! standard wait idiom depends on it:
//!
//! ```text
//! @wait1: bit $4210 : bmi @wait1     ; drain a flag already pending
//! @wait2: bit $4210 : bpl @wait2     ; now wait for the NEXT vblank
//! ```
//!
//! That is gilyon `cputest`'s `wait_for_vblank`, verbatim. Without
//! clear-on-read the first loop never exits and the ROM hangs before
//! running a single test — so this behaviour is load-bearing for the
//! whole suite, not an edge case. It is also why [`Timing::read_rdnmi`]
//! takes `&mut self` and why `peek` must never route to it.
//!
//! ## Auto-joypad
//!
//! When `$4200` bit 0 is set, the CPU reads the controllers
//! automatically starting just after vblank begins, taking roughly three
//! scanlines. `$4212` bit 0 reports "busy" for that window, and software
//! that reads `$4218`-`$421F` during it gets garbage — which is the
//! reason the busy flag exists and the reason this models the window
//! rather than just the result.

/// Master cycles per dot.
pub const MASTER_PER_DOT: u64 = 4;
/// Dots per scanline.
pub const DOTS_PER_LINE: u64 = 341;
/// Master cycles per scanline.
pub const MASTER_PER_LINE: u64 = DOTS_PER_LINE * MASTER_PER_DOT;
/// Scanlines per NTSC frame.
pub const LINES_PER_FRAME: u16 = 262;
/// First scanline of vblank in 224-line mode.
pub const VBLANK_START_LINE: u16 = 225;
/// First scanline of vblank with overscan (`$2133` bit 2) enabled.
///
/// Overscan does not add lines to the frame — 262 is 262 either way. It
/// moves the boundary, taking 15 lines from vblank and giving them to the
/// display. That means vblank gets SHORTER, which is why a game that
/// turns overscan on and keeps a long vblank DMA routine starts writing
/// VRAM during active display.
pub const VBLANK_START_LINE_OVERSCAN: u16 = 240;
/// Roughly three scanlines, per §3.1's "starts ~line 225, 3-4 lines".
pub const AUTO_JOYPAD_CYCLES: u64 = 4224;

/// What advancing the clock produced.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Events {
    /// Vblank began during this advance — the NMI edge.
    pub vblank_started: bool,
    /// An H/V IRQ comparison matched.
    pub irq: bool,
    /// The auto-joypad window just closed — the moment the controller
    /// state is latched into `$4218`-`$421F`.
    pub auto_joypad_done: bool,
    /// A new frame began: HDMA must re-initialise from its tables.
    pub frame_started: bool,
    /// How many visible scanlines were crossed — HDMA runs one unit per
    /// visible line, so this is a count and not a flag.
    pub visible_lines_crossed: u32,
}

/// The frame clock.
#[derive(Debug, Clone)]
pub struct Timing {
    /// Master cycles elapsed within the current scanline.
    pub line_cycles: u64,
    pub line: u16,
    pub frame: u64,
    /// `$4210` bit 7. Set when vblank begins, cleared by reading it.
    pub nmi_flag: bool,
    /// Master cycles remaining in the auto-joypad window.
    pub auto_joypad_remaining: u64,
    /// First vblank line: 225 normally, 240 with overscan (W7-06).
    pub vblank_start: u16,
}

impl Default for Timing {
    fn default() -> Self {
        Self::new()
    }
}

impl Timing {
    #[must_use]
    pub fn new() -> Self {
        Self {
            vblank_start: VBLANK_START_LINE,
            line_cycles: 0,
            line: 0,
            frame: 0,
            nmi_flag: false,
            auto_joypad_remaining: 0,
        }
    }

    /// Current dot within the scanline.
    #[must_use]
    pub fn dot(&self) -> u16 {
        (self.line_cycles / MASTER_PER_DOT) as u16
    }

    #[must_use]
    pub fn in_vblank(&self) -> bool {
        self.line >= self.vblank_start
    }

    /// Set by the PPU when `$2133`'s overscan bit changes (ticket W7-06).
    pub fn set_overscan(&mut self, overscan: bool) {
        self.vblank_start = if overscan {
            VBLANK_START_LINE_OVERSCAN
        } else {
            VBLANK_START_LINE
        };
    }

    #[must_use]
    pub fn auto_joypad_busy(&self) -> bool {
        self.auto_joypad_remaining > 0
    }

    /// `$4210` RDNMI — bit 7 is the vblank flag, **cleared by this read**.
    /// The low nibble is the CPU version (2 on every retail console).
    pub fn read_rdnmi(&mut self) -> u8 {
        let v = (u8::from(self.nmi_flag) << 7) | 0x02;
        self.nmi_flag = false;
        v
    }

    /// `$4212` HVBJOY — vblank, hblank and auto-joypad-busy flags.
    #[must_use]
    pub fn read_hvbjoy(&self) -> u8 {
        let mut v = 0;
        if self.in_vblank() {
            v |= 0x80;
        }
        // Hblank is roughly dot 274 onward.
        if self.dot() >= 274 {
            v |= 0x40;
        }
        if self.auto_joypad_busy() {
            v |= 0x01;
        }
        v
    }

    /// Advance by `cycles` master cycles.
    ///
    /// `irq_match` is asked once per dot crossed, so an H/V comparison
    /// cannot be stepped over by a long instruction — which is exactly
    /// how a coarser model loses IRQs that a game relies on.
    pub fn advance(
        &mut self,
        cycles: u64,
        auto_joypad_enabled: bool,
        mut irq_match: impl FnMut(u16, u16) -> bool,
    ) -> Events {
        let mut events = Events::default();
        let mut left = cycles;

        while left > 0 {
            let to_next_dot = MASTER_PER_DOT - (self.line_cycles % MASTER_PER_DOT);
            let chunk = left.min(to_next_dot);
            self.line_cycles += chunk;
            left -= chunk;
            if self.auto_joypad_remaining > 0 {
                self.auto_joypad_remaining = self.auto_joypad_remaining.saturating_sub(chunk);
                // Latching on the CLOSING EDGE, not "whenever idle".
                // Software reads $4218 expecting the value sampled by the
                // window that just finished; re-latching continuously
                // would make the port track the controller in real time
                // and quietly hide any bug in the window's timing.
                if self.auto_joypad_remaining == 0 {
                    events.auto_joypad_done = true;
                }
            }

            if self.line_cycles >= MASTER_PER_LINE {
                self.line_cycles -= MASTER_PER_LINE;
                self.line += 1;
                if self.line >= LINES_PER_FRAME {
                    self.line = 0;
                    self.frame += 1;
                }
                if self.line < self.vblank_start {
                    events.visible_lines_crossed += 1;
                }
                if self.line == 0 {
                    events.frame_started = true;
                }
                if self.line == self.vblank_start {
                    // The NMI edge. The flag latches here and stays set
                    // until something reads $4210 — a game that never
                    // reads it still sees it on the next poll.
                    self.nmi_flag = true;
                    events.vblank_started = true;
                    if auto_joypad_enabled {
                        self.auto_joypad_remaining = AUTO_JOYPAD_CYCLES;
                    }
                }
            }

            if self.line_cycles.is_multiple_of(MASTER_PER_DOT) && irq_match(self.dot(), self.line) {
                events.irq = true;
            }
        }
        events
    }
}

/// The four controller-button words latched by auto-joypad
/// (`$4218`-`$421F`).
#[derive(Debug, Clone, Copy, Default)]
pub struct Joypads {
    pub ports: [u16; 4],
    /// What auto-joypad latched, which is what `$4218`+ actually return.
    pub latched: [u16; 4],
}

impl Joypads {
    /// Called when the auto-joypad window closes.
    pub fn latch(&mut self) {
        self.latched = self.ports;
    }
}

impl Timing {
    /// Serialise the raster clock (ticket W7-09).
    ///
    /// `vblank_start` is saved rather than recomputed from SETINI: it is
    /// pushed into this type by the system when `$2133` is written, so a
    /// restore that recomputed it would depend on region ordering.
    pub(crate) fn save(
        &self,
        o: &mut crate::state::StateOut,
    ) -> Result<(), rf_core_api::StateError> {
        o.u64(self.line_cycles)?;
        o.u16(self.line)?;
        o.u64(self.frame)?;
        o.bool(self.nmi_flag)?;
        o.u64(self.auto_joypad_remaining)?;
        o.u16(self.vblank_start)
    }

    pub(crate) fn load(
        &mut self,
        i: &mut crate::state::StateIn,
    ) -> Result<(), rf_core_api::StateError> {
        self.line_cycles = i.u64()?;
        self.line = i.u16()?;
        self.frame = i.u64()?;
        self.nmi_flag = i.bool()?;
        self.auto_joypad_remaining = i.u64()?;
        self.vblank_start = i.u16()?;
        Ok(())
    }
}

impl Joypads {
    pub(crate) fn save(
        &self,
        o: &mut crate::state::StateOut,
    ) -> Result<(), rf_core_api::StateError> {
        for v in self.ports {
            o.u16(v)?;
        }
        for v in self.latched {
            o.u16(v)?;
        }
        Ok(())
    }

    pub(crate) fn load(
        &mut self,
        i: &mut crate::state::StateIn,
    ) -> Result<(), rf_core_api::StateError> {
        for v in &mut self.ports {
            *v = i.u16()?;
        }
        for v in &mut self.latched {
            *v = i.u16()?;
        }
        Ok(())
    }
}
