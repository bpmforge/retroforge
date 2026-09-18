//! Indexed video output types (FR-CORE-004).
//!
//! Pixels cross the core boundary as **indexed color + source metadata**,
//! never pre-composed RGB (ARCHITECTURE §5, ADR-4). No RGB type may appear
//! anywhere in this module or in [`crate::CoreSink::video_scanline`]'s
//! signature — palette-to-RGB resolution happens downstream in the
//! renderer, using a profile/palette the core knows nothing about.

/// Source layer a pixel was drawn from.
///
/// Console PPUs differ in background-plane topology (NES: one BG plane;
/// SNES: up to four BG planes depending on the active mode), so this is not
/// a fixed enumeration of console-specific planes — `Background(n)` is a
/// core-defined plane index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PixelLayer {
    /// The PPU's universal backdrop color: no background or sprite pixel is
    /// opaque at this position.
    ///
    /// The `Default`, and the only defensible one: a zero-valued pixel is
    /// one nothing has claimed, which is precisely the backdrop.
    #[default]
    Backdrop,
    /// A background/tilemap plane. `n` is core-defined (NES: always 0;
    /// SNES: 0-3 depending on the active BG mode).
    Background(u8),
    /// A sprite/OBJ layer pixel.
    Sprite,
}

/// One pixel of indexed video output plus the source metadata needed for
/// layer extraction, HUD separation, de-flicker, and palette-aware upscaling
/// without heuristics on RGB (ARCHITECTURE §5; FR-CORE-004).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PpuPixel {
    /// Index into the active palette/CGRAM. Not an RGB value — resolving to
    /// a display color is the renderer's job, using palette data obtained
    /// separately via [`crate::StateView`] or a profile.
    pub palette_index: u8,
    /// Which layer produced this pixel.
    pub layer: PixelLayer,
    /// OAM index of the sprite that produced this pixel, if `layer` is
    /// [`PixelLayer::Sprite`]. `None` for background/backdrop pixels.
    pub sprite_id: Option<u8>,
    /// Core-defined priority value used to resolve BG/sprite overlap
    /// (hardware priority bits, not a rendering hint).
    pub priority: u8,
}

/// How colour math combines a [`SubPixel`] with the main-screen
/// [`PpuPixel`] at the same x (ticket W7-16).
///
/// The core decides WHICH operation applies at each x — that is hardware
/// state ($2130-$2132, the colour window, per-layer enables) — and the
/// renderer performs it. Splitting it that way is what keeps law 4 intact:
/// a core still emits palette indices, and the RGB that colour math
/// produces (which need not exist anywhere in CGRAM) is computed by the
/// only layer allowed to know about colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorMathOp {
    /// No math at this x: the main-screen pixel stands alone.
    #[default]
    None,
    /// `main + sub`, clamped.
    Add,
    /// `(main + sub) / 2`.
    AddHalf,
    /// `main - sub`, clamped at zero.
    Subtract,
    /// `(main - sub) / 2`.
    SubtractHalf,
}

/// One sub-screen pixel, and how it combines with the main screen.
///
/// **A parallel channel, never a replacement.** Same rule as
/// [`OverlayPixel`]: [`crate::CoreSink::video_scanline`] carries the
/// accuracy-exact hardware framebuffer and nothing here may displace it
/// (the W1-05a ruling on [`PpuPixel::palette_index`]). A consumer that
/// ignores [`crate::CoreSink::sub_scanline`] still gets a correct main
/// screen; it just cannot draw the blend.
///
/// ## Why this exists
///
/// Ticket W7-05 implemented colour math and could not deliver it. The
/// blend produces an RGB value that need not exist in CGRAM, so it cannot
/// travel through an indexed [`PpuPixel`], and a golden hash over palette
/// indices could not see it if it did. W7-06 then found the same wall from
/// the other side: hires modes 5/6 work because "the main/subscreen pixels
/// are rendered as half-pixels of the high-resolution image" (fullsnes), so
/// a core with no sub-screen draws half the picture. Both wanted this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SubPixel {
    /// Raw palette-RAM value, exactly as [`PpuPixel::palette_index`].
    /// Meaningless when `fixed` is set.
    pub palette_index: u8,
    /// Which layer produced this sub-screen pixel.
    pub layer: PixelLayer,
    /// How to combine with the main-screen pixel at this x.
    pub op: ColorMathOp,
    /// When set, the sub-screen source at this x is the scanline's fixed
    /// colour (`$2132`, passed alongside the slice) rather than
    /// `palette_index`.
    ///
    /// A flag rather than a sentinel index because every index 0-255 is a
    /// legitimate colour, so there is no value left over to mean "not an
    /// index".
    pub fixed: bool,
}

