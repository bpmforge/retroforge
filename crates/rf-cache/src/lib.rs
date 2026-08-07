//! Asset cache: decoded tiles/maps/AI outputs keyed by rom+asset hash
//!
//! See /docs/MODULE_DESIGN.md and /docs/design/ for the contract this crate
//! must implement. Do not add public API here without a ticket in plan.json.
//!
//! Ticket W4-08 adds the actual store: a content-addressed, LRU-bounded
//! cache over opaque payload bytes (`ARCHITECTURE.md` §7's `(rom_sha256,
//! asset_hash, producer, settings_hash)` key). See [`Cache`] and
//! [`CacheKey`]. `Cache::get`/`Cache::put` stay payload-agnostic by design;
//! [`Cache::put_canvas_chunk`]/[`Cache::get_canvas_chunk`] (ticket W4-03b,
//! `canvas_chunk` module) are a typed convenience layered on top, not a
//! change to that contract.

mod canvas_chunk;
mod entry;
mod error;
mod fsutil;
mod index;
mod key;
mod store;

pub use error::CacheError;
pub use key::CacheKey;
pub use store::Cache;

/// Crate marker used by the test harness to confirm workspace wiring.
pub const CRATE_NAME: &str = "rf-cache";

/// The payload shape a persisted stitched-canvas chunk takes (ticket
/// W4-03a shaped this type; ticket W4-08 built the real cache — keying,
/// eviction, on-disk format; ticket W4-03b is what actually constructs and
/// persists values of this shape, via `rf_enhance::persistence` and
/// [`Cache::put_canvas_chunk`]/[`Cache::get_canvas_chunk`], per
/// `docs/design/ENHANCEMENT_RUNTIME.md` §3: "Canvas chunks are cached in
/// `rf-cache` keyed by ROM hash + scene id, so a revisited level restores
/// instantly across sessions"). The key is an ordinary [`CacheKey`] —
/// `rom_sha256` plus `asset_hash` set to the scene id's hex form
/// (`rf_enhance::persistence::canvas_cache_key`); no dedicated key type was
/// needed after all.
///
/// Field shape mirrors `rf_enhance::stitcher::Canvas`'s in-memory layout
/// (flat, row-major, explicit bounds) rather than a sparse/map-based
/// representation, for the same determinism reason that type's own doc
/// gives: a hash-keyed structure has no guaranteed stable iteration order
/// to serialize, a flat buffer does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanvasChunk {
    /// World-space X of this chunk's top-left cell.
    pub origin_x: i64,
    /// World-space Y of this chunk's top-left cell.
    pub origin_y: i64,
    /// Row stride, in cells.
    pub width: u32,
    /// Row count.
    pub height: u32,
    /// Row-major indexed pixels, `width * height` long, `None` where the
    /// chunk has an unvisited gap (ENHANCEMENT_RUNTIME §3's honesty
    /// requirement: "shows only visited areas; cannot know unvisited
    /// geometry" — a persisted chunk must preserve that distinction, not
    /// silently fill gaps).
    pub cells: Vec<Option<rf_core_api::PpuPixel>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_is_wired() {
        assert_eq!(super::CRATE_NAME, "rf-cache");
    }

    /// Not a behavior test (there is no behavior yet) -- just confirms the
    /// shape is constructible and its unvisited-gap contract (`None`
    /// cells) actually holds, so a later ticket populating this type
    /// starts from a shape that already type-checks against
    /// `rf_core_api::PpuPixel`.
    #[test]
    fn canvas_chunk_is_constructible_with_unvisited_gaps() {
        let chunk = CanvasChunk {
            origin_x: -128,
            origin_y: 0,
            width: 2,
            height: 1,
            cells: vec![None, None],
        };
        assert_eq!(chunk.cells.len(), (chunk.width * chunk.height) as usize);
        assert!(chunk.cells.iter().all(Option::is_none));
    }
}
