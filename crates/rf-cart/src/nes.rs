//! iNES / NES 2.0 header parsing (FR-CORE-010, FR-CORE-013).
//!
//! Layout reference: [nesdev.org/wiki/INES](https://www.nesdev.org/wiki/INES)
//! and [nesdev.org/wiki/NES_2.0](https://www.nesdev.org/wiki/NES_2.0) — a
//! stable community spec, unchanged for well over a decade. Mapper number
//! list: [nesdev.org/wiki/Mapper](https://www.nesdev.org/wiki/Mapper).

use crate::error::CartError;

/// The four bytes every iNES/NES 2.0 file starts with.
pub const INES_MAGIC: [u8; 4] = [0x4E, 0x45, 0x53, 0x1A]; // "NES\x1A"

const HEADER_LEN: usize = 16;
const TRAINER_LEN: usize = 512;
const PRG_BANK: usize = 16 * 1024;
const CHR_BANK: usize = 8 * 1024;

/// Mapper numbers `rf-nes` currently implements
/// (`docs/design/EMULATION_CORES.md` §3.4). Extend this list in lockstep
/// with whatever ticket lands the next mapper in rf-nes — this table is
/// the enforcement point for FR-CORE-013's "unknown mapper" diagnostic.
/// Mappers this build can run.
///
/// 11, 71, 79 and 206 were added by ticket W14-04, and by measurement
/// rather than by reputation: a census of 1281 real NES archives
/// (2026-09-15) bucketed all 223 refusals by mapper number, and these four
/// were the largest buckets at 54, 17, 28 and 17 games — 116 between them.
const SUPPORTED_MAPPERS: &[u16] = &[
    0, 1, 2, 3, 4, 5, 7, 9, 11, 18, 28, 34, 47, 64, 66, 69, 71, 79, 87, 118, 119, 144, 148, 206,
    232,
];

/// A handful of well-known mapper names, used only to make an
/// unsupported-mapper diagnostic more useful. Not exhaustive — absence
/// from this table does not imply the mapper is supported (see
/// `SUPPORTED_MAPPERS`).
fn mapper_name(id: u16) -> Option<&'static str> {
    Some(match id {
        0 => "NROM",
        1 => "MMC1 / SxROM",
        2 => "UxROM",
        3 => "CNROM",
        4 => "MMC3 / TxROM",
        5 => "MMC5 / ExROM",
        7 => "AxROM",
        9 => "MMC2 / PxROM",
        10 => "MMC4 / FxROM",
        11 => "Color Dreams",
        16 => "Bandai FCG",
        18 => "Jaleco SS88006",
        19 => "Namco 129/163",
        21 | 22 | 23 | 25 => "VRC2/VRC4",
        24 | 26 => "VRC6",
        34 => "BNROM / NINA-001",
        47 => "MMC3 2-in-1 (outer bank)",
        66 => "GxROM",
        69 => "Sunsoft FME-7",
        71 => "Camerica / Codemasters",
        73 => "VRC3",
        75 => "VRC1",
        79 => "NINA-03/06",
        85 => "VRC7",
        118 => "TxSROM",
        119 => "TQROM",
        206 => "DxROM",
        _ => return None,
    })
}

/// Which of the two header formats this cartridge used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NesFormat {
    /// The original (1990s) iNES header.
    INes,
    /// The extended NES 2.0 header (backward-compatible superset).
    Nes2,
}

