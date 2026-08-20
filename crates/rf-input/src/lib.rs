//! Keyboard input, mapping, and deterministic input log (ticket W1-07).
//!
//! Host-agnostic by construction (`scripts/validate-arch.sh` rule 3: this
//! crate must not depend on `rf-nes`/`rf-snes`; it depends on
//! `rf-core-api` only, for [`rf_core_api::InputFrame`]). It also has no
//! `egui`/`eframe` dependency — the `egui::Key -> rf_input::Key`
//! translation table lives in `crates/retroforge/src/input_map.rs`, the one
//! other module in that crate allowed to touch `egui` besides `app.rs`.
//!
//! Two different things are both called a "latch" in this project — see
//! [`InputLatch`]'s doc for which one this crate implements (FR-FE-003's
//! host-side per-frame latch), and how it differs from the NES
//! controller's own hardware strobe latch (`rf_nes::system::controller`,
//! out of scope here).
//!
//! See `docs/design/SAVE_STATES.md` §3 and [`replay`] for the `.rfreplay`
//! input-log format (FR-STATE-006).
pub mod bindings;
mod button;
#[cfg(feature = "gilrs")]
mod gilrs_backend;
mod key;
mod keymap;
mod latch;
pub mod pad;
pub mod replay;
mod snes_button;

pub use bindings::{BindingError, BindingWarning, Bindings};
pub use button::{Button, NesButton};
#[cfg(feature = "gilrs")]
pub use gilrs_backend::GilrsBackend;
pub use key::Key;
pub use keymap::KeyMap;
pub use latch::InputLatch;
pub use pad::{PadBackend, PadButton, PadEvent, PadId, PadMap, PadRouter};
pub use replay::{
    PortLogKey, ReplayError, ReplayHeader, ReplayLog, ReplayPlayer, ReplayRecorder, StartType,
};
pub use snes_button::SnesButton;

/// Crate marker used by the test harness to confirm workspace wiring.
pub const CRATE_NAME: &str = "rf-input";

#[cfg(test)]
mod tests {
    #[test]
    fn crate_is_wired() {
        assert_eq!(super::CRATE_NAME, "rf-input");
    }
}
