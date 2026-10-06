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
use rf_enhance::scene_graph::{Anchor, CanvasId, HudRegion, SceneLayer};
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

/// Ticket W20-17: a HUD band cut from the live frame, to be pinned over
/// the ultrawide view (`SceneLayer::HudPinned`'s pixels — this crate
/// resolves the handle, as it does for `StitchedCanvas`). `bottom` says
/// which edge of the screen it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedHud {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub bottom: bool,
}

/// Ticket W20-17: cut the scanlines `start..end` out of a `width`-wide
/// RGBA frame. `None` when the band is empty or falls outside the frame.
/// `bottom` is decided by which half of the frame the band starts in — a
/// HUD sits at an edge (`rf_enhance::hud::detect`'s own rule).
#[must_use]
pub fn hud_band(
    frame: &[u8],
    width: usize,
    height: usize,
    start: u16,
    end: u16,
) -> Option<PinnedHud> {
    let (start, end) = (usize::from(start), usize::from(end).min(height));
    if width == 0 || start >= end || frame.len() != width * height * 4 {
        return None;
    }
    Some(PinnedHud {
        rgba: frame[start * width * 4..end * width * 4].to_vec(),
        width: u32::try_from(width).ok()?,
        height: u32::try_from(end - start).ok()?,
        bottom: start * 2 >= height,
    })
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
/// `hud` (ticket W20-17), when given, becomes a `SceneLayer::HudPinned`
/// over the canvas: centred, at its edge, shrunk by the same FM-13
/// divisor as the world so it keeps its size relative to it. The stitcher
/// already leaves HUD rows out of the canvas (`stitch_frame` skips
/// `is_hud` bands), so without this the ultrawide view has no HUD at all.
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
    hud: Option<&PinnedHud>,
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

    let (mut scene_graph, fog) = ultrawide_scene_over_canvas(
        LIVE_CANVAS_ID,
        canvas,
        center_world,
        target_w,
        target_h,
        FogStyle::Themed {
            palette_index: FOG_PALETTE_INDEX,
        },
    );

    // Ticket W20-17: the HUD band, at the divisor's scale, centred on its
    // edge of the target.
    let hud_scaled = hud.and_then(|h| {
        let (w, hh) = ((h.width / divisor).max(1), (h.height / divisor).max(1));
        let rgba = resample_nearest_rgba(&h.rgba, h.width, h.height, w, hh);
        (!rgba.is_empty() && w <= target_w && hh <= target_h).then_some((rgba, w, hh, h.bottom))
    });
    if let Some((_, w, hh, bottom)) = &hud_scaled {
        scene_graph.layers.push(SceneLayer::HudPinned {
            region: HudRegion {
                x: u16::try_from((target_w - w) / 2).unwrap_or(u16::MAX),
                y: 0,
                width: u16::try_from(*w).unwrap_or(u16::MAX),
                height: u16::try_from(*hh).unwrap_or(u16::MAX),
            },
            anchor: if *bottom {
                Anchor::BottomLeft
            } else {
                Anchor::TopLeft
            },
        });
    }

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

    // Handle resolution (module doc): the real producers are
    // `StitchedCanvas` (`rf_enhance::scene_graph` module doc) and, since
    // W20-17, `HudPinned` -- anything else here would be this crate faking
    // a producer the brief forbids. Each entry: pixels, size, position.
    let mut layer_buffers: Vec<(Vec<u8>, u32, u32, i32, i32)> =
        Vec::with_capacity(scene_graph.layers.len());
    for layer in &scene_graph.layers {
        match layer {
            SceneLayer::StitchedCanvas {
                canvas: id,
                fog: style,
            } => {
                debug_assert_eq!(*id, LIVE_CANVAS_ID);
                layer_buffers.push((
                    render_canvas_window_rgba(
                        canvas,
                        &fog,
                        center_world,
                        layer_w,
                        layer_h,
                        *style,
                        divisor,
                    ),
                    layer_w,
                    layer_h,
                    0,
                    0,
                ));
            }
            SceneLayer::HudPinned { region, anchor } => {
                let Some((rgba, w, h, _)) = &hud_scaled else {
                    unreachable!("HudPinned is pushed only with its pixels above");
                };
                let y = match anchor {
                    Anchor::TopLeft | Anchor::TopRight => i64::from(region.y),
                    Anchor::BottomLeft | Anchor::BottomRight => {
                        i64::from(target_h) - i64::from(*h) - i64::from(region.y)
                    }
                };
                layer_buffers.push((
                    rgba.clone(),
                    *w,
                    *h,
                    i32::from(region.x),
                    i32::try_from(y).unwrap_or(0),
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
        .map(|(rgba, width, height, dst_x, dst_y)| CompositeLayer {
            rgba,
            width: *width,
            height: *height,
            dst_x: *dst_x,
            dst_y: *dst_y,
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
    /// Show the composited Diorama view (ticket W16-13) — only ever
    /// returned for `CameraToggle::Original` (Diorama is not a camera
    /// mode of its own; it replaces the flat play view the same way
    /// `atmosphere_fog`/`full_level_view` are settings-driven overlays,
    /// not entries in [`CameraToggle`]) and only when not peeking.
    Diorama {
        rgba: &'a [u8],
        width: u32,
        height: u32,
    },
}

/// Pure decision function for which view to paint (mutation-verify target
/// 1, ticket brief: "force the camera to Original regardless of toggle ->
/// the 'output differs' test must fail"). Kept free of `egui` so it is
/// headlessly testable — see `tests::toggling_to_ultrawide_actually_shows
/// _different_content_than_original` below, which is exactly the "assert
/// the rendered output differs" proof the brief's vacuity trap (a) demands.
///
/// `diorama` is `Some` exactly when the caller considers Diorama
/// currently effective (Game-Aware, a profile with collision, the toggle
/// on — `crate::enhance_ui::feature_rows`'s own gating) AND a render has
/// actually been produced this session; `None` covers every "not
/// effective right now" case, including "never composed" and "the last
/// compose failed" alike, folded together deliberately — unlike Ultrawide
/// (a deliberate camera CHOICE the user made, worth a distinct "why not"
/// message), a Diorama that cannot render for any reason degrades
/// silently to the flat view it would otherwise replace, never blocking
/// play (acceptance criterion 1's "disabling returns to the flat enhanced
/// view the same frame" applies just as much to "temporarily unavailable"
/// as to "turned off").
///
/// `peeking` is hold-to-peek (acceptance criterion 2): checked FIRST and
/// unconditionally forces [`ActiveView::Original`], ahead of both camera
/// state and Diorama, so a mutation that let Diorama or Ultrawide leak
/// through a held peek fails a test here rather than shipping (see
/// `tests::holding_peek_forces_original_even_with_both_ultrawide_and_
/// diorama_ready` below).
#[must_use]
pub fn select_active_view<'a>(
    toggle: CameraToggle,
    ultrawide: Option<&'a Result<UltrawideRender, String>>,
    diorama: Option<&'a DioramaRender>,
    peeking: bool,
) -> ActiveView<'a> {
    if peeking {
        return ActiveView::Original;
    }
    match toggle {
        CameraToggle::Original => match diorama {
            Some(render) => ActiveView::Diorama {
                rgba: &render.rgba,
                width: render.width,
                height: render.height,
            },
            None => ActiveView::Original,
        },
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

/// Ticket W20-17: resize an RGBA buffer nearest-neighbour. The fog's
/// density map is stretched to the picture it is drawn over (an HD pack
/// composites at a multiple of the core's frame, and
/// `rf_renderer::fog::FogPass::render` needs both inputs the same size);
/// a pinned HUD band is shrunk by the ultrawide view's FM-13 divisor.
/// Each output pixel samples the source pixel it falls inside, so an
/// integer multiple is an exact block upscale. Empty when any size is
/// zero or `rgba` is not `src_w * src_h * 4` bytes.
#[must_use]
pub fn resample_nearest_rgba(
    rgba: &[u8],
    src_w: u32,
    src_h: u32,
    out_w: u32,
    out_h: u32,
) -> Vec<u8> {
    let (sw, sh, ow, oh) = (
        src_w as usize,
        src_h as usize,
        out_w as usize,
        out_h as usize,
    );
    if sw == 0 || sh == 0 || ow == 0 || oh == 0 || rgba.len() != sw * sh * 4 {
        return Vec::new();
    }
    if (sw, sh) == (ow, oh) {
        return rgba.to_vec();
    }
    let mut out = Vec::with_capacity(ow * oh * 4);
    for y in 0..oh {
        let row = (y * sh / oh) * sw;
        for x in 0..ow {
            let i = (row + x * sw / ow) * 4;
            out.extend_from_slice(&rgba[i..i + 4]);
        }
    }
    out
}

// ---------------------------------------------------------------------
// Diorama pass (ticket W16-06; `docs/design/ENHANCEMENT_WAVE_16.md` §5):
// the SceneGraph::Geometry -> rf_renderer::diorama_mesh resolution step,
// same mediator role this module already plays for the ultrawide canvas
// and the fog pass (module doc's opening line).
// ---------------------------------------------------------------------

/// NES sprites are always 8px wide regardless of PPUCTRL bit 5
/// (nesdev.org/wiki/PPU_OAM) — only the height varies, 8 or 16.
const SPRITE_WIDTH_PX: f32 = 8.0;

/// The result of [`compose_diorama`]: straight-alpha RGBA plus the size it
/// was rendered at — same shape [`UltrawideRender`] already uses for the
/// other GPU-composited view this module mediates.
pub struct DioramaRender {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// One sprite's world-space billboard footprint (ticket W16-13: honours
/// PPUCTRL bit 5 via `sprite_height_px`, replacing W16-06's fixed 8x8 —
/// [`SPRITE_WIDTH_PX`]'s own doc explains why only the height varies). A
/// pure function so the 8x16 behaviour is unit-testable with no GPU and no
/// `LevelSession` at all (`tests::` below).
#[must_use]
fn sprite_billboard(
    world_x: i32,
    world_y: i32,
    camera_x: i64,
    camera_y: i64,
    viewport: (u16, u16),
    sprite_height_px: u8,
) -> rf_renderer::diorama_mesh::Billboard {
    let height = f32::from(sprite_height_px.max(1));
    let screen_x = (world_x - i32::try_from(camera_x).unwrap_or(0)) as f32;
    let screen_y = (world_y - i32::try_from(camera_y).unwrap_or(0)) as f32;
    let u0 = screen_x / f32::from(viewport.0);
    let v0 = screen_y / f32::from(viewport.1);
    let u1 = (screen_x + SPRITE_WIDTH_PX) / f32::from(viewport.0);
    let v1 = (screen_y + height) / f32::from(viewport.1);
    rf_renderer::diorama_mesh::Billboard {
        center_x_px: world_x as f32 + SPRITE_WIDTH_PX / 2.0,
        center_z_px: world_y as f32 + height / 2.0,
        width_px: SPRITE_WIDTH_PX,
        height_px: height,
        uv: [u0, v0, u1, v1],
    }
}

/// Build one Diorama frame for `session`'s decoded level (module doc).
///
/// `ground_rgba`/`ground_w`/`ground_h` is the decoded level's rendered
/// ground texture (`render_level_rgba`) — taken as an already-rendered
/// buffer, not `chr`/`table`/`palette` to re-decode every call, because
/// `level_view.rs`'s own "decoded once, not per frame" rule applies just
/// as much to this CPU rasterization as it does to the level decode
/// itself: the caller (`crate::app`) renders it once per ROM and passes
/// the same buffer every frame (ticket W16-13).
///
/// `sprite_rgba`/`sprite_w`/`sprite_h` is the already-extracted
/// sprite-only layer for the CURRENT live frame, screen-space
/// (`rf_renderer::layers::LayeredFrame::sprite_rgba`, forwarded through
/// `core_thread::FrameMsg::sprite_rgba` — this crate's own existing layer-
/// extraction pipeline, ticket W3-03, not new plumbing for this ticket).
/// Each billboard's UV rect is a sub-rectangle of this SAME texture at the
/// sprite's own on-screen position — simplest-correct source per this
/// ticket's brief ("OAM + sprite pixels", picked over building a second,
/// per-sprite atlas). `sprite_height_px` is `FrameMsg::sprite_height_px`
/// (PPUCTRL bit 5) — see [`sprite_billboard`].
///
/// # Errors
/// - `"no diorama geometry for this profile"` when [`crate::level_view::
///   LevelSession::diorama_geometry`] returns `None` (no collision table,
///   or a declared `bits` with no `"solid"` entry) — checked by the
///   caller via `has_collision()` before ever reaching this function, so
///   in practice this is a caller-consistency assertion, not a runtime
///   surprise.
/// - Otherwise propagates [`rf_renderer::diorama::DioramaPass::render`]'s
///   `Err` (GPU readback timeout).
#[allow(clippy::too_many_arguments)]
pub fn compose_diorama(
    gpu: &GpuContext,
    pass: &rf_renderer::diorama::DioramaPass,
    session: &crate::level_view::LevelSession,
    ground_rgba: &[u8],
    ground_w: u32,
    ground_h: u32,
    read: &dyn Fn(u32) -> u8,
    entity_table: &[u8],
    sprite_rgba: &[u8],
    sprite_w: u32,
    sprite_h: u32,
    sprite_height_px: u8,
    out_width: u32,
    out_height: u32,
) -> Result<DioramaRender, String> {
    let geometry_layer = session
        .diorama_geometry()
        .ok_or_else(|| "no diorama geometry for this profile".to_string())?;
    let rf_enhance::scene_graph::SceneLayer::Geometry {
        tiles_w,
        tiles_h,
        tile_px,
        solid,
        depth,
        ..
    } = geometry_layer
    else {
        return Err("diorama_geometry did not return SceneLayer::Geometry".to_string());
    };

    let camera = rf_enhance::level_view::live_camera(&session.profile, read);
    let sprites = rf_enhance::level_view::sprites_in_world(&session.profile, entity_table, camera);
    let viewport = rf_enhance::level_view::ORIGINAL_VIEWPORT;
    let billboards: Vec<rf_renderer::diorama_mesh::Billboard> = sprites
        .iter()
        .map(|s| sprite_billboard(s.x, s.y, camera.x, camera.y, viewport, sprite_height_px))
        .collect();

    let scene = rf_renderer::diorama_mesh::DioramaScene {
        tiles_w,
        tiles_h,
        tile_px: tile_px as f32,
        solid: &solid,
        depth: &depth,
        billboards: &billboards,
    };
    let vertices = rf_renderer::diorama_mesh::build_vertices(&scene);

    let rgba = pass.render(
        gpu,
        &vertices,
        ground_rgba,
        ground_w,
        ground_h,
        sprite_rgba,
        sprite_w,
        sprite_h,
        tiles_w,
        tiles_h,
        tile_px,
        out_width,
        out_height,
    )?;
    Ok(DioramaRender {
        rgba,
        width: out_width,
        height: out_height,
    })
}

// ---------------------------------------------------------------------
// Mode 7 ground pass (ticket W16-14; `docs/design/ENHANCEMENT_WAVE_16.md`
// §9): the app-shell resolution step for `rf_renderer::mode7_plane`'s
// frame-aware entry point, same mediator role this module already plays
// for the walls-tier Diorama pass above and the fog/ultrawide passes —
// `rf-renderer` may not depend on `rf-snes`
// (`scripts/validate-arch.sh` rule 3), so turning a raw SNES OAM snapshot
// into billboards happens here, not there.
// ---------------------------------------------------------------------

/// One SNES sprite's screen-space billboard footprint for the Mode 7
/// ground (ticket W16-14 acceptance: "sprites stay billboards on that
/// ground at their OAM footprint").
///
/// **Screen space, not world space — deliberately, and unlike
/// [`sprite_billboard`] above.** A Mode 7 game's OBJs are ordinary 2D
/// sprites the hardware composites ON TOP of the projected background
/// (fullsnes: OBJ is never itself affine-transformed); they have no
/// "world" position of their own to translate a scrolled camera against,
/// the way a tile-based NES/SNES level's OAM does. So this places each
/// billboard's centre proportionally within the SAME `extent` the ground
/// quad already occupies (`rf_renderer::mode7_plane::ground_extent_px`) —
/// `screen_x / screen_w * extent`, `screen_y / screen_h * extent` — which
/// keeps every sprite visibly ON the quad, ordered top-to-bottom exactly
/// as the screen shows them, without claiming a frame-accurate
/// re-derivation of where Mode 7's own projection would actually put that
/// pixel in playfield space (`rf_renderer::mode7_plane`'s own module doc
/// already states that same limitation for the ground quad itself).
///
/// `screen_w`/`screen_h` are the sprite layer's OWN pixel dimensions
/// (`sprite_w`/`sprite_h` below — the buffer the UV rect indexes into),
/// deliberately NOT [`rf_renderer::mode7_plane::SNES_NATIVE_WIDTH_PX`]:
/// that constant is the hardware-scale reference `ground_extent_px` uses,
/// a different unit from "how many pixels wide is this frame's sprite
/// texture" (they usually agree for an ordinary SNES frame, but a
/// widescreen-decoded or hi-res frame's `sprite_rgba` is NOT 256 px wide,
/// and using the wrong denominator here would misalign the UV rect
/// against the texture it actually samples).
///
/// **Sprite size**: [`rf_snes::debug::obj_sizes`]'s SMALL size for every
/// sprite — the OAM high table's per-sprite "large" bit is not consulted
/// (a future refinement, not a correctness bug this ticket's acceptance
/// depends on: the sprite still renders at its own screen position, only
/// a "large"-selected sprite's footprint may read a touch small).
#[must_use]
fn mode7_sprite_billboard(
    sprite: &rf_snes::debug::SpriteEntry,
    obj_size_select: u8,
    screen_w: f32,
    screen_h: f32,
    extent: f32,
) -> rf_renderer::diorama_mesh::Billboard {
    let (small, _) = rf_snes::debug::obj_sizes(obj_size_select);
    let (w, h) = (f32::from(small.0), f32::from(small.1));
    let screen_x = f32::from(sprite.x);
    let screen_y = f32::from(sprite.y);
    let u0 = screen_x / screen_w.max(1.0);
    let v0 = screen_y / screen_h.max(1.0);
    let u1 = (screen_x + w) / screen_w.max(1.0);
    let v1 = (screen_y + h) / screen_h.max(1.0);
    rf_renderer::diorama_mesh::Billboard {
        center_x_px: u0.clamp(0.0, 1.0) * extent,
        center_z_px: v0.clamp(0.0, 1.0) * extent,
        width_px: w,
        height_px: h,
        uv: [u0, v0, u1, v1],
    }
}

/// Build one Mode 7 ground frame (ticket W16-14; module doc). Unlike
/// [`compose_diorama`], this needs no [`crate::level_view::LevelSession`]
/// or collision profile at all: the ground comes straight from the live
/// VRAM/CGRAM snapshot (`rf_snes::debug::render_mode7_plane_rgba`, the
/// caller's job per that function's own doc) and the camera from the
/// frame's own [`rf_core_api::Mode7Registers`] top/bottom pair
/// (`rf_renderer::mode7_plane::derive_pitch_deg`).
///
/// `sprites`/`obj_size_select` are [`rf_snes::debug::decode_oam`]'s output
/// and `$2101`'s size-select bits — the caller's live snapshot, same
/// "already-extracted, not re-decoded here" shape [`compose_diorama`]
/// takes for its own sprite layer.
///
/// # Errors
/// Propagates [`rf_renderer::mode7_plane::render_mode7_ground_frame`]'s
/// `Err` (GPU readback timeout).
#[allow(clippy::too_many_arguments)]
pub fn compose_mode7_ground(
    gpu: &GpuContext,
    pass: &rf_renderer::diorama::DioramaPass,
    top: &rf_core_api::Mode7Registers,
    bottom: &rf_core_api::Mode7Registers,
    ground_rgba: &[u8],
    ground_w: u32,
    ground_h: u32,
    sprites: &[rf_snes::debug::SpriteEntry],
    obj_size_select: u8,
    native_screen_width_px: f32,
    sprite_rgba: &[u8],
    sprite_w: u32,
    sprite_h: u32,
    out_width: u32,
    out_height: u32,
) -> Result<DioramaRender, String> {
    let extent = rf_renderer::mode7_plane::ground_extent_px(top, native_screen_width_px).max(
        rf_renderer::mode7_plane::ground_extent_px(bottom, native_screen_width_px),
    );
    let billboards: Vec<rf_renderer::diorama_mesh::Billboard> = sprites
        .iter()
        .map(|s| {
            mode7_sprite_billboard(s, obj_size_select, sprite_w as f32, sprite_h as f32, extent)
        })
        .collect();

    let rgba = rf_renderer::mode7_plane::render_mode7_ground_frame(
        pass,
        gpu,
        top,
        bottom,
        ground_rgba,
        ground_w,
        ground_h,
        native_screen_width_px,
        &billboards,
        sprite_rgba,
        sprite_w,
        sprite_h,
        out_width,
        out_height,
    )?;
    Ok(DioramaRender {
        rgba,
        width: out_width,
        height: out_height,
    })
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
        let view = select_active_view(CameraToggle::Original, Some(&render), None, false);
        assert!(matches!(view, ActiveView::Original));
    }

    #[test]
    fn ultrawide_toggle_with_no_snapshot_yet_reports_unavailable_not_a_panic() {
        let view = select_active_view(CameraToggle::Ultrawide, None, None, false);
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
        let view = select_active_view(CameraToggle::Ultrawide, Some(&render), None, false);
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

    // --- select_active_view: Diorama (ticket W16-13) ---------------------

    #[test]
    fn original_toggle_with_no_diorama_render_shows_original() {
        let view = select_active_view(CameraToggle::Original, None, None, false);
        assert!(matches!(view, ActiveView::Original));
    }

    /// Same vacuity-trap shape as the Ultrawide test above: Diorama's
    /// content must provably differ from Original, not merely be "some
    /// other variant".
    #[test]
    fn original_toggle_with_a_ready_diorama_render_shows_diorama_content_not_original() {
        let render = DioramaRender {
            rgba: vec![7, 8, 9, 255],
            width: 1,
            height: 1,
        };
        let view = select_active_view(CameraToggle::Original, None, Some(&render), false);
        match view {
            ActiveView::Diorama {
                rgba,
                width,
                height,
            } => {
                assert_eq!(rgba, &[7, 8, 9, 255]);
                assert_eq!((width, height), (1, 1));
            }
            _ => panic!("expected ActiveView::Diorama"),
        }
    }

    /// Acceptance criterion 2: hold-to-peek shows the original frame —
    /// forced ahead of BOTH Ultrawide and Diorama, whichever is nominally
    /// selected/ready. A mutation that checked `peeking` only inside the
    /// `Original` arm (letting a peeked Ultrawide toggle through) fails
    /// here.
    #[test]
    fn holding_peek_forces_original_even_with_both_ultrawide_and_diorama_ready() {
        let ultrawide: Result<UltrawideRender, String> = Ok(UltrawideRender {
            rgba: vec![1, 1, 1, 255],
            width: 1,
            height: 1,
            reduction: None,
        });
        let diorama = DioramaRender {
            rgba: vec![2, 2, 2, 255],
            width: 1,
            height: 1,
        };
        for toggle in [CameraToggle::Original, CameraToggle::Ultrawide] {
            let view = select_active_view(toggle, Some(&ultrawide), Some(&diorama), true);
            assert!(
                matches!(view, ActiveView::Original),
                "peeking must force Original regardless of toggle {toggle:?}"
            );
        }
    }

    /// Toggling to Ultrawide takes precedence over an also-ready Diorama
    /// render — Diorama only ever shows for `CameraToggle::Original`
    /// (`ActiveView::Diorama`'s own doc).
    #[test]
    fn ultrawide_toggle_wins_over_a_ready_diorama_render() {
        let ultrawide: Result<UltrawideRender, String> = Ok(UltrawideRender {
            rgba: vec![1, 1, 1, 255],
            width: 1,
            height: 1,
            reduction: None,
        });
        let diorama = DioramaRender {
            rgba: vec![2, 2, 2, 255],
            width: 1,
            height: 1,
        };
        let view = select_active_view(
            CameraToggle::Ultrawide,
            Some(&ultrawide),
            Some(&diorama),
            false,
        );
        assert!(matches!(view, ActiveView::Ultrawide { .. }));
    }

    // --- sprite_billboard: 8x16 footprint (ticket W16-13) ----------------

    #[test]
    fn sprite_billboard_is_8x8_by_default() {
        let b = sprite_billboard(10, 20, 0, 0, (256, 240), 8);
        assert_eq!(b.width_px, 8.0);
        assert_eq!(b.height_px, 8.0);
        assert_eq!(b.center_x_px, 14.0);
        assert_eq!(b.center_z_px, 24.0);
        assert_eq!(
            b.uv,
            [10.0 / 256.0, 20.0 / 240.0, 18.0 / 256.0, 28.0 / 240.0]
        );
    }

    /// The actual acceptance criterion: an 8x16-mode sprite (PPUCTRL bit 5
    /// set) gets a footprint TWICE as tall, width unchanged — a mutation
    /// that ignored `sprite_height_px` entirely (always emitting 8x8)
    /// would still pass every other test in this module but fails here.
    #[test]
    fn sprite_billboard_honours_8x16_mode() {
        let b8 = sprite_billboard(10, 20, 0, 0, (256, 240), 8);
        let b16 = sprite_billboard(10, 20, 0, 0, (256, 240), 16);
        assert_eq!(
            b16.width_px, b8.width_px,
            "width never depends on sprite height mode"
        );
        assert_eq!(b16.height_px, 16.0);
        assert_ne!(b16.height_px, b8.height_px);
        assert_ne!(
            b16.center_z_px, b8.center_z_px,
            "a taller footprint anchors its centre differently on the Z (screen-Y) axis"
        );
        assert_ne!(
            b16.uv, b8.uv,
            "the taller footprint's UV rect must cover more of the texture"
        );
    }

    #[test]
    fn sprite_billboard_subtracts_the_camera_before_building_the_uv_rect() {
        let at_origin = sprite_billboard(100, 100, 0, 0, (256, 240), 8);
        let with_camera = sprite_billboard(100, 100, 50, 50, (256, 240), 8);
        assert_ne!(
            at_origin.uv, with_camera.uv,
            "the camera offset must actually reach the UV rect, not just the world-space centre"
        );
    }

    // --- compose_ultrawide: empty canvas is an error, not a panic -------

    #[test]
    fn composing_an_empty_canvas_is_a_clean_error() {
        let Some(gpu) = gpu_or_skip("composing_an_empty_canvas_is_a_clean_error") else {
            return;
        };
        let compositor = EnhancedCompositor::new(&gpu);
        let canvas = Canvas::new();
        let result = compose_ultrawide(&gpu, &compositor, &canvas, 8192, None);
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

        let render = compose_ultrawide(&gpu, &compositor, &canvas, 8192, None)
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

        let render = compose_ultrawide(&gpu, &compositor, &canvas, stale_policy_max_dim, None)
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

    // --- HUD pinned over the ultrawide view (ticket W20-17) ------------

    #[test]
    fn hud_band_cuts_the_declared_rows_and_knows_its_edge() {
        // 2x4 frame; row y is filled with the value y.
        let frame: Vec<u8> = (0..4u8).flat_map(|y| [y; 8]).collect();
        let top = hud_band(&frame, 2, 4, 0, 1).expect("row 0");
        assert_eq!((top.width, top.height, top.bottom), (2, 1, false));
        assert_eq!(top.rgba, vec![0; 8]);
        let bottom = hud_band(&frame, 2, 4, 3, 9).expect("clamped to the frame");
        assert_eq!((bottom.height, bottom.bottom), (1, true));
        assert_eq!(bottom.rgba, vec![3; 8]);
        assert_eq!(hud_band(&frame, 2, 4, 2, 2), None, "empty band");
        assert_eq!(hud_band(&frame, 2, 4, 5, 6), None, "outside the frame");
        assert_eq!(hud_band(&frame[..8], 2, 4, 0, 1), None, "wrong length");
    }

    /// Mutation target: drop the `HudPinned` layer and the HUD colour
    /// vanishes from the composite.
    #[test]
    fn a_pinned_hud_lands_centred_on_its_edge_over_the_canvas() {
        let Some(gpu) = gpu_or_skip("a_pinned_hud_lands_centred_on_its_edge_over_the_canvas")
        else {
            return;
        };
        let compositor = EnhancedCompositor::new(&gpu);
        let mut canvas = Canvas::new();
        for y in 0..4 {
            canvas.blit_row(0, y, &[Some(bg(0)); 8]);
        }
        let red = [250, 10, 10, 255];
        let hud = |bottom| PinnedHud {
            rgba: red.repeat(4),
            width: 4,
            height: 1,
            bottom,
        };
        let world = palette_index_to_rgb(0);
        for bottom in [false, true] {
            let render = compose_ultrawide(&gpu, &compositor, &canvas, 8192, Some(&hud(bottom)))
                .expect("compose");
            assert_eq!((render.width, render.height), (8, 4));
            let px = |x: usize, y: usize| {
                let i = (y * 8 + x) * 4;
                [render.rgba[i], render.rgba[i + 1], render.rgba[i + 2]]
            };
            let row = if bottom { 3 } else { 0 };
            for x in 0..8 {
                let want = if (2..6).contains(&x) {
                    [250, 10, 10]
                } else {
                    world
                };
                assert_eq!(px(x, row), want, "bottom={bottom} x={x}");
            }
            let other = if bottom { 0 } else { 3 };
            assert_eq!(px(3, other), world, "the far edge stays world");
        }
    }

    // --- Fog/steam pass conversions (ticket W16-04) ---------------------

    #[test]
    fn resample_nearest_rgba_is_a_block_upscale_and_refuses_bad_sizes() {
        // 2x1 source: a dark pixel then a bright one.
        let src = [10, 10, 10, 255, 200, 200, 200, 255];
        assert_eq!(resample_nearest_rgba(&src, 2, 1, 2, 1), src.to_vec());
        let up = resample_nearest_rgba(&src, 2, 1, 4, 2);
        assert_eq!(up.len(), 4 * 2 * 4);
        let reds: Vec<u8> = up.chunks_exact(4).map(|p| p[0]).collect();
        assert_eq!(reds, [10, 10, 200, 200, 10, 10, 200, 200]);
        assert!(
            resample_nearest_rgba(&src, 3, 1, 4, 2).is_empty(),
            "wrong length"
        );
        assert!(
            resample_nearest_rgba(&src, 2, 1, 0, 2).is_empty(),
            "zero size"
        );
    }

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
