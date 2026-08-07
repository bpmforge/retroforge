//! Scene identity (ticket W4-03b; `docs/design/ENHANCEMENT_RUNTIME.md` §3):
//! "Scene identity = perceptual hash of framebuffer edges + mapper bank
//! state; scene change ⇒ new canvas (rooms/levels get separate canvases)."
//!
//! ## Why edges of a coarse block grid, not full-frame content (FM-11)
//!
//! `docs/design/FAILURE_MODES.md`'s FM-11 ("Stitcher canvas explosion,
//! false scene cuts") is the entire reason this is not a hash of the raw
//! pixel buffer: a full-content hash changes on *any* sprite movement or
//! HUD counter tick, so every single frame would be "a new scene" and
//! [`crate::stitcher::Canvas`] would never accumulate more than one
//! frame's worth of content before being discarded for a fresh one.
//!
//! [`compute_scene_id`] instead:
//! 1. Keeps only background/backdrop pixels
//!    ([`crate::stitcher::stitchable_pixel`] — the same filter the
//!    stitcher itself applies, so a sprite currently occluding a cell
//!    contributes nothing to either the canvas or the identity hash).
//! 2. Averages those pixels into an `GRID_ROWS` x `GRID_COLS` block grid
//!    (coarse — 8x8 over a 256x240 NES frame is 32x30-pixel blocks), so a
//!    small moving sprite or a few HUD digit pixels shift a block's
//!    average by only a small fraction of it.
//! 3. Thresholds each *adjacent-block* average difference into a 3-way
//!    edge bucket (down / flat / up) — a genuine noise floor, not just
//!    "hash whatever the averages happen to be" (which would still be
//!    sensitive to the exact same one-sprite-changes-one-block-slightly
//!    problem, just with smaller magnitude).
//! 4. Hashes the edge-bucket sequence together with the caller-supplied
//!    `mapper_bank_state` bytes (this module has no access to real mapper
//!    state — `rf_core_api::StateView::mapper_state` is borrowed,
//!    "valid only between frames", and not part of `FrameBundle`; the
//!    composer that actually owns a `StateView` passes the bytes in, the
//!    same "pure function, caller supplies the environment-specific bit"
//!    pattern `crate::camera`'s FM-13 policy uses for the adapter limit).
//!
//! ## Threshold derivation (not fit to a fixture after the fact)
//!
//! `EDGE_THRESHOLD` is chosen from a stated noise budget, not nudged until
//! a test passed: with `GRID_COLS = GRID_ROWS = 8` over a 256x240 frame,
//! each block is 32x30 = 960 px. An 8x8 sprite (64 px) occluding one
//! block removes at most `64/960 ≈ 6.7%` of its samples; against this
//! module's own textured-background test fixture (values spread across a
//! `0..=255` range in thirds), that can move a block's average by at most
//! roughly `0.067 * 255 ≈ 17`, and two *adjacent* blocks losing opposite
//! amounts in the same frame doubles that to `~34` in the worst case.
//! `EDGE_THRESHOLD = 48` clears that with headroom while staying well
//! under the `~80` gap this module's own textured-block test fixture uses
//! between genuinely different blocks, so real edges still register.
//!
//! ## Documented, MEASURED gap: stable per-call, NOT stable across a
//! scrolling session
//!
//! This grid is anchored to **screen space**, recomputed fresh from each
//! frame's visible pixels. Two DIFFERENT things were measured, not
//! assumed, and they point in different directions:
//!
//! - **Per-call jitter** (`small_scroll_deltas_do_not_create_a_false_scene_cut`):
//!   a single call's world content shifted by 1-4px against the previous
//!   call's — this IS stable on this 256x240/8x8-grid fixture (block
//!   averaging absorbs that much drift the same way it absorbs sprite/HUD
//!   noise); at 8px and beyond, single-call shifts are NOT stable, printed
//!   but deliberately not asserted either way (a still-open gap must not
//!   be pinned as required behavior in either direction).
//! - **Cumulative session drift**
//!   (`cumulative_drift_over_a_scrolling_session_fragments_the_scene_id`) —
//!   the axis FM-11's own detection column actually measures ("canvas
//!   count per scene watermark", a count over a play SESSION, not one
//!   call): 50 frames advancing 3px/frame (a typical NES scroll rate,
//!   150px total travel — about five block-widths) produced **36 distinct
//!   scene ids**, even with sprite/HUD noise correctly collapsed to a
//!   single id when the background does NOT move (the watermark test
//!   above). Per-call stability at 1-4px does **not** imply session-level
//!   stability across many such calls — each 3px step is individually
//!   invisible, but they compound, and this implementation has no
//!   world-space memory between calls to resist that. Concretely: a real
//!   scrolling level would fragment into roughly one new "scene" (and
//!   hence, per `crate::persistence`, one new persisted canvas) every few
//!   block-widths of travel — the same FM-11 failure mode this module's
//!   design otherwise avoids, just on the scrolling axis instead of the
//!   sprite/HUD-noise axis.
//!
//! Per this ticket's own honesty requirement (inherited from W4-03a:
//! neither fixture ROM ever reached real scrolling gameplay), this is
//! named here rather than silently assumed away, and neither number above
//! is asserted as a pass/fail criterion — both are `eprintln!`d
//! measurements a future ticket can compare against after a real change. A
//! scroll-invariant scene identity across an ENTIRE level (e.g.
//! phase-aligning block boundaries to world coordinates so a given world
//! block's content never rotates, or leaning more heavily on
//! `mapper_bank_state` for cartridges where each level occupies its own
//! bank) is left to a future ticket informed by a real scrolling fixture
//! (W5-01).
//!
//! ## Real-fixture update (W4-03d, `crates/rf-enhance/src/scene_tracker.rs`)
//!
//! [`crate::scene_tracker::SceneTracker`] solves the session-level
//! fragmentation above by not calling [`compute_scene_id`] on every
//! scrolling frame at all (continuity-based identity). Doing so on the
//! REAL RF-Scroller fixture surfaced a second, DIFFERENT, previously
//! unmeasured gap in this module's own edge-threshold approach: real NES
//! `palette_index` values cluster far closer together than the wide
//! 0/96/176-separated thirds this module's own synthetic test fixtures use
//! (`crates/rf-harness/tests/rf_scroller_scene_tracker.rs`'s module doc has
//! the full measurement) — on that fixture, `block_averages`' largest
//! adjacent-block delta anywhere in the level never exceeds roughly 10,
//! nowhere near `EDGE_THRESHOLD`'s 48, so [`compute_scene_id`]'s edge
//! signature is constant (all "flat") for the entire level, and collides
//! with an unrelated ROM's (Alter Ego) equally low-contrast signature. No
//! single `EDGE_THRESHOLD` can fix this without also breaking this
//! module's own sprite/HUD-noise watermark test (that test's own
//! perturbation math, `~17-34` per block, is smaller than the real-content
//! deltas that would need catching) — a genuine open gap in this
//! algorithm's design, not a tuning miss, left for a future ticket.

