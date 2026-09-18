//! Atmosphere-layer detector (ticket W16-03; `docs/design/ENHANCEMENT_WAVE_16.md`
//! §4; D-004/`crate::trust`'s heuristic trust ladder).
//!
//! ## What this is, in one sentence
//!
//! A SNES background plane is flagged an *atmosphere candidate* (fog,
//! mist, heat haze) when, over a window of frames, it (1) carries
//! half/additive colour math on a large share of its own on-screen
//! pixels, (2) scrolls slowly and independently of the dominant
//! background plane, and (3) is built from very few distinct 8x8 tile
//! patterns — three independent, cheap-to-compute signals a hand-authored
//! fog layer tends to share, none of which alone would be safe to trigger
//! on (a HUD is also slow and low-variety; a normal parallax layer can
//! also use colour math for a translucency effect that is not fog).
//!
//! ## Shadow rung, by construction (D-004, `ENHANCEMENT_RUNTIME.md` §2a)
//!
//! [`AtmosphereDetector`] has exactly one thing it can do: [`observe`]
//! computes and records to [`AtmosphereDetector::report_card`]. There is
//! no `set_enabled`/`should_act` path and no method that changes what any
//! frame renders — the same structural argument `crate::sprite_historian`
//! makes for its own mode-invariant proof applies here even more simply:
//! this type has no *action* to gate in the first place, so "detect and
//! record, never act" is a fact about the type, not a promise about how a
//! caller uses it. The one place this module touches rendering is
//! [`observe`]'s return value — freshly-triggered [`SceneLayer::ExtractedBg`]
//! values, produced for the report card / a future Enhance-workspace
//! preview only (`ENHANCEMENT_WAVE_16.md` §4's "still shadow" ruling on
//! this exact producer) — nothing in this crate or its dependents composes
//! that layer into a rendered frame yet.
//!
//! `HEURISTIC_ID` is this heuristic's `crate::trust::TrustLadder` key, for
//! whichever caller eventually wires per-game trust state the way
//! `crate::experiments` documents doing for its own heuristics; this
//! ticket does not need to call the ladder itself because shadow is its
//! only rung.
//!
//! ## Colour math signal (`ColorMathOp`, hardware-verified)
//!
//! `rf_core_api::ColorMathOp::Add`/`AddHalf` is the SNES hardware's own
//! additive/half-additive blend
//! (<https://snes.nesdev.org/wiki/Color_math>: "Half color math... Add
//! (clamp)... Add (half)"), computed by the renderer from a
//! [`rf_core_api::SubPixel::op`] the core decided applies at each x
//! (`video.rs`'s own doc). This module never re-derives colour math from
//! `$2130`-`$2132`-style register state (it has none — it reads frames,
//! never core state, CLAUDE.md law 4); it reads the already-resolved
//! `op` the core already computed, exactly like every other consumer of
//! that channel.
//!
//! ## Independence from the main plane
//!
//! "The main plane" is not a fixed index (`PixelLayer::Background(n)`'s
//! own doc: SNES BG mode decides which of 0-3 exist and what they mean) —
//! this module treats whichever background plane has the greatest average
//! on-screen pixel coverage over the window as the dominant/"main" plane
//! for that window, and compares every OTHER present plane's scroll speed
//! against it. A plane cannot be its own "main plane" comparison, so the
//! dominant plane itself is never a candidate.
//!
//! ## What this module deliberately does NOT catch (design corrections)
//!
//! `ENHANCEMENT_WAVE_16.md` §4's two corrections are binding: Super
//! Metroid's Norfair heat is palette cycling (no colour math plane at
//! all) and Castlevania IV's rotating room is Mode 7 geometry (no
//! background-plane colour math either) — this detector's colour-math
//! gate (§ below) means BOTH fail to trigger by construction, proven by
//! `crates/rf-harness/tests/atmosphere_layer_red_fixture.rs`'s
//! `palette_cycling_scene_does_not_trigger` rather than asserted in prose
//! alone.
//!
//! ## Thresholds are calibrated against this ticket's own fixture, not a
//! verified commercial game
//!
//! `ENHANCEMENT_WAVE_16.md` §4 says plainly that A Link to the Past's exact
//! fog registers were never verified from a technical source, and that
//! shipping shadow-first (rather than hand-tuning per game) is the point.
//! Every constant below is documented with the reasoning behind its
//! specific value; none of them claims to reproduce a measured commercial
//! title.
use std::collections::BTreeSet;
use std::collections::VecDeque;

use rf_core_api::{ColorMathOp, CoreEvent, PixelLayer, PpuPixel, SubPixel};

use crate::scene_graph::{BgLayerId, SceneLayer};

/// This heuristic's `crate::trust::TrustLadder` key (module doc).
pub const HEURISTIC_ID: &str = "atmosphere-layer";

/// SNES BG mode 0's four simultaneous background planes are the most any
/// mode exposes (`PixelLayer::Background`'s own doc: "SNES: 0-3 depending
/// on the active BG mode") — sized for the per-layer accumulator arrays,
/// not a design choice of its own.
pub const MAX_BG_LAYERS: u8 = 4;

