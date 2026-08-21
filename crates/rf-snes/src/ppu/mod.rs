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
    /// This core composes ONE screen and emits indexed pixels (law 4), so
    /// it can produce the main screen's half of that image and nothing
    /// else — which is exactly what a mode-5 ROM looks like here: half
    /// the picture, against the backdrop. Carrying a sub-screen across
    /// `CoreSink` is W7-16's contract change.
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
            setini: SetIni::default(),
            overscan_changed: false,
            windows: window::Windows::default(),
            color_math: window::ColorMath::default(),
            mosaic: window::Mosaic::default(),
            line_state: vec![None; VISIBLE_LINES_OVERSCAN as usize],
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

    /// Compose one visible scanline.
    ///
    /// Returns accuracy-exact pixels plus the overlay channel; see the
    /// module doc for why dropped sprites are never in the first.
    #[must_use]
    pub fn render_scanline(&mut self, y: u16) -> Scanline {
        // Compose from the registers latched when the beam reached this
        // line, not from wherever they have since been left. Without
        // this, HDMA's per-line changes all collapse onto the frame's
        // final state.
        if let Some(latched) = self.with_line_state(y) {
            let mut shadow = latched;
            let line = shadow.render_scanline_live(y);
            // Limit flags accumulate on the real PPU, not the shadow.
            self.range_over |= shadow.range_over;
            self.time_over |= shadow.time_over;
            return line;
        }
        self.render_scanline_live(y)
    }

    fn render_scanline_live(&mut self, y: u16) -> Scanline {
        let backdrop = PpuPixel {
            palette_index: 0,
            layer: PixelLayer::Backdrop,
            sprite_id: None,
            priority: 0,
            dropped_by_limit: false,
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

        let bg_pixels = bg::render_backgrounds(self, y);
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
                        dropped_by_limit: false,
                    };
                }
                // Sprites still compose over mode 7.
                if let Some((index, id)) = objs.pixels[x] {
                    pixels[x] = PpuPixel {
                        palette_index: index,
                        layer: PixelLayer::Sprite,
                        sprite_id: Some(id),
                        priority: objs.priority[x],
                        dropped_by_limit: false,
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
                                dropped_by_limit: false,
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
                                dropped_by_limit: false,
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