use rf_core_api::PpuPixel;

use crate::stitcher::stitchable_pixel;

const GRID_COLS: usize = 8;
const GRID_ROWS: usize = 8;

/// Noise floor for adjacent-block average differences — see module doc
/// "Threshold derivation" for how this value was chosen.
const EDGE_THRESHOLD: i32 = 48;

const FNV_OFFSET_BASIS: u64 = 1_469_598_103_934_665_603;
const FNV_PRIME: u64 = 1_099_511_628_211;

/// A scene's identity: the same content (background edges + mapper bank
/// state) always hashes to the same value; genuinely different content
/// (a different level, or a different mapper bank) hashes differently
/// with overwhelming probability. Not cryptographic — FNV-1a, chosen for
/// determinism and the zero-dependency budget this crate is held to
/// (ticket brief: "No other new crate"), not collision resistance against
/// an adversary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SceneId(pub u64);

impl SceneId {
    /// Fixed-length lowercase-hex form, for use as `rf_cache::CacheKey`'s
    /// `asset_hash` (`crate::persistence`) — matches the hex-string
    /// convention `rf_cache::key::CacheKey` already uses for its own
    /// content hash.
    #[must_use]
    pub fn to_hex(self) -> String {
        format!("{:016x}", self.0)
    }
}

/// Compute a scene's identity from one frame's video buffer plus the
/// mapper bank state in effect for that frame (module doc). Pure function:
/// the same three inputs always produce the same [`SceneId`], and nothing
/// here reaches into any cross-frame state.
///
/// `video` is `FrameBundle::video`'s shape: row-major, `width * height`
/// pixels long. `mapper_bank_state` is caller-supplied raw bytes (module
/// doc's "why a plain slice, not a live `StateView`" explanation) —
/// pass an empty slice for a mapper with no persistent banking state.
#[must_use]
pub fn compute_scene_id(
    video: &[PpuPixel],
    width: u16,
    height: u16,
    mapper_bank_state: &[u8],
) -> SceneId {
    let signature = edge_signature(video, width, height);
    let mut hash = FNV_OFFSET_BASIS;
    for byte in &signature {
        hash = fnv_mix(hash, *byte);
    }
    // A length-prefix-free separator would let (edges=[], bank=[1]) collide
    // with (edges=[1], bank=[]); mixing the signature's own length first
    // rules that out, same "field-boundary shifting" concern
    // `rf_cache::key::CacheKey::content_hash` already documents.
    hash = fnv_mix_u64(hash, signature.len() as u64);
    for byte in mapper_bank_state {
        hash = fnv_mix(hash, *byte);
    }
    SceneId(hash)
}

