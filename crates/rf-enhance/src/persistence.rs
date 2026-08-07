//! Canvas persistence through `rf-cache` (ticket W4-03b;
//! `docs/design/ENHANCEMENT_RUNTIME.md` §3: "Canvas chunks are cached in
//! `rf-cache` keyed by ROM hash + scene id, so a revisited level restores
//! instantly across sessions").
//!
//! `rf_cache::CanvasChunk` was shaped by W4-03a and left unconstructed —
//! this module is what actually builds one (from
//! [`crate::stitcher::Canvas`]) and saves/restores it through a
//! [`rf_cache::Cache`], keyed by ROM hash plus
//! [`crate::scene_identity::SceneId`].

use rf_cache::{Cache, CacheError, CacheKey, CanvasChunk};

use crate::scene_identity::SceneId;
use crate::stitcher::Canvas;

/// Fixed producer identity for the deterministic (non-AI) stitcher —
/// `rf_cache::CacheKey::producer`'s own doc: "or a fixed name for a
/// deterministic (non-AI) producer such as the map stitcher".
const PRODUCER: &str = "rf-enhance-stitcher";

/// Chunk wire-format version tag. Bumping this on a future incompatible
/// change makes old cached canvases miss cleanly (a different
/// `settings_hash` is just a cache miss) rather than risk misinterpreting
/// old bytes under a changed wire format.
const SETTINGS_HASH: &str = "canvas-chunk-v1";

/// The cache key a canvas for `rom_sha256` at `scene_id` is stored under.
#[must_use]
pub fn canvas_cache_key(rom_sha256: &str, scene_id: SceneId) -> CacheKey {
    CacheKey {
        rom_sha256: rom_sha256.to_string(),
        asset_hash: scene_id.to_hex(),
        producer: PRODUCER.to_string(),
        settings_hash: SETTINGS_HASH.to_string(),
    }
}

/// Export a live [`Canvas`] to the persistable [`CanvasChunk`] shape,
/// preserving origin, bounds, and the `None`/`Some` per-cell distinction
/// exactly (W4-08's persistence lesson — dropping origin/bounds shifts
/// world coordinates silently on the next session; collapsing `None` to a
/// filled cell is the fog dishonesty this ticket's brief names
/// explicitly).
#[must_use]
pub fn canvas_to_chunk(canvas: &Canvas) -> CanvasChunk {
    let (origin_x, origin_y) = canvas.origin();
    CanvasChunk {
        origin_x,
        origin_y,
        width: canvas.width() as u32,
        height: canvas.height() as u32,
        cells: canvas.cells().to_vec(),
    }
}

/// The inverse of [`canvas_to_chunk`].
#[must_use]
pub fn chunk_to_canvas(chunk: &CanvasChunk) -> Canvas {
    Canvas::from_raw_parts(
        chunk.origin_x,
        chunk.origin_y,
        chunk.width as usize,
        chunk.height as usize,
        chunk.cells.clone(),
    )
}

/// Persist `canvas` for `(rom_sha256, scene_id)` into `cache`.
///
/// # Errors
/// Propagates any [`CacheError`] from the underlying `rf_cache::Cache::put`
/// (e.g. an I/O or containment failure).
pub fn save_canvas(
    cache: &mut Cache,
    rom_sha256: &str,
    scene_id: SceneId,
    canvas: &Canvas,
) -> Result<(), CacheError> {
    let key = canvas_cache_key(rom_sha256, scene_id);
    let chunk = canvas_to_chunk(canvas);
    cache.put_canvas_chunk(&key, &chunk)
}

