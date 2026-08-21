//! Future AI pipeline hooks: async jobs, local-first, cache-backed (stub)
//!
//! See /docs/MODULE_DESIGN.md and /docs/design/ for the contract this crate
//! must implement. Do not add public API here without a ticket in plan.json.

pub mod animation;
pub mod pack;
pub mod pipeline;
pub mod upscale;

#[cfg(feature = "onnx")]
pub mod onnx;

/// Crate marker used by the test harness to confirm workspace wiring.
pub const CRATE_NAME: &str = "rf-ai";

#[cfg(test)]
mod tests {
    #[test]
    fn crate_is_wired() {
        assert_eq!(super::CRATE_NAME, "rf-ai");
    }
}
