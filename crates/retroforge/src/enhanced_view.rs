//! Enhanced-camera mediator (ticket W4-03e): translates rf-enhance's
//! `SceneGraph` into `rf_renderer::CompositeLayer` values and renders the
//! result via `rf_renderer::EnhancedCompositor` — the resolution step
//! `ARCHITECTURE.md` §3 assigns to the app shell, since `rf-renderer` and
//! `rf-enhance` have no edge between them in either direction
//! (`rf_renderer::composite`'s own module doc). This module has **no**
//! `egui`/`eframe` dependency (`crate` root doc's "exactly two modules
//! touch egui" inventory stays exactly two) — its behavior (SceneGraph ->
//! CompositeLayer resolution, fog rendering, FM-13 policy+message) is
//! provable headlessly (see this module's own tests); [`crate::app`] is a
//! thin caller that uploads the resulting RGBA to an egui texture.
//!
//! ## FM-13's two halves meet here (W4-03e pre-flight note)
//!
//! [`compose_ultrawide`] calls `rf_enhance::camera::fm13_zoom_divisor` (the
//! POLICY half) with a caller-supplied `policy_max_dim` — in the live app
//! that's `GpuContext::adapter_limits.max_texture_dimension_2d`
//! (`crate::app`'s wiring) — to pick a zoom-out level *before* building any
//! pixel buffer. `EnhancedCompositor::composite` then applies its own,
//! independent ENFORCEMENT clamp against the real device-granted limit
//! (`rf_renderer::composite`'s own doc: `min(adapter_limits,
//! device.limits())`). These two numbers are not always the same value:
//! `egui-wgpu`'s default device descriptor requests
//! `max_texture_dimension_2d: 8192` regardless of what the adapter itself
//! can do (verified against `egui-wgpu-0.35.0/src/setup.rs`'s
//! `device_descriptor` closure, not assumed), so on real hardware capable
//! of more (16384 on Metal, per `rf_renderer::gpu`'s own doc), the ADAPTER
//! limit this module's policy sees can genuinely exceed the DEVICE limit
//! the compositor enforces. When that happens the compositor's own
//! reduction is real, not a self-inflicted test artifact, and
//! [`UltrawideRender::reduction`] is exactly what criterion 3 requires
//! surfacing as a user-visible message — swallowing it here would make a
//! real GPU fallback invisible to the user (FM-13's whole point).
//!
//! ## Only `StitchedCanvas` has a producer
//!
//! `rf_enhance::scene_graph`'s own module doc: every other `SceneLayer`
//! variant is shape-only, with no producer anywhere in this workspace yet.
//! [`compose_ultrawide`] resolves exactly the one variant
//! `rf_enhance::camera::ultrawide_scene_over_canvas` ever constructs and
//! treats any other variant as unreachable — building a fake resolver for
//! the other six would be exactly the scaffolding the brief forbids.

use rf_core_api::{PixelLayer, PpuPixel};
use rf_enhance::camera::{
    fm13_apply_divisor, fm13_zoom_divisor, ultrawide_scene_over_canvas, FogMask, FogStyle,
};
use rf_enhance::scene_graph::{CanvasId, SceneLayer};
use rf_enhance::stitcher::Canvas;
use rf_renderer::palette::palette_index_to_rgb;
use rf_renderer::{CompositeLayer, EnhancedCompositor, GpuContext, TargetReduction};

/// Fixed handle: this ticket's shell only ever resolves one live canvas at
/// a time (the current scene's `Canvas`, `crate::canvas_accum::
/// CanvasAccumulator::current_canvas`) — `CanvasId` exists for a future
/// multi-canvas registry (`rf_enhance::scene_graph`'s own module doc), not
/// needed here.
const LIVE_CANVAS_ID: CanvasId = CanvasId(0);

/// `docs/design/ENHANCEMENT_RUNTIME.md` §3's "themed fog" — palette index
/// `0x0C` (`assets/nes.pal` decodes this to a dark teal, RGB `(0,54,66)`),
/// deliberately not one of the near-black entries (`0x0D`-`0x0F` all
/// decode to `(3,3,3)`) so fogged area is visually distinguishable from
/// genuinely dark *visited* content in a screenshot, without claiming a
/// color that could pass for real tile art either.
const FOG_PALETTE_INDEX: u8 = 0x0C;

