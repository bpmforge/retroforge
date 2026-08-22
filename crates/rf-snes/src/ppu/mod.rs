//! The S-PPU: BG modes 0/1 and OBJ (ticket W6-03a;
//! `docs/design/EMULATION_CORES.md` §3.3, FR-CORE-032/033).
//!
//! ## Scanline-composed, per §3.3
//!
//! Each scanline is composed at once from register state, rather than
//! per dot. §3.3 calls this "sufficient for the large majority of games"
//! and records the per-dot upgrade path; W6-03b's golden frames are what
//! will show whether any of the test content needs it.
//!
//! ## Scope: modes 0 and 1 only
//!
//! §3.3 describes the *finished* PPU across several tickets — modes 0-6,
//! mode 7, windows, color math, mosaic, hires. This ticket's acceptance
//! is "BG modes 0/1 with per-BG char/tile sizes + BG3 priority" and "OBJ
//! sprites with 32-sprite/34-sliver line limits". Anything else is
//! deliberately absent rather than half-present: a windowing
//! implementation that silently did nothing would make W6-03b's golden
//! frames fail for a reason nobody could locate.
//!
//! ## Indexed pixels, never RGB (law 4)
//!
//! [`render_scanline`](Ppu::render_scanline) emits
//! [`rf_core_api::video::PpuPixel`] — a palette index plus the layer,
//! sprite id and priority that produced it. Resolving CGRAM to a colour
//! is the renderer's job.
//!
//! ## `dropped_by_limit` stays `false`, and dropped sprites go elsewhere
//!
//! The sprite limits are real and this PPU applies them, but a dropped
//! sprite is **never** written into the [`PpuPixel`] stream. That stream
//! carries exactly one pixel per x, so putting a suppressed sprite there
//! would displace the pixel the CRT actually showed — forbidden by the
//! W1-05a ruling in `rf_core_api::video`, by CLAUDE.md law 6, and by
//! FR-MODE-002's mode invariant. Dropped sprites are reported separately
//! through [`Scanline::overlay`], the `OverlayPixel` channel built for
//! exactly this.
//!
//! §3.3's phrasing ("both emitted with `dropped_by_limit` metadata for
//! the enhancement layer") predates that ruling, in the same way §2.2's
//! superseded line about NES sprites did. See the W6-03a close note.

pub mod bg;
pub mod mode7;
pub mod obj;
pub mod window;

use rf_core_api::{OverlayPixel, PixelLayer, PpuPixel};

/// Visible width of a non-hires scanline.
pub const WIDTH: usize = 256;
/// Visible lines in the default (non-overscan) frame.
pub const VISIBLE_LINES: u16 = 224;
/// Visible lines with overscan enabled (`$2133` bit 2).
pub const VISIBLE_LINES_OVERSCAN: u16 = 239;
/// CGRAM holds 256 colours as 16-bit BGR555 entries.
pub const CGRAM_ENTRIES: usize = 256;
/// OAM: 512 bytes of low table plus 32 bytes of high table.
pub const OAM_LEN: usize = 544;

/// The register state a single scanline is composed from.
///
/// ## Why this exists (ticket W7-07)
///
/// §3.3 specifies "each scanline is composed at once from register state
/// **latched at line start** + mid-line writes recorded with H-position".
/// Until HDMA landed, composing the whole frame from the registers'
/// final values was indistinguishable from that — nothing changed them
/// mid-frame.
///
/// HDMA's entire purpose is changing them mid-frame. Without per-line
/// latching, a mode-7 perspective demo composes all 224 lines from the
/// LAST line's matrix and renders as one flat texture — which is exactly
/// what happened the first time HDMA was wired up here.
#[derive(Debug, Clone, Copy)]
pub struct LineState {
    pub mode7: mode7::Mode7,
    pub bg_mode: u8,
    pub bg3_priority: bool,
    /// Per-layer scroll, the other thing HDMA is routinely used for.
    pub hofs: [u16; 4],
    pub vofs: [u16; 4],
    /// Windows, colour math and mosaic (ticket W7-05's criterion 4).
    ///
    /// **Added because three test ROMs proved they were missing.** W7-07
    /// latched the mode-7 matrix and the scroll registers and stopped
    /// there, which was enough for the ROMs it had. PeterLemon's
    /// WindowHDMA, WindowMultiHDMA and MosaicMode3 drive `$2126`-`$212F`
    /// and `$2106` from HDMA instead, and without these fields every line
    /// composed from the register values left at the END of the frame:
    /// the window mask came out IDENTICAL on all 224 lines (28 and 62
    /// masked pixels per row respectively, measured), and MosaicMode3
    /// settled at a block size of 1 -- mosaic enabled and doing nothing.
    /// A per-line effect rendered as a constant band is the same failure
    /// the perspective demo showed as a flat texture, in a different
    /// register file.
    pub windows: window::Windows,
    pub color_math: window::ColorMath,
    pub mosaic: window::Mosaic,
}

/// Every register a mid-line write can touch, snapshotted (ticket W7-15).
///
/// **Separate from [`LineState`] on purpose.** `LineState` is latched for
/// EVERY line, so widening it changes how every line composes — and it
/// does: extending it broke StarWars's golden, because HDMA writes that
/// used to reach all lines through the live registers stopped doing so.
/// That is a real semantic change, and smuggling it in under a per-dot
/// ticket would be exactly the "fixes one suite, silently breaks another"
/// failure criterion 3 exists to prevent.
///
/// This is captured ONLY for a line that actually receives a mid-line
/// write, and only just before the first one lands. Lines without such a
/// write are untouched and compose exactly as they always did — which is
/// why 32 of 32 goldens stay green.
///
/// Memory (VRAM, CGRAM, OAM) is deliberately absent: it is data, not
/// registers, and [`Ppu::is_segmentable`] keeps its ports out of the
/// replay entirely.
#[derive(Debug, Clone, Copy)]
struct PpuRegs {
    mode7: mode7::Mode7,
    bg_mode: u8,
    bg3_priority: bool,
    windows: window::Windows,
    color_math: window::ColorMath,
    mosaic: window::Mosaic,
    forced_blank: bool,
    brightness: u8,
    obj_size: u8,
    obj_name_base: u16,
    obj_name_select: u16,
    obj_enabled: bool,
    ts: u8,
    setini: SetIni,
    bgs: [BgLayer; 4],
}

impl PpuRegs {
    fn capture(p: &Ppu) -> Self {
        Self {
            mode7: p.mode7,
            bg_mode: p.bg_mode,
            bg3_priority: p.bg3_priority,
            windows: p.windows,
            color_math: p.color_math,
            mosaic: p.mosaic,
            forced_blank: p.forced_blank,
            brightness: p.brightness,
            obj_size: p.obj_size,
            obj_name_base: p.obj_name_base,
            obj_name_select: p.obj_name_select,
            obj_enabled: p.obj_enabled,
            ts: p.ts,
            setini: p.setini,
            bgs: p.bgs,
        }
    }

