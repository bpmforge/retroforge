//! Shared test-only support code for rf-ai's `#[ignore]`d benchmark specs
//! (ticket W16-01). `pub mod minijson` is a verbatim copy of
//! `crates/rf-renderer/src/bin/bench_passes/minijson.rs` -- see that
//! file's module doc for why it is duplicated rather than shared through
//! a crate this ticket's write_scope does not span.
pub mod minijson;
