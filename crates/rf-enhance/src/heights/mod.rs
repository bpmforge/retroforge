//! Automatic heights, data half (ticket W34-01; `docs/design/HD2D_PLAN.md`
//! section 2.2).
//!
//! Only the pure data the learner will read lives here so far: the per-frame
//! tile grids the cores' shell builds ([`tile_grid`]). This crate may not
//! depend on `rf-snes` or `rf-nes` (validate-arch rule 3), so decoding a
//! console's tilemap into these types is the mediator's job
//! (`retroforge::tile_capture`); what arrives here is already console-free.

pub mod player;
pub mod tile_grid;

pub use player::{
    cluster, Dirs, Entity, FrameInput, Orientation, OrientationGuess, OrientationSource, Player,
    PlayerSource, PlayerTracker, SpriteBox, TrackEvidence,
};
pub use tile_grid::{Fnv64, TileCell, TileGrid, TileKey, TileObs};
