//! Camera policy (ticket W4-03b): the ultrawide camera + fog, produced as
//! DATA (`docs/design/ENHANCEMENT_RUNTIME.md` §7's `Camera`/`SceneLayer`),
//! plus the FM-13 zoom-fallback policy
//! (`docs/design/FAILURE_MODES.md`'s FM-13 row: "Camera falls back to
//! smaller target/zoom level; never device-lost by our own alloc").
//!
//! ## Scope fence
//!
//! GPU compositing of a [`crate::scene_graph::SceneGraph`] and the actual
//! allocation-size enforcement against a real `wgpu` adapter are ticket
//! W4-03c's job, not this crate's — see that ticket's pre-flight notes for
//! why (`FR-REND-004`'s crate column is `rf-renderer`, out of this
//! ticket's `write_scope`). Everything here is a pure function or a plain
//! data value; nothing allocates a texture or touches `wgpu` (this crate
//! has no `wgpu` dependency at all, by design — the FM-13 policy below
//! takes the adapter's limit as a plain `u32`).

use crate::scene_graph::{CanvasId, SceneGraph, SceneLayer};
use crate::stitcher::Canvas;

/// Which of ENHANCEMENT_RUNTIME §7's three camera modes is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraMode {
    /// Unmodified console output.
    Original,
    /// Cropped/expanded view over the stitched canvas.
    Ultrawide,
    /// Zoomed out to show the whole known level.
    FullMap,
}

/// A 2D affine camera (RENDERER.md §3: "2D affine (translate/zoom)").
/// Data only — animating it (smooth pans for room transitions) is the
/// composer's job, not this type's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Camera {
    pub mode: CameraMode,
    /// World-space point the camera is centered on.
    pub center_world: (i64, i64),
    /// FM-13 zoom-fallback divisor currently in effect (1 = no
    /// zoom-out) — see [`fm13_zoom_divisor`].
    pub zoom_divisor: u32,
    /// Output/target size this camera's view composites into
    /// (`RENDERER.md` §3: "a virtual canvas whose size is decoupled from
    /// console output").
    pub target_width: u32,
    pub target_height: u32,
}

/// How unvisited stitched-canvas area renders (FR-ENH-004: "the UI shall
/// never imply unvisited geometry is known") — DATA only; W4-03c is the
/// one that actually composites a fog fill from this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FogStyle {
    /// A themed flat fill (`ENHANCEMENT_RUNTIME.md` §3: "unexplored area
    /// renders as themed fog").
    Themed { palette_index: u8 },
}

/// A canvas's visited/unvisited mask, derived from
/// [`crate::stitcher::Canvas`]'s own `Option<PpuPixel>` cells — never from
/// pixel colour (module doc's fog vacuity trap: unvisited and
/// visited-but-black are both dark, so nothing here may branch on
/// `.palette_index`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FogMask {
    pub origin_x: i64,
    pub origin_y: i64,
    pub width: usize,
    pub height: usize,
    /// `true` = unvisited (the canvas cell at this position is `None`),
    /// `false` = visited (the cell is `Some`, regardless of its colour).
    pub unvisited: Vec<bool>,
}

/// Derive a [`FogMask`] from `canvas`'s own `None`/`Some` distinction
/// (FR-ENH-004; `crate::stitcher::Canvas`'s module doc names this the
/// honesty requirement). One bool per cell, same row-major layout as
/// [`Canvas::cells`].
#[must_use]
pub fn fog_mask_from_canvas(canvas: &Canvas) -> FogMask {
    let (origin_x, origin_y) = canvas.origin();
    FogMask {
        origin_x,
        origin_y,
        width: canvas.width(),
        height: canvas.height(),
        unvisited: canvas.cells().iter().map(Option::is_none).collect(),
    }
}

/// Build the ultrawide-camera scene graph over a stitched canvas, as DATA
/// (criterion 2: "produced as data ... GPU compositing of that data is
/// ticket W4-03c and is deliberately out of your scope"). `canvas_id` is
/// the handle the shell will resolve to `canvas`'s actual pixels
/// (`crate::scene_graph` module doc); `fog` is derived separately via
/// [`fog_mask_from_canvas`] since [`FogStyle`] only carries the *theme*, not
/// the per-cell mask (the mask is canvas-shaped data, not camera/style
/// data — kept as two return values rather than folding the mask into
/// [`FogStyle`] itself, which would force every consumer of the camera+
/// layer data to also carry a full per-cell buffer even when it only
/// wants the theme, e.g. a debug HUD listing which fog style is active).
#[must_use]
pub fn ultrawide_scene_over_canvas(
    canvas_id: CanvasId,
    canvas: &Canvas,
    center_world: (i64, i64),
    target_width: u32,
    target_height: u32,
    fog: FogStyle,
) -> (SceneGraph, FogMask) {
    let graph = SceneGraph {
        camera: Camera {
            mode: CameraMode::Ultrawide,
            center_world,
            zoom_divisor: 1,
            target_width,
            target_height,
        },
        layers: vec![SceneLayer::StitchedCanvas {
            canvas: canvas_id,
            fog,
        }],
    };
    (graph, fog_mask_from_canvas(canvas))
}