/// One console's Mode 7 affine-transform register file for one frame
/// (ticket W16-09; `docs/design/ENHANCEMENT_WAVE_16.md` §9; snesdev wiki
/// "Mode 7" for the matrix/offset semantics; `rf_snes::ppu::mode7::Mode7`
/// for the hardware write-twice-latch behaviour these values are read
/// back from).
///
/// **A cross-console field, not an SNES-only one bolted onto a generic
/// type by name alone.** This struct carries no reference to SNES at all
/// — just the eight signed values and two flip bits a Mode 7-shaped
/// affine ground transform needs — so a future console with an analogous
/// per-scanline affine background (there is none today) could populate it
/// too. A core that never has Mode 7 (NES, and SNES whenever BG mode is
/// not 7) never constructs one — see [`crate::CoreEvent::Mode7`]'s own
/// doc for the zero-cost-when-unsubscribed mechanism this rides on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mode7Registers {
    /// `$211B`/`$211C`/`$211D`/`$211E` — signed 8.8 fixed point matrix.
    pub a: i16,
    pub b: i16,
    pub c: i16,
    pub d: i16,
    /// `$211F`/`$2120` — signed 13-bit centre of rotation.
    pub x0: i16,
    pub y0: i16,
    /// `$210D`/`$210E`'s Mode-7 half — signed 13-bit scroll.
    pub hofs: i16,
    pub vofs: i16,
    /// `$211A` bits 0/1.
    pub flip_x: bool,
    pub flip_y: bool,
}

/// One overlay-only pixel: a sprite the hardware's per-scanline sprite limit
/// dropped, recorded separately from the accuracy-exact [`PpuPixel`] sink so
/// it can never displace a real pixel (see [`PpuPixel::dropped_by_limit`]'s
/// doc for why that displacement is forbidden). Carried through
/// [`crate::CoreSink::overlay_scanline`], a channel independent of
/// [`crate::CoreSink::video_scanline`] — ticket W3-05a.
///
/// Unlike [`PpuPixel`], this carries no layer/priority/sprite-id: by the
/// time a core emits one, it has already resolved sprite-vs-sprite (lower
/// OAM index wins) and sprite-behind-background priority against the real,
/// already-rendered frame — the same rules [`PixelLayer`]/`PpuPixel`
/// document, applied by the core before this struct exists at all. A
/// consumer's only remaining job is: if `opaque`, paint `palette_index` over
/// whatever [`PpuPixel`] already resolved at this position; if not, leave it
/// alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverlayPixel {
    /// Same semantics as [`PpuPixel::palette_index`] (raw 6-bit palette-RAM
    /// value, not an address) — meaningless when `opaque` is `false`.
    pub palette_index: u8,
    /// Whether a dropped sprite actually won this pixel. `false` means
    /// "draw nothing here" (either no dropped sprite reaches this x, its
    /// pattern bit is transparent, a higher-priority real sprite already
    /// claims it, or an opaque background pixel outranks a
    /// behind-background dropped sprite) — never a sentinel palette index.
    pub opaque: bool,
}
