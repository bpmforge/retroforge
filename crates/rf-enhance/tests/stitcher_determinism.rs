//! Ticket W4-03a acceptance criterion 2: "stitcher determinism test" —
//! and the specific anti-vacuity bar the ticket brief itself sets: the
//! same ROM's input log must produce a byte-identical canvas, proven by
//! driving the REAL `rf_core_api` `FrameBundle`/`CoreEvent`/
//! `triple_buffer` pipeline and `rf_enhance::bus::FrameSubscriber` — not
//! by handing `ScrollTracker`/`Stitcher` raw `(x, y)` tuples directly (a
//! pure function trivially agreeing with itself proves nothing about how
//! the tracker handles real mid-frame writes).
//!
//! ## Why this doesn't drive a real `rf-nes` core (and what it drives instead)
//!
//! `scripts/validate-arch.sh` rule 3 mechanically forbids any crate other
//! than `retroforge`/`rf-harness`/`rf-nes`/`rf-snes` from depending on a
//! console core directly — `rf-enhance` (this crate) is not on that list,
//! by design (`ENHANCEMENT_RUNTIME.md` §1's event flow has cores feed
//! `FrameBundle` to this crate, never the reverse). So "the real event
//! stream" here means the real WIRE TYPES carrying a hand-authored but
//! REALISTIC log — constructed to match `rf-nes`'s own, independently
//! verified real emission order
//! (`crates/rf-nes/src/ppu/tests/event_emission.rs`'s
//! `one_full_frame_emits_the_exact_expected_event_sequence`: `ScrollWrite`
//! events queued before any tick-driven event, then `FrameEnd`,
//! `FrameStart`, `Scanline(0..240)`, `VblankStart`) and this same
//! ticket's `rf-nes` change (a `$2006`-driven `ScrollWrite` fires
//! mid-picture, gated on rendering-enabled + `scanline < 240` —
//! `crates/rf-nes/src/ppu/scroll.rs`'s `write_addr` doc).
//!
//! What makes this non-vacuous: the fixture drives 40 frames of state —
//! an HUD reset plus a mid-frame split every frame, a scroll wraparound
//! partway through, band boundaries the tracker must derive purely from
//! `Scanline`/`ScrollWrite` interleaving — fed through the SAME
//! `FrameSubscriber::subscribed_events()` filtering path a real
//! deployment would use, not handed to `ScrollTracker` directly. The
//! fixture's video content is a deterministic function of WORLD
//! coordinates (a fixed "level texture"), independently computed here
//! (not imported from `rf_enhance::scroll_tracker::WorldAxis`, so this
//! test cannot pass by sharing a bug with the production accumulator) —
//! so a correct pipeline must reconstruct the exact same content at the
//! exact same world position on both runs, not merely produce SOME
//! stable-with-itself output.
use std::sync::Arc;

use rf_core_api::{triple_buffer, CoreEvent, EventMask, FrameBundle, PixelLayer, PpuPixel};
use rf_enhance::bus::FrameSubscriber;
use rf_enhance::scroll_tracker::ScrollTracker;
use rf_enhance::stitcher::{stitch_frame, Canvas};

const WIDTH: u16 = 256;
const HEIGHT: u16 = 240;
const HUD_ROWS: u16 = 16;

/// A fixed "level texture": deterministic function of world coordinates,
/// standing in for real background art nothing here needs to author.
fn level_pixel(world_x: i64, world_y: i64) -> PpuPixel {
    let v = (world_x.rem_euclid(97) ^ world_y.rem_euclid(61)) as u8;
    PpuPixel {
        palette_index: v,
        layer: PixelLayer::Background(0),
        sprite_id: None,
        priority: 0,
    }
}

/// A value `level_pixel` can never itself produce (bounded well under 128
/// by construction: XOR of a value `< 97` and a value `< 61`) — any HUD
/// content painted with this exact pixel found anywhere in the canvas is
/// unambiguous proof of a HUD-exclusion leak, never a coincidental content
/// collision.
fn hud_sentinel_pixel() -> PpuPixel {
    PpuPixel {
        palette_index: 255,
        layer: PixelLayer::Background(0),
        sprite_id: None,
        priority: 0,
    }
}

/// One frame's raw (wrapping) hardware scroll register value for the
/// world (non-HUD) band — walks past the X wrap boundary (512) partway
/// through a 40-frame run, so the fixture exercises wraparound, not just
/// steady small deltas.
fn raw_world_scroll_for_frame(frame: u32) -> (u16, u16) {
    let x = (490 + frame * 3) % 512;
    let y = frame / 4; // slow vertical drift, well under the 480 wrap
    (x as u16, y as u16)
}