/// FM-13 zoom-fallback policy (`FAILURE_MODES.md`'s own test column: "UT:
/// mocked adapter limits"). Pure function of `(desired target size,
/// adapter max_texture_dimension_2d)` — the limit crosses as a plain
/// `u32` so this crate needs no `wgpu` dependency (module doc); W4-03c
/// reads the real adapter limit and enforces it at allocation, this
/// function only decides the policy.
///
/// Returns the smallest positive integer divisor `d` such that dividing
/// both `desired_w` and `desired_h` by `d` (ceiling-rounded,
/// [`fm13_apply_divisor`]) fits within `max_dim` on both axes. An integer
/// divisor rather than a float zoom factor sidesteps float-determinism
/// concerns and composes cleanly with pixel-grid target allocation at the
/// enforcement boundary. `d == 1` means "no fallback needed" (the desired
/// size already fits).
#[must_use]
pub fn fm13_zoom_divisor(desired_w: u32, desired_h: u32, max_dim: u32) -> u32 {
    if max_dim == 0 {
        // Nothing fits at any divisor; refusing the allocation entirely
        // is the enforcement boundary's job (W4-03c), not this policy's --
        // returning the largest divisor this type can express at least
        // never *understates* how much shrinking is needed.
        return u32::MAX;
    }
    let ceiling = desired_w.max(desired_h).max(1);
    let mut divisor = 1u32;
    while divisor < ceiling && !fits(desired_w, desired_h, max_dim, divisor) {
        divisor += 1;
    }
    divisor
}

/// Apply a divisor to a desired size, ceiling-rounded so the result never
/// exceeds `desired / divisor` and — combined with a `divisor` from
/// [`fm13_zoom_divisor`] — always fits within the `max_dim` that produced
/// it.
#[must_use]
pub fn fm13_apply_divisor(desired_w: u32, desired_h: u32, divisor: u32) -> (u32, u32) {
    let divisor = divisor.max(1);
    (ceil_div(desired_w, divisor), ceil_div(desired_h, divisor))
}

fn ceil_div(n: u32, d: u32) -> u32 {
    n.div_ceil(d)
}

fn fits(w: u32, h: u32, max_dim: u32, divisor: u32) -> bool {
    let (tw, th) = fm13_apply_divisor(w, h, divisor);
    tw <= max_dim && th <= max_dim
}

#[cfg(test)]
mod tests {
    use super::*;
    use rf_core_api::{PixelLayer, PpuPixel};

    fn bg(v: u8) -> PpuPixel {
        PpuPixel {
            palette_index: v,
            layer: PixelLayer::Background(0),
            sprite_id: None,
            priority: 0,
        }
    }

    // --- Fog: derives from None/Some, never from colour ----------------

    #[test]
    fn fog_mask_distinguishes_unvisited_from_visited_black_by_none_not_colour() {
        let mut canvas = Canvas::new();
        // Cell (0,0): visited, and happens to be black (palette index 0).
        canvas.blit_row(0, 0, &[Some(bg(0))]);
        // Cell (1,0): never visited (None) -- also renders dark under any
        // naive colour-based scheme, which is exactly the vacuity trap.
        // `blit_row` only grows bounds; cell (1,0) stays None because
        // nothing ever wrote it.
        canvas.blit_row(5, 0, &[Some(bg(0))]); // forces bounds to include x=1..5 as unvisited gaps

        let mask = fog_mask_from_canvas(&canvas);
        let (ox, oy) = canvas.origin();
        let idx =
            |wx: i64, wy: i64| -> usize { ((wy - oy) as usize) * mask.width + (wx - ox) as usize };

        assert!(
            !mask.unvisited[idx(0, 0)],
            "a visited cell, even a black one, must not be marked as fog"
        );
        assert!(
            mask.unvisited[idx(1, 0)],
            "a genuinely unvisited cell must be marked as fog regardless of any colour"
        );
        assert!(!mask.unvisited[idx(5, 0)]);
    }