/// A runtime camera choice (acceptance criterion 2). Deliberately omits
/// `rf_enhance::camera::CameraMode::FullMap` — this ticket's brief names
/// only "Original / Ultrawide".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CameraToggle {
    #[default]
    Original,
    Ultrawide,
}

impl CameraToggle {
    #[must_use]
    pub fn flipped(self) -> Self {
        match self {
            CameraToggle::Original => CameraToggle::Ultrawide,
            CameraToggle::Ultrawide => CameraToggle::Original,
        }
    }
}

/// The result of compositing the Ultrawide camera's view over a stitched
/// canvas.
pub struct UltrawideRender {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// FM-13: `Some` when `EnhancedCompositor::composite` had to shrink
    /// the requested target below what was asked for (module doc's "two
    /// halves" section) — this is the ENFORCEMENT half's own signal,
    /// independent of whether the POLICY half already shrank things.
    pub reduction: Option<TargetReduction>,
}

impl UltrawideRender {
    /// FM-13 criterion 3: the "view too large for GPU, reduced" toast
    /// text, or `None` if nothing was reduced. A pure function of
    /// `self.reduction` so the wording is unit-testable with no GPU at all
    /// (`tests::fm13_message_names_both_sizes` below).
    #[must_use]
    pub fn fm13_message(&self) -> Option<String> {
        self.reduction.map(|r| {
            format!(
                "view too large for GPU, reduced from {}x{} to {}x{} (device limit {}px)",
                r.requested.0, r.requested.1, r.allocated.0, r.allocated.1, r.max_dim
            )
        })
    }
}

/// Render `canvas`'s entire visited extent as the Ultrawide camera view
/// (module doc): builds `rf_enhance`'s `SceneGraph` (exactly one
/// `SceneLayer::StitchedCanvas`, camera centered on the canvas), resolves
/// its `CanvasId` handle to real pixels (this crate's job per
/// `ARCHITECTURE.md` §3 — see module doc), and composites via `gpu`/
/// `compositor`. Unvisited canvas cells (per `rf_enhance::camera::
/// fog_mask_from_canvas`, derived from `Canvas`'s own `None` cells,
/// FR-ENH-004) render as flat fog, never as guessed geometry.
///
/// `policy_max_dim` feeds `rf_enhance::camera::fm13_zoom_divisor` (module
/// doc's "two halves" section); the live caller passes
/// `gpu.adapter_limits.max_texture_dimension_2d` (`crate::app`'s wiring).
///
/// # Errors
/// - `"canvas is empty"` if nothing has been stitched yet — no producer
///   exists for an empty view; this is not a GPU failure.
/// - Otherwise propagates `EnhancedCompositor::composite`'s `Err` (GPU
///   readback timeout, `rf_renderer::gpu::GPU_WAIT`).
pub fn compose_ultrawide(
    gpu: &GpuContext,
    compositor: &EnhancedCompositor,
    canvas: &Canvas,
    policy_max_dim: u32,
) -> Result<UltrawideRender, String> {
    if canvas.width() == 0 || canvas.height() == 0 {
        return Err("canvas is empty -- nothing stitched yet".to_string());
    }

    let (origin_x, origin_y) = canvas.origin();
    let center_world = (
        origin_x + (canvas.width() as i64) / 2,
        origin_y + (canvas.height() as i64) / 2,
    );

    // FM-13 POLICY half: shrink the desired (whole-canvas) target before
    // building any pixel buffer.
    let divisor = fm13_zoom_divisor(
        canvas.width() as u32,
        canvas.height() as u32,
        policy_max_dim,
    );
    let (target_w, target_h) =
        fm13_apply_divisor(canvas.width() as u32, canvas.height() as u32, divisor);

    let (scene_graph, fog) = ultrawide_scene_over_canvas(
        LIVE_CANVAS_ID,
        canvas,
        center_world,
        target_w,
        target_h,
        FogStyle::Themed {
            palette_index: FOG_PALETTE_INDEX,
        },
    );

    // A LAYER's own texture is bound by the same real wgpu per-axis limit
    // a render TARGET is (`create_texture` validates every texture, source
    // or attachment, against the same `max_texture_dimension_2d`) — but
    // `EnhancedCompositor::composite` only clamps the TARGET
    // (`rf_renderer::composite`'s own doc), not each layer's own upload.
    // Clamping the layer buffer's size here, against the REAL device-
    // granted limit (never the possibly-stale `policy_max_dim` this
    // function was called with), is what keeps `compositor.composite`
    // below from ever being asked to `create_texture` an over-limit
    // SOURCE texture — independent of whatever the TARGET request ends up
    // needing (module doc's "two halves" section: the target's own
    // reduction is still genuinely decoupled from this).
    let layer_limit = gpu.device.limits().max_texture_dimension_2d.max(1);
    let layer_w = target_w.min(layer_limit);
    let layer_h = target_h.min(layer_limit);

    // Handle resolution (module doc): the only real producer today is
    // `StitchedCanvas` (`rf_enhance::scene_graph` module doc) -- anything
    // else here would be this crate faking a producer the brief forbids.
    let mut layer_buffers: Vec<Vec<u8>> = Vec::with_capacity(scene_graph.layers.len());
    for layer in &scene_graph.layers {
        match layer {
            SceneLayer::StitchedCanvas {
                canvas: id,
                fog: style,
            } => {
                debug_assert_eq!(*id, LIVE_CANVAS_ID);
                layer_buffers.push(render_canvas_window_rgba(
                    canvas,
                    &fog,
                    center_world,
                    layer_w,
                    layer_h,
                    *style,
                    divisor,
                ));
            }
            other => {
                unreachable!(
                    "only SceneLayer::StitchedCanvas has a producer (rf_enhance::scene_graph \
                     module doc) -- got {other:?}"
                );
            }
        }
    }
    let layers: Vec<CompositeLayer<'_>> = layer_buffers
        .iter()
        .map(|rgba| CompositeLayer {
            rgba,
            width: layer_w,
            height: layer_h,
            dst_x: 0,
            dst_y: 0,
        })
        .collect();

    // FM-13 ENFORCEMENT half (module doc): independent of the policy step
    // above -- `compositor.composite` re-derives its own limit from `gpu`
    // and clamps again regardless of what this function already assumed.
    // Requesting the ORIGINAL (possibly policy-under-shrunk) `target_w`/
    // `target_h` here, not the layer-clamped size, is what lets a genuine
    // policy/enforcement gap (module doc) still surface as
    // `CompositeOutcome::reduction`.
    let outcome = compositor
        .composite(gpu, &layers, target_w, target_h)
        .map_err(|e| format!("ultrawide composite failed: {e}"))?;

    Ok(UltrawideRender {
        rgba: outcome.rgba,
        width: outcome.width,
        height: outcome.height,
        reduction: outcome.reduction,
    })
}