/// Test-local (deliberately NOT imported from `rf_enhance`) wraparound
/// unwrap, matching `WorldAxis::advance`'s algorithm, used only to decide
/// what content `build_log`'s video buffer should show at each row — see
/// module doc for why duplicating rather than importing matters here.
fn unwrap_delta(state: &mut Option<(i64, i64)>, raw: u16, modulus: i64) -> i64 {
    let raw = i64::from(raw).rem_euclid(modulus);
    let world = match *state {
        Some((prev_world, prev_raw)) => {
            let mut delta = (raw - prev_raw).rem_euclid(modulus);
            if delta > modulus / 2 {
                delta -= modulus;
            }
            prev_world + delta
        }
        None => raw,
    };
    *state = Some((world, raw));
    world
}

/// Builds the full, ordered per-frame event log (module doc: emission
/// order matches `rf-nes`'s own verified real ordering) plus the matching
/// video buffer for that frame.
fn build_log(frames: u32) -> Vec<(Vec<CoreEvent>, Vec<PpuPixel>)> {
    let mut log = Vec::with_capacity(frames as usize);
    let mut world_x_state: Option<(i64, i64)> = None;
    let mut world_y_state: Option<(i64, i64)> = None;

    for frame in 0..frames {
        let (raw_world_x, raw_world_y) = raw_world_scroll_for_frame(frame);
        let world_x = unwrap_delta(&mut world_x_state, raw_world_x, 512);
        let world_y_at_split = unwrap_delta(&mut world_y_state, raw_world_y, 480);

        let mut events = vec![CoreEvent::ScrollWrite {
            x: 0,
            y: 0,
            layer: PixelLayer::Background(0),
        }]; // HUD reset (vblank $2005, unconditional emission)
        events.push(CoreEvent::FrameEnd);
        events.push(CoreEvent::FrameStart);
        for sy in 0..HUD_ROWS {
            events.push(CoreEvent::Scanline(sy));
        }
        events.push(CoreEvent::ScrollWrite {
            x: raw_world_x,
            y: raw_world_y,
            layer: PixelLayer::Background(0),
        }); // the $2006-driven mid-frame split
        for sy in HUD_ROWS..HEIGHT {
            events.push(CoreEvent::Scanline(sy));
        }
        events.push(CoreEvent::VblankStart);

        let mut video = vec![
            PpuPixel {
                palette_index: 0,
                layer: PixelLayer::Backdrop,
                sprite_id: None,
                priority: 0,
            };
            WIDTH as usize * HEIGHT as usize
        ];
        // HUD rows: a sentinel pixel value `level_pixel` can never itself
        // produce (that function's output is bounded well under 128 by
        // construction -- XOR of a value < 97 and a value < 61), so any
        // sentinel found in the canvas is unambiguously a leak, never a
        // coincidental content collision.
        for sy in 0..HUD_ROWS {
            for sx in 0..WIDTH {
                video[sy as usize * WIDTH as usize + sx as usize] = hud_sentinel_pixel();
            }
        }
        // World rows: the fixed level texture at each row's actual world
        // position (x constant across the band; y increments per row from
        // world_y_at_split, matching `ScanlineBand::y_at`'s own derivation).
        for sy in HUD_ROWS..HEIGHT {
            let world_y = world_y_at_split + i64::from(sy - HUD_ROWS);
            for sx in 0..WIDTH {
                let wx = world_x + i64::from(sx);
                video[sy as usize * WIDTH as usize + sx as usize] = level_pixel(wx, world_y);
            }
        }

        log.push((events, video));
    }
    log
}

/// Runs the full log through the real bus + tracker + stitcher pipeline
/// once, returning the final canvas AND, per frame, the world band with
/// the largest span (the "primary"/world band `ScrollTracker` itself
/// selected) — exposed so a test can check the tracker's own computed
/// world position directly, not infer it indirectly through canvas
/// content (module doc's "not vacuous" concern cuts both ways: an
/// indirect content check can pass by coincidence when consecutive
/// frames' bands overlap heavily in world space, exactly the failure mode
/// found while building this fixture — see
/// `a_second_independent_pipeline_instance_computes_the_exact_predicted_world_position`'s
/// own doc).
fn run_pipeline(
    log: &[(Vec<CoreEvent>, Vec<PpuPixel>)],
) -> (Canvas, Vec<rf_enhance::scroll_tracker::BandWorldPos>) {
    let (mut writer, reader) = triple_buffer(FrameBundle::empty());
    let subscriber =
        FrameSubscriber::new(reader, EventMask::SCANLINE.union(EventMask::SCROLL_WRITE));
    let mut tracker = ScrollTracker::new();
    let mut canvas = Canvas::new();
    let mut primary_positions = Vec::with_capacity(log.len());

    for (frame_count, (events, video)) in log.iter().enumerate() {
        let bundle = FrameBundle::new(
            frame_count as u64,
            WIDTH,
            HEIGHT,
            video.clone(),
            events.clone(),
        );
        writer.publish(bundle);
        let subscribed = subscriber.subscribed_events();
        let bands = tracker.observe_frame(&subscribed);
        let latest: Arc<FrameBundle> = subscriber.latest();
        stitch_frame(&mut canvas, &latest.video, WIDTH, &bands);

        let primary = bands
            .iter()
            .max_by_key(|(b, _)| b.end - b.start)
            .map(|(_, pos)| *pos)
            .expect("every frame in this fixture has at least one band");
        primary_positions.push(primary);
    }
    (canvas, primary_positions)
}