/// How many frames the sliding window covers before a verdict is
/// possible. Half a second at 60 fps: long enough that one or two
/// coincidental frames (a sprite briefly occluding the plane, a single
/// stray colour-math toggle) cannot swing the averages, short enough that
/// a scene the player has been standing in for under a second still gets
/// a verdict rather than never evaluating small/short scenes at all.
pub const WINDOW_FRAMES: usize = 30;

/// Minimum share of a plane's own on-screen pixels that must carry
/// [`ColorMathOp::Add`]/[`ColorMathOp::AddHalf`] over the window, averaged
/// across the window's total pixel counts (not per-frame, so one
/// low-coverage frame cannot mask an otherwise-solid signal). Acceptance
/// criterion 1's own wording is "a large share" — one half is the plain
/// reading of "large share" for a first shadow-only cut, not a measured
/// commercial number (module doc's calibration note).
pub const MIN_COLOR_MATH_SHARE: f32 = 0.5;

/// Ceiling on a candidate plane's own average per-frame scroll speed (the
/// L1 distance `|dx| + |dy|` between consecutive frames' raw scroll
/// writes, in pixels) for it to count as "scrolls slowly". Chosen so a
/// plane drifting less than a couple of pixels a frame — visually a slow
/// parallax layer, not a foreground element keeping pace with the camera —
/// qualifies; a plane scrolling at ordinary camera speed (this ticket's
/// fixture drives its dominant plane at several pixels/frame) does not.
pub const MAX_SLOW_SCROLL_PX_PER_FRAME: f32 = 2.0;

/// Ceiling on the ratio of a candidate plane's average scroll speed to
/// the dominant plane's, for the two to count as scrolling
/// "independently" (candidate meaningfully slower, not just "also slow"
/// in absolute terms when nothing on screen moves fast). Applied only
/// when the dominant plane has nonzero average speed — a candidate
/// evaluated against a session where nothing scrolls at all is judged on
/// [`MAX_SLOW_SCROLL_PX_PER_FRAME`] alone (module doc: independence has no
/// meaning against a plane that never establishes a "fast" baseline).
pub const MAX_SPEED_RATIO_TO_MAIN: f32 = 0.5;

/// Ceiling on the average number of distinct 8x8 tile patterns (by raw
/// palette-index content, not tile-id — this module reads indexed pixels,
/// never ROM/CHR data, CLAUDE.md law 4) a candidate plane is built from
/// per frame, over the window. A handful of repeating cloud/mist/haze
/// tiles is the visual hallmark of a fog layer; ordinary background art
/// (this ticket's fixture drives its dominant plane through a
/// checkerboard of eight distinct tile values) comfortably exceeds this.
pub const MAX_DISTINCT_TILES_PER_FRAME: f32 = 4.0;

/// Per-layer, per-frame raw measurements the window accumulates before
/// any threshold is applied — kept separate from the threshold logic
/// (`AtmosphereDetector::evaluate`) so each half of the heuristic can be
/// read (and tested) independently of the other.
#[derive(Clone, Copy, Default)]
struct LayerFrameStat {
    /// How many MAIN-screen pixels this plane won this frame
    /// ([`main_layer_presence`]) — used only to pick the dominant plane.
    main_pixels: u32,
    /// This plane's own sub-screen presence this frame ([`sub_layer_stats`])
    /// — its candidacy weight; an absent/occluded plane contributes zero
    /// and is excluded from every average.
    opaque_pixels: u32,
    /// Of those, how many carried [`ColorMathOp::Add`]/[`ColorMathOp::AddHalf`].
    math_pixels: u32,
    /// Distinct 8x8 tile-content patterns this plane's sub-screen pixels
    /// formed this frame (see [`sub_layer_tile_variety`]).
    distinct_tiles: u32,
    /// `|dx| + |dy|` versus this plane's previous frame's raw scroll
    /// write, when at least one sample exists yet (module doc: no
    /// wraparound handling — a genuine register wrap would read as a
    /// large jump, which only makes this heuristic MORE conservative,
    /// never falsely positive, the safe direction for a shadow-only
    /// detector).
    scroll_speed: f32,
    has_scroll_sample: bool,
}

/// One shadow-rung verdict recorded to the report card (acceptance
/// criterion 2: "candidate plane, share of math pixels, scroll ratio,
/// variety score").
#[derive(Debug, Clone, PartialEq)]
pub struct AtmosphereCandidate {
    /// Core-defined `PixelLayer::Background` index (module doc).
    pub layer: u8,
    /// Average share of this plane's own pixels carrying half/additive
    /// colour math over the triggering window (`0.0..=1.0`).
    pub color_math_share: f32,
    /// This plane's average scroll speed divided by the dominant plane's
    /// (module doc's "independence" section) — `f32::INFINITY` is never
    /// produced (the trigger condition only fires when the ratio already
    /// cleared [`MAX_SPEED_RATIO_TO_MAIN`] or the dominant plane was
    /// static, in which case this is the candidate's raw speed).
    pub scroll_ratio: f32,
    /// Average distinct-8x8-tile count per frame over the window.
    pub variety_score: f32,
    /// The scene identity this verdict was recorded under (caller-supplied
    /// — this module has no scene-identity opinion of its own, matching
    /// `crate::trust::TrustLadder`'s own "a plain string" stance).
    pub scene: String,
}

