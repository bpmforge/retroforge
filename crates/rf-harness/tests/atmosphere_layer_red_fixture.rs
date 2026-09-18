//! Red fixture for the atmosphere-layer detector (ticket W16-03; FR-ENH-013;
//! `docs/design/ENHANCEMENT_WAVE_16.md` §4; `crate::atmosphere` in
//! `rf-enhance`).
//!
//! ## Why this is a synthetic `Vec<PpuPixel>`/`Vec<SubPixel>` fixture, not a
//! new `RF-Scroller-S` ROM variant
//!
//! `docs/design/ENHANCEMENT_WAVE_16.md` §4's own acceptance text allows
//! either an "RF-Scroller-S variant or a new dedicated fixture", and this
//! ticket's own instructions say plainly: build a real fog-plane ROM
//! variant only if the toolchain to rebuild `fixtures/snes/rf-scroller-s`
//! is present, else fall back to a synthetic frame sequence and say so.
//! This machine has neither `cc65`, `ca65` nor `ld65` on `PATH` (checked
//! at the time this ticket was worked — CLAUDE.md's own "GitHub is
//! STORAGE, not a gate" section already establishes that a fixture
//! requiring a toolchain absent from the developer's own machine cannot be
//! rebuilt here), so this file drives the real `rf_enhance::atmosphere`
//! detector with hand-built frame data instead of a compiled SNES ROM —
//! the same fallback `crates/rf-enhance/src/atmosphere.rs`'s own unit
//! tests use, promoted to this crate only so a red fixture the CI-adjacent
//! test suite runs lives outside the crate implementing the heuristic
//! (the same separation `rf_scroller_gem_blink_red_fixture.rs` keeps from
//! `crate::sprite_historian`'s own unit tests).
//!
//! ## What "stays red" means here (FR-ENH-013)
//!
//! [`atmosphere_plane_triggers_and_stays_triggered`] asserts the detector
//! fires on a scene shaped like a fog layer (colour math + slow
//! independent scroll + low tile variety) — if a future change to the
//! detector's thresholds or its colour-math/tile-variety/scroll reads
//! stops this from firing, this test goes red, which is the point.
//! [`plain_scene_does_not_trigger`] and
//! [`palette_cycling_scene_does_not_trigger`] are the paired negative
//! controls FR-ENH-013 implies (a heuristic that fires on everything is
//! not a signal either) — the latter is also
//! `docs/design/ENHANCEMENT_WAVE_16.md` §4's binding correction: Super
//! Metroid's Norfair heat is palette cycling, not a colour-math plane, and
//! this detector must not confuse the two.

use rf_core_api::{ColorMathOp, CoreEvent, PixelLayer, PpuPixel, SubPixel};
use rf_enhance::atmosphere::{
    AtmosphereDetector, MAX_DISTINCT_TILES_PER_FRAME, MIN_COLOR_MATH_SHARE, WINDOW_FRAMES,
};
use rf_enhance::scene_graph::SceneLayer;

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

/// The dominant/main plane: layer 0 wins every on-screen pixel, laid out
/// with 8 distinct per-tile palette values (well above
/// [`MAX_DISTINCT_TILES_PER_FRAME`]) so it can never itself be mistaken
/// for the fog candidate.
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