/// Deterministic (not `std::collections::HashMap`-based — `Canvas`'s own
/// doc explains why that would be a hazard here) checksum over the whole
/// canvas, for the "verify byte-identical by checksum" bar this ticket's
/// mutation-testing step is held to.
fn canvas_checksum(canvas: &Canvas) -> u64 {
    let mix = |h: u64, byte: u8| -> u64 { (h ^ u64::from(byte)).wrapping_mul(1_099_511_628_211) };
    let mut h: u64 = 1_469_598_103_934_665_603; // FNV-1a offset basis
    let (ox, oy) = canvas.origin();
    for b in ox.to_le_bytes() {
        h = mix(h, b);
    }
    for b in oy.to_le_bytes() {
        h = mix(h, b);
    }
    for y in 0..canvas.height() as i64 {
        for x in 0..canvas.width() as i64 {
            match canvas.get(ox + x, oy + y) {
                Some(px) => {
                    h = mix(h, 1);
                    h = mix(h, px.palette_index);
                }
                None => h = mix(h, 0),
            }
        }
    }
    h
}

#[test]
fn same_input_log_produces_a_byte_identical_canvas() {
    let log = build_log(40);
    let (canvas_a, positions_a) = run_pipeline(&log);
    let (canvas_b, positions_b) = run_pipeline(&log);

    assert_eq!(
        canvas_a, canvas_b,
        "two runs of the identical input log must produce a byte-identical canvas"
    );
    assert_eq!(
        positions_a, positions_b,
        "two runs must compute identical per-frame world positions too, not just an \
         identical-by-coincidence final canvas"
    );
    assert_eq!(
        canvas_checksum(&canvas_a),
        canvas_checksum(&canvas_b),
        "checksum must agree too, not just derived PartialEq"
    );

    // Anti-vacuity: real, varied, wrap-spanning content -- not an empty or
    // uniform structure two runs trivially agree on.
    assert!(
        canvas_a.width() > WIDTH as usize,
        "40 frames of scrolling plus a wrap must grow the canvas past one screen width, got {}",
        canvas_a.width()
    );
    let (ox, oy) = canvas_a.origin();
    let mut distinct_palette_indices = std::collections::BTreeSet::new();
    for y in 0..canvas_a.height() as i64 {
        for x in 0..canvas_a.width() as i64 {
            if let Some(px) = canvas_a.get(ox + x, oy + y) {
                distinct_palette_indices.insert(px.palette_index);
            }
        }
    }
    assert!(
        distinct_palette_indices.len() > 10,
        "stitched content must be varied, not a uniform fill: {} distinct values",
        distinct_palette_indices.len()
    );
}

#[test]
fn hud_rows_never_appear_in_the_canvas() {
    let log = build_log(10);
    let (canvas, _) = run_pipeline(&log);
    let (ox, oy) = canvas.origin();
    let sentinel = hud_sentinel_pixel();
    for y in 0..canvas.height() as i64 {
        for x in 0..canvas.width() as i64 {
            assert_ne!(
                canvas.get(ox + x, oy + y),
                Some(sentinel),
                "HUD sentinel content must never reach the canvas at world ({x}, {y})"
            );
        }
    }
}

/// The direct, non-inferred check the fixture's own construction needs:
/// earlier versions of this test compared canvas CONTENT at a predicted
/// world coordinate against expectation, and that check kept passing under
/// a wraparound-heuristic mutation it was meant to catch — because
/// consecutive frames' bands span 224 world-Y rows each while the fixture
/// only drifts vertically by ~1 row every 4 frames, so heavily overlapping
/// EARLIER (correct) frames' content satisfied a LATER (mutated) frame's
/// prediction by coincidence, `level_pixel` being a pure function of world
/// coordinates alone with no frame-identity dependence. Checking
/// `ScrollTracker`'s own returned [`rf_enhance::scroll_tracker::BandWorldPos`]
/// directly sidesteps that: there is no canvas indirection left to produce
/// a false pass.
#[test]
fn scroll_tracker_computes_the_exact_predicted_world_position_every_frame() {
    let log = build_log(20);
    let (_, positions) = run_pipeline(&log);

    let mut world_x_state: Option<(i64, i64)> = None;
    let mut world_y_state: Option<(i64, i64)> = None;
    for (frame, pos) in positions.iter().enumerate() {
        let (raw_x, raw_y) = raw_world_scroll_for_frame(frame as u32);
        let expected_world_x = unwrap_delta(&mut world_x_state, raw_x, 512);
        let expected_world_y = unwrap_delta(&mut world_y_state, raw_y, 480);
        assert_eq!(
            pos.world_x, expected_world_x,
            "frame {frame}: ScrollTracker's own computed world_x diverged from the \
             independently-predicted wraparound-unwrapped value"
        );
        assert_eq!(
            pos.world_y_at_start, expected_world_y,
            "frame {frame}: ScrollTracker's own computed world_y diverged"
        );
    }
}