/// Restore a previously-[`save_canvas`]d canvas for `(rom_sha256,
/// scene_id)`, if present. `Ok(None)` on a cache miss (never an error —
/// matches `rf_cache::Cache::get`'s own contract).
///
/// # Errors
/// Propagates any [`CacheError`] from the underlying `rf_cache::Cache::get`.
pub fn load_canvas(
    cache: &mut Cache,
    rom_sha256: &str,
    scene_id: SceneId,
) -> Result<Option<Canvas>, CacheError> {
    let key = canvas_cache_key(rom_sha256, scene_id);
    Ok(cache
        .get_canvas_chunk(&key)?
        .map(|chunk| chunk_to_canvas(&chunk)))
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
            dropped_by_limit: false,
        }
    }

    /// Matches `rf_cache::store`'s own test helper pattern exactly: pid +
    /// wall-clock + atomic sequence, deliberately NOT canonicalized here
    /// (`Cache::open` does that itself) -- `entry.rs`'s variant
    /// canonicalizes and is the wrong one to copy for a `Cache::open`
    /// caller (`rf-cache/src/store.rs`'s own test module comment explains
    /// why: macOS's `/var` -> `/private/var` symlink).
    fn unique_dir(label: &str) -> std::path::PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "rf-enhance-persistence-test-{label}-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ))
    }

    /// A canvas with a NON-zero, asymmetric origin and NON-square bounds
    /// -- deliberately, so a mutation that drops origin/bounds (hardcodes
    /// `(0,0)`, or transposes width/height) is actually caught: a
    /// zero-origin or square-bounds fixture would let such a mutation
    /// through by coincidence.
    fn build_canvas() -> Canvas {
        let mut canvas = Canvas::new();
        // World rect: x in [-37, -32), y in [12, 15) -- origin (-37, 12),
        // width 5, height 3.
        canvas.blit_row(
            -37,
            12,
            &[Some(bg(1)), None, Some(bg(3)), Some(bg(4)), None],
        );
        canvas.blit_row(
            -37,
            13,
            &[None, Some(bg(6)), None, Some(bg(8)), Some(bg(9))],
        );
        canvas.blit_row(-37, 14, [Some(bg(10)); 5].as_slice());
        canvas
    }

    #[test]
    fn canvas_to_chunk_round_trip_preserves_origin_bounds_and_cells() {
        let canvas = build_canvas();
        let chunk = canvas_to_chunk(&canvas);
        assert_eq!(chunk.origin_x, -37);
        assert_eq!(chunk.origin_y, 12);
        assert_eq!(chunk.width, 5);
        assert_eq!(chunk.height, 3);

        let restored = chunk_to_canvas(&chunk);
        assert_eq!(restored, canvas);
    }

    /// The persistence vacuity trap (ticket brief, trap 4): proves the
    /// restore came from DISK, constructing a genuinely fresh `Cache` over
    /// the same root -- not merely reusing the live in-memory object --
    /// and checks origin/bounds survive, not just "the canvas is still
    /// there".
    #[test]
    fn save_then_reopen_a_fresh_cache_restores_origin_bounds_and_cells() {
        let root = unique_dir("roundtrip");
        let canvas = build_canvas();
        let scene_id = SceneId(0xDEAD_BEEF_u64);

        {
            let mut cache = Cache::open(&root, 10_000_000).unwrap();
            save_canvas(&mut cache, "rom-abc", scene_id, &canvas).unwrap();
        } // cache dropped -- simulates process exit

        let mut fresh_cache = Cache::open(&root, 10_000_000).unwrap();
        let restored = load_canvas(&mut fresh_cache, "rom-abc", scene_id)
            .unwrap()
            .expect("a saved canvas must be found by a freshly opened cache");

        assert_eq!(
            restored.origin(),
            (-37, 12),
            "origin must survive a disk round trip"
        );
        assert_eq!(restored.width(), 5, "width must survive a disk round trip");
        assert_eq!(
            restored.height(),
            3,
            "height must survive a disk round trip"
        );
        assert_eq!(
            restored, canvas,
            "cells (including None gaps) must survive exactly"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn load_on_a_never_saved_scene_is_none_not_error() {
        let root = unique_dir("miss");
        let mut cache = Cache::open(&root, 10_000_000).unwrap();
        let result = load_canvas(&mut cache, "rom-xyz", SceneId(1)).unwrap();
        assert!(result.is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    /// Different scene ids for the same ROM must not collide.
    #[test]
    fn distinct_scene_ids_persist_independently() {
        let root = unique_dir("distinct-scenes");
        let mut cache = Cache::open(&root, 10_000_000).unwrap();
        let canvas_a = build_canvas();
        let mut canvas_b = Canvas::new();
        canvas_b.blit_row(0, 0, &[Some(bg(99))]);

        save_canvas(&mut cache, "rom-abc", SceneId(1), &canvas_a).unwrap();
        save_canvas(&mut cache, "rom-abc", SceneId(2), &canvas_b).unwrap();

        let restored_a = load_canvas(&mut cache, "rom-abc", SceneId(1))
            .unwrap()
            .unwrap();
        let restored_b = load_canvas(&mut cache, "rom-abc", SceneId(2))
            .unwrap()
            .unwrap();
        assert_eq!(restored_a, canvas_a);
        assert_eq!(restored_b, canvas_b);

        std::fs::remove_dir_all(&root).ok();
    }
}