/// Nametable mirroring as declared by the header (mapper may override at
/// runtime; this is only the header's declared default).
///
/// `OneScreenLower`/`OneScreenUpper` (ticket W2-02) can never come from a
/// header — no iNES/NES 2.0 field encodes them — so `parse_nes_header`
/// below never produces either variant. They exist here only because
/// `rf_nes`'s MMC1 implementation needs *some* `Mirroring` value to return
/// from its own runtime mirroring-control register
/// ([nesdev.org/wiki/MMC1](https://www.nesdev.org/wiki/MMC1)'s control
/// register bits 0-1, values 0/1), and `rf-nes`'s `ppu/mem.rs` is the only
/// place in the workspace that matches on `Mirroring` exhaustively —
/// adding the variants here is safe (verified: `grep -rn "Mirroring::"
/// crates/` before adding, ticket W2-02 pre-flight).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mirroring {
    Horizontal,
    Vertical,
    FourScreen,
    /// All four logical nametables alias the single physical page normally
    /// used by nametable 0 ("screen A").
    OneScreenLower,
    /// All four logical nametables alias the single physical page normally
    /// used by nametable 1 ("screen B").
    OneScreenUpper,
    /// Each logical nametable names its own backing (ticket W14-12, widened
    /// by W14-16): `0`/`1` are CIRAM pages A and B — TxSROM (mapper 118)
    /// drives CIRAM A10 from CHR bank bit 7, so any of the sixteen
    /// combinations is reachable — and `2`/`3` are MMC5's extra nametable
    /// RAM and its fill tile, which only a PPU that carries those can
    /// serve. The PPU decides what a value it cannot serve falls back to.
    PerTable([u8; 4]),
}

/// Parsed iNES/NES 2.0 header fields (FR-CORE-010).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NesHeader {
    pub format: NesFormat,
    /// iNES: 0-255. NES 2.0: 0-4095 (12-bit extended mapper number).
    pub mapper: u16,
    /// NES 2.0 only.
    pub submapper: Option<u8>,
    /// PRG ROM size in bytes.
    pub prg_rom_size: usize,
    /// CHR ROM size in bytes; 0 means the cartridge uses CHR RAM instead.
    pub chr_rom_size: usize,
    /// Best-effort **volatile** PRG RAM size in bytes (iNES byte 8, or
    /// NES 2.0 byte 10's low nibble). iNES has no separate non-volatile
    /// field, so this already covers the whole 8 KiB fallback there.
    pub prg_ram_size: usize,
    /// NES 2.0 byte 10's high nibble: battery-backed PRG **NVRAM** size
    /// in bytes, on top of [`Self::prg_ram_size`] (ticket W14-22, e.g.
    /// MMC5's ETROM board: 8 KiB volatile + 8 KiB NVRAM = 16 KiB total
    /// across its two chips). Always `0` for iNES 1.0, which has no
    /// field to carry it.
    pub prg_nvram_size: usize,
    pub mirroring: Mirroring,
    pub battery: bool,
    /// Whether a 512-byte trainer follows the header, before PRG data.
    pub trainer: bool,
}

