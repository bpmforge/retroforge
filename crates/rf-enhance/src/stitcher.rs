//! `Stitcher`: the wideNES-style persistent canvas
//! `docs/design/ENHANCEMENT_RUNTIME.md` §3 describes — "blit the newly
//! revealed strip of the rendered background into a persistent large
//! canvas keyed by scene. Over play, the canvas accumulates the level as
//! visited." (ticket W4-03a).
//!
//! Consumes [`crate::scroll_tracker::ScrollTracker`]'s per-frame
//! `(ScanlineBand, BandWorldPos)` output plus that frame's background
//! pixels, and paints each non-HUD band's newly-visible strip into
//! [`Canvas`] at its world-space position.
//!
//! ## Scope fence (this ticket is the tracker/stitcher CORE only)
//!
//! [`Canvas`] here is an **in-memory, per-session** buffer — real,
//! functional, and what [`crate::stitcher`]'s determinism test actually
//! exercises. It is deliberately NOT persisted, NOT cached across
//! sessions, and NOT keyed by scene identity: `crates/rf-cache` (this
//! ticket shapes its eventual canvas-chunk type, does not populate it)
//! and scene-keyed persistence are W4-08/W4-03b's job — see this
//! ticket's own `plan.json` notes and `rf_cache::CanvasChunk`'s doc for
//! the exact handoff. There is exactly one canvas per [`Stitcher`]
//! instance, not one per scene.
//!
//! ## Honesty (ENHANCEMENT_RUNTIME §3's stated limitation)
//!
//! [`Canvas`] stores `Option<PpuPixel>` per cell: `None` means "never
//! observed", never silently rendered as black/backdrop. A cell a sprite
//! is currently occluding is left exactly as it was (its last real
//! observation, or still `None`) rather than guessed at — `rf-renderer`'s
//! `LayeredFrame` doc already names this as a real information-loss
//! ceiling ("bg_rgba is the background minus whatever a sprite occluded
//! it this frame, not the full BG plane underneath"); this module cannot
//! recover what that doc says is lost, so it does not pretend to.
use rf_core_api::{PixelLayer, PpuPixel};

use crate::scroll_tracker::{BandWorldPos, ScanlineBand};

/// A growable, deterministically-laid-out canvas of observed background
/// pixels in world space. Backed by a flat `Vec` (row-major, bounded by
/// `origin`/`width`/`height`), not a hash-keyed map — iteration order of a
/// `std::collections::HashMap` is not guaranteed stable across runs (its
/// default hasher is randomly seeded), which would make a "byte-identical
/// across two runs" determinism check meaningless if the canvas were ever
/// serialized by walking map entries. A flat buffer with an explicit,
/// content-derived bounding box has no such hazard: growth is a pure
/// function of which world coordinates have been touched, and equality is
/// plain `Vec` equality.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Canvas {
    origin_x: i64,
    origin_y: i64,
    width: usize,
    height: usize,
    cells: Vec<Option<PpuPixel>>,
}

impl Canvas {
    #[must_use]
    pub fn new() -> Self {
        Canvas {
            origin_x: 0,
            origin_y: 0,
            width: 0,
            height: 0,
            cells: Vec::new(),
        }
    }

    #[must_use]
    pub fn origin(&self) -> (i64, i64) {
        (self.origin_x, self.origin_y)
    }

    #[must_use]
    pub fn width(&self) -> usize {
        self.width
    }

    #[must_use]
    pub fn height(&self) -> usize {
        self.height
    }

    /// Read-only access to the raw row-major cell buffer, `width() *
    /// height()` long — for conversion to a persisted representation
    /// (`crate::persistence`) and for fog derivation
    /// (`crate::camera::fog_mask_from_canvas`), both of which need the
    /// `None`/`Some` distinction cell-for-cell, not just resolved colour.
    #[must_use]
    pub fn cells(&self) -> &[Option<PpuPixel>] {
        &self.cells
    }