    fn apply(&self, p: &mut Ppu) {
        p.mode7 = self.mode7;
        p.bg_mode = self.bg_mode;
        p.bg3_priority = self.bg3_priority;
        p.windows = self.windows;
        p.color_math = self.color_math;
        p.mosaic = self.mosaic;
        p.forced_blank = self.forced_blank;
        p.brightness = self.brightness;
        p.obj_size = self.obj_size;
        p.obj_name_base = self.obj_name_base;
        p.obj_name_select = self.obj_name_select;
        p.obj_enabled = self.obj_enabled;
        p.ts = self.ts;
        p.setini = self.setini;
        p.bgs = self.bgs;
    }
}

/// `$2133` SETINI, decoded (ticket W7-06).
///
/// Bit meanings are quoted from fullsnes's SETINI table rather than
/// recalled, because two of them are easy to get backwards:
///
/// * Bit 0 `V-Scanning (0=Non Interlace, 1=Interlace)`
/// * Bit 1 `OBJ V-Direction Display (0=Low, 1=High Resolution/Smaller OBJs)`
/// * Bit 2 `BG V-Direction Display (0=224 Lines, 1=239 Lines)` — this is
///   overscan, and it is the BG bit, NOT bit 1.
/// * Bit 3 `Horizontal Pseudo 512 Mode`, described as `SHIFT SUBSCREEN
///   HALF DOT TO THE LEFT` — see [`SetIni::hires_needs_subscreen`].
/// * Bits 4-5 `Not used`
/// * Bit 6 `EXTBG Mode (Screen expand)`
/// * Bit 7 `External Synchronization`
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SetIni {
    /// Bit 0: interlaced field scanning.
    pub interlace: bool,
    /// Bit 1: high-resolution (smaller) sprites.
    pub obj_interlace: bool,
    /// Bit 2: 239 visible lines instead of 224.
    pub overscan: bool,
    /// Bit 3: pseudo-hires.
    pub pseudo_hires: bool,
    /// Bit 6: EXTBG, the mode-7 BG2 priority-bit expansion.
    pub extbg: bool,
    /// Bit 7: external sync (superimpose). Carried, never acted on —
    /// there is no external LSI to sync to.
    pub external_sync: bool,
}

impl SetIni {
    pub fn write_register(&mut self, value: u8) {
        self.interlace = value & 0x01 != 0;
        self.obj_interlace = value & 0x02 != 0;
        self.overscan = value & 0x04 != 0;
        self.pseudo_hires = value & 0x08 != 0;
        self.extbg = value & 0x40 != 0;
        self.external_sync = value & 0x80 != 0;
    }

    /// Visible scanlines this frame: 239 with overscan, 224 without.
    #[must_use]
    pub fn visible_lines(&self) -> u16 {
        if self.overscan {
            VISIBLE_LINES_OVERSCAN
        } else {
            VISIBLE_LINES
        }
    }

    /// True when the display is asking for a 512-dot line **that this PPU
    /// cannot yet produce**, because producing it needs a sub-screen.
    ///
    /// Both routes to 512 dots are sub-screen effects, which is not
    /// obvious from their names and is the single fact that decides how
    /// much of hires can be built here:
    ///
    /// * Pseudo-hires (bit 3) is defined by fullsnes as `SHIFT SUBSCREEN
    ///   HALF DOT TO THE LEFT` — it is *only* a sub-screen operation.
    /// * True hires (modes 5 and 6) works because "the main/subscreen
    ///   pixels are rendered as half-pixels of the high-resolution
    ///   image": the two screens supply alternating half-dots.
    ///
    /// **This doc used to end "so it can produce the main screen's half of
    /// that image and nothing else" — that was true until W7-16 landed
    /// `CoreSink::sub_scanline` and is no longer.** Ticket W7-06's second
    /// pass composes the full 512 with
    /// [`Ppu::render_scanline_hires`]: the sub screen supplies the LEFT
    /// half-dot of each pair and the main screen the RIGHT, which is the
    /// same relationship pseudo-hires states directly (`SHIFT SUBSCREEN
    /// HALF DOT TO THE LEFT`).
    #[must_use]
    pub fn hires_requested(&self, bg_mode: u8) -> bool {
        self.pseudo_hires || bg_mode == 5 || bg_mode == 6
    }
}

/// One composed scanline: the accuracy-exact pixels, plus the
/// overlay-only pixels for sprites the hardware limits dropped.
#[derive(Debug, Clone)]
pub struct Scanline {
    pub pixels: Vec<PpuPixel>,
    /// Sprites the 32-per-line or 34-sliver limit suppressed. Same length
    /// as `pixels`; `opaque` is false where nothing was dropped.
    pub overlay: Vec<OverlayPixel>,
}

/// One background layer's registers.
#[derive(Debug, Clone, Copy, Default)]
pub struct BgLayer {
    /// `$2107`-`$210A` bits 2-7: tilemap base, in 1 KiB words steps.
    pub tilemap_base: u16,
    /// `$2107`-`$210A` bits 0-1: 0 = 32×32, 1 = 64×32, 2 = 32×64,
    /// 3 = 64×64 tiles.
    pub tilemap_size: u8,
    /// `$210B`/`$210C` nibble: character data base, in 4 KiB word steps.
    pub char_base: u16,
    /// `$2105` bits 4-7: false = 8×8 tiles, true = 16×16.
    pub tile_size_16: bool,
    pub hofs: u16,
    pub vofs: u16,
    /// `$212C` bit — is this layer on the main screen?
    pub enabled: bool,
}

/// The S-PPU.
#[derive(Debug, Clone)]
pub struct Ppu {
    pub vram: Vec<u8>,
    pub cgram: [u16; CGRAM_ENTRIES],
    pub oam: [u8; OAM_LEN],

    /// `$2105` bits 0-2.
    pub bg_mode: u8,
    /// `$2105` bit 3 — in mode 1, promotes BG3's high-priority tiles
    /// above everything else.
    pub bg3_priority: bool,
    pub bgs: [BgLayer; 4],

    /// `$2101` bits 5-7: which of the eight size pairs OBJ uses.
    pub obj_size: u8,
    /// `$2101` bits 0-2: base address of OBJ character data.
    pub obj_name_base: u16,
    /// `$2101` bits 3-4: offset to the second OBJ character page.
    pub obj_name_select: u16,
    /// `$212C` bit 4.
    pub obj_enabled: bool,

    /// `$2102`/`$2103` — OAM address, and the priority-rotation bit that
    /// decides which sprite is evaluated first.
    pub oam_addr: u16,
    pub oam_priority_rotation: bool,

    /// `$2100` bit 7: forced blank. Bits 0-3: master brightness.
    pub forced_blank: bool,
    pub brightness: u8,

