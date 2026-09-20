//! Windows and colour math (ticket W7-05;
//! `docs/design/EMULATION_CORES.md` §3.3).
//!
//! ## Windows are layer masks, and they compose with LOGIC
//!
//! There are two windows, each a left/right span (`$2126`-`$2129`). Every
//! layer independently chooses which of the two it obeys and whether it
//! inverts each (`$2123`-`$2125`), and then combines them with one of
//! four operations (`$212A`/`$212B`): **OR, AND, XOR, XNOR**.
//!
//! The operation only matters when a layer enables BOTH windows — with
//! one window enabled, all four combiners agree. That is exactly why a
//! naive implementation that hard-codes OR passes most test content and
//! fails the rest, and why the tests below drive both-enabled cases
//! specifically.
//!
//! An inverted window is not "the other side of the span": `$2123`'s
//! invert bit flips the membership test, so an inverted window with
//! left > right (an empty span) covers *everything*.
//!
//! ## Colour math, and what a core can honestly emit
//!
//! Colour math (`$2130`-`$2132`) adds or subtracts the **sub-screen**
//! from the **main screen**, optionally halving the result, per layer.
//! Its output is an RGB value that need not exist anywhere in CGRAM.
//!
//! That collides with CLAUDE.md law 4: cores emit *indexed* pixels. A
//! blended colour has no palette index, so it cannot travel through
//! [`rf_core_api::PpuPixel`] at all — and the golden-frame hash, which
//! covers palette indices only, could not see it even if it did.
//!
//! So this module splits the feature honestly:
//!
//! * **[`ColorMath::clip_to_black`]** — the "clip main screen to black"
//!   path *is* expressible, because black is palette index 0. It runs on
//!   the indexed path and is gated by the colour window.
//! * **[`ColorMath::blend`]** — the add/sub/half arithmetic is computed
//!   here, in RGB, for tests and for a renderer that asks. It does NOT
//!   enter the `PpuPixel` stream.
//!
//! Carrying a sub-screen and a blend mode across `CoreSink` is an
//! `rf-core-api` contract change, which is outside this ticket. See the
//! W7-05 close note.

/// The two window spans and each layer's mask configuration.
#[derive(Debug, Clone, Copy, Default)]
pub struct Windows {
    /// `$2126`-`$2129`: window 1 and 2 left/right edges, inclusive.
    pub w1_left: u8,
    pub w1_right: u8,
    pub w2_left: u8,
    pub w2_right: u8,
    /// `$2123`-`$2125`, two bits per window per layer: enable and invert.
    /// Index 0-3 are BG1-BG4, 4 is OBJ, 5 is the colour window.
    pub enable: [(bool, bool); 6],
    pub invert: [(bool, bool); 6],
    /// `$212A`/`$212B`, two bits per layer.
    pub logic: [WindowLogic; 6],
    /// `$212E`/`$212F`: is the mask applied on the main / sub screen?
    pub main_mask: u8,
    pub sub_mask: u8,
}

/// How a layer combines its two windows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WindowLogic {
    #[default]
    Or,
    And,
    Xor,
    Xnor,
}

impl WindowLogic {
    #[must_use]
    pub fn from_bits(bits: u8) -> Self {
        match bits & 0x03 {
            0 => WindowLogic::Or,
            1 => WindowLogic::And,
            2 => WindowLogic::Xor,
            _ => WindowLogic::Xnor,
        }
    }

    #[must_use]
    pub fn apply(self, a: bool, b: bool) -> bool {
        match self {
            WindowLogic::Or => a || b,
            WindowLogic::And => a && b,
            WindowLogic::Xor => a != b,
            WindowLogic::Xnor => a == b,
        }
    }
}