/// Extract a `target_w`x`target_h` RGBA window of `canvas` centered on
/// `center_world`, sampling every `divisor`-th world pixel per axis
/// (FM-13's zoom-out: `divisor > 1` means one destination pixel represents
/// a `divisor`x`divisor` world block — nearest/point sampling, matching
/// this project's "nearest-neighbor unless a filter pass says otherwise"
/// convention, `RENDERER.md` §2). Unvisited cells (per `fog`, derived from
/// `canvas`'s own `None` cells — FR-ENH-004, never from colour) render as
/// `fog_style`'s flat fill; a destination pixel outside `fog`'s own
/// bounds (module doc's "canvas keeps growing" case) is treated the same
/// way `Canvas::get`'s own doc does: indistinguishable from "inside bounds
/// but never visited".
fn render_canvas_window_rgba(
    canvas: &Canvas,
    fog: &FogMask,
    center_world: (i64, i64),
    target_w: u32,
    target_h: u32,
    fog_style: FogStyle,
    divisor: u32,
) -> Vec<u8> {
    let FogStyle::Themed { palette_index } = fog_style;
    let fog_rgb = palette_index_to_rgb(palette_index);
    let divisor = i64::from(divisor.max(1));
    let half_w = (i64::from(target_w) * divisor) / 2;
    let half_h = (i64::from(target_h) * divisor) / 2;
    let origin_world_x = center_world.0 - half_w;
    let origin_world_y = center_world.1 - half_h;

    let mut rgba = vec![0u8; target_w as usize * target_h as usize * 4];
    for dy in 0..target_h {
        for dx in 0..target_w {
            let wx = origin_world_x + i64::from(dx) * divisor;
            let wy = origin_world_y + i64::from(dy) * divisor;
            let visited = fog_index(fog, wx, wy).is_some_and(|i| !fog.unvisited[i]);
            let rgb = if visited {
                canvas
                    .get(wx, wy)
                    .map_or(fog_rgb, |px| palette_index_to_rgb(px.palette_index))
            } else {
                fog_rgb
            };
            let idx = (dy as usize * target_w as usize + dx as usize) * 4;
            rgba[idx] = rgb[0];
            rgba[idx + 1] = rgb[1];
            rgba[idx + 2] = rgb[2];
            rgba[idx + 3] = 0xFF;
        }
    }
    rgba
}