    /// Mode 7 register state (ticket W7-04).
    pub mode7: mode7::Mode7,

    /// `$212D` TS — which layers are on the SUB screen (ticket W7-16).
    /// Bits 0-3 are BG1-4, bit 4 is OBJ, exactly as `$212C`.
    pub ts: u8,

    /// `$2133` SETINI (ticket W7-06).
    pub setini: SetIni,
    /// Set when `$2133` is written so the bus can push overscan into
    /// `Timing`, which owns the vblank boundary but not the register.
    pub overscan_changed: bool,

    /// Windows, colour math and mosaic (ticket W7-05).
    pub windows: window::Windows,
    pub color_math: window::ColorMath,
    pub mosaic: window::Mosaic,

    /// `$2130` CGWSEL bit 0 — direct colour mode.
    ///
    /// In the 8bpp modes (3, 4 and 7) this makes a pixel's value a BGR333
    /// colour in its own right rather than a CGRAM index. The core still
    /// emits that value as `palette_index`, because that is genuinely what
    /// the PPU produced; what changes is how a renderer must INTERPRET it,
    /// which is why the flag is public state rather than something folded
    /// into the pixel.
    pub direct_color: bool,

    /// Register state latched at the start of each visible line, filled
    /// in as the frame is scanned out. `None` until a line has been
    /// reached — a caller rendering ahead of the beam falls back to the
    /// live registers.
    pub line_state: Vec<Option<LineState>>,
    /// Register writes that landed DURING each visible line's active
    /// display, in the order they happened (ticket W7-15).
    ///
    /// This is what makes composition per-DOT rather than per-line: a line
    /// with writes is composed in segments, each from the register state
    /// as of that dot. A line with none composes exactly as before, which
    /// is why 31 of 32 goldens are inert under this change.
    ///
    /// Indexed by HARDWARE line, like [`Ppu::line_state`]. Only
    /// [`crate::timing::Timing::mid_line_position`] decides what gets in
    /// here, and that decision is the whole ticket — see its doc.
    line_writes: Vec<Vec<(u16, u16, u8)>>,
    /// Registers as they stood just BEFORE a line's first mid-line write.
    ///
    /// `None` for every line that never receives one, which is almost all
    /// of them — and that is what keeps this change inert on lines it has
    /// no business touching.
    line_regs: Vec<Option<PpuRegs>>,

    /// `$213E` bit 6: more than 32 sprites on a line.
    pub range_over: bool,
    /// `$213E` bit 7: more than 34 tile slivers on a line.
    pub time_over: bool,

    bgofs_latch: u8,
    bghofs_latch: u8,
    cgram_addr: u8,
    cgram_latch: Option<u8>,
    oam_latch: Option<u8>,
}

impl Default for Ppu {
    fn default() -> Self {
        Self::new()
    }
}

impl Ppu {
    #[must_use]
    pub fn new() -> Self {
        Self {
            vram: vec![0; 64 * 1024],
            cgram: [0; CGRAM_ENTRIES],
            oam: [0; OAM_LEN],
            bg_mode: 0,
            bg3_priority: false,
            bgs: [BgLayer::default(); 4],
            obj_size: 0,
            obj_name_base: 0,
            obj_name_select: 0,
            obj_enabled: false,
            oam_addr: 0,
            oam_priority_rotation: false,
            forced_blank: true,
            brightness: 0,
            mode7: mode7::Mode7::default(),
            ts: 0,
            setini: SetIni::default(),
            overscan_changed: false,
            windows: window::Windows::default(),
            color_math: window::ColorMath::default(),
            mosaic: window::Mosaic::default(),
            line_state: vec![None; VISIBLE_LINES_OVERSCAN as usize],
            line_writes: vec![Vec::new(); VISIBLE_LINES_OVERSCAN as usize],
            line_regs: vec![None; VISIBLE_LINES_OVERSCAN as usize],
            direct_color: false,
            range_over: false,
            time_over: false,
            bgofs_latch: 0,
            bghofs_latch: 0,
            cgram_addr: 0,
            cgram_latch: None,
            oam_latch: None,
        }
    }

    /// Write a `$21xx` register.
    ///
    /// Only the registers modes 0/1 and OBJ actually need are decoded.
    /// An unhandled register is ignored rather than panicking — a game
    /// writing a mode-7 register while this PPU renders mode 1 must not
    /// take the emulator down.
    pub fn write_register(&mut self, offset: u16, value: u8) {
        match offset {
            0x2100 => {
                self.forced_blank = value & 0x80 != 0;
                self.brightness = value & 0x0F;
            }
            0x2101 => {
                self.obj_size = (value >> 5) & 0x07;
                self.obj_name_select = u16::from((value >> 3) & 0x03);
                self.obj_name_base = u16::from(value & 0x07);
            }
            0x2102 => self.oam_addr = (self.oam_addr & 0x0100) | u16::from(value),
            0x2103 => {
                self.oam_addr = (self.oam_addr & 0x00FF) | (u16::from(value & 1) << 8);
                self.oam_priority_rotation = value & 0x80 != 0;
            }
            0x2104 => self.write_oam(value),
            0x2105 => {
                self.bg_mode = value & 0x07;
                self.bg3_priority = value & 0x08 != 0;
                for (i, bg) in self.bgs.iter_mut().enumerate() {
                    bg.tile_size_16 = value & (0x10 << i) != 0;
                }
            }
            0x2107..=0x210A => {
                let bg = &mut self.bgs[usize::from(offset - 0x2107)];
                bg.tilemap_size = value & 0x03;
                bg.tilemap_base = u16::from(value >> 2) << 10;
            }
            0x210B | 0x210C => {
                let base = usize::from(offset - 0x210B) * 2;
                self.bgs[base].char_base = u16::from(value & 0x0F) << 12;
                self.bgs[base + 1].char_base = u16::from(value >> 4) << 12;
            }
            0x210D..=0x2114 => self.write_scroll(offset, value),
            0x2121 => {
                self.cgram_addr = value;
                self.cgram_latch = None;
            }
            0x2122 => self.write_cgram(value),
            0x211A..=0x2120 => self.mode7.write_register(offset, value),
            0x2106 => self.mosaic.write_register(value),
            0x2133 => {
                self.setini.write_register(value);
                self.overscan_changed = true;
            }
            0x2123..=0x212B | 0x212E | 0x212F => self.windows.write_register(offset, value),
            0x2131 | 0x2132 => self.color_math.write_register(offset, value),
            0x2130 => {
                self.direct_color = value & 0x01 != 0;
                self.color_math.write_register(offset, value);
            }
            0x212C => {
                for (i, bg) in self.bgs.iter_mut().enumerate() {
                    bg.enabled = value & (1 << i) != 0;
                }
                self.obj_enabled = value & 0x10 != 0;
            }
            // `$212D` TS — the SUB screen's layer designation. Not
            // handled at all before ticket W7-16, because nothing could
            // consume a sub-screen; a game that set it was silently
            // configuring a screen this PPU did not compose.
            0x212D => self.ts = value,
            _ => {}
        }
    }