fn fnv_mix(hash: u64, byte: u8) -> u64 {
    (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME)
}

fn fnv_mix_u64(hash: u64, value: u64) -> u64 {
    let mut h = hash;
    for byte in value.to_le_bytes() {
        h = fnv_mix(h, byte);
    }
    h
}

/// Per-block average `palette_index` of background/backdrop-layer pixels
/// only (sprites excluded entirely, module doc point 1); a block with no
/// background samples at all (fully sprite-occluded, or genuinely
/// off-frame) defaults to `0` rather than propagating an `Option` through
/// the rest of the pipeline — a degenerate all-sprite block is not a case
/// any real frame produces, and `0` is a stable, deterministic default.
fn block_averages(video: &[PpuPixel], width: u16, height: u16) -> [[i32; GRID_COLS]; GRID_ROWS] {
    let width = width as usize;
    let height = height as usize;
    let block_w = (width / GRID_COLS).max(1);
    let block_h = (height / GRID_ROWS).max(1);

    let mut sums = [[0i64; GRID_COLS]; GRID_ROWS];
    let mut counts = [[0i64; GRID_COLS]; GRID_ROWS];

    for y in 0..height {
        let row_block = (y / block_h).min(GRID_ROWS - 1);
        let row_start = y * width;
        for x in 0..width {
            let Some(pixel) = video.get(row_start + x) else {
                continue; // degrade, never index-oob, matching stitcher.rs's own stance
            };
            let Some(bg) = stitchable_pixel(pixel) else {
                continue; // sprite pixel -- excluded, module doc point 1
            };
            let col_block = (x / block_w).min(GRID_COLS - 1);
            sums[row_block][col_block] += i64::from(bg.palette_index);
            counts[row_block][col_block] += 1;
        }
    }

    let mut averages = [[0i32; GRID_COLS]; GRID_ROWS];
    for row in 0..GRID_ROWS {
        for col in 0..GRID_COLS {
            averages[row][col] = if counts[row][col] > 0 {
                (sums[row][col] / counts[row][col]) as i32
            } else {
                0
            };
        }
    }
    averages
}

