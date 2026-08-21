//! Per-scene canvas accumulation (ticket W4-03e): joins
//! `rf_enhance::scroll_tracker::ScrollTracker` + `scene_tracker::
//! SceneTracker` + `stitcher::{Canvas, stitch_frame}` into the one thing
//! none of them individually are — a stateful "accumulate the level as
//! visited, per scene" pipeline, fed one real `rf_core_api::FrameBundle`
//! at a time. No `egui`/`eframe` dependency (see `crate` root doc's
//! "exactly two modules touch egui" inventory) — [`crate::core_thread`]
//! drives this on the core thread; see that module for why.
//!
//! ## Every frame, not just the ones the UI thread happens to see
//!
//! [`CanvasAccumulator::observe_frame`] MUST be called for every frame the
//! core produces, in order, with no gaps — `FrameBundle::events` carries
//! only THAT frame's `ScrollWrite`/`Scanline` events
//! (`rf_core_api::frame_bundle`'s own doc), so a skipped frame's scroll
//! data is gone, not recoverable from a later frame's bundle. This is
//! exactly why `crate::core_thread::core_thread_main` drives this type
//! directly (every real frame passes through there exactly once,
//! regardless of the UI thread's own repaint cadence) rather than the app
//! shell polling `CoreHandle::frame_bundle`'s triple buffer — that reader
//! is explicitly latest-wins/lossy (`rf_core_api::triple_buffer`'s own
//! doc: "one that polls slower simply misses intermediate frames"), which
//! would silently corrupt both scroll continuity (spurious scene cuts,
//! `MAX_CONTINUOUS_STEP_PX` tripped by a dropped frame's world-position
//! delta, not a real one) and the stitched canvas (holes where a dropped
//! frame's newly-revealed strip never got blitted) under any UI-thread
//! frame drop — which, at typical egui repaint cadence, is common, not
//! rare.
//!
//! ## One canvas per scene, not one canvas total
//!
//! `rf_enhance::stitcher::Canvas`'s own module doc is explicit that it is
//! "NOT keyed by scene identity" and there is "exactly one canvas per
//! `Stitcher` instance" — managing multiple canvases keyed by
//! `SceneTracker`'s id is this ticket's job. A scene cut (room transition,
//! warp) gets a fresh (or persisted-and-restored) `Canvas`, so stitching
//! after a cut can never paint into a different level's world-coordinate
//! space.
use std::collections::HashMap;

use rf_cache::Cache;
use rf_core_api::FrameBundle;
use rf_enhance::persistence::{load_canvas, save_canvas};
use rf_enhance::scene_identity::SceneId;
use rf_enhance::scene_tracker::SceneTracker;
use rf_enhance::scroll_tracker::ScrollTracker;
use rf_enhance::stitcher::{stitch_frame, Canvas};

/// Stateful per-ROM accumulator (module doc). One instance per loaded ROM
/// session, same lifetime shape as `crate::stepper::EmuStepper` itself.
pub struct CanvasAccumulator {
    scroll: ScrollTracker,
    scene_tracker: SceneTracker,
    canvases: HashMap<SceneId, Canvas>,
    /// `None` when no cache root was configured or `rf_cache::Cache::open`
    /// failed (e.g. an unwritable placeholder directory) — persistence is
    /// strictly additive: every acceptance criterion this ticket has is
    /// satisfiable from `canvases` alone, so a missing/failed cache
    /// degrades to "no cross-session restore", never a crash.
    cache: Option<Cache>,
    rom_sha256: String,
    last_scene_id: Option<SceneId>,
}

impl CanvasAccumulator {
    #[must_use]
    pub fn new(rom_sha256: String, cache: Option<Cache>) -> Self {
        CanvasAccumulator {
            scroll: ScrollTracker::new(),
            scene_tracker: SceneTracker::new(),
            canvases: HashMap::new(),
            cache,
            rom_sha256,
            last_scene_id: None,
        }
    }