/// How many of `video`'s pixels each background layer WON on the main
/// screen this frame (`rf_core_api::PpuPixel::layer`'s own semantics: the
/// already-resolved overlap winner) — used only to pick the *dominant*
/// plane (module doc's "independence" section). Deliberately NOT the
/// source of a candidate's own colour-math/tile-variety signal: a plane
/// driving colour math is typically the SUB-screen contributor at that x
/// (`SubPixel`'s own doc — "a parallel channel, never a replacement"), so
/// it may win zero main-screen pixels while still being exactly the
/// atmosphere layer this heuristic must find. [`sub_layer_stats`] is the
/// candidate-side counterpart that reads the right channel for that.
fn main_layer_presence(video: &[PpuPixel]) -> [u32; MAX_BG_LAYERS as usize] {
    let mut counts = [0u32; MAX_BG_LAYERS as usize];
    for px in video {
        if let PixelLayer::Background(n) = px.layer {
            if n < MAX_BG_LAYERS {
                counts[usize::from(n)] += 1;
            }
        }
    }
    counts
}

/// A candidate plane's own on-screen presence and colour-math coverage,
/// read from the sub-screen channel (module doc on [`main_layer_presence`]
/// explains why: [`SubPixel::layer`]/[`SubPixel::op`] is where a plane's
/// own contribution to colour math lives, per `video.rs`'s own doc on
/// [`ColorMathOp`]). `opaque_pixels` counts every sub-screen pixel this
/// plane contributed regardless of `op` (its raw on-screen presence);
/// `math_pixels` is the subset carrying [`ColorMathOp::Add`]/
/// [`ColorMathOp::AddHalf`] (<https://snes.nesdev.org/wiki/Color_math>).
fn sub_layer_stats(sub: &[SubPixel]) -> [(u32, u32); MAX_BG_LAYERS as usize] {
    let mut stats = [(0u32, 0u32); MAX_BG_LAYERS as usize];
    for s in sub {
        let PixelLayer::Background(n) = s.layer else {
            continue;
        };
        if n >= MAX_BG_LAYERS {
            continue;
        }
        let entry = &mut stats[usize::from(n)];
        entry.0 += 1;
        if matches!(s.op, ColorMathOp::Add | ColorMathOp::AddHalf) {
            entry.1 += 1;
        }
    }
    stats
}

/// Distinct 8x8 tile-content patterns per background layer this frame
/// (module doc's "very few distinct 8x8 tile patterns" signal), read from
/// the same sub-screen channel [`sub_layer_stats`] does (module doc on
/// [`main_layer_presence`]) — the candidate's own indexed-pixel identity,
/// not whatever won the main screen at that x. A tile is identified by
/// the exact sequence of palette indices this plane contributed within
/// its 8x8 cell, in raster order — cells this plane never touches this
/// frame contribute no entry at all, so sparse-but-uniform coverage is
/// not penalized for cells it never drew.
fn sub_layer_tile_variety(
    sub: &[SubPixel],
    width: u16,
    height: u16,
) -> [u32; MAX_BG_LAYERS as usize] {
    let tile_cols = usize::from(width).div_ceil(8);
    let tile_rows = usize::from(height).div_ceil(8);
    let tile_count = tile_cols * tile_rows;
    let mut tiles: Vec<Vec<u8>> = vec![Vec::new(); tile_count * MAX_BG_LAYERS as usize];

    for y in 0..height {
        for x in 0..width {
            let idx = usize::from(y) * usize::from(width) + usize::from(x);
            let Some(s) = sub.get(idx) else { continue };
            let PixelLayer::Background(n) = s.layer else {
                continue;
            };
            if n >= MAX_BG_LAYERS {
                continue;
            }
            let tile_idx = (usize::from(y) / 8) * tile_cols + (usize::from(x) / 8);
            tiles[usize::from(n) * tile_count + tile_idx].push(s.palette_index);
        }
    }

    let mut counts = [0u32; MAX_BG_LAYERS as usize];
    for n in 0..usize::from(MAX_BG_LAYERS) {
        let mut seen: BTreeSet<Vec<u8>> = BTreeSet::new();
        for tile_idx in 0..tile_count {
            let cell = &tiles[n * tile_count + tile_idx];
            if cell.is_empty() {
                continue;
            }
            seen.insert(cell.clone());
        }
        counts[n] = u32::try_from(seen.len()).unwrap_or(u32::MAX);
    }
    counts
}