/// Thresholded (module doc point 3) horizontal- and vertical-adjacent
/// block edge sequence, in a fixed, grid-size-determined order — same
/// order every call, so two frames with the same content always produce
/// byte-identical signatures.
fn edge_signature(video: &[PpuPixel], width: u16, height: u16) -> Vec<u8> {
    let averages = block_averages(video, width, height);
    let mut signature =
        Vec::with_capacity(GRID_ROWS * (GRID_COLS - 1) + GRID_COLS * (GRID_ROWS - 1));

    for row in &averages {
        for pair in row.windows(2) {
            signature.push(edge_bucket(pair[0] - pair[1]));
        }
    }
    for rows in averages.windows(2) {
        let (row_a, row_b) = (&rows[0], &rows[1]);
        for (a, b) in row_a.iter().zip(row_b.iter()) {
            signature.push(edge_bucket(a - b));
        }
    }
    signature
}

/// `2` = a rising edge, `0` = a falling edge, `1` = "flat" (within the
/// noise floor) -- see module doc "Threshold derivation".
fn edge_bucket(diff: i32) -> u8 {
    if diff > EDGE_THRESHOLD {
        2
    } else if diff < -EDGE_THRESHOLD {
        0
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rf_core_api::PixelLayer;

    const WIDTH: u16 = 256;
    const HEIGHT: u16 = 240;

    /// Three well-separated base values (module doc: adjacent blocks
    /// always land in different thirds when their `(row+col) % 3` differs
    /// by exactly one), plus small deterministic per-pixel texture so a
    /// block is never degenerately uniform (an implementation detail a
    /// flat-fill fixture could accidentally hide).
    fn block_base(row_block: usize, col_block: usize) -> i32 {
        const BASES: [i32; 3] = [16, 96, 176];
        BASES[(row_block + col_block) % 3]
    }

    fn textured_value(x: usize, y: usize, row_block: usize, col_block: usize) -> u8 {
        let base = block_base(row_block, col_block);
        let noise = ((x.wrapping_mul(7) + y.wrapping_mul(13)) % 9) as i32 - 4; // -4..=4
        (base + noise).clamp(0, 255) as u8
    }

    fn bg_pixel(v: u8) -> PpuPixel {
        PpuPixel {
            palette_index: v,
            layer: PixelLayer::Background(0),
            sprite_id: None,
            priority: 0,
            dropped_by_limit: false,
        }
    }

    fn sprite_pixel() -> PpuPixel {
        PpuPixel {
            palette_index: 255,
            layer: PixelLayer::Sprite,
            sprite_id: Some(0),
            priority: 0,
            dropped_by_limit: false,
        }
    }

    /// A synthetic "scene": textured background per the block pattern
    /// above, with an 8x8 sprite at `(sprite_x, sprite_y)` and an 8x8 HUD
    /// counter-style region at fixed screen position whose *content*
    /// (not position) is `hud_value` -- both are the two discriminating
    /// perturbations the ticket's criterion (b) names explicitly.
    fn build_frame(sprite_x: usize, sprite_y: usize, hud_value: u8) -> Vec<PpuPixel> {
        let width = WIDTH as usize;
        let height = HEIGHT as usize;
        let block_w = width / GRID_COLS;
        let block_h = height / GRID_ROWS;
        let mut video = vec![bg_pixel(0); width * height];

        for y in 0..height {
            let row_block = (y / block_h).min(GRID_ROWS - 1);
            for x in 0..width {
                let col_block = (x / block_w).min(GRID_COLS - 1);
                video[y * width + x] = bg_pixel(textured_value(x, y, row_block, col_block));
            }
        }

        // HUD counter: an 8x8 region at a fixed screen position, its
        // background-layer CONTENT varying with `hud_value` -- real NES
        // score digits are background tiles, not a separate layer.
        for y in 0..8 {
            for x in 0..8 {
                video[y * width + x] = bg_pixel(hud_value);
            }
        }

        // Sprite: excluded entirely from the identity hash
        // (`stitchable_pixel`), so its position must not matter.
        for y in 0..8 {
            for x in 0..8 {
                let (sx, sy) = (sprite_x + x, sprite_y + y);
                if sx < width && sy < height {
                    video[sy * width + sx] = sprite_pixel();
                }
            }
        }

        video
    }

    fn different_scene_frame() -> Vec<PpuPixel> {
        let width = WIDTH as usize;
        let height = HEIGHT as usize;
        let block_w = width / GRID_COLS;
        let block_h = height / GRID_ROWS;
        let mut video = vec![bg_pixel(0); width * height];
        for y in 0..height {
            let row_block = (y / block_h).min(GRID_ROWS - 1);
            for x in 0..width {
                let col_block = (x / block_w).min(GRID_COLS - 1);
                // Inverted block-index pattern relative to `block_base`:
                // every block that was bucket 0 is now bucket 2 and vice
                // versa, so every single adjacent-pair edge flips sign.
                const BASES: [i32; 3] = [176, 96, 16];
                let base = BASES[(row_block + col_block) % 3];
                let noise = ((x.wrapping_mul(7) + y.wrapping_mul(13)) % 9) as i32 - 4;
                video[y * width + x] = bg_pixel((base + noise).clamp(0, 255) as u8);
            }
        }
        video
    }

    // --- Criterion (a): identical frame -> same id ---------------------

    #[test]
    fn identical_frame_produces_the_same_id() {
        let frame = build_frame(100, 100, 7);
        let a = compute_scene_id(&frame, WIDTH, HEIGHT, &[1, 2, 3]);
        let b = compute_scene_id(&frame, WIDTH, HEIGHT, &[1, 2, 3]);
        assert_eq!(a, b);
    }

    // --- Criterion (b): the discriminating case -------------------------

    #[test]
    fn sprite_moved_and_hud_changed_within_the_same_scene_keeps_the_same_id() {
        let frame_a = build_frame(40, 40, 0x11);
        let frame_b = build_frame(200, 180, 0x22); // sprite moved far; HUD digit changed
        let bank_state = [9u8, 9, 9];

        let id_a = compute_scene_id(&frame_a, WIDTH, HEIGHT, &bank_state);
        let id_b = compute_scene_id(&frame_b, WIDTH, HEIGHT, &bank_state);
        assert_eq!(
            id_a, id_b,
            "sprite movement and a HUD counter tick must not create a false scene cut (FM-11)"
        );
    }

    // --- Criterion (c): genuinely different scene -> different id ------

    #[test]
    fn different_mapper_bank_state_alone_changes_the_id() {
        let frame = build_frame(0, 0, 0);
        let id_a = compute_scene_id(&frame, WIDTH, HEIGHT, &[1, 2, 3]);
        let id_b = compute_scene_id(&frame, WIDTH, HEIGHT, &[1, 2, 4]);
        assert_ne!(
            id_a, id_b,
            "a different mapper bank must be a different scene"
        );
    }

    #[test]
    fn a_genuinely_different_level_changes_the_id() {
        let bank_state = [5u8];
        let id_a = compute_scene_id(&build_frame(0, 0, 0), WIDTH, HEIGHT, &bank_state);
        let id_b = compute_scene_id(&different_scene_frame(), WIDTH, HEIGHT, &bank_state);
        assert_ne!(
            id_a, id_b,
            "an unrelated level's content must not collide with this one's"
        );
    }

    // --- FM-11 watermark: canvas count per scene stays bounded ---------
    //
    // NAMED FOR ITS OWN LIMIT, DELIBERATELY. FM-11's detection column in
    // `docs/design/FAILURE_MODES.md` is "canvas count per scene
    // watermark", and a test called simply "FM-11 watermark" passing
    // green would tell a future reader that canvas explosion is guarded
    // in general. It is not. This covers exactly one half of FM-11 --
    // per-frame sprite/HUD noise over an UNCHANGED background -- and the
    // other half, sustained scrolling, is a measured OPEN GAP recorded by
    // `cumulative_drift_over_a_scrolling_session_fragments_the_scene_id`
    // below, which prints rather than asserts. Read that test before
    // trusting this one's scope.
    //
    // (Same lesson this board recorded one ticket earlier in
    // `rf_cache::fsutil`: a test that *exercises* a branch is not a test
    // that *discriminates* it, and a name that overstates coverage is how
    // that mistake survives review.)

    #[test]
    fn fifty_frames_of_sprite_and_hud_noise_over_an_unchanged_background_stay_one_scene_id() {
        use std::collections::HashSet;
        let bank_state = [3u8, 1, 4];
        let mut seen = HashSet::new();
        for frame_no in 0..50u32 {
            // Sprite sweeps across the whole screen; HUD digit increments
            // every frame -- exactly the per-frame noise FM-11 worries
            // about, with a CONSTANT background (no scene change should
            // ever be detected across this run).
            let sx = ((frame_no * 5) % (WIDTH as u32 - 8)) as usize;
            let sy = ((frame_no * 3) % (HEIGHT as u32 - 8)) as usize;
            let hud = (frame_no % 256) as u8;
            let frame = build_frame(sx, sy, hud);
            seen.insert(compute_scene_id(&frame, WIDTH, HEIGHT, &bank_state));
        }
        assert_eq!(
            seen.len(),
            1,
            "canvas count per scene must stay bounded (FM-11) -- got {} distinct scene ids \
             across 50 frames of pure sprite/HUD noise over an unchanged background",
            seen.len()
        );
    }

    // --- Documented, measured (not assumed) scroll-sensitivity gap -----

    fn world_frame_at_shift(shift: usize) -> Vec<PpuPixel> {
        let width = WIDTH as usize;
        let height = HEIGHT as usize;
        let block_w = width / GRID_COLS;
        let block_h = height / GRID_ROWS;
        // An infinite-in-x "world texture": the same block pattern as
        // `build_frame`'s background, but sampled at a world x offset
        // rather than always starting at world x = 0.
        let mut video = vec![bg_pixel(0); width * height];
        for y in 0..height {
            let row_block = (y / block_h).min(GRID_ROWS - 1);
            for x in 0..width {
                let world_x = x + shift;
                let col_block = ((world_x / block_w) % GRID_COLS).min(GRID_COLS - 1);
                video[y * width + x] = bg_pixel(textured_value(world_x, y, row_block, col_block));
            }
        }
        video
    }

    /// Module doc's "documented, MEASURED gap", positive direction only:
    /// small per-call scroll deltas (sub-block jitter) do not create a
    /// false scene cut, the same discriminating property as sprite/HUD
    /// noise, just along a different axis. Measured, not assumed: an
    /// earlier version of this test also pinned an `assert_ne!` on an 8px
    /// delta changing the id, which is exactly backwards -- it would turn
    /// the documented follow-up (world-aligned block boundaries fixing
    /// this) into a "regression" the moment it landed. A gap that is
    /// still open must never be asserted as required behavior, so larger
    /// deltas are swept and PRINTED, never asserted, in
    /// `cumulative_drift_over_a_scrolling_session_fragments_the_scene_id`
    /// below.
    #[test]
    fn small_scroll_deltas_do_not_create_a_false_scene_cut() {
        let bank_state = [7u8];
        let base_id = compute_scene_id(&world_frame_at_shift(0), WIDTH, HEIGHT, &bank_state);

        // Measured stable range on this fixture: block averaging over 960
        // px/block absorbs a few pixels of world-content drift the same
        // way it absorbs sprite/HUD noise (module doc) -- small enough
        // that no edge bucket crosses `EDGE_THRESHOLD`.
        for shift in [1usize, 2, 3, 4] {
            let id = compute_scene_id(&world_frame_at_shift(shift), WIDTH, HEIGHT, &bank_state);
            assert_eq!(
                id, base_id,
                "measured stable range: a {shift}px scroll must not create a false scene cut"
            );
        }

        // Recorded, not asserted: a gap this ticket's own module doc names
        // as open (not attempted here) must not be pinned as required
        // behavior in either direction.
        for shift in [8usize, 16, 32, 64] {
            let id = compute_scene_id(&world_frame_at_shift(shift), WIDTH, HEIGHT, &bank_state);
            eprintln!(
                "single-call shift={shift}px: same_as_base={}",
                id == base_id
            );
        }
    }

    /// FM-11's own detection column is "canvas count per SCENE watermark"
    /// -- a count over a play SESSION, not a single call. The test above
    /// only proves per-call stability at small deltas; it says nothing
    /// about what happens across many calls that each advance a few
    /// pixels, which is the actual shape of real scrolling play. This
    /// test measures exactly that instead of inferring it: 50 frames, the
    /// SAME background texture as `world_frame_at_shift` but advancing by
    /// 3px/frame (a typical NES scroll rate) -- 150px of total travel,
    /// about five block-widths -- with sprite position and HUD content
    /// also varying every frame (same noise `build_frame` exercises), same
    /// mapper bank state throughout.
    #[test]
    fn cumulative_drift_over_a_scrolling_session_fragments_the_scene_id() {
        use std::collections::HashSet;
        let width = WIDTH as usize;
        let height = HEIGHT as usize;
        let block_w = width / GRID_COLS;
        let block_h = height / GRID_ROWS;
        let bank_state = [7u8];

        let mut seen = HashSet::new();
        for frame_no in 0..50u32 {
            let world_shift = (frame_no as usize) * 3; // 3px/frame, typical NES scroll rate
            let mut video = vec![bg_pixel(0); width * height];
            for y in 0..height {
                let row_block = (y / block_h).min(GRID_ROWS - 1);
                for x in 0..width {
                    let world_x = x + world_shift;
                    let col_block = ((world_x / block_w) % GRID_COLS).min(GRID_COLS - 1);
                    video[y * width + x] =
                        bg_pixel(textured_value(world_x, y, row_block, col_block));
                }
            }
            // Same per-frame noise the FM-11 watermark test above uses,
            // layered on top of the scrolling background.
            let hud = (frame_no % 256) as u8;
            for y in 0..8 {
                for x in 0..8 {
                    video[y * width + x] = bg_pixel(hud);
                }
            }
            let (sx, sy) = (
                ((frame_no * 5) % (WIDTH as u32 - 8)) as usize,
                ((frame_no * 3) % (HEIGHT as u32 - 8)) as usize,
            );
            for y in 0..8 {
                for x in 0..8 {
                    let (px, py) = (sx + x, sy + y);
                    if px < width && py < height {
                        video[py * width + px] = sprite_pixel();
                    }
                }
            }

            seen.insert(compute_scene_id(&video, WIDTH, HEIGHT, &bank_state));
        }

        // Recorded, not pinned to a specific number -- see this test's own
        // doc and the module doc for what this means: cumulative scroll
        // travel, unlike per-call jitter or sprite/HUD noise, is NOT
        // proven stable by this implementation, and this print is the
        // honest measurement rather than an inference from the (narrower)
        // per-call test above.
        eprintln!(
            "cumulative_drift_over_a_scrolling_session: {} distinct scene ids across 50 frames \
             advancing 3px/frame (150px total travel)",
            seen.len()
        );
    }
}