/// Parse an iNES or NES 2.0 header from the start of `data` (FR-CORE-010).
///
/// Returns `Err` — never panics — for a missing magic, a truncated file,
/// an NES 2.0 exponent-notation size field (rare, unimplemented), or a
/// mapper number `rf-nes` doesn't implement yet (FR-CORE-013).
pub fn parse_nes_header(data: &[u8]) -> Result<NesHeader, CartError> {
    if data.len() < HEADER_LEN || data[0..4] != INES_MAGIC {
        return Err(CartError::InvalidHeader(
            "missing 'NES\\x1A' magic".to_string(),
        ));
    }

    let flags6 = data[6];
    let flags7 = data[7];
    let is_nes2 = flags7 & 0x0C == 0x08;

    let mirroring = if flags6 & 0x08 != 0 {
        Mirroring::FourScreen
    } else if flags6 & 0x01 != 0 {
        Mirroring::Vertical
    } else {
        Mirroring::Horizontal
    };
    let battery = flags6 & 0x02 != 0;
    let trainer = flags6 & 0x04 != 0;

    // Mapper D0-D3 from flags6 high nibble, D4-D7 from flags7 high nibble.
    let mut mapper = ((flags7 & 0xF0) as u16) | ((flags6 >> 4) as u16);

    let (format, submapper, prg_banks, chr_banks, prg_ram_size, prg_nvram_size) = if is_nes2 {
        let byte8 = data[8];
        let byte9 = data[9];
        mapper |= ((byte8 & 0x0F) as u16) << 8;
        let submapper = (byte8 >> 4) & 0x0F;

        let prg_msb = byte9 & 0x0F;
        let chr_msb = (byte9 >> 4) & 0x0F;
        if prg_msb == 0x0F || chr_msb == 0x0F {
            // Exponent-multiplier notation for implausibly large ROMs;
            // vanishingly rare in practice, not implemented.
            return Err(CartError::InvalidHeader(
                "NES 2.0 exponent-multiplier size notation is not supported".to_string(),
            ));
        }
        let prg_banks = ((prg_msb as usize) << 8) | data[4] as usize;
        let chr_banks = ((chr_msb as usize) << 8) | data[5] as usize;

        let prg_ram_nibble = data[10] & 0x0F;
        let prg_ram_size = if prg_ram_nibble == 0 {
            0
        } else {
            64usize << u32::from(prg_ram_nibble)
        };
        let prg_nvram_nibble = (data[10] >> 4) & 0x0F;
        let prg_nvram_size = if prg_nvram_nibble == 0 {
            0
        } else {
            64usize << u32::from(prg_nvram_nibble)
        };

        (
            NesFormat::Nes2,
            Some(submapper),
            prg_banks,
            chr_banks,
            prg_ram_size,
            prg_nvram_size,
        )
    } else {
        let prg_banks = data[4] as usize;
        let chr_banks = data[5] as usize;
        // iNES 1.0 informal convention: 0 means "assume 8 KB" for
        // compatibility with the many dumps that leave this byte zeroed.
        let prg_ram_byte = data[8];
        let prg_ram_size = if prg_ram_byte == 0 {
            8 * 1024
        } else {
            prg_ram_byte as usize * 8 * 1024
        };
        (NesFormat::INes, None, prg_banks, chr_banks, prg_ram_size, 0)
    };

    let prg_rom_size = prg_banks * PRG_BANK;
    let chr_rom_size = chr_banks * CHR_BANK;

    let needed = HEADER_LEN + if trainer { TRAINER_LEN } else { 0 } + prg_rom_size + chr_rom_size;
    if data.len() < needed {
        return Err(CartError::Truncated {
            context: "NES PRG/CHR data",
            needed,
            got: data.len(),
        });
    }

    if !SUPPORTED_MAPPERS.contains(&mapper) {
        return Err(CartError::UnsupportedMapper {
            id: mapper,
            name: mapper_name(mapper),
        });
    }

    Ok(NesHeader {
        format,
        mapper,
        submapper,
        prg_rom_size,
        chr_rom_size,
        prg_ram_size,
        prg_nvram_size,
        mirroring,
        battery,
        trainer,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a synthetic iNES 1.0 image: header + (optional trainer) +
    /// zeroed PRG/CHR payload of the declared sizes.
    #[allow(clippy::too_many_arguments)]
    fn build_ines(
        prg_banks: u8,
        chr_banks: u8,
        mapper: u16,
        vertical: bool,
        battery: bool,
        trainer: bool,
        four_screen: bool,
    ) -> Vec<u8> {
        let mapper_lo = (mapper & 0x0F) as u8;
        let mapper_hi = ((mapper >> 4) & 0x0F) as u8;
        let mut flags6 = mapper_lo << 4;
        if vertical {
            flags6 |= 0x01;
        }
        if battery {
            flags6 |= 0x02;
        }
        if trainer {
            flags6 |= 0x04;
        }
        if four_screen {
            flags6 |= 0x08;
        }
        let flags7 = mapper_hi << 4; // bits 2-3 = 0 => iNES 1.0

        let mut data = Vec::new();
        data.extend_from_slice(&INES_MAGIC);
        data.push(prg_banks);
        data.push(chr_banks);
        data.push(flags6);
        data.push(flags7);
        data.extend_from_slice(&[0u8; 8]); // bytes 8-15
        if trainer {
            data.extend(vec![0u8; TRAINER_LEN]);
        }
        data.extend(vec![0u8; prg_banks as usize * PRG_BANK]);
        data.extend(vec![0u8; chr_banks as usize * CHR_BANK]);
        data
    }

    /// Builds a synthetic NES 2.0 image with an explicit 12-bit mapper and
    /// submapper, small enough PRG/CHR bank counts to avoid the exponent
    /// notation edge case.
    fn build_nes2(mapper: u16, submapper: u8, prg_banks: u16, chr_banks: u16) -> Vec<u8> {
        build_nes2_with_prg_ram(mapper, submapper, prg_banks, chr_banks, 0)
    }

    /// [`build_nes2`], plus an explicit NES 2.0 byte 10 (PRG-RAM/PRG-NVRAM
    /// shift-count nibbles) for ticket W14-22's PRG-RAM-sizing tests.
    fn build_nes2_with_prg_ram(
        mapper: u16,
        submapper: u8,
        prg_banks: u16,
        chr_banks: u16,
        byte10: u8,
    ) -> Vec<u8> {
        let mapper_lo = (mapper & 0x0F) as u8;
        let mapper_mid = ((mapper >> 4) & 0x0F) as u8;
        let mapper_hi = ((mapper >> 8) & 0x0F) as u8;
        let flags6 = mapper_lo << 4;
        let flags7 = (mapper_mid << 4) | 0x08; // NES 2.0 identifier bits
        let byte8 = mapper_hi | (submapper << 4);
        let byte9 = ((prg_banks >> 8) as u8 & 0x0F) | (((chr_banks >> 8) as u8 & 0x0F) << 4);

        let mut data = Vec::new();
        data.extend_from_slice(&INES_MAGIC);
        data.push((prg_banks & 0xFF) as u8);
        data.push((chr_banks & 0xFF) as u8);
        data.push(flags6);
        data.push(flags7);
        data.push(byte8);
        data.push(byte9);
        data.push(byte10);
        data.extend_from_slice(&[0u8; 5]); // bytes 11-15
        data.extend(vec![0u8; prg_banks as usize * PRG_BANK]);
        data.extend(vec![0u8; chr_banks as usize * CHR_BANK]);
        data
    }

    #[test]
    fn parses_ines_nrom_horizontal_no_battery() {
        let rom = build_ines(2, 1, 0, false, false, false, false);
        let header = parse_nes_header(&rom).expect("valid NROM header");
        assert_eq!(header.format, NesFormat::INes);
        assert_eq!(header.mapper, 0);
        assert_eq!(header.prg_rom_size, 2 * PRG_BANK);
        assert_eq!(header.chr_rom_size, CHR_BANK);
        assert_eq!(header.mirroring, Mirroring::Horizontal);
        assert!(!header.battery);
        assert!(!header.trainer);
    }

    #[test]
    fn parses_ines_mmc1_vertical_battery_with_trainer() {
        let rom = build_ines(8, 0, 1, true, true, true, false);
        let header = parse_nes_header(&rom).expect("valid MMC1 header");
        assert_eq!(header.mapper, 1);
        assert_eq!(header.mirroring, Mirroring::Vertical);
        assert!(header.battery);
        assert!(header.trainer);
        assert_eq!(header.chr_rom_size, 0, "chr_rom_size 0 means CHR RAM");
    }

    #[test]
    fn four_screen_bit_overrides_mirroring_bit() {
        let rom = build_ines(2, 1, 4, true, false, false, true);
        let header = parse_nes_header(&rom).expect("valid MMC3 header");
        assert_eq!(header.mirroring, Mirroring::FourScreen);
    }

    #[test]
    fn parses_nes2_header_with_submapper() {
        let rom = build_nes2(4, 3, 4, 2);
        let header = parse_nes_header(&rom).expect("valid NES 2.0 header");
        assert_eq!(header.format, NesFormat::Nes2);
        assert_eq!(header.mapper, 4);
        assert_eq!(header.submapper, Some(3));
        assert_eq!(header.prg_rom_size, 4 * PRG_BANK);
        assert_eq!(header.chr_rom_size, 2 * CHR_BANK);
    }

    #[test]
    fn nes2_extended_12_bit_mapper_number_computed_correctly() {
        // Mapper 300 (0x12C) requires the NES 2.0 byte-8 high-nibble
        // extension; well beyond the supported set, so this also proves
        // FR-CORE-013 fires with the *correctly computed* number, not a
        // truncated 8-bit one.
        let rom = build_nes2(300, 0, 1, 1);
        let err = parse_nes_header(&rom).unwrap_err();
        assert_eq!(
            err,
            CartError::UnsupportedMapper {
                id: 300,
                name: None
            }
        );
    }

    #[test]
    fn unknown_mapper_fails_with_diagnostic_naming_the_number() {
        let rom = build_ines(1, 1, 10, false, false, false, false); // MMC4 / FxROM, not implemented (was MMC5 until W14-16)
        let err = parse_nes_header(&rom).unwrap_err();
        match &err {
            CartError::UnsupportedMapper { id, name } => {
                assert_eq!(*id, 10);
                assert_eq!(*name, Some("MMC4 / FxROM"));
            }
            other => panic!("expected UnsupportedMapper, got {other:?}"),
        }
        assert!(err.to_string().contains("10"));
    }

    #[test]
    fn rejects_missing_magic_without_panicking() {
        let data = vec![0u8; 16];
        let err = parse_nes_header(&data).unwrap_err();
        assert!(matches!(err, CartError::InvalidHeader(_)));
    }

    #[test]
    fn rejects_empty_input_without_panicking() {
        let err = parse_nes_header(&[]).unwrap_err();
        assert!(matches!(err, CartError::InvalidHeader(_)));
    }

    #[test]
    fn rejects_truncated_prg_data() {
        let mut rom = build_ines(2, 1, 0, false, false, false, false);
        rom.truncate(rom.len() - 100); // chop off part of the CHR payload
        let err = parse_nes_header(&rom).unwrap_err();
        assert!(matches!(err, CartError::Truncated { .. }));
    }

    #[test]
    fn nes2_exponent_notation_is_reported_not_panicked() {
        let mut rom = build_nes2(0, 0, 1, 1);
        rom[9] = 0x0F; // prg_msb == 0xF => exponent notation, unsupported
        let err = parse_nes_header(&rom).unwrap_err();
        assert!(matches!(err, CartError::InvalidHeader(_)));
    }

    /// Ticket W14-22 (HANDOFF from `rf-nes`): NES 2.0 byte 10's two
    /// nibbles are independent -- an ETROM-class board (Uncharted
    /// Waters' real header: `0x77`) declares 8 KiB volatile PRG-RAM
    /// (low nibble 7 -> `64 << 7`) AND 8 KiB non-volatile PRG-NVRAM
    /// (high nibble 7), 16 KiB total across its two chips.
    #[test]
    fn nes2_byte10_splits_volatile_ram_from_nvram() {
        let rom = build_nes2_with_prg_ram(5, 0, 32, 16, 0x77);
        let header = parse_nes_header(&rom).expect("valid MMC5 ETROM-shaped header");
        assert_eq!(
            header.prg_ram_size,
            8 * 1024,
            "low nibble: volatile PRG-RAM"
        );
        assert_eq!(
            header.prg_nvram_size,
            8 * 1024,
            "high nibble: battery-backed PRG-NVRAM"
        );
    }

    #[test]
    fn nes2_byte10_zero_means_no_ram_of_either_kind() {
        let rom = build_nes2(0, 0, 1, 1);
        let header = parse_nes_header(&rom).expect("valid header");
        assert_eq!(header.prg_ram_size, 0);
        assert_eq!(header.prg_nvram_size, 0);
    }

    #[test]
    fn ines_1_0_never_reports_nvram_separately() {
        let rom = build_ines(2, 1, 0, false, true, false, false);
        let header = parse_nes_header(&rom).expect("valid iNES header");
        assert_eq!(header.prg_nvram_size, 0, "no byte 10 in iNES 1.0");
    }
}

#[cfg(test)]
mod mapper28_tests {
    use super::*;

    /// Mapper 28 is accepted (ticket W7-11) — and the FR-CORE-013
    /// diagnostic still fires for what remains unsupported, naming the
    /// number.
    ///
    /// The second half matters as much as the first: widening a supported
    /// set is only safe if the refusal path still works, and a test that
    /// only checked acceptance would pass on a build that accepted
    /// everything.
    #[test]
    fn mapper_28_is_supported_and_unknown_mappers_still_name_themselves() {
        assert!(SUPPORTED_MAPPERS.contains(&28), "Action 53");
        assert!(SUPPORTED_MAPPERS.contains(&7), "AxROM");

        // Ticket W14-58 added 119 (TQROM) and 47 (MMC3 2-in-1) to
        // `SUPPORTED_MAPPERS` -- 210 and 13 stand in as still-refused
        // mappers so this test keeps proving the refusal path works.
        for unsupported in [210u16, 13] {
            assert!(
                !SUPPORTED_MAPPERS.contains(&unsupported),
                "mapper {unsupported} must still be refused"
            );
        }
    }
}
