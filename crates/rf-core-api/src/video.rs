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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelLayer {
    /// The PPU's universal backdrop color: no background or sprite pixel is
    /// opaque at this position.
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
    /// Whether this pixel represents a sprite that hardware's per-scanline
    /// sprite limit would drop (accurate CRT behavior: flicker /
    /// disappearance). **On the NES path this is always `false`** — see the
    /// ruling below. Always `false` for non-sprite pixels.
    ///
    /// # Ruling (Brad, 2026-08-03, raised during W1-05a)
    ///
    /// This doc previously said an enhancement bypassing the limit (W3-05)
    /// "uses this flag to decide whether to draw it anyway". That is not
    /// achievable through this struct: [`crate::CoreSink::video_scanline`]
    /// carries exactly **one** `PpuPixel` per x, so emitting a limit-dropped
    /// sprite there would displace the background or lower-index sprite the
    /// CRT actually showed — which CLAUDE.md law 6 ("Accuracy Mode is the
    /// reference") and FR-MODE-002's mode invariant both forbid.
    ///
    /// **The sink is therefore accuracy-exact**: it always carries the true
    /// hardware framebuffer. W3-05's sprite-limit bypass reconstructs dropped
    /// sprites from OAM (via [`crate::StateView`] / its SpriteHistorian),
    /// which its own acceptance criteria already imply, not from this flag.
    /// The field is retained for cores that can express a suppressed sprite
    /// without displacing a real pixel.
    ///
    /// # Update (W3-05a, 2026-08-06)
    ///
    /// [`OverlayPixel`]/[`crate::CoreSink::overlay_scanline`] is that
    /// mechanism, landed as a spike beneath the `StateView`-based route this
    /// doc still names as the eventual (W3-05) destination — see
    /// `rf-nes/src/ppu/sprites.rs`'s module doc for why the reconstruction
    /// lives in `rf-nes` today rather than `rf-enhance`. Do not read this as
    /// evidence `dropped_by_limit` was the intended carrier after all; it
    /// remains always `false` on the NES path and is not used by the
    /// sprite-limit bypass.
    pub dropped_by_limit: bool,
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