impl Windows {
    /// Is `x` inside a layer's combined window?
    ///
    /// A layer with neither window enabled is never masked. With ONE
    /// enabled, the logic operation is irrelevant — which is why it can
    /// only be tested with both.
    #[must_use]
    pub fn masks(&self, layer: usize, x: u8) -> bool {
        let (e1, e2) = self.enable[layer];
        let (i1, i2) = self.invert[layer];
        let in1 = (x >= self.w1_left && x <= self.w1_right) != i1;
        let in2 = (x >= self.w2_left && x <= self.w2_right) != i2;
        match (e1, e2) {
            (false, false) => false,
            (true, false) => in1,
            (false, true) => in2,
            (true, true) => self.logic[layer].apply(in1, in2),
        }
    }

    /// Write one of `$2123`-`$212B`, `$2126`-`$2129`, `$212E`, `$212F`.
    pub fn write_register(&mut self, offset: u16, value: u8) {
        match offset {
            // Two layers per register, four bits each: W1 enable/invert
            // then W2 enable/invert.
            0x2123..=0x2125 => {
                let base = usize::from(offset - 0x2123) * 2;
                for half in 0..2 {
                    let layer = base + half;
                    if layer >= 6 {
                        break;
                    }
                    let bits = (value >> (half * 4)) & 0x0F;
                    self.invert[layer] = (bits & 0x01 != 0, bits & 0x04 != 0);
                    self.enable[layer] = (bits & 0x02 != 0, bits & 0x08 != 0);
                }
            }
            0x2126 => self.w1_left = value,
            0x2127 => self.w1_right = value,
            0x2128 => self.w2_left = value,
            0x2129 => self.w2_right = value,
            0x212A => {
                for (i, slot) in self.logic.iter_mut().take(4).enumerate() {
                    *slot = WindowLogic::from_bits(value >> (i * 2));
                }
            }
            0x212B => {
                self.logic[4] = WindowLogic::from_bits(value);
                self.logic[5] = WindowLogic::from_bits(value >> 2);
            }
            0x212E => self.main_mask = value,
            0x212F => self.sub_mask = value,
            _ => {}
        }
    }
}

/// `$2130`-`$2132`: colour math configuration.
#[derive(Debug, Clone, Copy, Default)]
pub struct ColorMath {
    /// `$2130` bits 6-7, "Force Main Screen Black": when the colour
    /// window forces the main screen to black. Per fullsnes ("Color Math
    /// Control Register A"): `0=Never, 1=NotMathWin (outside the
    /// window), 2=MathWindow (inside the window), 3=Always` — note the
    /// register's own value 1 means OUTSIDE and 2 means INSIDE, the
    /// opposite of the naive "1, 2, 3 ascend in coverage" reading
    /// (W14-42: this was inverted here for both `clip_mode` and
    /// `prevent_mode`, forcing ActRaiser 2/Illusion of Gaia/Robotrek's
    /// whole main screen to black whenever they left the colour window
    /// disabled with `clip_mode=2`, since "nowhere is inside a disabled
    /// window" then hit the swapped `2 => !inside_color_window` arm).
    pub clip_mode: u8,
    /// `$2130` bits 4-5, "Color Math Enable": when colour math is
    /// prevented (the logical negation of fullsnes's own "Enable"
    /// framing). Per fullsnes: `0=Always (enabled), 1=MathWindow
    /// (enabled only inside), 2=NotMathWin (enabled only outside),
    /// 3=Never (disabled)` — same inside/outside assignment to 1/2 as
    /// `clip_mode` above, and previously inverted the same way.
    pub prevent_mode: u8,
    /// `$2130` bit 1: use the sub-screen rather than the fixed colour.
    pub use_subscreen: bool,
    /// `$2131` bit 7: subtract rather than add.
    pub subtract: bool,
    /// `$2131` bit 6: halve the result.
    pub half: bool,
    /// `$2131` bits 0-5: per-layer enable (BG1-4, OBJ, backdrop).
    pub enable: u8,
    /// `$2132` fixed colour, as 5-bit BGR components.
    pub fixed_r: u8,
    pub fixed_g: u8,
    pub fixed_b: u8,
}