fn fog_index(fog: &FogMask, world_x: i64, world_y: i64) -> Option<usize> {
    let (ix, iy) = (world_x - fog.origin_x, world_y - fog.origin_y);
    if ix < 0 || iy < 0 || ix as usize >= fog.width || iy as usize >= fog.height {
        return None;
    }
    Some(iy as usize * fog.width + ix as usize)
}

/// What [`select_active_view`] resolved to for a given [`CameraToggle`] —
/// `crate::app::video_panel`'s only job is to match on this and paint,
/// never to re-derive the branching itself (that branching is what this
/// module's own tests prove, headlessly).
pub enum ActiveView<'a> {
    /// Show the ordinary live NES frame unchanged — Accuracy Mode's own
    /// output, untouched by anything in this module (criterion 4).
    Original,
    /// Show the composited ultrawide view.
    Ultrawide {
        rgba: &'a [u8],
        width: u32,
        height: u32,
    },
    /// Toggled to Ultrawide, but no render is available yet (nothing
    /// stitched, or the last `compose_ultrawide` call failed) — carries
    /// the reason so the shell can show it rather than a blank pane.
    UltrawideUnavailable(&'a str),
}

/// Pure decision function for which view to paint (mutation-verify target
/// 1, ticket brief: "force the camera to Original regardless of toggle ->
/// the 'output differs' test must fail"). Kept free of `egui` so it is
/// headlessly testable — see `tests::toggling_to_ultrawide_actually_shows
/// _different_content_than_original` below, which is exactly the "assert
/// the rendered output differs" proof the brief's vacuity trap (a) demands.
#[must_use]
pub fn select_active_view<'a>(
    toggle: CameraToggle,
    ultrawide: Option<&'a Result<UltrawideRender, String>>,
) -> ActiveView<'a> {
    match toggle {
        CameraToggle::Original => ActiveView::Original,
        CameraToggle::Ultrawide => match ultrawide {
            Some(Ok(render)) => ActiveView::Ultrawide {
                rgba: &render.rgba,
                width: render.width,
                height: render.height,
            },
            Some(Err(msg)) => ActiveView::UltrawideUnavailable(msg),
            None => ActiveView::UltrawideUnavailable("no canvas snapshot received yet"),
        },
    }
}