/// Extract the candidate plane's own pixels (background-layer producer,
/// acceptance criterion 4), built from the sub-screen channel like the
/// rest of this candidate's signal ([`sub_layer_stats`]'s doc) — a
/// synthetic [`PpuPixel`] is constructed at each position this plane
/// contributed as a sub-screen source, using that position's own
/// `palette_index`; every other position is the backdrop default, so the
/// emitted layer carries exactly this plane's own content and nothing
/// else.
fn extracted_bg_layer(
    layer: u8,
    sub: &[SubPixel],
    width: u16,
    height: u16,
    scroll: (i64, i64),
) -> SceneLayer {
    let pixels: Vec<PpuPixel> = sub
        .iter()
        .map(|s| {
            if s.layer == PixelLayer::Background(layer) {
                PpuPixel {
                    palette_index: s.palette_index,
                    layer: PixelLayer::Background(layer),
                    sprite_id: None,
                    priority: 0,
                }
            } else {
                PpuPixel {
                    palette_index: 0,
                    layer: PixelLayer::Backdrop,
                    sprite_id: None,
                    priority: 0,
                }
            }
        })
        .collect();
    SceneLayer::ExtractedBg {
        layer: BgLayerId(layer),
        pixels,
        width,
        height,
        scroll,
    }
}

/// Build the `SceneLayer::ExtractedBg` layer directly for a profile-pinned
/// plane (ticket W16-04, `docs/design/ENHANCEMENT_WAVE_16.md` §4:
/// "a profile may pin the plane directly, skip detection, the same
/// mechanism anti-flicker uses today").
///
/// **Scope note on "the same mechanism anti-flicker uses today".**
/// `crate::trust::TrustLadder::pin` is that mechanism (FR-ENH-011: "profiles
/// may pin states") and is exactly what a caller should combine this
/// function with — `ladder.pin(HEURISTIC_ID, TrustState::Active)` alongside
/// calling this function instead of running [`AtmosphereDetector::observe`].
/// What this function does NOT do is parse a `[atmosphere] plane = n`
/// profile TOML section — `crates/rf-profiles/**` is outside this ticket's
/// `write_scope` (`crates/rf-renderer/**`, `crates/rf-enhance/**`,
/// `crates/retroforge/**`), and `[antiflicker]`
/// (`crates/rf-profiles/src/schema.rs`) itself has no call site that wires
/// it into `TrustLadder::pin` today either — there is no existing profile
/// -> ladder-pin wiring in this codebase to mirror yet. Adding the TOML
/// section and its loader wiring is a follow-up ticket with write access
/// to `rf-profiles`; this function is the ready-to-call producer for it,
/// the same "ready-to-wire" posture `crate::scene_graph::solidity_mask`'s
/// own doc takes for the identical write_scope reason.
#[must_use]
pub fn pinned_layer(
    layer: u8,
    sub: &[SubPixel],
    width: u16,
    height: u16,
    scroll: (i64, i64),
) -> SceneLayer {
    extracted_bg_layer(layer, sub, width, height, scroll)
}

/// Shadow-rung atmosphere-layer heuristic (module doc). Construct one per
/// play session and call [`observe`](AtmosphereDetector::observe) once per
/// frame, in order — same lifetime shape as `crate::sprite_historian::SpriteHistorian`
/// and `crate::scroll_tracker::ScrollTracker`.
pub struct AtmosphereDetector {
    window: VecDeque<[LayerFrameStat; MAX_BG_LAYERS as usize]>,
    last_raw_scroll: [Option<(u16, u16)>; MAX_BG_LAYERS as usize],
    /// (scene, layer) pairs already recorded, so a sliding window that
    /// keeps re-satisfying the trigger every frame does not spam the
    /// report card once per frame forever.
    already_recorded: BTreeSet<(String, u8)>,
    report_card: Vec<AtmosphereCandidate>,
}

impl AtmosphereDetector {
    #[must_use]
    pub fn new() -> Self {
        AtmosphereDetector {
            window: VecDeque::with_capacity(WINDOW_FRAMES),
            last_raw_scroll: [None; MAX_BG_LAYERS as usize],
            already_recorded: BTreeSet::new(),
            report_card: Vec::new(),
        }
    }

    #[must_use]
    pub fn report_card(&self) -> &[AtmosphereCandidate] {
        &self.report_card
    }

