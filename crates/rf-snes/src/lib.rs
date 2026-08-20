//! SNES core: 5A22 CPU, PPU1/2, S-SMP/S-DSP, DMA/HDMA, LoROM/HiROM
//!
//! See /docs/MODULE_DESIGN.md and /docs/design/ for the contract this crate
//! must implement. Do not add public API here without a ticket in plan.json.

pub mod bus;
pub mod cpu;
pub mod dma;
pub mod mapping;
pub mod ppu;
pub mod regs;
pub mod system;
pub mod timing;

pub use bus::SnesBus;
pub use mapping::{map, Target};
pub use ppu::Ppu;
pub use system::SnesSystem;

/// Crate marker used by the test harness to confirm workspace wiring.
pub const CRATE_NAME: &str = "rf-snes";

#[cfg(test)]
mod tests;
