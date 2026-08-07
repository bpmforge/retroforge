//! Asset cache: decoded tiles/maps/AI outputs keyed by rom+asset hash
//!
//! See /docs/MODULE_DESIGN.md and /docs/design/ for the contract this crate
//! must implement. Do not add public API here without a ticket in plan.json.

/// Crate marker used by the test harness to confirm workspace wiring.
pub const CRATE_NAME: &str = "rf-cache";

/// The payload shape a persisted stitched-canvas chunk will eventually
/// take (ticket W4-03a shapes this type; ticket W4-08 builds the real
/// cache — keying, eviction, on-disk format, the FM-13 allocation guard —
/// and ticket W4-03b is the one that actually constructs and persists
/// values of this shape, per `docs/design/ENHANCEMENT_RUNTIME.md` §3:
/// "Canvas chunks are cached in `rf-cache` keyed by ROM hash + scene id,
/// so a revisited level restores instantly across sessions").
///
/// Deliberately **not populated or constructed anywhere in this crate or
/// `rf-enhance` yet** — `rf_enhance::stitcher::Canvas` is this ticket's
/// real, in-memory, per-session working canvas, and nothing currently
/// converts one into a [`CanvasChunk`]. No key type accompanies this
/// struct either: "keyed by ROM hash + scene id" is scene-identity work
/// (`docs/design/ENHANCEMENT_RUNTIME.md` §3's "perceptual hash of
/// framebuffer edges + mapper bank state") this ticket's own scope fence
/// explicitly defers to W4-03b, so guessing at a key type here would be
/// exactly the "unvalidated scaffolding" this crate's module doc above
/// already warns against adding without a ticket backing it.
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