/// A full-screen sub-screen (colour-math) buffer for background layer 1 —
/// the "fog plane". `period` controls tile variety (1 = a single repeated
/// palette value, the lowest possible), `op`/`share` control colour-math
/// coverage exactly like `rf_enhance::atmosphere`'s own unit-test helper
/// of the same shape.
fn fog_sub(base_palette: u8, period: usize, op: ColorMathOp, share: f32) -> Vec<SubPixel> {
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
                layer: PixelLayer::Background(1),
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

/// RED FIXTURE (FR-ENH-013): a fog-shaped plane — half-additive colour
/// math on all of its own pixels, one repeated 8x8 tile pattern, and zero
/// scroll while the dominant plane scrolls at 8px/frame — must trigger the
/// detector and must keep triggering it. If this ever stops firing, the
/// detector (or one of its thresholds) regressed.
#[test]
fn atmosphere_plane_triggers_and_stays_triggered() {
    let mut detector = AtmosphereDetector::new();
    let video = dominant_video();
    let sub = fog_sub(20, 1, ColorMathOp::AddHalf, 1.0);

    let mut emitted = Vec::new();
    // Run comfortably past one window's worth of frames so this is a
    // genuine sustained-scene trigger, not a one-frame fluke.
    for f in 0..(WINDOW_FRAMES + 60) {
        let events = vec![scroll((f as u16).wrapping_mul(8), 0, 0)];
        let layers = detector.observe(&video, &sub, WIDTH, HEIGHT, &events, "fog-room");
        emitted.extend(layers);
    }

    let card = detector.report_card();
    assert_eq!(
        card.len(),
        1,
        "the fog-shaped plane (layer 1) must be recorded exactly once: {card:?}"
    );
    assert_eq!(card[0].layer, 1);
    assert!(
        card[0].color_math_share >= MIN_COLOR_MATH_SHARE,
        "recorded share {} must clear the documented threshold",
        card[0].color_math_share
    );
    assert!(
        card[0].variety_score <= MAX_DISTINCT_TILES_PER_FRAME,
        "recorded variety {} must clear the documented ceiling",
        card[0].variety_score
    );

    // Acceptance criterion 4: recording a candidate emits its ExtractedBg
    // layer, still shadow (nothing in this crate composites it).
    assert_eq!(
        emitted.len(),
        1,
        "one ExtractedBg emitted for the one new candidate"
    );
    match &emitted[0] {
        SceneLayer::ExtractedBg { layer, pixels, .. } => {
            assert_eq!(layer.0, 1);
            assert!(
                pixels.iter().any(|p| p.layer == PixelLayer::Background(1)),
                "the extracted layer must carry the fog plane's own pixels"
            );
        }
        other => panic!("expected ExtractedBg, got {other:?}"),
    }
}

/// Negative control: the same plane shape (slow scroll, low variety) but
/// zero colour math anywhere must never trigger.
#[test]
fn plain_scene_does_not_trigger() {
    let mut detector = AtmosphereDetector::new();
    let video = dominant_video();
    let sub = vec![none_sub(); video.len()];
    for _ in 0..(WINDOW_FRAMES + 10) {
        let _ = detector.observe(&video, &sub, WIDTH, HEIGHT, &[], "plain-room");
    }
    assert!(
        detector.report_card().is_empty(),
        "a plane with no colour math at all must never be flagged"
    );
}

/// `ENHANCEMENT_WAVE_16.md` §4's binding correction: Super Metroid's
/// Norfair heat shimmer is CGRAM palette cycling, not a colour-math plane
/// — this detector must not mistake the two. Simulated as a plane whose
/// palette index changes every frame (the "cycling") while its
/// `SubPixel::op` never becomes `Add`/`AddHalf`.
#[test]
fn palette_cycling_scene_does_not_trigger() {
    let mut detector = AtmosphereDetector::new();
    let video = dominant_video();
    for f in 0..(WINDOW_FRAMES + 10) {
        let cycled_palette = 30u8.wrapping_add((f % 4) as u8);
        let sub = fog_sub(cycled_palette, 1, ColorMathOp::None, 0.0);
        let events = vec![scroll((f as u16).wrapping_mul(8), 0, 0)];
        let _ = detector.observe(&video, &sub, WIDTH, HEIGHT, &events, "norfair");
    }
    assert!(
        detector.report_card().is_empty(),
        "palette cycling with zero colour math must never trigger, per \
         ENHANCEMENT_WAVE_16.md §4's binding Norfair-heat correction"
    );
}
