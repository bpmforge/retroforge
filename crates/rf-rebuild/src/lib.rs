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
pub mod collision;
pub mod covio;
pub mod dataregion;
pub mod exec;
pub mod facts;
pub mod hw;
pub mod ir;
pub mod isa;
pub mod naming;
pub mod objtable;
pub mod profile;
pub mod recipe;
pub mod superset;
pub mod verify;
pub mod windows;

pub use atlas::{
    entry, Atlas, AtlasError, Block, Edge, EdgeKind, Function, IndirectSite, InsnRec, Provenance,
    SiteKind, Stats, Target, VolatileRegion, Why, ATLAS_FORMAT_VERSION, CACHE_ASSET,
    CACHE_PRODUCER, CACHE_SETTINGS,
};
pub use build::build_atlas;
pub use covio::{decode_coverage, encode_coverage, COVERAGE_ASSET, COVERAGE_SETTINGS};
pub use exec::{
    Delta, Dispatch, Fetch, FunctionEvent, Node, RebuildExec, TraceEntry, TraceKind, MAX_DELTAS,
    MAX_SHADOW_DEPTH,
};
pub use facts::{merge_facts, FactMerge, FactRefusal};
pub use hw::{HwClass, Platform};
pub use isa::{DecodeMode, Decoded, Flow, Isa, ModeAfter};
pub use recipe::{
    classify_unmatched, match_recipe, resolve_patches, Confidence, DataRegion, EntryKind, Grade,
    HookPoints, Match, MatchReport, ObjectTable, PatchRefusal, PatchRow, RamMeaning, Recipe,
    RecipeFunction, ResolvedPatch, TileClass, TileSemantics, UnmatchedReason,
};
pub use superset::{superset_decode, Superset};
pub use verify::{
    decode_demoted, demoted_ids, encode_demoted, function_key, FunctionKey, Verification,
    VerifyRecord, DEMOTED_ASSET, DEMOTED_SETTINGS, PROVEN_FRAMES, PROVEN_RUNS, VERIFY_ASSET,
    VERIFY_SETTINGS,
};
pub use windows::WindowIndex;