/// Render a decoded level to rgba (ticket W11-02; FR-ENH-005,
/// `docs/design/FRONTEND_UI.md` §3.3's Map and VISION §2's "whole levels
/// on one screen").
///
/// ## Why this function had to be written rather than called
///
/// `SceneLayer::DecodedLevel` had **no producer anywhere**.
/// `rf_enhance::scene_graph`'s own module doc says so outright, and
/// [`compose_ultrawide`] above carries an `unreachable!` for every layer
/// that is not `StitchedCanvas`. The decode was real and tested, the
/// scene graph was real and tested, and nothing in the repository could
/// turn either into pixels — `authoring::preview_text` renders a decoded
/// level as **ASCII**, which is genuinely useful for authoring and is not
/// a picture. `level_view_demo.rs` asserts on the scene's *shape*, so it
/// stayed green for the whole time the feature could not be seen.
///
/// ## Why it lives in this crate
///
/// `ARCHITECTURE.md` §3 makes this module the `SceneGraph` ->
/// `CompositeLayer` mediator and puts handle resolution *outside*
/// `rf-enhance` — which is also the only arrangement that works, since
/// turning tile ids into pixels needs `rf_debugger::pattern` and
/// `rf-enhance` may not depend on `rf-debugger`.
///
/// ## Palette honesty
///
/// The level is drawn with the palette the profile's decode implies, not
/// with a guess sampled from the live frame. A level view tinted by
/// whatever palette happened to be loaded when you opened it would look
/// authoritative and be wrong in a way nobody could see — the same class
/// of error as fog that guesses geometry (FR-ENH-004), which the
/// stitcher refuses to make.
#[must_use]
pub fn render_level_rgba(
    level: &rf_enhance::decode::metatile_screens::DecodedLevel,
    chr: &[u8],
    table: rf_debugger::pattern::PatternTable,
    palette: [[u8; 3]; 4],
    geometry: rf_enhance::level_view::LevelGeometry,
) -> Vec<u8> {
    let (w, h) = (geometry.width_px as usize, geometry.height_px as usize);
    // Transparent, not black: a level whose decode produced fewer
    // metatiles than the grid claims should read as absent, not as a
    // solid floor that a player could mistake for terrain.
    let mut rgba = vec![0u8; w * h * 4];
    if w == 0 || h == 0 || level.metatile_count == 0 {
        return rgba;
    }
    let mpx = geometry.metatile_px.max(1) as usize;
    // Tiles per metatile edge: a size-4 metatile is 2x2 tiles, size-1 is
    // one. Derived, never assumed to be 2 — the same reasoning
    // `LevelGeometry::from_level` gives for not assuming 16 px.
    let tiles_per_edge = (mpx / 8).max(1);
    let per_metatile = level
        .metatile_tiles
        .len()
        .checked_div(level.metatile_count as usize)
        .unwrap_or(0);
    if per_metatile == 0 {
        return rgba;
    }

    for col in 0..level.width as usize {
        for row in 0..level.height as usize {
            // Column-major, as `DecodedLevel::metatiles` documents:
            // index `col * height + row`. Getting this backwards produces
            // a plausible-looking transposed level rather than an error,
            // which is exactly why it is spelled out here.
            let Some(&id) = level.metatiles.get(col * level.height as usize + row) else {
                continue;
            };
            let base = id as usize * per_metatile;
            for sub in 0..per_metatile.min(tiles_per_edge * tiles_per_edge) {
                let Some(&tile_id) = level.metatile_tiles.get(base + sub) else {
                    continue;
                };
                let Some(tile) = rf_debugger::pattern::decode_tile(chr, table, tile_id) else {
                    continue;
                };
                let px = rf_debugger::pattern::tile_to_rgba(&tile, palette);
                let ox = col * mpx + (sub % tiles_per_edge) * 8;
                let oy = row * mpx + (sub / tiles_per_edge) * 8;
                for ty in 0..8 {
                    let dy = oy + ty;
                    if dy >= h {
                        break;
                    }
                    for tx in 0..8 {
                        let dx = ox + tx;
                        if dx >= w {
                            break;
                        }
                        let s = (ty * 8 + tx) * 4;
                        let d = (dy * w + dx) * 4;
                        rgba[d..d + 4].copy_from_slice(&px[s..s + 4]);
                    }
                }
            }
        }
    }
    rgba
}

// ---------------------------------------------------------------------
// Fog/steam pass (ticket W16-04; `docs/design/ENHANCEMENT_WAVE_16.md` §4):
// the two pure conversions from `rf_enhance::atmosphere`'s own output into
// `rf_renderer::fog::FogPass`'s plain-data inputs. This is the app-shell
// resolution step ARCHITECTURE.md §3 assigns here (module doc's own
// opening line) — `rf-renderer` cannot know about `SceneLayer::ExtractedBg`
// and `rf-enhance` cannot know about an RGBA texture buffer.
// ---------------------------------------------------------------------

/// Build a density-map RGBA buffer from an `ExtractedBg` layer's own
/// pixels (`rf_enhance::scene_graph::SceneLayer::ExtractedBg`) —
/// `rf_renderer::fog`'s WGSL pass only reads the red channel, so this
/// writes the same value into every channel and full alpha.
///
/// **Approximation, stated plainly.** `PpuPixel::palette_index` is a raw
/// palette-table index, not a resolved colour — this crate has no CGRAM
/// snapshot threaded through `SceneLayer::ExtractedBg` (it carries none;
/// see that variant's own fields), so the index itself is used as a
/// brightness proxy, scaled into the full `u8` range. This is enough
/// structure for the fog shader's multi-octave sampling to read real
/// density variation rather than a flat plane, but it is not the plane's
/// true rendered colour — resolving through the actual palette is future
/// work for whichever ticket threads a palette snapshot through the scene
/// graph. A pixel that never belonged to this layer (backdrop-filled by
/// `rf_enhance::atmosphere::extracted_bg_layer`) reads as zero density.
#[must_use]
pub fn atmosphere_density_rgba(pixels: &[PpuPixel], layer: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(pixels.len() * 4);
    for px in pixels {
        let density = if px.layer == PixelLayer::Background(layer) {
            px.palette_index
        } else {
            0
        };
        out.extend_from_slice(&[density, density, density, 255]);
    }
    out
}

