//! Shared emulator-core traits, bus/event contracts, frame and AV types
//!
//! This is the contract every emulator core implements (`rf-nes`, `rf-snes`,
//! …); the frontend, debugger, enhancement layer and test harness all couple
//! through it rather than through any concrete core. See
//! `docs/ARCHITECTURE.md` §5/§6 and `docs/design/CONTRACTS.md` §1 for the
//! normative spec this module implements (ticket W0-04).
//!
//! Layering law (enforced by `scripts/validate-arch.sh`): this crate depends
//! on no other `rf-*` crate and performs no wall-clock/RNG access — see
//! [`cart`] for how that constrains `EmulatorCore::load`, and
//! `docs/ARCHITECTURE.md` §6 for the determinism model.
//!
//! Do not add public API here without a ticket in plan.json.

mod cart;
mod core;
mod error;
mod event;
mod frame_bundle;
mod input;
mod state_view;
mod triple_buffer;
mod video;

pub use cart::CartImage;
pub use core::{CoreConfig, CoreSink, EmulatorCore, ResetKind, Step, StepResult};
pub use error::{CoreError, StateError};
pub use event::{CoreEvent, EventMask};
pub use frame_bundle::{FrameBundle, FrameBundleBuilder};
pub use input::{InputFrame, MAX_INPUT_PORTS};
pub use state_view::{StateReader, StateView, StateWriter};
pub use triple_buffer::{triple_buffer, TripleBufferReader, TripleBufferWriter};
pub use video::{ColorMathOp, OverlayPixel, PixelLayer, PpuPixel, SubPixel};

/// Crate marker used by the test harness to confirm workspace wiring.
pub const CRATE_NAME: &str = "rf-core-api";

#[cfg(test)]
mod tests {
    #[test]
    fn crate_is_wired() {
        assert_eq!(super::CRATE_NAME, "rf-core-api");
    }
}