    /// Observe one frame's accuracy-exact video + sub-screen buffers
    /// (shared references — this module reads frames, never core state,
    /// CLAUDE.md law 4) plus its events (for per-layer scroll telemetry)
    /// and scene identity. Returns any freshly-triggered candidates' first
    /// [`SceneLayer::ExtractedBg`] layer (module doc: for the report card
    /// / a future preview only, never composited by anything in this
    /// crate).
    #[must_use]
    pub fn observe(
        &mut self,
        video: &[PpuPixel],
        sub: &[SubPixel],
        width: u16,
        height: u16,
        events: &[CoreEvent],
        scene: &str,
    ) -> Vec<SceneLayer> {
        let main_presence = main_layer_presence(video);
        let sub_stats = sub_layer_stats(sub);
        let tiles = sub_layer_tile_variety(sub, width, height);

        let mut frame_stats = [LayerFrameStat::default(); MAX_BG_LAYERS as usize];
        for n in 0..usize::from(MAX_BG_LAYERS) {
            frame_stats[n].main_pixels = main_presence[n];
            frame_stats[n].opaque_pixels = sub_stats[n].0;
            frame_stats[n].math_pixels = sub_stats[n].1;
            frame_stats[n].distinct_tiles = tiles[n];
        }

        // Last ScrollWrite per layer this frame wins (mirrors
        // `crate::scroll_tracker::compute_bands`'s own "carry the final
        // write" rule for a single stream, applied independently per
        // layer here since a SNES frame carries one stream per plane).
        let mut this_frame_raw = self.last_raw_scroll;
        for ev in events {
            if let CoreEvent::ScrollWrite {
                x,
                y,
                layer: PixelLayer::Background(n),
            } = *ev
            {
                if n < MAX_BG_LAYERS {
                    this_frame_raw[usize::from(n)] = Some((x, y));
                }
            }
        }
        for n in 0..usize::from(MAX_BG_LAYERS) {
            if let (Some((px, py)), Some((cx, cy))) = (self.last_raw_scroll[n], this_frame_raw[n]) {
                let dx = f32::from(cx) - f32::from(px);
                let dy = f32::from(cy) - f32::from(py);
                frame_stats[n].scroll_speed = dx.abs() + dy.abs();
                frame_stats[n].has_scroll_sample = true;
            }
        }
        self.last_raw_scroll = this_frame_raw;

        self.window.push_back(frame_stats);
        if self.window.len() > WINDOW_FRAMES {
            self.window.pop_front();
        }

        if self.window.len() < WINDOW_FRAMES {
            return Vec::new();
        }

        let mut new_layers = Vec::new();
        for candidate in self.evaluate(scene) {
            let key = (scene.to_string(), candidate.layer);
            if self.already_recorded.insert(key) {
                let scroll = self
                    .last_raw_scroll
                    .get(usize::from(candidate.layer))
                    .and_then(|s| *s)
                    .map(|(x, y)| (i64::from(x), i64::from(y)))
                    .unwrap_or((0, 0));
                new_layers.push(extracted_bg_layer(
                    candidate.layer,
                    sub,
                    width,
                    height,
                    scroll,
                ));
                self.report_card.push(candidate);
            }
        }
        new_layers
    }

    /// Threshold logic over the current full window (module doc's
    /// constants) — pure with respect to `self.window`/`self.last_raw_scroll`,
    /// no mutation, so it can be re-run every frame the window is full
    /// without side effects of its own (dedup happens in the caller).
    fn evaluate(&self, scene: &str) -> Vec<AtmosphereCandidate> {
        let n_layers = usize::from(MAX_BG_LAYERS);
        let mut avg_main = [0f32; 4];
        let mut avg_opaque = [0f32; 4];
        let mut math_share = [0f32; 4];
        let mut avg_tiles = [0f32; 4];
        let mut avg_speed = [0f32; 4];

        let window_len = self.window.len() as f32;
        for n in 0..n_layers {
            let mut sum_main = 0u64;
            let mut sum_opaque = 0u64;
            let mut sum_math = 0u64;
            let mut sum_tiles = 0u64;
            let mut speed_total = 0f32;
            let mut speed_samples = 0u32;
            for frame in &self.window {
                let s = frame[n];
                sum_main += u64::from(s.main_pixels);
                sum_opaque += u64::from(s.opaque_pixels);
                sum_math += u64::from(s.math_pixels);
                sum_tiles += u64::from(s.distinct_tiles);
                if s.has_scroll_sample {
                    speed_total += s.scroll_speed;
                    speed_samples += 1;
                }
            }
            avg_main[n] = sum_main as f32 / window_len;
            avg_opaque[n] = sum_opaque as f32 / window_len;
            math_share[n] = if sum_opaque > 0 {
                sum_math as f32 / sum_opaque as f32
            } else {
                0.0
            };
            avg_tiles[n] = sum_tiles as f32 / window_len;
            avg_speed[n] = if speed_samples > 0 {
                speed_total / speed_samples as f32
            } else {
                0.0
            };
        }

        // The dominant/"main" plane is picked by MAIN-screen coverage
        // (module doc on `main_layer_presence`) — a candidate is judged by
        // its own sub-screen presence (`avg_opaque`) instead, since an
        // atmosphere plane may win zero main-screen pixels while still
        // driving colour math as a sub-screen source.
        let dominant = (0..n_layers)
            .filter(|&n| avg_main[n] > 0.0)
            .max_by(|&a, &b| avg_main[a].total_cmp(&avg_main[b]));

        let Some(dominant) = dominant else {
            return Vec::new();
        };

        let mut out = Vec::new();
        for n in 0..n_layers {
            if n == dominant || avg_opaque[n] <= 0.0 {
                continue;
            }
            if math_share[n] < MIN_COLOR_MATH_SHARE {
                continue;
            }
            if avg_speed[n] > MAX_SLOW_SCROLL_PX_PER_FRAME {
                continue;
            }
            let ratio = if avg_speed[dominant] > 0.0 {
                avg_speed[n] / avg_speed[dominant]
            } else {
                avg_speed[n]
            };
            if avg_speed[dominant] > 0.0 && ratio > MAX_SPEED_RATIO_TO_MAIN {
                continue;
            }
            if avg_tiles[n] > MAX_DISTINCT_TILES_PER_FRAME {
                continue;
            }
            out.push(AtmosphereCandidate {
                layer: n as u8,
                color_math_share: math_share[n],
                scroll_ratio: ratio,
                variety_score: avg_tiles[n],
                scene: scene.to_string(),
            });
        }
        out
    }
}