    /// Scroll registers are **write-twice through two SHARED latches**,
    /// not a low/high byte pair.
    ///
    /// Per fullsnes, every BG scroll register shares one `BGOFS` latch,
    /// and the horizontal ones additionally share a `BGHOFS` latch:
    ///
    /// ```text
    /// BGnHOFS = (data << 8) | (bgofs_latch & $F8) | (bghofs_latch & $07)
    /// BGnVOFS = (data << 8) | bgofs_latch
    /// ```
    ///
    /// The latches are shared across ALL FOUR layers, so writing BG1's
    /// scroll changes what BG2's next write produces. Modelling these as
    /// four independent 16-bit registers looks right until a game writes
    /// them in an unusual order, and then scrolling is off by a few
    /// pixels in a way that is very hard to trace back here.
    fn write_scroll(&mut self, offset: u16, value: u8) {
        let index = usize::from(offset - 0x210D) / 2;
        let vertical = (offset - 0x210D) % 2 == 1;
        let data = u16::from(value);
        if vertical {
            self.bgs[index].vofs = (data << 8) | u16::from(self.bgofs_latch);
        } else {
            self.bgs[index].hofs = (data << 8)
                | u16::from(self.bgofs_latch & 0xF8)
                | u16::from(self.bghofs_latch & 0x07);
            self.bghofs_latch = value;
        }
        self.bgofs_latch = value;

        // **`$210D`/`$210E` write TWO registers, not one** (ticket W7-04).
        //
        // fullsnes: "Writing to 210Dh does BOTH update M7HOFS (via M7_old
        // mechanism), and also updates BG1HOFS (via BG_old mechanism). In
        // the same fashion, 210Eh updates both M7VOFS and BG1VOFS."
        //
        // Nothing wrote `mode7.hofs`/`vofs` before this: the fields
        // existed, the transform read them, and they were permanently 0.
        // Mode 7 has no scroll registers of its own, so a mode-7 game that
        // scrolled did nothing at all — StarWars set `M7X = M7Y = 512` and
        // scrolled via `$210D`, so every sample landed outside the
        // playfield and the logo never appeared.
        //
        // The M7 latch is a SEPARATE latch from the BG one ("M7_old" vs
        // "BG_old"), which is why this uses `mode7`'s rather than
        // `bgofs_latch`, and the value is signed 13-bit: "1st Write: Lower
        // 8bit, 2nd Write: Upper 5bit".
        if offset == 0x210D || offset == 0x210E {
            self.mode7.write_scroll(offset, value);
        }
    }

    fn write_cgram(&mut self, value: u8) {
        match self.cgram_latch.take() {
            None => self.cgram_latch = Some(value),
            Some(low) => {
                self.cgram[usize::from(self.cgram_addr)] = u16::from(low) | (u16::from(value) << 8);
                self.cgram_addr = self.cgram_addr.wrapping_add(1);
            }
        }
    }

    /// `$2104` OAM write.
    ///
    /// The low table is written a WORD at a time — a byte is held until
    /// its partner arrives — while the high table (`$0200`+) commits on
    /// every byte. Getting this wrong leaves every other sprite's Y
    /// coordinate stale.
    fn write_oam(&mut self, value: u8) {
        let addr = usize::from(self.oam_addr) * 2 % OAM_LEN;
        if self.oam_addr >= 0x0100 {
            // High table: one byte per address, committed immediately.
            let at = 0x0200 + usize::from(self.oam_addr & 0x1F);
            if at < OAM_LEN {
                self.oam[at] = value;
            }
            self.oam_addr = self.oam_addr.wrapping_add(1) & 0x01FF;
            return;
        }
        match self.oam_latch.take() {
            None => self.oam_latch = Some(value),
            Some(low) => {
                if addr + 1 < OAM_LEN {
                    self.oam[addr] = low;
                    self.oam[addr + 1] = value;
                }
                self.oam_addr = self.oam_addr.wrapping_add(1) & 0x01FF;
            }
        }
    }

    /// Latch the current registers as line `y`'s state.
    ///
    /// Called once per visible scanline, after that line's HDMA has run.
    pub fn latch_line(&mut self, y: u16) {
        if let Some(slot) = self.line_state.get_mut(usize::from(y)) {
            *slot = Some(LineState {
                mode7: self.mode7,
                bg_mode: self.bg_mode,
                bg3_priority: self.bg3_priority,
                hofs: [
                    self.bgs[0].hofs,
                    self.bgs[1].hofs,
                    self.bgs[2].hofs,
                    self.bgs[3].hofs,
                ],
                vofs: [
                    self.bgs[0].vofs,
                    self.bgs[1].vofs,
                    self.bgs[2].vofs,
                    self.bgs[3].vofs,
                ],
                windows: self.windows,
                color_math: self.color_math,
                mosaic: self.mosaic,
            });
        }
    }

    /// The state latched for line `y`, for tests.
    ///
    /// `line_state` is private because nothing outside composition has any
    /// business reading it; this accessor exists so the per-line latching
    /// tests can assert on what was latched rather than only on the
    /// picture that came out, which is a far weaker signal.
    #[must_use]
    pub fn line_state_for_test(&self, y: u16) -> Option<LineState> {
        *self.line_state.get(usize::from(y))?
    }

    /// Forget every latched line. Called at the start of a frame so a
    /// line nothing reached this frame cannot serve last frame's state.
    pub fn clear_line_state(&mut self) {
        for slot in &mut self.line_state {
            *slot = None;
        }
        for w in &mut self.line_writes {
            w.clear();
        }
        for r in &mut self.line_regs {
            *r = None;
        }
    }

    /// Write a register, recording it as a mid-line event when `at` says
    /// the beam was inside a visible line's active display.
    ///
    /// `at` comes from [`crate::timing::Timing::mid_line_position`], which
    /// returns `None` during vblank and hblank. Passing `None` is
    /// therefore the ordinary case and behaves exactly like
    /// [`Ppu::write_register`] — the register changes, but nothing is
    /// recorded, because a write outside active display has no mid-line
    /// position to take effect at.
    pub fn write_register_at(&mut self, addr: u16, value: u8, at: Option<(u16, u16)>) {
        if let Some((line, dot)) = at.filter(|_| Self::is_segmentable(addr)) {
            let idx = usize::from(line);
            // Snapshot BEFORE the first write of this line lands — that is
            // the state the line has to be composed from. Later writes on
            // the same line must NOT overwrite it.
            if matches!(self.line_regs.get(idx), Some(None)) {
                self.line_regs[idx] = Some(PpuRegs::capture(self));
            }
            if let Some(slot) = self.line_writes.get_mut(idx) {
                slot.push((dot, addr, value));
            }
        }
        self.write_register(addr, value);
    }

