//! NES core: 2A03 CPU, PPU, APU, mappers (consumes rf-core-api only)
//!
//! See /docs/MODULE_DESIGN.md and /docs/design/ for the contract this crate
//! must implement. Do not add public API here without a ticket in plan.json.

pub mod cpu;

pub use cpu::{Cpu, CpuBus};

/// Crate marker used by the test harness to confirm workspace wiring.
pub const CRATE_NAME: &str = "rf-nes";

#[cfg(test)]
mod tests {
    #[test]
    fn crate_is_wired() {
        assert_eq!(super::CRATE_NAME, "rf-nes");
    }
}