/// Convert an atmosphere layer's raw hardware scroll delta between two
/// captures into `rf_renderer::fog::FogParams`'s UV-space drift-per-second
/// — the "follows the layer's scroll telemetry" half of acceptance
/// criterion 1.
///
/// `width`/`height` are the layer's own pixel dimensions (`SceneLayer::
/// ExtractedBg::width`/`height`): a scroll register moves in *pixels*, a
/// UV coordinate is `[0, 1]` over that same buffer, so dividing the pixel
/// delta by the buffer size is the direct unit conversion, not a tuned
/// constant. `dt_secs <= 0.0` returns zero drift rather than dividing by
/// zero or a negative time — a caller passing a bad timestamp gets a
/// stationary fog frame, not a crash or a wraparound value that would
/// send the fog spinning.
#[must_use]
pub fn atmosphere_scroll_drift_per_second(
    prev_scroll: (i64, i64),
    cur_scroll: (i64, i64),
    width: u16,
    height: u16,
    dt_secs: f32,
) -> (f32, f32) {
    if dt_secs <= 0.0 || width == 0 || height == 0 {
        return (0.0, 0.0);
    }
    let dx_px = (cur_scroll.0 - prev_scroll.0) as f32;
    let dy_px = (cur_scroll.1 - prev_scroll.1) as f32;
    let dx_uv = dx_px / f32::from(width);
    let dy_uv = dy_px / f32::from(height);
    (dx_uv / dt_secs, dy_uv / dt_secs)
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

    fn gpu_or_skip(test_name: &str) -> Option<GpuContext> {
        match GpuContext::request_headless() {
            Ok(gpu) => Some(gpu),
            Err(e) => {
                eprintln!(
                    "SKIP {test_name}: no wgpu adapter in this environment ({e}) -- clean skip"
                );
                None
            }
        }
    }

    // --- select_active_view: pure toggle logic, no GPU needed -----------

    #[test]
    fn original_toggle_always_shows_original_even_with_a_ready_ultrawide_render() {
        let render: Result<UltrawideRender, String> = Ok(UltrawideRender {
            rgba: vec![9, 9, 9, 255],
            width: 1,
            height: 1,
            reduction: None,
        });
        let view = select_active_view(CameraToggle::Original, Some(&render));
        assert!(matches!(view, ActiveView::Original));
    }

    #[test]
    fn ultrawide_toggle_with_no_snapshot_yet_reports_unavailable_not_a_panic() {
        let view = select_active_view(CameraToggle::Ultrawide, None);
        assert!(matches!(view, ActiveView::UltrawideUnavailable(_)));
    }

    /// Vacuity trap (a) from the ticket brief, at the pure-logic layer: a
    /// mutation that forces `ActiveView::Original` regardless of `toggle`
    /// would still pass `original_toggle_always_shows_original...` above,
    /// but fails HERE, because Ultrawide's own render carries content that
    /// provably differs from anything Original could show (different byte
    /// content AND different dimensions).
    #[test]
    fn ultrawide_toggle_with_a_ready_render_shows_ultrawide_content_not_original() {
        let render: Result<UltrawideRender, String> = Ok(UltrawideRender {
            rgba: vec![1, 2, 3, 255, 4, 5, 6, 255],
            width: 2,
            height: 1,
            reduction: None,
        });
        let view = select_active_view(CameraToggle::Ultrawide, Some(&render));
        match view {
            ActiveView::Ultrawide {
                rgba,
                width,
                height,
            } => {
                assert_eq!(rgba, &[1, 2, 3, 255, 4, 5, 6, 255]);
                assert_eq!((width, height), (2, 1));
            }
            _ => panic!("expected ActiveView::Ultrawide"),
        }
    }

    // --- compose_ultrawide: empty canvas is an error, not a panic -------

    #[test]
    fn composing_an_empty_canvas_is_a_clean_error() {
        let Some(gpu) = gpu_or_skip("composing_an_empty_canvas_is_a_clean_error") else {
            return;
        };
        let compositor = EnhancedCompositor::new(&gpu);
        let canvas = Canvas::new();
        let result = compose_ultrawide(&gpu, &compositor, &canvas, 8192);
        assert!(result.is_err());
    }

    // --- Fog: mutation-verify target 2 ("drop the fog mask -> the fog
    // test must fail"). Vacuity trap (b): must distinguish unvisited from
    // visited-but-dark by exact byte value, not "looks dark". -----------

    #[test]
    fn fog_marks_exactly_the_canvas_none_cells_not_by_colour() {
        let Some(gpu) = gpu_or_skip("fog_marks_exactly_the_canvas_none_cells_not_by_colour") else {
            return;
        };
        let compositor = EnhancedCompositor::new(&gpu);

        let mut canvas = Canvas::new();
        // Row of 4 cells at y=0: visited/unvisited/visited/unvisited,
        // alternating -- `blit_row` grows bounds to cover all 4 even
        // though only indices 0 and 2 are `Some`.
        canvas.blit_row(0, 0, &[Some(bg(0)), None, Some(bg(0)), None]);

        let render = compose_ultrawide(&gpu, &compositor, &canvas, 8192)
            .expect("a small in-bounds canvas must compose cleanly");
        assert_eq!(render.width, 4);
        assert_eq!(render.height, 1);

        let fog_rgb = palette_index_to_rgb(FOG_PALETTE_INDEX);
        let visited_rgb = palette_index_to_rgb(0);
        assert_ne!(
            fog_rgb, visited_rgb,
            "test fixture invalid: fog colour must differ from the visited pixel's own colour, \
             or this test cannot discriminate anything"
        );

        let px = |render: &UltrawideRender, x: usize| -> [u8; 4] {
            let i = x * 4;
            [
                render.rgba[i],
                render.rgba[i + 1],
                render.rgba[i + 2],
                render.rgba[i + 3],
            ]
        };
        assert_eq!(
            &px(&render, 0)[..3],
            &visited_rgb[..],
            "cell 0 is visited (Some) -- must render its real colour, not fog"
        );
        assert_eq!(
            &px(&render, 1)[..3],
            &fog_rgb[..],
            "cell 1 is unvisited (None) -- must render as fog"
        );
        assert_eq!(
            &px(&render, 2)[..3],
            &visited_rgb[..],
            "cell 2 is visited (Some) -- must render its real colour, not fog"
        );
        assert_eq!(
            &px(&render, 3)[..3],
            &fog_rgb[..],
            "cell 3 is unvisited (None) -- must render as fog"
        );
    }

    // --- FM-13 message: mutation-verify target 3 ("swallow the
    // TargetReduction -> the FM-13 message test must fail"). Deliberately
    // decoupled from the POLICY half via a caller-injected, deliberately
    // STALE `policy_max_dim` -- proves the compositor's own independent
    // enforcement (not just a pre-shrunk request that never needed
    // clamping) is what gets surfaced, exactly the real egui-adapter-vs-
    // device-limit gap this module's own doc names. -----------------------

    #[test]
    fn compositor_reduction_beyond_the_policys_estimate_surfaces_as_a_message() {
        let Some(gpu) =
            gpu_or_skip("compositor_reduction_beyond_the_policys_estimate_surfaces_as_a_message")
        else {
            return;
        };
        let compositor = EnhancedCompositor::new(&gpu);
        // `request_headless` requests the device with the adapter's own
        // limits (`rf_renderer::gpu`'s own doc), so this environment's
        // real ceiling for BOTH adapter and device agree here.
        let real_max_dim = gpu
            .adapter_limits
            .max_texture_dimension_2d
            .min(gpu.device.limits().max_texture_dimension_2d);

        // A canvas genuinely wider than the real ceiling -- built cheaply
        // (single row) rather than actually allocating that many cells'
        // worth of area.
        let requested_width = real_max_dim + 4096;
        let mut canvas = Canvas::new();
        canvas.blit_row(0, 0, &[Some(bg(1))]);
        canvas.blit_row(i64::from(requested_width) - 1, 0, &[Some(bg(1))]);
        assert_eq!(canvas.width() as u32, requested_width);

        // Deliberately STALE policy limit, larger than the real device
        // ceiling -- simulates the real egui-wgpu gap (adapter reports
        // 16384, egui's own device only grants 8192): the POLICY half
        // under-shrinks (or does not shrink at all), so only the
        // compositor's own ENFORCEMENT clamp catches this.
        let stale_policy_max_dim = requested_width + 8192;

        let render = compose_ultrawide(&gpu, &compositor, &canvas, stale_policy_max_dim)
            .expect("must fall back to a smaller target, never fail on our own oversized ask");

        assert!(
            render.width <= real_max_dim,
            "allocated width ({}) must never exceed the real device limit ({real_max_dim})",
            render.width
        );
        let message = render.fm13_message().expect(
            "a request beyond the real limit must surface a user-visible FM-13 message -- \
             swallowing CompositeOutcome::reduction here is exactly the bug criterion 3 exists \
             to catch",
        );
        assert!(
            message.contains("view too large for GPU, reduced"),
            "message was: {message}"
        );
        assert!(
            message.contains(&requested_width.to_string()),
            "message must name the requested size, message was: {message}"
        );
    }

    #[test]
    fn fm13_message_is_none_when_nothing_was_reduced() {
        let render = UltrawideRender {
            rgba: vec![],
            width: 100,
            height: 100,
            reduction: None,
        };
        assert_eq!(render.fm13_message(), None);
    }

    #[test]
    fn fm13_message_names_both_sizes() {
        let render = UltrawideRender {
            rgba: vec![],
            width: 8192,
            height: 4096,
            reduction: Some(TargetReduction {
                requested: (20000, 5000),
                allocated: (8192, 4096),
                max_dim: 8192,
            }),
        };
        let message = render.fm13_message().expect("reduction was Some");
        assert!(message.contains("20000"));
        assert!(message.contains("5000"));
        assert!(message.contains("8192"));
        assert!(message.contains("4096"));
        assert!(message.contains("view too large for GPU, reduced"));
    }

    // --- Fog/steam pass conversions (ticket W16-04) ---------------------

    fn bg_px(layer: u8, palette_index: u8) -> PpuPixel {
        PpuPixel {
            palette_index,
            layer: PixelLayer::Background(layer),
            sprite_id: None,
            priority: 0,
        }
    }

    fn backdrop_px() -> PpuPixel {
        PpuPixel {
            palette_index: 0,
            layer: PixelLayer::Backdrop,
            sprite_id: None,
            priority: 0,
        }
    }

    #[test]
    fn density_rgba_carries_only_the_named_layers_own_pixels() {
        let pixels = vec![bg_px(1, 42), backdrop_px(), bg_px(0, 200), bg_px(1, 9)];
        let rgba = atmosphere_density_rgba(&pixels, 1);
        assert_eq!(rgba.len(), pixels.len() * 4);
        assert_eq!(&rgba[0..4], &[42, 42, 42, 255], "layer 1's own pixel");
        assert_eq!(
            &rgba[4..8],
            &[0, 0, 0, 255],
            "backdrop reads as zero density"
        );
        assert_eq!(
            &rgba[8..12],
            &[0, 0, 0, 255],
            "another layer's pixel must not leak into this layer's density"
        );
        assert_eq!(&rgba[12..16], &[9, 9, 9, 255]);
    }

    #[test]
    fn scroll_drift_converts_pixel_delta_to_uv_per_second() {
        // 8px/frame horizontal scroll, 64-wide layer, 1/60s frame -> UV
        // delta is 8/64 = 0.125 per frame, so 7.5 UV units/sec.
        let (dx, dy) = atmosphere_scroll_drift_per_second((0, 0), (8, 0), 64, 32, 1.0 / 60.0);
        assert!((dx - 7.5).abs() < 1e-4, "dx = {dx}");
        assert_eq!(dy, 0.0);
    }

    #[test]
    fn scroll_drift_is_zero_for_a_non_positive_or_zero_sized_input() {
        assert_eq!(
            atmosphere_scroll_drift_per_second((0, 0), (8, 8), 64, 32, 0.0),
            (0.0, 0.0),
            "dt <= 0 must not divide by zero or go negative-time"
        );
        assert_eq!(
            atmosphere_scroll_drift_per_second((0, 0), (8, 8), 0, 32, 1.0 / 60.0),
            (0.0, 0.0),
            "a zero-width layer must not divide by zero"
        );
    }
}