    /// Reconstruct a canvas from previously-exported raw parts (e.g. a
    /// restored `rf_cache::CanvasChunk` — `crate::persistence`'s restore
    /// path), preserving origin and bounds exactly rather than only the
    /// cell contents (W4-08's persistence lesson, restated in this
    /// ticket's brief: a restored canvas that drops origin/bounds makes
    /// world coordinates shift silently on the next session).
    ///
    /// # Panics
    /// Panics if `cells.len() != width * height` — an assembly bug, the
    /// same "caller bug, not a runtime condition to degrade through"
    /// stance `FrameBundle::new` takes for the same shape of invariant.
    #[must_use]
    pub fn from_raw_parts(
        origin_x: i64,
        origin_y: i64,
        width: usize,
        height: usize,
        cells: Vec<Option<PpuPixel>>,
    ) -> Self {
        assert_eq!(
            cells.len(),
            width * height,
            "Canvas::from_raw_parts: {} cells for a {width}x{height} canvas (need {})",
            cells.len(),
            width * height
        );
        Canvas {
            origin_x,
            origin_y,
            width,
            height,
            cells,
        }
    }

    /// The observed pixel at world coordinates, `None` for both "outside
    /// the canvas's current bounds" and "inside bounds but never visited"
    /// — indistinguishable to a caller by design (module doc: unvisited
    /// is unvisited, whether or not the canvas happens to have grown that
    /// far yet).
    #[must_use]
    pub fn get(&self, world_x: i64, world_y: i64) -> Option<PpuPixel> {
        let (idx_x, idx_y) = (world_x - self.origin_x, world_y - self.origin_y);
        if idx_x < 0 || idx_y < 0 || idx_x as usize >= self.width || idx_y as usize >= self.height {
            return None;
        }
        self.cells[idx_y as usize * self.width + idx_x as usize]
    }

    /// Grow (never shrink) the canvas so `[world_x, world_x+extra_width)`
    /// x `[world_y, world_y+extra_height)` is addressable, preserving
    /// every already-observed cell at its own world coordinate. Pure
    /// function of the current bounds and the requested region — the same
    /// two inputs always produce the same new bounds, regardless of call
    /// history beyond what those bounds already encode.
    fn ensure_bounds(
        &mut self,
        world_x: i64,
        world_y: i64,
        extra_width: usize,
        extra_height: usize,
    ) {
        if self.width == 0 || self.height == 0 {
            self.origin_x = world_x;
            self.origin_y = world_y;
            self.width = extra_width;
            self.height = extra_height;
            self.cells = vec![None; self.width * self.height];
            return;
        }
        let min_x = self.origin_x.min(world_x);
        let min_y = self.origin_y.min(world_y);
        let max_x = (self.origin_x + self.width as i64).max(world_x + extra_width as i64);
        let max_y = (self.origin_y + self.height as i64).max(world_y + extra_height as i64);
        let new_width = (max_x - min_x) as usize;
        let new_height = (max_y - min_y) as usize;
        if min_x == self.origin_x
            && min_y == self.origin_y
            && new_width == self.width
            && new_height == self.height
        {
            return; // already covers the requested region -- no reallocation
        }

        let mut new_cells = vec![None; new_width * new_height];
        let dst_x_offset = (self.origin_x - min_x) as usize;
        let dst_y_offset = (self.origin_y - min_y) as usize;
        for y in 0..self.height {
            let src_start = y * self.width;
            let dst_start = (y + dst_y_offset) * new_width + dst_x_offset;
            new_cells[dst_start..dst_start + self.width]
                .copy_from_slice(&self.cells[src_start..src_start + self.width]);
        }
        self.origin_x = min_x;
        self.origin_y = min_y;
        self.width = new_width;
        self.height = new_height;
        self.cells = new_cells;
    }