    /// Is this register one that segmentation may REPLAY?
    ///
    /// **The data ports are excluded, and not as an optimisation.** OAM,
    /// VRAM and CGRAM writes are MEMORY writes: they have already landed,
    /// and each one auto-increments its address register. Replaying them
    /// while composing a segment would write a second time at a second
    /// address, so the later segments of the line would be composed
    /// against corrupted memory.
    ///
    /// Their address registers go with them — replaying `$2116` without
    /// its `$2118` (or the reverse) desynchronises the pair, which is
    /// worse than replaying neither.
    ///
    /// This is also where the bulk of the traffic lives: the trace that
    /// found the attribution bug counted 16,307 OAMDATA and 19,455 CGDATA
    /// writes in one frame. Those are transfers, not mid-line effects.
    fn is_segmentable(addr: u16) -> bool {
        !matches!(
            addr,
            // OAMADDL/H + OAMDATA
            0x2102..=0x2104
            // VMADDL/H + VMDATAL/H
            | 0x2116..=0x2119
            // CGADD + CGDATA
            | 0x2121..=0x2122
        )
    }

    /// Mid-line writes recorded for a hardware line, for tests.
    #[must_use]
    pub fn line_writes_for_test(&self, line: u16) -> Vec<(u16, u16, u8)> {
        self.line_writes
            .get(usize::from(line))
            .cloned()
            .unwrap_or_default()
    }

    /// Compose one line, splitting it at every mid-line register write.
    ///
    /// **Segmentation rather than a per-dot fetcher.** Register state only
    /// changes where a write happens, so composing a line once per segment
    /// and splicing at the write's dot yields exactly what a dot-by-dot
    /// composer would — while leaving the background, sprite and mode-7
    /// fetchers, and the seventeen goldens that protect them, untouched.
    ///
    /// The cost is one full-line composition per mid-line write. That is
    /// affordable precisely BECAUSE attribution is correct: before it, a
    /// single line could carry thousands of misattributed frame-setup
    /// writes.
    fn compose_line_segmented(&mut self, line: u16) -> Scanline {
        let writes = self
            .line_writes
            .get(usize::from(line))
            .cloned()
            .unwrap_or_default();
        // Rewind to the registers as they stood before this line's first
        // mid-line write, then replay forward. Lines with no writes skip
        // this entirely and compose exactly as they always did.
        if let Some(Some(regs)) = self.line_regs.get(usize::from(line)).copied() {
            regs.apply(self);
        }
        let phase = self.hires_phase(bg::HiresPhase::Odd);
        let mut out = self.render_scanline_live(line, phase);
        for (dot, addr, value) in writes {
            self.write_register(addr, value);
            let phase = self.hires_phase(bg::HiresPhase::Odd);
            let seg = self.render_scanline_live(line, phase);
            // Both are exactly WIDTH: `render_scanline_live` always
            // composes 256 dots, and the hires widening to 512 happens
            // afterwards in `render_scanline_hires`. So `dot` indexes
            // directly and no scaling is needed here — a mid-line SETINI
            // or BGMODE write changes which registers the SEGMENTS are
            // composed from, not how wide this composition is.
            let from = usize::from(dot);
            if from < out.pixels.len() {
                out.pixels[from..].copy_from_slice(&seg.pixels[from..]);
                out.overlay[from..].copy_from_slice(&seg.overlay[from..]);
            }
        }
        out
    }

    /// A copy of this PPU with line `y`'s latched registers applied.
    ///
    /// Returns `None` when that line was never latched, in which case the
    /// live registers are already the right answer.
    #[must_use]
    fn with_line_state(&self, y: u16) -> Option<Self> {
        let state = (*self.line_state.get(usize::from(y))?)?;
        let mut p = self.clone();
        p.mode7 = state.mode7;
        p.bg_mode = state.bg_mode;
        p.bg3_priority = state.bg3_priority;
        for (i, bg) in p.bgs.iter_mut().enumerate() {
            bg.hofs = state.hofs[i];
            bg.vofs = state.vofs[i];
        }
        p.windows = state.windows;
        p.color_math = state.color_math;
        p.mosaic = state.mosaic;
        Some(p)
    }

    /// `$213E` STAT77 — the hardware's own report of the two OBJ limits.
    #[must_use]
    pub fn read_stat77(&self) -> u8 {
        // Low nibble is the PPU1 version (1 on retail).
        0x01 | (u8::from(self.range_over) << 6) | (u8::from(self.time_over) << 7)
    }

    /// Read a 16-bit little-endian word from VRAM at a WORD address.
    #[must_use]
    pub fn vram_word(&self, word_addr: u16) -> u16 {
        let at = usize::from(word_addr) * 2 % self.vram.len();
        u16::from(self.vram[at]) | (u16::from(self.vram[at + 1]) << 8)
    }

    /// Compose the SUB-SCREEN for a visible row (ticket W7-16).
    ///
    /// The sub-screen is the same composition with `$212D` (TS) selecting
    /// the layers instead of `$212C` (TM), which is why this reuses the
    /// main path wholesale rather than duplicating priority resolution: a
    /// second copy of that order is a second thing to get wrong, and the
    /// two orders are not merely similar, they are the same.
    ///
    /// Returns the pixels plus each x's colour-math operation, decided
    /// here because the operation is hardware state — `$2130`'s prevent
    /// mode against the colour window, `$2131`'s per-layer enable, and its
    /// add/subtract and half bits. The renderer performs the arithmetic;
    /// the core says whether and which.
    #[must_use]
    pub fn render_sub_scanline(&mut self, y: u16) -> (Vec<rf_core_api::SubPixel>, u16) {
        use rf_core_api::{ColorMathOp, SubPixel};

        let line = y + 1;
        let mut shadow = self.with_line_state(line).unwrap_or_else(|| self.clone());
        // Swap TM for TS: same composition, the other screen's layers.
        let ts = shadow.ts;
        for (i, bg) in shadow.bgs.iter_mut().enumerate() {
            bg.enabled = ts & (1 << i) != 0;
        }
        shadow.obj_enabled = ts & 0x10 != 0;
        // The SUB screen owns the EVEN half-dot of every pair on a
        // true-hires line, so its backgrounds are fetched from the even
        // 512-columns. On any other line this is `None` and the fetch is
        // the ordinary 256-wide one — including pseudo-hires, which
        // really is two independent screens.
        let phase = shadow.hires_phase(bg::HiresPhase::Even);
        let composed = shadow.render_scanline_live(line, phase);

        let math = &shadow.color_math;
        let fixed = shadow.color_math.fixed_bgr555();
        let pixels = composed
            .pixels
            .iter()
            .enumerate()
            .map(|(x, px)| {
                let inside = shadow.windows.masks(5, x as u8);
                // Which main-screen layer is being blended INTO decides
                // whether $2131 enables math here at all. The main screen
                // is what carries that layer, so it is read from `self`.
                let main_layer = match self.layer_at(line, x) {
                    Some(rf_core_api::PixelLayer::Background(n)) => usize::from(n),
                    Some(rf_core_api::PixelLayer::Sprite) => 4,
                    _ => 5,
                };
                let enabled = math.enable & (1 << main_layer) != 0;
                let op = if !enabled || math.prevented(inside) {
                    ColorMathOp::None
                } else {
                    match (math.subtract, math.half) {
                        (false, false) => ColorMathOp::Add,
                        (false, true) => ColorMathOp::AddHalf,
                        (true, false) => ColorMathOp::Subtract,
                        (true, true) => ColorMathOp::SubtractHalf,
                    }
                };
                SubPixel {
                    palette_index: px.palette_index,
                    layer: px.layer,
                    op,
                    // With no sub-screen layer opaque here, hardware uses
                    // the fixed colour rather than the backdrop.
                    fixed: matches!(px.layer, rf_core_api::PixelLayer::Backdrop),
                }
            })
            .collect();
        (pixels, fixed)
    }