    /// Feed one frame (module doc: every frame, no gaps). Returns the
    /// scene id this frame was attributed to.
    ///
    /// `mapper_bank_state`: caller-supplied raw bytes, same contract
    /// `rf_enhance::scene_identity::compute_scene_id` and
    /// `SceneTracker::observe_frame` already use — `&[]` for NROM/no-
    /// banking mappers. `crate::core_thread` currently always passes
    /// `&[]` (no `rf-nes` API exposes mapper bank state to this crate
    /// yet — the same inherited gap `rf_enhance::scene_tracker`'s own
    /// module doc names, not a new one this ticket introduces).
    pub fn observe_frame(&mut self, bundle: &FrameBundle, mapper_bank_state: &[u8]) -> SceneId {
        let bands = self.scroll.observe_frame(&bundle.events);
        let scene_id = self.scene_tracker.observe_frame(
            &bundle.video,
            bundle.width,
            bundle.height,
            &bundle.events,
            mapper_bank_state,
        );

        if !self.canvases.contains_key(&scene_id) {
            let restored = self
                .cache
                .as_mut()
                .and_then(|cache| load_canvas(cache, &self.rom_sha256, scene_id).unwrap_or(None));
            self.canvases.insert(scene_id, restored.unwrap_or_default());
        }
        let canvas = self
            .canvases
            .get_mut(&scene_id)
            .expect("just inserted above if it was missing");
        stitch_frame(canvas, &bundle.video, bundle.width, &bands);

        self.last_scene_id = Some(scene_id);
        scene_id
    }

    /// The current scene id, if at least one frame has been observed.
    #[must_use]
    pub fn current_scene_id(&self) -> Option<SceneId> {
        self.last_scene_id
    }

    /// A clone of the current scene's canvas, for a caller to hand off
    /// across a thread boundary (`crate::core_thread::CoreEvent::
    /// CanvasSnapshot`) — cloned only on request, never every frame
    /// (a whole-`Canvas` clone is O(canvas size); at 60Hz that would be
    /// tens of MB/s for a level of any real size, which is why this is a
    /// pull, not a push).
    #[must_use]
    pub fn current_canvas(&self) -> Option<Canvas> {
        self.last_scene_id
            .and_then(|id| self.canvases.get(&id).cloned())
    }