/// Informational timing, not a criterion benchmark: this ticket cannot
/// measure through the real core thread (`retroforge`, the app shell
/// that owns it, is out of write scope, and no `EnhancementRuntime` wires
/// `ScrollTracker`/`Stitcher` into a running core anywhere yet -- module
/// doc's "why this doesn't drive a real rf-nes core" applies here too).
/// What CAN be measured honestly is this crate's own per-frame cost, on
/// real 256x240 video buffers through the real event/bus pipeline, wall
/// clock (fine in a `tests/` file -- `scripts/validate-arch.sh` rule 4's
/// determinism lint only scans `crates/*/src`).
///
/// W14-44: this used to time a single `run_pipeline` call and assert
/// `< 10 ms`/frame, which failed twice under `cargo test --workspace`
/// with sibling `cargo build`s saturating the machine (2.73 s measured,
/// ~22.75 ms/frame, vs 0.45 s / ~3.75 ms/frame alone) while passing 3/3
/// in isolation -- not a code regression, a scheduler-contention false
/// positive. Two other fixes were considered and rejected:
/// - **Process/thread CPU time** (`CLOCK_THREAD_CPUTIME_ID` via `libc`)
///   would be immune to preemption, but `rf-enhance`'s `write_scope` is
///   `crates/rf-enhance/**` + `docs/TESTING.md`; adding a new direct
///   dependency rewrites `rf-enhance`'s entry in the root `Cargo.lock`,
///   which is out of scope. `std` has no CPU-time clock.
/// - **Widening the wall-clock budget** loses sensitivity outright:
///   0.45 s / 120 frames is already only a 2.7x margin under 10 ms, so
///   widening far enough to survive a 6x contention spike (as measured
///   above) would stop catching a real 5x regression.
///
/// So: **best of N wall-clock repetitions**, same 10 ms/frame budget.
/// `run_pipeline` reads no global/static state and spawns no threads
/// (confirmed by inspection), so re-running it repeatedly measures the
/// same cost each time with no cross-call interference. The loop below
/// (law 8: bounded `for _ in 0..MAX_REPS`, unconditional advance, no
/// hand-rolled index) breaks the instant one repetition beats the
/// budget, so the common uncontended case still costs exactly one
/// ~0.45 s repetition; only a run unlucky enough to be preempted on
/// every one of `MAX_REPS` tries pays for all of them. A single
/// transient scheduling hiccup can no longer fail this test; a real
/// systemic regression still fails every repetition and is still
/// caught.
#[test]
fn per_frame_tracking_and_stitching_cost_fits_comfortably_in_the_frame_budget() {
    const MAX_REPS: u32 = 5;
    const BUDGET_MS: f64 = 10.0;

    let log = build_log(120);
    let mut best_per_frame_ms = f64::INFINITY;
    let mut last_canvas_dims = (0usize, 0usize);

    for rep in 0..MAX_REPS {
        let start = std::time::Instant::now();
        let (canvas, _) = run_pipeline(&log);
        let elapsed = start.elapsed();
        let per_frame_ms = elapsed.as_secs_f64() * 1000.0 / log.len() as f64;
        last_canvas_dims = (canvas.width(), canvas.height());
        eprintln!(
            "per_frame_tracking_and_stitching_cost: rep {rep} = {per_frame_ms:.4} ms/frame \
             over {} frames",
            log.len(),
        );
        if per_frame_ms < best_per_frame_ms {
            best_per_frame_ms = per_frame_ms;
        }
        if best_per_frame_ms < BUDGET_MS {
            break;
        }
    }

    eprintln!(
        "per_frame_tracking_and_stitching_cost: best of up to {MAX_REPS} = \
         {best_per_frame_ms:.4} ms/frame (final canvas {}x{})",
        last_canvas_dims.0, last_canvas_dims.1
    );
    assert!(
        best_per_frame_ms < BUDGET_MS,
        "tracking+stitching cost {best_per_frame_ms:.4} ms/frame (best of {MAX_REPS}) exceeds \
         the {BUDGET_MS} ms loose bound (16.67 ms is the whole-frame budget at 60 fps)"
    );
}