    /// The main screen's layer at one position, for the colour-math
    /// enable test. Cheap enough at one line per call and always in step
    /// with what `render_scanline` produced.
    fn layer_at(&mut self, line: u16, x: usize) -> Option<rf_core_api::PixelLayer> {
        let mut shadow = self.with_line_state(line).unwrap_or_else(|| self.clone());
        // Which main-screen layer is being blended into, for the sub
        // pixel at dot `x`. On a true-hires line the main screen owns the
        // ODD half-dot of the pair while this sub pixel will land on the
        // EVEN one — asking `Odd` is still right, because the two
        // half-dots are the same DOT and `$2131`'s per-layer enable is a
        // per-dot decision, not a per-half-dot one. (Nothing in the
        // golden suite can catch a mistake here: the goldens hash palette
        // indices and this decides a `ColorMathOp`.)
        let phase = shadow.hires_phase(bg::HiresPhase::Odd);
        shadow
            .render_scanline_live(line, phase)
            .pixels
            .get(x)
            .map(|p| p.layer)
    }

    /// Compose one visible scanline.
    ///
    /// Returns accuracy-exact pixels plus the overlay channel; see the
    /// module doc for why dropped sprites are never in the first.
    #[must_use]
    pub fn render_scanline(&mut self, y: u16) -> Scanline {
        // **`y` is a 0-based VISIBLE ROW; the hardware scanline is one
        // more** (ticket W7-13). fullsnes: the V counter runs 0-261 with
        // "1-224 (or 1-239 if overscan is enabled) visible on the
        // screen" -- line 0 is vblank, and the first row of the picture
        // is drawn on line 1. A BG row is fetched as `vofs + L` for
        // hardware line L, so a renderer that passed the 0-based row
        // straight through fetches `vofs + 0` for the first row and
        // draws the whole picture one row low.
        //
        // **This was found by comparing against PeterLemon's own
        // reference screenshots, not by reading the docs.** Rings and
        // 8x8BG1Map2BPP32x328PAL each matched their shipped PNG at
        // exactly 100% with a one-line offset and 79%/85% without --
        // two different ROMs in two different BG modes agreeing on the
        // same off-by-one. Every golden in this project was pinned over
        // it, which is precisely why an eyeball check could not catch
        // it: a picture one row low looks entirely correct.
        let line = y + 1;
        // Compose from the registers latched when the beam reached this
        // line, not from wherever they have since been left. Without
        // this, HDMA's per-line changes all collapse onto the frame's
        // final state.
        if let Some(latched) = self.with_line_state(line) {
            let mut shadow = latched;
            // Segmented: the line is split at every mid-line register
            // write. With no writes this is exactly the old single
            // composition, which is why it is inert on 31 of 32 goldens.
            let composed = shadow.compose_line_segmented(line);
            // Limit flags accumulate on the real PPU, not the shadow.
            self.range_over |= shadow.range_over;
            self.time_over |= shadow.time_over;
            // Hires is decided from the LATCHED registers, like everything
            // else on this line: a mid-frame BGMODE or SETINI write must
            // change the line it was written on, not retroactively rewrite
            // earlier ones. That is the same reason `with_line_state`
            // exists at all.
            if shadow.setini.hires_requested(shadow.bg_mode) {
                return shadow.render_scanline_hires(line, composed);
            }
            return composed;
        }
        let composed = self.compose_line_segmented(line);
        if self.setini.hires_requested(self.bg_mode) {
            return self.render_scanline_hires(line, composed);
        }
        composed
    }

    /// Compose a 512-dot scanline for pseudo-hires and modes 5/6
    /// (ticket W7-06 criterion 2; `EMULATION_CORES.md` §3.3, "pseudo-hires
    /// and hires modes 5/6 emit 512-wide scanlines").
    ///
    /// **The width tag is the slice length**, which is why this needs no
    /// `rf-core-api` change: `CoreSink::video_scanline` already takes
    /// `&[PpuPixel]`, so a 512-long line *is* a 512-wide line. Adding a
    /// separate width field would have created a second source of truth
    /// that could disagree with the data beside it.
    ///
    /// **Which screen supplies which half-dot**: pseudo-hires is defined
    /// by fullsnes as `SHIFT SUBSCREEN HALF DOT TO THE LEFT`, so the sub
    /// screen lands on the LEFT (even) dot of each pair and the main
    /// screen on the RIGHT (odd). True hires in modes 5/6 agrees — "the
    /// main/subscreen pixels are rendered as half-pixels of the
    /// high-resolution image".
    ///
    /// **An earlier version of this comment claimed that swapping the two
    /// would make a hires ROM "look subtly soft rather than obviously
    /// wrong", and that an eyeball would pass it. Both halves were
    /// false** (RF-L-11 — a doc comment is a claim about code, and this
    /// one was never checked). Swapping the order changes 9,012 bytes of
    /// InterlaceFont and is plainly visible. It also was not the bug: the
    /// real defect was that BOTH screens fetched the same 256-wide
    /// background, so both orders rendered the same mangled glyphs. The
    /// fetch is what [`bg::HiresPhase`] fixed; this function only
    /// interleaves.
    ///
    /// A sub-screen pixel whose source is the `$2132` fixed colour has no
    /// palette index to carry, so it contributes the backdrop index here.
    /// That is a deliberate narrowing: the fixed colour is a *colour
    /// math* input and this is the *picture* path, and law 4 forbids this
    /// crate from resolving either to RGB.
    /// Which half-dot `screen` fetches from, or `None` when this line is
    /// not TRUE hires.
    ///
    /// **Only modes 5 and 6 take a phase**, and that is the whole point
    /// of this function existing rather than reusing
    /// [`SetIni::hires_requested`], which deliberately answers a
    /// different question: "does this line emit 512 dots?" Both routes to
    /// 512 answer yes, but only one of them changes the BG fetch.
    ///
    /// * **pseudo-hires** (`$2133` bit 3, any mode) — genuinely two
    ///   independent 256-wide screens, interleaved. `None` for both.
    /// * **true hires** (modes 5/6) — ONE 512-wide picture fetched at
    ///   double rate, split across the two screens. `Even`/`Odd`.
    ///
    /// Collapsing the two is what made every mode-5 ROM render the 256
    /// picture with its columns duplicated; see [`bg::HiresPhase`] for
    /// the measurement.
    fn hires_phase(&self, screen: bg::HiresPhase) -> bg::HiresPhase {
        if matches!(self.bg_mode, 5 | 6) {
            screen
        } else {
            bg::HiresPhase::None
        }
    }

