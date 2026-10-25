//! Rebuild lab core: the code atlas (ticket W36-02;
//! `docs/design/REBUILD_LAB.md` §5-7).
//!
//! Core layer: depends on `rf-core-api` only, never on a console core.
//! The consoles implement [`Isa`]; this crate turns a
//! [`rf_core_api::coverage::CoverageLog`] plus the PRG image into an
//! [`Atlas`] of functions and a control-flow graph, with provenance on
//! everything, and serialises it for the per-ROM cache.

pub mod atlas;
mod build;
pub mod isa;
pub mod superset;
pub mod windows;

pub use atlas::{
    entry, Atlas, AtlasError, Block, Edge, EdgeKind, Function, IndirectSite, InsnRec, Provenance,
    SiteKind, Stats, Target, VolatileRegion, Why, ATLAS_FORMAT_VERSION, CACHE_ASSET,
    CACHE_PRODUCER, CACHE_SETTINGS,
};
pub use build::build_atlas;
pub use isa::{DecodeMode, Decoded, Flow, Isa};
pub use superset::{superset_decode, Superset};
pub use windows::WindowIndex;