impl ColorMath {
    pub fn write_register(&mut self, offset: u16, value: u8) {
        match offset {
            0x2130 => {
                self.clip_mode = (value >> 6) & 0x03;
                self.prevent_mode = (value >> 4) & 0x03;
                self.use_subscreen = value & 0x02 != 0;
            }
            0x2131 => {
                self.subtract = value & 0x80 != 0;
                self.half = value & 0x40 != 0;
                self.enable = value & 0x3F;
            }
            0x2132 => {
                // One write can set any combination of the three
                // components — bits 5/6/7 select which the low five bits
                // apply to, so a single write can set all three at once.
                let level = value & 0x1F;
                if value & 0x20 != 0 {
                    self.fixed_r = level;
                }
                if value & 0x40 != 0 {
                    self.fixed_g = level;
                }
                if value & 0x80 != 0 {
                    self.fixed_b = level;
                }
            }
            _ => {}
        }
    }

    /// Should the main screen be forced to black at this position?
    ///
    /// This is the one part of colour math expressible on an indexed
    /// path, because black IS palette index 0.
    ///
    /// fullsnes ("Color Math Control Register A", `$2130` bits 6-7):
    /// `1=NotMathWin` forces black OUTSIDE the window, `2=MathWindow`
    /// forces black INSIDE it (W14-42 citation — see `clip_mode`'s doc).
    #[must_use]
    pub fn clip_to_black(&self, inside_color_window: bool) -> bool {
        match self.clip_mode {
            0 => false,
            1 => !inside_color_window,
            2 => inside_color_window,
            _ => true,
        }
    }

    /// Is colour math prevented at this position?
    ///
    /// fullsnes ("Color Math Control Register A", `$2130` bits 4-5):
    /// `1=MathWindow` means math is ENABLED (not prevented) INSIDE the
    /// window, so it is prevented outside; `2=NotMathWin` is enabled
    /// outside, prevented inside (W14-42 citation — see `prevent_mode`'s
    /// doc).
    #[must_use]
    pub fn prevented(&self, inside_color_window: bool) -> bool {
        match self.prevent_mode {
            0 => false,
            1 => !inside_color_window,
            2 => inside_color_window,
            _ => true,
        }
    }

    /// Blend a main-screen colour with a sub-screen (or fixed) colour.
    ///
    /// Components are 5-bit BGR555, as CGRAM stores them. The result is
    /// RGB and therefore **cannot** travel through the indexed
    /// `PpuPixel` stream — see the module doc.
    #[must_use]
    pub fn blend(&self, main: (u8, u8, u8), sub: (u8, u8, u8)) -> (u8, u8, u8) {
        let op = |m: u8, s: u8| -> u8 {
            let v = if self.subtract {
                i16::from(m) - i16::from(s)
            } else {
                i16::from(m) + i16::from(s)
            };
            let v = if self.half { v / 2 } else { v };
            v.clamp(0, 31) as u8
        };
        (op(main.0, sub.0), op(main.1, sub.1), op(main.2, sub.2))
    }

    /// The fixed colour, for when `$2130` bit 1 says not to use the
    /// sub-screen.
    #[must_use]
    pub fn fixed(&self) -> (u8, u8, u8) {
        (self.fixed_r, self.fixed_g, self.fixed_b)
    }

    /// `$2132`'s fixed colour packed as BGR555, the form CGRAM uses.
    ///
    /// Packed here rather than at the sink so the core hands out one
    /// colour encoding, not two — a renderer that had to know which of
    /// `fixed()` and CGRAM used which layout would eventually get it
    /// backwards.
    #[must_use]
    pub fn fixed_bgr555(&self) -> u16 {
        u16::from(self.fixed_r & 0x1F)
            | (u16::from(self.fixed_g & 0x1F) << 5)
            | (u16::from(self.fixed_b & 0x1F) << 10)
    }
}

/// `$2106` MOSAIC.
#[derive(Debug, Clone, Copy, Default)]
pub struct Mosaic {
    /// 1-16 pixels; the register stores size-1 in bits 4-7.
    pub size: u8,
    /// Bits 0-3: which backgrounds are affected.
    pub enable: u8,
}