    /// Blit one row of already-layer-filtered pixels
    /// (`Some` = observed background pixel, `None` = skip -- sprite
    /// occlusion or otherwise unavailable, module doc) starting at world
    /// coordinates `(world_x, world_y)`. Grows the canvas to cover the
    /// row's full span first (even the skipped cells -- the row was
    /// visited, even where a sprite is temporarily in front of it), then
    /// writes only the `Some` entries, leaving every other cell exactly as
    /// it was (a prior real observation, or still unvisited).
    pub fn blit_row(&mut self, world_x: i64, world_y: i64, row: &[Option<PpuPixel>]) {
        if row.is_empty() {
            return;
        }
        self.ensure_bounds(world_x, world_y, row.len(), 1);
        let row_in_canvas_y = (world_y - self.origin_y) as usize;
        let row_in_canvas_x = (world_x - self.origin_x) as usize;
        let dst_start = row_in_canvas_y * self.width + row_in_canvas_x;
        for (i, px) in row.iter().enumerate() {
            if let Some(px) = px {
                self.cells[dst_start + i] = Some(*px);
            }
        }
    }
}

impl Default for Canvas {
    fn default() -> Self {
        Self::new()
    }
}

/// Only background/backdrop pixels are stitchable content (ENHANCEMENT_RUNTIME
/// §3: "the newly revealed strip of the rendered background" — sprites move
/// independently of level geometry and would corrupt a world-space canvas
/// if baked in). Mirrors `rf_renderer::LayeredFrame`'s BG/sprite split
/// without depending on that crate: the routing rule is exactly
/// `PpuPixel::layer`, already available on every `FrameBundle::video`
/// pixel via `rf-core-api` (which this crate already depends on) — pulling
/// in `rf-renderer` for one `matches!` would invert the intended
/// dependency direction (`ENHANCEMENT_RUNTIME.md` §1: `rf-enhance`
/// produces the scene graph `rf-renderer` consumes, not the reverse).
pub(crate) fn stitchable_pixel(pixel: &PpuPixel) -> Option<PpuPixel> {
    match pixel.layer {
        PixelLayer::Backdrop | PixelLayer::Background(_) => Some(*pixel),
        PixelLayer::Sprite => None,
    }
}

