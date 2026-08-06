//! NES core: 2A03 CPU, PPU, APU, mappers.
//!
//! See /docs/MODULE_DESIGN.md and /docs/design/ for the contract this crate
//! must implement. Do not add public API here without a ticket in plan.json.
//!
//! Dependency note (ticket W1-02): this crate depends on `rf-cart` for
//! cartridge *metadata* (parsed iNES/NES 2.0 header, hashes) only — it does
//! not hand over PRG/CHR bytes, so `system::cartridge` slices those out of
//! the raw ROM image itself. `rf-cart` is a peer core crate, not an upper
//! layer, so this is exempt from `scripts/validate-arch.sh` rule 1 (see
//! that script's `CORE_CRATES`/`FORBIDDEN` lists).
//!
//! Dependency note (ticket W1-04a): this crate also depends on
//! `rf-core-api` for the cross-core contract types [`ppu`] emits pixels
//! through (`CoreSink`/`PpuPixel`/`PixelLayer`) — also exempt from rule 1,
//! since `rf-core-api` is the shared contract crate every core is expected
//! to depend on, not an upper layer.

pub mod cpu;
pub mod mappers;
pub mod ppu;
pub mod system;
pub mod trace;

pub use cpu::{Cpu, CpuBus};
pub use mappers::Mapper;
pub use ppu::Ppu;
pub use system::{NesBus, NesLoadError, NesRom};
pub use trace::{format_trace_line, TracePeek};

/// Crate marker used by the test harness to confirm workspace wiring.
pub const CRATE_NAME: &str = "rf-nes";

#[cfg(test)]
mod tests {
    #[test]
    fn crate_is_wired() {
        assert_eq!(super::CRATE_NAME, "rf-nes");
    }
}