impl Mosaic {
    pub fn write_register(&mut self, value: u8) {
        self.size = (value >> 4) + 1;
        self.enable = value & 0x0F;
    }

    /// Snap a coordinate to its mosaic block for `bg`.
    ///
    /// A size of 1 is a no-op, which is why most content never notices a
    /// broken implementation.
    #[must_use]
    pub fn snap(&self, bg: usize, v: u16) -> u16 {
        if self.size <= 1 || self.enable & (1 << bg) == 0 {
            return v;
        }
        v - (v % u16::from(self.size))
    }
}

impl WindowLogic {
    /// The 2-bit register encoding, for save states (ticket W7-09).
    ///
    /// The inverse, [`WindowLogic::from_bits`], already exists for the
    /// `$212A`/`$212B` decode and masks to two bits, so a round trip
    /// cannot produce an out-of-range value and needs no fallible form.
    fn to_bits(self) -> u8 {
        match self {
            WindowLogic::Or => 0,
            WindowLogic::And => 1,
            WindowLogic::Xor => 2,
            WindowLogic::Xnor => 3,
        }
    }
}

impl Windows {
    pub(crate) fn save(
        &self,
        o: &mut crate::state::StateOut,
    ) -> Result<(), rf_core_api::StateError> {
        o.u8(self.w1_left)?;
        o.u8(self.w1_right)?;
        o.u8(self.w2_left)?;
        o.u8(self.w2_right)?;
        for (a, b) in self.enable {
            o.bool(a)?;
            o.bool(b)?;
        }
        for (a, b) in self.invert {
            o.bool(a)?;
            o.bool(b)?;
        }
        for l in self.logic {
            o.u8(l.to_bits())?;
        }
        o.u8(self.main_mask)?;
        o.u8(self.sub_mask)
    }

    pub(crate) fn load(
        &mut self,
        i: &mut crate::state::StateIn,
    ) -> Result<(), rf_core_api::StateError> {
        self.w1_left = i.u8()?;
        self.w1_right = i.u8()?;
        self.w2_left = i.u8()?;
        self.w2_right = i.u8()?;
        for e in &mut self.enable {
            *e = (i.bool()?, i.bool()?);
        }
        for v in &mut self.invert {
            *v = (i.bool()?, i.bool()?);
        }
        for l in &mut self.logic {
            *l = WindowLogic::from_bits(i.u8()?);
        }
        self.main_mask = i.u8()?;
        self.sub_mask = i.u8()?;
        Ok(())
    }
}

impl ColorMath {
    pub(crate) fn save(
        &self,
        o: &mut crate::state::StateOut,
    ) -> Result<(), rf_core_api::StateError> {
        o.u8(self.clip_mode)?;
        o.u8(self.prevent_mode)?;
        o.bool(self.use_subscreen)?;
        o.bool(self.subtract)?;
        o.bool(self.half)?;
        o.u8(self.enable)?;
        o.u8(self.fixed_r)?;
        o.u8(self.fixed_g)?;
        o.u8(self.fixed_b)
    }

    pub(crate) fn load(
        &mut self,
        i: &mut crate::state::StateIn,
    ) -> Result<(), rf_core_api::StateError> {
        self.clip_mode = i.u8()?;
        self.prevent_mode = i.u8()?;
        self.use_subscreen = i.bool()?;
        self.subtract = i.bool()?;
        self.half = i.bool()?;
        self.enable = i.u8()?;
        self.fixed_r = i.u8()?;
        self.fixed_g = i.u8()?;
        self.fixed_b = i.u8()?;
        Ok(())
    }
}

impl Mosaic {
    pub(crate) fn save(
        &self,
        o: &mut crate::state::StateOut,
    ) -> Result<(), rf_core_api::StateError> {
        o.u8(self.size)?;
        o.u8(self.enable)
    }

    pub(crate) fn load(
        &mut self,
        i: &mut crate::state::StateIn,
    ) -> Result<(), rf_core_api::StateError> {
        self.size = i.u8()?;
        self.enable = i.u8()?;
        Ok(())
    }
}