/// Blits one frame's worth of classified bands into `canvas`. `video` is
/// the frame's full row-major pixel buffer (`FrameBundle::video` shape:
/// `width * height`, accuracy-exact); `width` is that frame's row stride.
/// HUD bands (module doc) are never blitted — they are fixed UI content,
/// not level geometry, and painting them into world space would be
/// exactly the dishonesty ENHANCEMENT_RUNTIME §3 warns against (the HUD
/// would appear to be part of the level at whatever position happened to
/// be under it the first time it was captured).
pub fn stitch_frame(
    canvas: &mut Canvas,
    video: &[PpuPixel],
    width: u16,
    bands: &[(ScanlineBand, BandWorldPos)],
) {
    let width = width as usize;
    for (band, pos) in bands {
        if band.is_hud {
            continue;
        }
        for sy in band.start..band.end {
            let row_start = sy as usize * width;
            let Some(row) = video.get(row_start..row_start + width) else {
                continue; // frame shorter than this band claims -- degrade, never panic/index-oob
            };
            let filtered: Vec<Option<PpuPixel>> = row.iter().map(stitchable_pixel).collect();
            let world_y = pos.world_y_at_start + i64::from(sy - band.start);
            canvas.blit_row(pos.world_x, world_y, &filtered);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rf_core_api::PixelLayer;

    fn bg_pixel(idx: u8) -> PpuPixel {
        PpuPixel {
            palette_index: idx,
            layer: PixelLayer::Background(0),
            sprite_id: None,
            priority: 0,
        }
    }

    fn sprite_pixel(idx: u8) -> PpuPixel {
        PpuPixel {
            palette_index: idx,
            layer: PixelLayer::Sprite,
            sprite_id: Some(0),
            priority: 0,
        }
    }

    #[test]
    fn canvas_starts_empty_and_reports_none_everywhere() {
        let canvas = Canvas::new();
        assert_eq!(canvas.get(0, 0), None);
        assert_eq!(canvas.get(-5, 100), None);
    }

    #[test]
    fn blit_row_places_pixels_at_the_given_world_coordinates() {
        let mut canvas = Canvas::new();
        let row = vec![Some(bg_pixel(1)), Some(bg_pixel(2)), Some(bg_pixel(3))];
        canvas.blit_row(10, 5, &row);
        assert_eq!(canvas.get(10, 5), Some(bg_pixel(1)));
        assert_eq!(canvas.get(11, 5), Some(bg_pixel(2)));
        assert_eq!(canvas.get(12, 5), Some(bg_pixel(3)));
        assert_eq!(
            canvas.get(13, 5),
            None,
            "one past the blitted row: still unvisited"
        );
    }

    #[test]
    fn blit_row_grows_to_negative_coordinates_without_losing_existing_content() {
        let mut canvas = Canvas::new();
        canvas.blit_row(0, 0, &[Some(bg_pixel(9))]);
        canvas.blit_row(-3, -2, &[Some(bg_pixel(7))]);
        assert_eq!(
            canvas.get(0, 0),
            Some(bg_pixel(9)),
            "original content survives a grow"
        );
        assert_eq!(canvas.get(-3, -2), Some(bg_pixel(7)));
    }

    #[test]
    fn a_sprite_occluded_cell_is_skipped_not_overwritten_with_garbage() {
        let mut canvas = Canvas::new();
        canvas.blit_row(0, 0, &[Some(bg_pixel(5))]);
        // Same cell revisited, this time sprite-occluded (None).
        canvas.blit_row(0, 0, &[None]);
        assert_eq!(
            canvas.get(0, 0),
            Some(bg_pixel(5)),
            "a None (sprite-occluded) write must not clobber a real prior observation"
        );
    }

    #[test]
    fn a_never_visited_cell_inside_grown_bounds_stays_none() {
        let mut canvas = Canvas::new();
        canvas.blit_row(0, 0, &[Some(bg_pixel(1)), None, Some(bg_pixel(3))]);
        assert_eq!(
            canvas.get(1, 0),
            None,
            "sprite-occluded on first visit: never observed"
        );
    }

    #[test]
    fn stitchable_pixel_keeps_background_and_backdrop_drops_sprites() {
        assert!(stitchable_pixel(&bg_pixel(1)).is_some());
        assert!(stitchable_pixel(&PpuPixel {
            palette_index: 0,
            layer: PixelLayer::Backdrop,
            sprite_id: None,
            priority: 0,
        })
        .is_some());
        assert!(stitchable_pixel(&sprite_pixel(1)).is_none());
    }

    #[test]
    fn stitch_frame_skips_hud_bands_entirely() {
        let mut canvas = Canvas::new();
        let width = 4u16;
        let video: Vec<PpuPixel> = (0..(width as usize * 2))
            .map(|i| bg_pixel(i as u8))
            .collect();
        let bands = vec![(
            ScanlineBand {
                start: 0,
                end: 2,
                base_scroll: (0, 0),
                is_hud: true,
            },
            BandWorldPos {
                world_x: 100,
                world_y_at_start: 100,
            },
        )];
        stitch_frame(&mut canvas, &video, width, &bands);
        assert_eq!(
            canvas.get(100, 100),
            None,
            "HUD band content must never reach the canvas"
        );
        assert_eq!(
            canvas.width(),
            0,
            "canvas must not even grow for an all-HUD frame"
        );
    }

    #[test]
    fn stitch_frame_places_a_non_hud_band_at_its_world_position() {
        let mut canvas = Canvas::new();
        let width = 2u16;
        let video = vec![bg_pixel(11), bg_pixel(12), bg_pixel(21), bg_pixel(22)];
        let bands = vec![(
            ScanlineBand {
                start: 0,
                end: 2,
                base_scroll: (0, 0),
                is_hud: false,
            },
            BandWorldPos {
                world_x: 50,
                world_y_at_start: 200,
            },
        )];
        stitch_frame(&mut canvas, &video, width, &bands);
        assert_eq!(canvas.get(50, 200), Some(bg_pixel(11)));
        assert_eq!(canvas.get(51, 200), Some(bg_pixel(12)));
        assert_eq!(
            canvas.get(50, 201),
            Some(bg_pixel(21)),
            "row 1 lands at world_y+1, per y_at"
        );
        assert_eq!(canvas.get(51, 201), Some(bg_pixel(22)));
    }
}