    #[test]
    fn fog_mask_survives_a_canvas_to_chunk_round_trip() {
        let mut canvas = Canvas::new();
        canvas.blit_row(-2, 3, &[Some(bg(0)), None, Some(bg(200))]);
        let before = fog_mask_from_canvas(&canvas);

        let chunk = crate::persistence::canvas_to_chunk(&canvas);
        let restored = crate::persistence::chunk_to_canvas(&chunk);
        let after = fog_mask_from_canvas(&restored);

        assert_eq!(
            before, after,
            "the visited/unvisited distinction must survive a chunk round trip unchanged"
        );
    }

    // --- Ultrawide camera + fog: produced as data -----------------------

    #[test]
    fn ultrawide_scene_over_canvas_produces_an_ultrawide_camera_and_a_stitched_layer() {
        let mut canvas = Canvas::new();
        canvas.blit_row(0, 0, &[Some(bg(1)), Some(bg(2))]);
        let (graph, fog) = ultrawide_scene_over_canvas(
            CanvasId(42),
            &canvas,
            (10, 20),
            3440,
            1440,
            FogStyle::Themed { palette_index: 5 },
        );

        assert_eq!(graph.camera.mode, CameraMode::Ultrawide);
        assert_eq!(graph.camera.center_world, (10, 20));
        assert_eq!(graph.camera.target_width, 3440);
        assert_eq!(graph.camera.target_height, 1440);
        assert_eq!(graph.layers.len(), 1);
        match &graph.layers[0] {
            SceneLayer::StitchedCanvas {
                canvas: id,
                fog: style,
            } => {
                assert_eq!(*id, CanvasId(42));
                assert_eq!(*style, FogStyle::Themed { palette_index: 5 });
            }
            other => panic!("expected StitchedCanvas, got {other:?}"),
        }
        assert_eq!(fog.width, canvas.width());
        assert_eq!(fog.height, canvas.height());
    }

    // --- FM-13: at / below / above the limit ----------------------------

    #[test]
    fn at_the_limit_needs_no_fallback() {
        let d = fm13_zoom_divisor(2000, 1000, 2000);
        assert_eq!(d, 1);
        let (w, h) = fm13_apply_divisor(2000, 1000, d);
        assert!(w <= 2000 && h <= 2000);
        assert_eq!((w, h), (2000, 1000));
    }

    #[test]
    fn below_the_limit_needs_no_fallback() {
        let d = fm13_zoom_divisor(800, 600, 2000);
        assert_eq!(d, 1);
        let (w, h) = fm13_apply_divisor(800, 600, d);
        assert_eq!((w, h), (800, 600));
    }

    /// The discriminating case (FM-13 vacuity trap): a desired size that
    /// genuinely EXCEEDS the limit.
    #[test]
    fn above_the_limit_picks_the_smallest_divisor_that_fits_and_the_target_genuinely_fits() {
        let desired = (4000u32, 2000u32);
        let max_dim = 2000u32;
        let d = fm13_zoom_divisor(desired.0, desired.1, max_dim);

        // Property 1: the resulting target genuinely fits -- not merely
        // "smaller than desired" (the weak assertion the ticket brief
        // warns is vacuous).
        let (tw, th) = fm13_apply_divisor(desired.0, desired.1, d);
        assert!(
            tw <= max_dim && th <= max_dim,
            "target {tw}x{th} must fit within {max_dim}"
        );

        // Property 2: it is the SMALLEST divisor (largest zoom) that
        // fits -- d-1 must NOT fit, ruling out an over-conservative
        // policy (e.g. a fixed divisor of 8) that would still pass
        // property 1 alone.
        assert_eq!(
            d, 2,
            "4000x2000 into a 2000 limit needs exactly a /2 divisor"
        );
        let (tw_smaller, th_smaller) = fm13_apply_divisor(desired.0, desired.1, d - 1);
        assert!(
            tw_smaller > max_dim || th_smaller > max_dim,
            "divisor {} (one less than the policy's answer) must NOT fit -- otherwise the \
             policy did not pick the largest zoom that fits",
            d - 1
        );
    }

    #[test]
    fn a_square_target_far_over_the_limit_also_fits_after_the_policy() {
        let d = fm13_zoom_divisor(9000, 9000, 1024);
        let (w, h) = fm13_apply_divisor(9000, 9000, d);
        assert!(w <= 1024 && h <= 1024);
    }
}
