//! Automatic heights (tickets W34-01..03; `docs/design/HD2D_PLAN.md`
//! section 2).
//!
//! [`tile_grid`] is the per-frame data the cores' shell builds, [`player`]
//! finds the player and the camera's orientation, [`learner`] turns play into
//! evidence per tile and [`table`] / [`canvas`] keep it (per ROM, per scene)
//! in `rf-cache`. This crate may not
//! depend on `rf-snes` or `rf-nes` (validate-arch rule 3), so decoding a
//! console's tilemap into these types is the mediator's job
//! (`retroforge::tile_capture`); what arrives here is already console-free.

pub mod canvas;
pub mod learner;
pub mod player;
pub mod table;
pub mod tile_grid;

pub use canvas::{canvas_cache_key, load_canvas, save_canvas, HeightCanvas};
pub use learner::{FrameObs, HeightGrid, HeightLearner, Signals, Stats};
pub use player::{
    cluster, Dirs, Entity, FrameInput, Orientation, OrientationGuess, OrientationSource, Player,
    PlayerSource, PlayerTracker, SpriteBox, TrackEvidence,
};
pub use table::{
    load_table, save_table, table_cache_key, Class, Entry, Evidence, Learned, TileHeightTable,
};
pub use tile_grid::{Fnv64, TileCell, TileGrid, TileKey, TileObs};