    fn render_scanline_hires(&mut self, y: u16, main: Scanline) -> Scanline {
        let (sub, _fixed_color) = self.render_sub_scanline(y);
        let width = main.pixels.len();
        let mut pixels = Vec::with_capacity(width * 2);
        let mut overlay = Vec::with_capacity(width * 2);

        for x in 0..width {
            // LEFT half-dot: the sub screen.
            let left = sub.get(x).map_or(main.pixels[x], |sp| PpuPixel {
                // A fixed-colour sub pixel carries no index — see the doc.
                palette_index: if sp.fixed { 0 } else { sp.palette_index },
                layer: sp.layer,
                sprite_id: None,
                priority: 0,
            });
            pixels.push(left);
            // RIGHT half-dot: the main screen, unchanged. The main
            // screen's pixels stay accuracy-exact at their own positions;
            // hires interleaves them, it does not alter them.
            pixels.push(main.pixels[x]);

            // The overlay is per-dot too, or it would no longer line up
            // with `pixels` — its own contract is "same length as
            // pixels". A dropped sprite belongs to the MAIN screen, so
            // the left half-dot carries an empty entry rather than a
            // duplicate, which would double every dropped sprite's width.
            overlay.push(OverlayPixel {
                palette_index: 0,
                // `opaque: false` is "draw nothing here" — the documented
                // way to say a dropped sprite does not reach this x, and
                // NOT a sentinel index.
                opaque: false,
            });
            overlay.push(main.overlay[x]);
        }

        Scanline { pixels, overlay }
    }

    /// Compose one scanline from the live registers.
    ///
    /// `y` here is the **hardware scanline** (1-224), not a 0-based row —
    /// see [`Ppu::render_scanline`], which is the only caller and does the
    /// conversion.
    fn render_scanline_live(&mut self, y: u16, phase: bg::HiresPhase) -> Scanline {
        let backdrop = PpuPixel {
            palette_index: 0,
            layer: PixelLayer::Backdrop,
            sprite_id: None,
            priority: 0,
        };
        let mut pixels = vec![backdrop; WIDTH];
        let mut overlay = vec![
            OverlayPixel {
                palette_index: 0,
                opaque: false
            };
            WIDTH
        ];

        if self.forced_blank {
            // Forced blank shows the backdrop and, importantly, does NOT
            // evaluate sprites — so the limit flags do not accumulate
            // while the screen is off.
            return Scanline { pixels, overlay };
        }

        let bg_pixels = bg::render_backgrounds(self, y, phase);
        // Mode 7 replaces BG1 entirely: its "tilemap" is an affine
        // transform, so it is rendered by its own module and injected as
        // BG1's contribution rather than fetched through the tile path.
        //
        // Density 1 — hardware. Law 6: Accuracy Mode is the reference and
        // HD-Mode-7 is an opt-in overlay, so nothing on this path can
        // reach for a higher density.
        let mode7_line = (self.bg_mode == 7).then(|| mode7::render_scanline(self, y, 1));
        let objs = obj::render_objects(self, y);
        self.range_over |= objs.range_over;
        self.time_over |= objs.time_over;

        for x in 0..WIDTH {
            if let Some(line) = &mode7_line {
                if let Some(index) = line[x] {
                    pixels[x] = PpuPixel {
                        palette_index: index,
                        layer: PixelLayer::Background(0),
                        sprite_id: None,
                        priority: 0,
                    };
                }
                // Sprites still compose over mode 7.
                if let Some((index, id)) = objs.pixels[x] {
                    pixels[x] = PpuPixel {
                        palette_index: index,
                        layer: PixelLayer::Sprite,
                        sprite_id: Some(id),
                        priority: objs.priority[x],
                    };
                }
            } else if let Some(p) = self.compose(x, &bg_pixels, &objs) {
                pixels[x] = p;
            }
            // Colour math's "clip main screen to black" IS expressible on
            // an indexed path, because black is palette index 0. The
            // add/sub blend is not — see the window module doc.
            if self
                .color_math
                .clip_to_black(self.windows.masks(5, x as u8))
            {
                pixels[x] = backdrop;
            }

            if let Some(dropped) = objs.dropped[x] {
                // Only paint a dropped sprite where the real frame did not
                // already put an opaque sprite: the enhancement layer
                // restores what the CRT lost, it does not reorder what it
                // kept.
                if !matches!(pixels[x].layer, PixelLayer::Sprite) {
                    overlay[x] = OverlayPixel {
                        palette_index: dropped,
                        opaque: true,
                    };
                }
            }
        }

        Scanline { pixels, overlay }
    }

    /// Resolve one x against the mode's priority order.
    fn compose(
        &self,
        x: usize,
        bg_pixels: &bg::BgScanlines,
        objs: &obj::ObjScanline,
    ) -> Option<PpuPixel> {
        for slot in self.priority_order() {
            match slot {
                Slot::Obj(pri) => {
                    if self.layer_masked(4, x) {
                        continue;
                    }
                    if let Some((index, id)) = objs.pixels[x] {
                        if objs.priority[x] == pri {
                            return Some(PpuPixel {
                                palette_index: index,
                                layer: PixelLayer::Sprite,
                                sprite_id: Some(id),
                                priority: pri,
                            });
                        }
                    }
                }
                Slot::Bg(n, pri) => {
                    // A layer masked by its window is absent here, so the
                    // next slot down wins — windows remove a layer from
                    // the priority resolution rather than painting over
                    // its result.
                    if self.layer_masked(usize::from(n), x) {
                        continue;
                    }
                    let layer = &bg_pixels.layers[usize::from(n)];
                    if let Some(index) = layer.pixels[x] {
                        if layer.priority[x] == pri {
                            return Some(PpuPixel {
                                palette_index: index,
                                layer: PixelLayer::Background(n),
                                sprite_id: None,
                                priority: pri,
                            });
                        }
                    }
                }
            }
        }
        None
    }