impl Default for AtmosphereDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDTH: u16 = 64;
    const HEIGHT: u16 = 32;

    fn bg_px(layer: u8, palette_index: u8) -> PpuPixel {
        PpuPixel {
            palette_index,
            layer: PixelLayer::Background(layer),
            sprite_id: None,
            priority: 0,
        }
    }

    fn none_sub() -> SubPixel {
        SubPixel {
            palette_index: 0,
            layer: PixelLayer::Backdrop,
            op: ColorMathOp::None,
            fixed: false,
        }
    }

    /// A full-screen MAIN-screen frame: layer 0 wins every pixel, laid out
    /// as a checkerboard of 8 distinct per-tile palette values so its own
    /// tile variety is well above [`MAX_DISTINCT_TILES_PER_FRAME`] (the
    /// dominant plane must never itself be mistaken for a candidate). This
    /// is `video` in every test — the candidate plane's own signal lives
    /// entirely in the separately-constructed `sub` buffer
    /// ([`candidate_sub`]), matching real SNES colour math where the
    /// atmosphere layer is typically a sub-screen contributor, not the
    /// main-screen winner (module doc on `main_layer_presence`).
    fn dominant_video() -> Vec<PpuPixel> {
        let mut video = Vec::with_capacity(usize::from(WIDTH) * usize::from(HEIGHT));
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let tile_id = (usize::from(y) / 8) * 100 + usize::from(x) / 8;
                video.push(bg_px(0, (tile_id % 8) as u8));
            }
        }
        video
    }

    /// A full-screen sub-screen buffer for one candidate `layer`:
    /// `period` controls tile variety (period 1 = one repeated palette
    /// value = lowest possible variety; a larger period cycles through
    /// more distinct values, raising it), and `share` is the fraction of
    /// the layer's own pixels that carry `op` (the rest carry
    /// [`ColorMathOp::None`] but still count as this plane's own
    /// presence, exactly like a plane that colour-maths only part of its
    /// own coverage).
    fn candidate_sub(
        layer: u8,
        base_palette: u8,
        period: usize,
        op: ColorMathOp,
        share: f32,
    ) -> Vec<SubPixel> {
        let len = usize::from(WIDTH) * usize::from(HEIGHT);
        let target_count = (len as f32 * share) as usize;
        let mut sub = Vec::with_capacity(len);
        let mut applied = 0usize;
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let tile_id = (usize::from(y) / 8) * 100 + usize::from(x) / 8;
                let palette = base_palette.wrapping_add((tile_id % period) as u8);
                let this_op = if applied < target_count {
                    applied += 1;
                    op
                } else {
                    ColorMathOp::None
                };
                sub.push(SubPixel {
                    palette_index: palette,
                    layer: PixelLayer::Background(layer),
                    op: this_op,
                    fixed: false,
                });
            }
        }
        sub
    }

    fn scroll(x: u16, y: u16, layer: u8) -> CoreEvent {
        CoreEvent::ScrollWrite {
            x,
            y,
            layer: PixelLayer::Background(layer),
        }
    }

    // --- Shadow-only structural facts ---------------------------------

    #[test]
    fn a_fresh_detector_has_an_empty_report_card() {
        let d = AtmosphereDetector::new();
        assert!(d.report_card().is_empty());
    }

    #[test]
    fn no_verdict_before_the_window_fills() {
        let mut d = AtmosphereDetector::new();
        let video = dominant_video();
        let sub = candidate_sub(1, 20, 1, ColorMathOp::AddHalf, 1.0);
        for f in 0..(WINDOW_FRAMES - 1) {
            let layers = d.observe(&video, &sub, WIDTH, HEIGHT, &[], "scene-a");
            assert!(layers.is_empty(), "frame {f}");
        }
        assert!(
            d.report_card().is_empty(),
            "must not trigger before the window is full"
        );
    }

    // --- The red fixture: colour math + slow independent scroll + low variety

    #[test]
    fn an_atmosphere_plane_triggers_and_emits_its_extracted_bg_layer() {
        let mut d = AtmosphereDetector::new();
        let video = dominant_video();
        // Layer 1: one distinct tile value everywhere (period 1) => low
        // variety; AddHalf on all of its pixels; never scrolls.
        let sub = candidate_sub(1, 20, 1, ColorMathOp::AddHalf, 1.0);

        let mut triggered_layers = Vec::new();
        for f in 0..WINDOW_FRAMES {
            // Dominant plane (layer 0) scrolls fast every frame; candidate
            // (layer 1) never gets a ScrollWrite at all, i.e. speed 0 --
            // as independent and slow as a plane can be.
            let events = vec![scroll((f as u16) * 8, 0, 0)];
            let layers = d.observe(&video, &sub, WIDTH, HEIGHT, &events, "scene-a");
            triggered_layers.extend(layers);
        }

        let card = d.report_card();
        assert_eq!(card.len(), 1, "exactly one candidate: layer 1");
        assert_eq!(card[0].layer, 1);
        assert!(card[0].color_math_share >= MIN_COLOR_MATH_SHARE);
        assert!(card[0].variety_score <= MAX_DISTINCT_TILES_PER_FRAME);

        assert_eq!(
            triggered_layers.len(),
            1,
            "acceptance criterion 4: a candidate recorded emits ExtractedBg exactly once"
        );
        match &triggered_layers[0] {
            SceneLayer::ExtractedBg {
                layer,
                pixels,
                width,
                height,
                ..
            } => {
                assert_eq!(layer.0, 1);
                assert_eq!(*width, WIDTH);
                assert_eq!(*height, HEIGHT);
                assert!(
                    pixels.iter().any(|p| p.layer == PixelLayer::Background(1)),
                    "the extracted layer must carry layer 1's own pixels"
                );
                assert!(
                    pixels.iter().all(|p| p.layer != PixelLayer::Background(0)),
                    "the extracted layer must NOT carry the dominant plane's pixels"
                );
            }
            other => panic!("expected ExtractedBg, got {other:?}"),
        }
    }

    #[test]
    fn the_same_scene_does_not_report_the_same_candidate_twice() {
        let mut d = AtmosphereDetector::new();
        let video = dominant_video();
        let sub = candidate_sub(1, 20, 1, ColorMathOp::AddHalf, 1.0);
        for _ in 0..(WINDOW_FRAMES + 20) {
            let _ = d.observe(&video, &sub, WIDTH, HEIGHT, &[], "scene-a");
        }
        assert_eq!(
            d.report_card().len(),
            1,
            "a sliding window that keeps satisfying the trigger must not spam the report card"
        );
    }

    // --- A plain scene must not trigger --------------------------------

    #[test]
    fn a_plain_scene_with_no_colour_math_does_not_trigger() {
        let mut d = AtmosphereDetector::new();
        let video = dominant_video();
        // Layer 1 present, low variety, slow/no scroll -- but NO colour
        // math at all.
        let sub = candidate_sub(1, 20, 1, ColorMathOp::None, 0.0);
        for _ in 0..(WINDOW_FRAMES + 5) {
            let _ = d.observe(&video, &sub, WIDTH, HEIGHT, &[], "scene-b");
        }
        assert!(
            d.report_card().is_empty(),
            "no colour math anywhere must never trigger the detector"
        );
    }

    #[test]
    fn a_single_dominant_plane_alone_does_not_trigger() {
        let mut d = AtmosphereDetector::new();
        let video = dominant_video();
        let sub = vec![none_sub(); video.len()];
        for _ in 0..(WINDOW_FRAMES + 5) {
            let _ = d.observe(&video, &sub, WIDTH, HEIGHT, &[], "scene-c");
        }
        assert!(
            d.report_card().is_empty(),
            "one plane has nothing to be independent of"
        );
    }

    // --- The design's binding correction: palette cycling must NOT trigger

    #[test]
    fn palette_cycling_scene_does_not_trigger() {
        // Super Metroid's Norfair heat (ENHANCEMENT_WAVE_16.md §4's
        // correction): a plane that visually shimmers via CGRAM palette
        // cycling, not colour math -- slow-scrolling, low tile variety,
        // but its `SubPixel::op` is always `None`. Simulated here as a
        // layer whose PALETTE INDEX changes every frame (the cycling)
        // while its op never becomes Add/AddHalf.
        let mut d = AtmosphereDetector::new();
        let video = dominant_video();
        for f in 0..(WINDOW_FRAMES + 5) {
            let cycled_palette = 30 + (f % 4) as u8;
            let sub = candidate_sub(1, cycled_palette, 1, ColorMathOp::None, 0.0);
            let events = vec![scroll((f as u16) * 8, 0, 0)];
            let _ = d.observe(&video, &sub, WIDTH, HEIGHT, &events, "norfair");
        }
        assert!(
            d.report_card().is_empty(),
            "palette cycling with zero colour math must never trigger -- the detector as \
             specified will not find it, per ENHANCEMENT_WAVE_16.md §4's binding correction"
        );
    }

    // --- A fast-scrolling colour-math plane (keeps pace with the camera,
    // not "independent") must not trigger.

    #[test]
    fn a_colour_math_plane_that_tracks_the_camera_speed_does_not_trigger() {
        let mut d = AtmosphereDetector::new();
        let video = dominant_video();
        let sub = candidate_sub(1, 20, 1, ColorMathOp::AddHalf, 1.0);
        for f in 0..(WINDOW_FRAMES + 5) {
            // Both planes scroll at the same rate -- not independent.
            let events = vec![scroll((f as u16) * 8, 0, 0), scroll((f as u16) * 8, 0, 1)];
            let _ = d.observe(&video, &sub, WIDTH, HEIGHT, &events, "scene-d");
        }
        assert!(
            d.report_card().is_empty(),
            "a plane scrolling at the same rate as the dominant plane is not an atmosphere layer"
        );
    }

    // --- High tile variety disqualifies even with colour math + slow scroll

    #[test]
    fn high_tile_variety_disqualifies_a_colour_math_plane() {
        let mut d = AtmosphereDetector::new();
        let video = dominant_video();
        // period 8 => up to 8 distinct palette values per row of tiles,
        // comfortably above MAX_DISTINCT_TILES_PER_FRAME.
        let sub = candidate_sub(1, 20, 8, ColorMathOp::AddHalf, 1.0);
        for _ in 0..(WINDOW_FRAMES + 5) {
            let _ = d.observe(&video, &sub, WIDTH, HEIGHT, &[], "scene-e");
        }
        assert!(
            d.report_card().is_empty(),
            "high tile variety must disqualify a plane even with full colour math coverage"
        );
    }

    // --- Low colour-math coverage on the candidate's own pixels must not trigger

    #[test]
    fn low_colour_math_share_disqualifies_a_slow_low_variety_plane() {
        let mut d = AtmosphereDetector::new();
        let video = dominant_video();
        // Only 20% of layer 1's own pixels carry AddHalf -- below
        // MIN_COLOR_MATH_SHARE (0.5).
        let sub = candidate_sub(1, 20, 1, ColorMathOp::AddHalf, 0.2);
        for _ in 0..(WINDOW_FRAMES + 5) {
            let _ = d.observe(&video, &sub, WIDTH, HEIGHT, &[], "scene-f");
        }
        assert!(
            d.report_card().is_empty(),
            "colour math on only a fifth of the plane's own pixels is not \"a large share\""
        );
    }

    // --- Ticket W16-04: the pin mechanism and the ladder gating it ------

    use crate::trust::{TrustLadder, TrustState};

    /// `pinned_layer` must build the same shape [`extracted_bg_layer`]
    /// would, WITHOUT running the detector at all -- "skip detection" is
    /// the whole point of a pin.
    #[test]
    fn pinned_layer_builds_an_extracted_bg_without_running_the_detector() {
        let sub = candidate_sub(2, 30, 1, ColorMathOp::AddHalf, 1.0);
        let layer = pinned_layer(2, &sub, WIDTH, HEIGHT, (5, 7));
        match layer {
            SceneLayer::ExtractedBg {
                layer: BgLayerId(n),
                width,
                height,
                scroll,
                pixels,
            } => {
                assert_eq!(n, 2);
                assert_eq!(width, WIDTH);
                assert_eq!(height, HEIGHT);
                assert_eq!(scroll, (5, 7));
                assert!(pixels.iter().any(|p| p.layer == PixelLayer::Background(2)));
            }
            other => panic!("expected ExtractedBg, got {other:?}"),
        }
    }

    /// The ladder test the ticket names explicitly: shadow does not
    /// render, active does. This module has no renderer to call, so
    /// "render" here is `TrustLadder::should_act` -- the one question
    /// every real call site (the app shell's fog-pass invocation) must
    /// ask before it may call `pinned_layer`/`FogPass::render` at all.
    #[test]
    fn the_trust_ladder_gates_whether_the_fog_pass_may_act() {
        let mut ladder = TrustLadder::new();
        let scene = "scene-a";

        assert!(
            !ladder.should_act(HEURISTIC_ID, scene),
            "fresh install: shadow, must not act"
        );

        ladder.set_state(HEURISTIC_ID, TrustState::Advisory);
        assert!(
            !ladder.should_act(HEURISTIC_ID, scene),
            "advisory is a suggestion, not an action -- must still not act"
        );

        ladder.set_state(HEURISTIC_ID, TrustState::Active);
        assert!(
            ladder.should_act(HEURISTIC_ID, scene),
            "active must act -- this is the state a real caller checks before \
             calling `pinned_layer`/rendering the fog pass"
        );
    }

    /// A profile pin overrides the ladder the same way it does for any
    /// other heuristic (`crate::trust`'s own test of this exact
    /// mechanism) -- checked again here under this heuristic's own name,
    /// since acceptance criterion 3 names it specifically.
    #[test]
    fn a_profile_pin_forces_the_plane_active_regardless_of_the_users_setting() {
        let mut ladder = TrustLadder::new();
        ladder.set_state(HEURISTIC_ID, TrustState::Shadow);
        ladder.pin(HEURISTIC_ID, TrustState::Active);
        assert!(ladder.should_act(HEURISTIC_ID, "scene-a"));
    }
}
