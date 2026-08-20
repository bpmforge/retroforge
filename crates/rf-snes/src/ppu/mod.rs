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
pub mod obj;

use rf_core_api::{OverlayPixel, PixelLayer, PpuPixel};

/// Visible width of a non-hires scanline.
pub const WIDTH: usize = 256;
/// Visible lines in the default (non-overscan) frame.
pub const VISIBLE_LINES: u16 = 224;
/// CGRAM holds 256 colours as 16-bit BGR555 entries.
pub const CGRAM_ENTRIES: usize = 256;
/// OAM: 512 bytes of low table plus 32 bytes of high table.
pub const OAM_LEN: usize = 544;

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
        let objs = obj::render_objects(self, y);
        self.range_over |= objs.range_over;
        self.time_over |= objs.time_over;

        for x in 0..WIDTH {
            if let Some(p) = self.compose(x, &bg_pixels, &objs) {
                pixels[x] = p;
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
