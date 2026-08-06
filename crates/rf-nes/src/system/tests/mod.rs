//! `NesBus` test tree — one module per ticket W1-02 acceptance criterion:
//!
//! - `memory_map` — criterion 1, mirroring.
//! - `rom_loading` — criterion 2, NROM via rf-cart.
//! - `oam_dma` — criterion 3, cycle-stealing plus byte order.
//! - `controller` — criterion 4, strobe protocol.
//! - `integration` — the master-clock seam, proven against a real `Cpu`.
//!
//! `nestest` is ticket W1-03's acceptance criterion 2 (FR-CORE-021): the
//! real nestest ROM/log golden-trace diff, gitignored-artifact-absent-skip
//! discipline documented in that module.
//!
//! `mmc3_irq` is ticket W2-03's acceptance criterion 3: the analytic
//! (never-recorded, see that module's own doc) scanline-IRQ assertion.
mod controller;
mod integration;
mod memory_map;
mod mmc3_irq;
mod nestest;
mod oam_dma;
mod rom_loading;

use super::NesRom;

/// Build a minimal synthetic NROM iNES image (mapper 0), matching the
/// layout `crates/rf-cart/src/nes.rs`'s own tests use: magic + 2 header
/// bytes (PRG/CHR bank counts) + flags6/7 + 8 reserved bytes + PRG + CHR
/// payload. `prg_fill` lets callers embed a recognizable byte pattern
/// instead of all-zero PRG data.
pub(super) fn build_nrom_ines(
    prg_banks: u8,
    chr_banks: u8,
    prg_fill: impl Fn(usize) -> u8,
) -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(&rf_cart::nes::INES_MAGIC);
    data.push(prg_banks);
    data.push(chr_banks);
    data.extend_from_slice(&[0u8; 10]); // flags6, flags7, 8 reserved bytes: all zero => mapper 0, iNES 1.0
    for i in 0..(prg_banks as usize * 16 * 1024) {
        data.push(prg_fill(i));
    }
    data.extend(vec![0u8; chr_banks as usize * 8 * 1024]);
    data
}

/// A ready-to-use bus over a 16 KiB NROM image whose PRG byte `i` is
/// `i as u8` (wrapping) — a simple recognizable pattern for mirroring
/// assertions.
pub(super) fn bus_with_pattern_rom(prg_banks: u8, chr_banks: u8) -> super::NesBus {
    let raw = build_nrom_ines(prg_banks, chr_banks, |i| i as u8);
    let rom = NesRom::from_ines_bytes(&raw).expect("valid synthetic NROM image");
    super::NesBus::new(rom)
}
