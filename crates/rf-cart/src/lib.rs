//! Cartridge loading: iNES/NES2.0/SNES headers, hashing, mapper detection
//!
//! See /docs/MODULE_DESIGN.md and /docs/design/ for the contract this crate
//! must implement. Do not add public API here without a ticket in plan.json.
//!
//! Scope (ticket W0-02): header parsing + normalization + hashing +
//! mapper/chip detection for NES (iNES/NES 2.0) and SNES (LoROM/HiROM).
//! Mapper *behavior* lives in `rf-nes`/`rf-snes`; this crate only detects
//! and, for anything not yet implemented there, fails with a diagnostic
//! naming the mapper/chip instead of half-booting (FR-CORE-013).

pub mod error;
pub mod hash;
pub mod nes;
pub mod snes;

pub use error::CartError;
pub use hash::{RomHashes, RomIdentity};
pub use nes::{Mirroring, NesFormat, NesHeader};
pub use snes::{Coprocessor, DspWindow, Sa1Board, SnesHeader, SnesMapMode, SA1_IRAM_LEN};

/// Crate marker used by the test harness to confirm workspace wiring.
pub const CRATE_NAME: &str = "rf-cart";

/// A parsed and hashed cartridge image, console-tagged.
///
/// `Cartridge::load` is the one-shot entry point: sniff the format, parse
/// the header, and hash the raw + normalized image in one pass. The
/// underlying `nes`/`snes`/`hash` functions stay public for callers that
/// already know the console or want the pieces separately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cartridge {
    Nes {
        header: NesHeader,
        identity: RomIdentity,
    },
    Snes {
        header: SnesHeader,
        identity: RomIdentity,
    },
}

impl Cartridge {
    /// Detect NES vs SNES from content (iNES/NES 2.0 have a fixed magic;
    /// SNES has none, so it's the fallback — never trust a file
    /// extension), parse the header, and hash the normalized + raw image.
    pub fn load(raw: &[u8]) -> Result<Cartridge, CartError> {
        if raw.len() >= 4 && raw[0..4] == nes::INES_MAGIC {
            let header = nes::parse_nes_header(raw)?;
            let identity = hash::identity_nes(raw);
            Ok(Cartridge::Nes { header, identity })
        } else {
            let header = snes::parse_snes_header(raw)?;
            let identity = hash::identity_snes(raw);
            Ok(Cartridge::Snes { header, identity })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_is_wired() {
        assert_eq!(super::CRATE_NAME, "rf-cart");
    }

    #[test]
    fn cartridge_load_dispatches_to_nes_parser_on_ines_magic() {
        // Minimal valid NROM image: 1x16KB PRG, 1x8KB CHR.
        let mut rom = Vec::new();
        rom.extend_from_slice(&nes::INES_MAGIC);
        rom.extend_from_slice(&[1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        rom.extend(vec![0u8; 16 * 1024 + 8 * 1024]);

        let cart = Cartridge::load(&rom).expect("valid NROM cartridge");
        match cart {
            Cartridge::Nes { header, .. } => assert_eq!(header.mapper, 0),
            Cartridge::Snes { .. } => panic!("expected NES cartridge"),
        }
    }

    #[test]
    fn cartridge_load_falls_back_to_snes_parser_without_ines_magic() {
        let mut rom = vec![0u8; 0x8000];
        let base = 0x7FC0;
        rom[base + 0x15] = 0x20; // LoROM, slow
        rom[base + 0x16] = 0x00; // ROM only
        rom[base + 0x17] = 5; // 1<<5 KB = 32 KB
        rom[base + 0x18] = 0;
        let checksum: u16 = 0x1234;
        let complement = checksum ^ 0xFFFF;
        rom[base + 0x1C..base + 0x1E].copy_from_slice(&complement.to_le_bytes());
        rom[base + 0x1E..base + 0x20].copy_from_slice(&checksum.to_le_bytes());
        rom[base + 0x3C..base + 0x3E].copy_from_slice(&0x8000u16.to_le_bytes());

        let cart = Cartridge::load(&rom).expect("valid LoROM cartridge");
        match cart {
            Cartridge::Snes { header, .. } => assert_eq!(header.map_mode, SnesMapMode::LoRom),
            Cartridge::Nes { .. } => panic!("expected SNES cartridge"),
        }
    }
}