    /// Is `layer` (0-3 = BG1-4, 4 = OBJ) masked out at `x` on the main
    /// screen?
    ///
    /// Two things must BOTH be true: the layer's window must cover `x`,
    /// and `$212E` must say this layer's mask applies to the main screen.
    /// Games routinely configure a window and leave it disabled on the
    /// main screen — checking only the first makes content vanish.
    #[must_use]
    fn layer_masked(&self, layer: usize, x: usize) -> bool {
        self.windows.main_mask & (1 << layer) != 0 && self.windows.masks(layer, x as u8)
    }

    /// The mode's layer priority, front to back.
    ///
    /// These orders are the whole substance of "BG3 priority": in mode 1
    /// with `$2105` bit 3 set, BG3's high-priority tiles jump from near
    /// the BACK of the order to the very FRONT, above sprites. It is the
    /// piece a mode-0/1 renderer most often gets silently wrong, because
    /// most test content never sets the bit.
    fn priority_order(&self) -> Vec<Slot> {
        use Slot::{Bg, Obj};
        match self.bg_mode {
            1 if self.bg3_priority => vec![
                Bg(2, 1),
                Obj(3),
                Bg(0, 1),
                Bg(1, 1),
                Obj(2),
                Bg(0, 0),
                Bg(1, 0),
                Obj(1),
                Obj(0),
                Bg(2, 0),
            ],
            1 => vec![
                Obj(3),
                Bg(0, 1),
                Bg(1, 1),
                Obj(2),
                Bg(0, 0),
                Bg(1, 0),
                Obj(1),
                Bg(2, 1),
                Obj(0),
                Bg(2, 0),
            ],
            // Modes 2-5 share one order: two layers, each with two
            // priority levels, interleaved with the four sprite levels.
            2..=5 => vec![
                Obj(3),
                Bg(0, 1),
                Obj(2),
                Bg(1, 1),
                Obj(1),
                Bg(0, 0),
                Obj(0),
                Bg(1, 0),
            ],
            // Mode 6 has BG1 only.
            6 => vec![Obj(3), Bg(0, 1), Obj(2), Obj(1), Bg(0, 0), Obj(0)],
            // Mode 0: four 2bpp layers, BG3/BG4 behind BG1/BG2.
            _ => vec![
                Obj(3),
                Bg(0, 1),
                Bg(1, 1),
                Obj(2),
                Bg(0, 0),
                Bg(1, 0),
                Obj(1),
                Bg(2, 1),
                Bg(3, 1),
                Obj(0),
                Bg(2, 0),
                Bg(3, 0),
            ],
        }
    }
}

/// One entry in a mode's priority order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    /// A sprite priority level, 0-3.
    Obj(u8),
    /// A background index and its tilemap priority bit.
    Bg(u8, u8),
}

impl Ppu {
    /// Serialise every PPU register (ticket W7-09).
    ///
    /// **`line_state` is deliberately absent** — see `crate::state`'s
    /// module doc. It is per-frame scratch, cleared at frame start, and
    /// states are taken at frame boundaries only.
    ///
    /// The three write-twice latches (`bgofs_latch`, `bghofs_latch`,
    /// `cgram_latch`, `oam_latch`) ARE saved, and the two `Option` ones as
    /// present-flag plus value: "no byte pending" and "a pending zero" are
    /// different machines, and conflating them makes the next write to
    /// `$2122` or `$2104` land as a whole word instead of half of one.
    pub(crate) fn save(
        &self,
        o: &mut crate::state::StateOut,
    ) -> Result<(), rf_core_api::StateError> {
        o.u8(self.bg_mode)?;
        o.bool(self.bg3_priority)?;
        for bg in &self.bgs {
            o.u16(bg.tilemap_base)?;
            o.u8(bg.tilemap_size)?;
            o.u16(bg.char_base)?;
            o.bool(bg.tile_size_16)?;
            o.u16(bg.hofs)?;
            o.u16(bg.vofs)?;
            o.bool(bg.enabled)?;
        }
        o.u8(self.obj_size)?;
        o.u16(self.obj_name_base)?;
        o.u16(self.obj_name_select)?;
        o.bool(self.obj_enabled)?;
        o.u16(self.oam_addr)?;
        o.bool(self.oam_priority_rotation)?;
        o.bool(self.forced_blank)?;
        o.u8(self.brightness)?;
        self.mode7.save(o)?;
        o.u8(self.setini.to_bits())?;
        o.bool(self.overscan_changed)?;
        self.windows.save(o)?;
        self.color_math.save(o)?;
        self.mosaic.save(o)?;
        o.bool(self.direct_color)?;
        o.bool(self.range_over)?;
        o.bool(self.time_over)?;
        o.u8(self.bgofs_latch)?;
        o.u8(self.bghofs_latch)?;
        o.u8(self.cgram_addr)?;
        o.opt_u8(self.cgram_latch)?;
        o.opt_u8(self.oam_latch)
    }

    pub(crate) fn load(
        &mut self,
        i: &mut crate::state::StateIn,
    ) -> Result<(), rf_core_api::StateError> {
        self.bg_mode = i.u8()?;
        self.bg3_priority = i.bool()?;
        for bg in &mut self.bgs {
            bg.tilemap_base = i.u16()?;
            bg.tilemap_size = i.u8()?;
            bg.char_base = i.u16()?;
            bg.tile_size_16 = i.bool()?;
            bg.hofs = i.u16()?;
            bg.vofs = i.u16()?;
            bg.enabled = i.bool()?;
        }
        self.obj_size = i.u8()?;
        self.obj_name_base = i.u16()?;
        self.obj_name_select = i.u16()?;
        self.obj_enabled = i.bool()?;
        self.oam_addr = i.u16()?;
        self.oam_priority_rotation = i.bool()?;
        self.forced_blank = i.bool()?;
        self.brightness = i.u8()?;
        self.mode7.load(i)?;
        self.setini.write_register(i.u8()?);
        self.overscan_changed = i.bool()?;
        self.windows.load(i)?;
        self.color_math.load(i)?;
        self.mosaic.load(i)?;
        self.direct_color = i.bool()?;
        self.range_over = i.bool()?;
        self.time_over = i.bool()?;
        self.bgofs_latch = i.u8()?;
        self.bghofs_latch = i.u8()?;
        self.cgram_addr = i.u8()?;
        self.cgram_latch = i.opt_u8()?;
        self.oam_latch = i.opt_u8()?;
        // A restored PPU must not serve last frame's latched lines.
        self.clear_line_state();
        Ok(())
    }
}

impl SetIni {
    /// Re-encode as the `$2133` byte, so a save state stores the register
    /// the ROM wrote rather than six booleans that could drift from it.
    #[must_use]
    pub fn to_bits(self) -> u8 {
        u8::from(self.interlace)
            | (u8::from(self.obj_interlace) << 1)
            | (u8::from(self.overscan) << 2)
            | (u8::from(self.pseudo_hires) << 3)
            | (u8::from(self.extbg) << 6)
            | (u8::from(self.external_sync) << 7)
    }
}