    /// Persist the current scene's canvas to the cache, if one was
    /// configured. Best-effort: a persistence failure degrades to "not
    /// cached this time", never a crash — matches the "cache is strictly
    /// additive" posture the `cache` field doc states.
    pub fn flush(&mut self) {
        let Some(id) = self.last_scene_id else {
            return;
        };
        let Some(cache) = self.cache.as_mut() else {
            return;
        };
        if let Some(canvas) = self.canvases.get(&id) {
            let _ = save_canvas(cache, &self.rom_sha256, id, canvas);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rf_core_api::{CoreEvent, PixelLayer, PpuPixel};

    const WIDTH: u16 = 256;
    const HEIGHT: u16 = 240;

    fn bg(v: u8) -> PpuPixel {
        PpuPixel {
            palette_index: v,
            layer: PixelLayer::Background(0),
            sprite_id: None,
            priority: 0,
        }
    }

    /// A TEXTURED frame (same "world-shifted, XOR-modulo texture" shape
    /// `rf_enhance::scene_tracker`'s own test fixtures use), with a
    /// `ScrollWrite` moving the primary band to world x `shift` — content
    /// and scroll register move together, matching how a real scrolling
    /// frame behaves. A SOLID fill (tried first) is a vacuity trap here:
    /// `rf_enhance::scene_identity::compute_scene_id`'s edge-based
    /// signature is "all flat" for ANY solid-color frame regardless of the
    /// fill value (its own module doc's documented degenerate case), so a
    /// solid-fill fixture cannot discriminate "different scene" from "same
    /// scene, different fill" — exactly the failure this function avoids.
    fn frame_bundle(frame_count: u64, shift: i64, seed: u8) -> FrameBundle {
        let width = WIDTH as usize;
        let height = HEIGHT as usize;
        let mut video = Vec::with_capacity(width * height);
        for y in 0..height {
            for x in 0..width {
                let world_x = x as i64 + shift;
                let v = (world_x.rem_euclid(97) as u8) ^ ((y as i64).rem_euclid(61) as u8) ^ seed;
                video.push(bg(v));
            }
        }
        let events = vec![
            CoreEvent::ScrollWrite {
                x: shift.rem_euclid(512) as u16,
                y: 0,
                layer: PixelLayer::Background(0),
            },
            CoreEvent::Scanline(0),
            CoreEvent::Scanline(HEIGHT - 1),
        ];
        FrameBundle::new(frame_count, WIDTH, HEIGHT, video, events)
    }

    #[test]
    fn a_fresh_accumulator_has_no_current_scene_or_canvas() {
        let acc = CanvasAccumulator::new("rom-a".to_string(), None);
        assert_eq!(acc.current_scene_id(), None);
        assert_eq!(acc.current_canvas(), None);
    }

    #[test]
    fn observing_a_frame_produces_a_scene_and_a_non_empty_canvas() {
        let mut acc = CanvasAccumulator::new("rom-a".to_string(), None);
        let id = acc.observe_frame(&frame_bundle(0, 0, 5), &[]);
        assert_eq!(acc.current_scene_id(), Some(id));
        let canvas = acc
            .current_canvas()
            .expect("a canvas must exist after one frame");
        assert!(canvas.width() > 0 && canvas.height() > 0);
    }

    /// Mutation-shaped: proves `stitch_frame` is actually being called
    /// with the REAL bands from `ScrollTracker`, not e.g. always `(0, 0)`
    /// — a continuous scroll across several frames must accumulate a
    /// canvas WIDER than a single frame's own width, exactly the "whole
    /// level" property this ticket exists to make visible.
    #[test]
    fn a_continuous_scrolling_session_grows_the_canvas_past_one_frames_width() {
        let mut acc = CanvasAccumulator::new("rom-a".to_string(), None);
        for frame in 0..20u64 {
            acc.observe_frame(&frame_bundle(frame, (frame as i64) * 4, 7), &[]);
        }
        let canvas = acc.current_canvas().expect("canvas must exist");
        assert!(
            canvas.width() > WIDTH as usize,
            "20 frames of steady rightward scroll (up to 76px) must accumulate a canvas wider \
             than one frame's own {WIDTH}px, got {}",
            canvas.width()
        );
    }

    /// A genuine scene cut (world position jump far past
    /// `rf_enhance::scene_tracker::MAX_CONTINUOUS_STEP_PX`) must route
    /// subsequent frames into a SEPARATE canvas, not corrupt the
    /// original's world-coordinate space — the "one canvas per scene, not
    /// one canvas total" property this module's doc names.
    #[test]
    fn a_scene_cut_starts_a_separate_canvas_not_corrupting_the_first() {
        let mut acc = CanvasAccumulator::new("rom-a".to_string(), None);
        let id_before = acc.observe_frame(&frame_bundle(0, 0, 3), &[]);
        let canvas_before = acc.current_canvas().unwrap();

        // A raw scroll write far beyond any plausible single-frame scroll
        // rate (300 -> wraps to a -212 world delta, same fixture shape
        // `rf_enhance::scene_tracker`'s own discontinuity test uses) with
        // DIFFERENT fill content, so this is a genuinely different scene.
        let id_after = acc.observe_frame(&frame_bundle(1, 300, 250), &[]);
        assert_ne!(
            id_before, id_after,
            "a scroll jump this large must be treated as a scene cut"
        );
        let canvas_after = acc.current_canvas().unwrap();
        assert_ne!(
            canvas_before, canvas_after,
            "the post-cut canvas must be a different (fresh) one, not the same object"
        );
    }

    #[test]
    fn flush_with_no_cache_configured_is_a_harmless_no_op() {
        let mut acc = CanvasAccumulator::new("rom-a".to_string(), None);
        acc.observe_frame(&frame_bundle(0, 0, 1), &[]);
        acc.flush(); // must not panic
    }

    fn unique_cache_dir(label: &str) -> std::path::PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "retroforge-canvas-accum-test-{label}-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ))
    }

    /// Persistence round trip through a REAL `rf_cache::Cache`, same
    /// "fresh cache over the same root, not the live in-memory object"
    /// discipline `rf_enhance::persistence`'s own vacuity-trap test uses.
    #[test]
    fn flush_then_a_fresh_accumulator_restores_the_canvas_from_disk() {
        let root = unique_cache_dir("roundtrip");
        let cache = rf_cache::Cache::open(&root, 10_000_000).unwrap();

        let mut acc = CanvasAccumulator::new("rom-xyz".to_string(), Some(cache));
        let id = acc.observe_frame(&frame_bundle(0, 0, 9), &[]);
        acc.flush();
        let saved_canvas = acc.current_canvas().unwrap();
        drop(acc); // simulate process exit -- nothing live carries over

        let fresh_cache = rf_cache::Cache::open(&root, 10_000_000).unwrap();
        let mut fresh_acc = CanvasAccumulator::new("rom-xyz".to_string(), Some(fresh_cache));
        // Observing a frame for the SAME scene id must restore from disk
        // before stitching -- feed the exact same first frame again so the
        // stitch is idempotent and the comparison below is meaningful.
        let restored_id = fresh_acc.observe_frame(&frame_bundle(0, 0, 9), &[]);
        assert_eq!(
            restored_id, id,
            "same content must produce the same scene id"
        );
        let restored_canvas = fresh_acc.current_canvas().unwrap();
        assert_eq!(
            restored_canvas, saved_canvas,
            "a fresh accumulator over the same cache root must restore the identical canvas"
        );

        std::fs::remove_dir_all(&root).ok();
    }
}
