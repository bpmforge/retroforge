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
    /// `true` if this pixel represents a sprite that hardware's
    /// per-scanline sprite limit would drop (accurate CRT behavior:
    /// flicker/disappearance). Accuracy Mode renders it dropped either way;
    /// an enhancement that bypasses the limit (W3-05) uses this flag to
    /// decide whether to draw it anyway. Always `false` for non-sprite
    /// pixels.
    pub dropped_by_limit: bool,
}
