//! SNES (LoROM/HiROM) header parsing, copier-header handling, and
//! enhancement-chip detection (FR-CORE-010, FR-CORE-013, FR-CORE-035).
//!
//! Header layout reference:
//! [snes.nesdev.org/wiki/ROM_header](https://snes.nesdev.org/wiki/ROM_header).
//! The 21-byte title plus fixed fields live at `header_base+$00..$20`, and
//! the interrupt vector table (used here only for the RESET vector sanity
//! check) at `header_base+$20..$40`. `header_base` is `$7FC0` for LoROM,
//! `$FFC0` for HiROM, both relative to the copier-header-stripped image.
//!
//! `docs/design/EMULATION_CORES.md` §3.5: "Header detection in `rf-cart`:
//! score candidate headers at $7FC0/$FFC0 (checksum/complement, mapper
//! byte, reset vector sanity) — never trust the extension." ExHiROM and
//! enhancement-chip carts (SA-1, Super FX, DSP-1, ...) are explicitly
//! deferred; `rf-cart` reports "unsupported chip: <name>" for them rather
//! than half-booting.

use crate::error::CartError;

const COPIER_HEADER_LEN: usize = 512;
const LOROM_HEADER_OFFSET: usize = 0x7FC0;
const HIROM_HEADER_OFFSET: usize = 0xFFC0;
/// Header fields ($00-$1F) plus the interrupt vector table ($20-$3F).
const HEADER_BLOCK_LEN: usize = 0x40;
/// RESET vector lives at file offset $FFFC/$7FFC, i.e. header_base + $3C.
const RESET_VECTOR_OFFSET: usize = 0x3C;

/// Which of the two currently-supported SNES memory maps this cart uses.
/// ExHiROM and the coprocessor-carrying map modes (SA-1, S-DD1, SPC7110)
/// are detected but rejected — see module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnesMapMode {
    LoRom,
    HiRom,
}

/// Parsed SNES header fields (FR-CORE-010).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnesHeader {
    pub map_mode: SnesMapMode,
    /// Map-mode byte bit 4: 3.58 MHz FastROM vs 2.68 MHz SlowROM.
    pub fast_rom: bool,
    /// ROM size in bytes, decoded from the `1<<N` kilobyte size byte.
    pub rom_size: usize,
    /// RAM size in bytes, decoded the same way.
    pub ram_size: usize,
    pub battery: bool,
    pub checksum: u16,
    pub checksum_complement: u16,
    /// Whether a 512-byte copier header was stripped before parsing.
    pub had_copier_header: bool,
}

/// Decode a `1<<N` kilobyte size byte into a byte count, without panicking
/// on an implausible (attacker-controlled) exponent.
fn kb_pow2(exp: u8) -> Result<usize, CartError> {
    if exp == 0 {
        return Ok(0);
    }
    1usize
        .checked_shl(u32::from(exp))
        .and_then(|banks| banks.checked_mul(1024))
        .ok_or_else(|| CartError::InvalidHeader(format!("implausible SNES size exponent: {exp}")))
}

/// Name a coprocessor from the cartridge-type byte's high nibble
/// (snes.nesdev.org/wiki/ROM_header).
fn coprocessor_name(chipset: u8) -> &'static str {
    match (chipset & 0xF0) >> 4 {
        0x0 => "DSP",
        0x1 => "Super FX (GSU)",
        0x2 => "OBC1",
        0x3 => "SA-1",
        0x4 => "S-DD1",
        0x5 => "S-RTC",
        0xE => "Super Game Boy / Satellaview",
        0xF => "custom coprocessor",
        _ => "unrecognized coprocessor",
    }
}

/// Name a map-mode nibble (map-mode byte low 4 bits).
fn map_mode_name(nibble: u8) -> &'static str {
    match nibble {
        0x0 => "LoROM",
        0x1 => "HiROM",
        0x2 => "S-DD1",
        0x3 => "SA-1",
        0x5 => "ExHiROM",
        0xA => "SPC7110",
        _ => "unrecognized map mode",
    }
}

struct Candidate {
    base: usize,
    score: i32,
}

/// Score a candidate header location per the heuristic named in
/// `docs/design/EMULATION_CORES.md` §3.5: checksum/complement agreement,
/// RESET vector sanity, and self-consistency of the declared map mode with
/// the location being tested. Returns `None` if `data` isn't even long
/// enough to contain a header block at `base`.
fn score_candidate(data: &[u8], base: usize) -> Option<Candidate> {
    if data.len() < base + HEADER_BLOCK_LEN {
        return None;
    }
    let mode_nibble = data[base + 0x15] & 0x0F;
    let checksum = u16::from_le_bytes([data[base + 0x1E], data[base + 0x1F]]);
    let complement = u16::from_le_bytes([data[base + 0x1C], data[base + 0x1D]]);
    let reset_hi = data[base + RESET_VECTOR_OFFSET + 1];

    let mut score = 0;
    if checksum ^ complement == 0xFFFF && checksum != 0 {
        score += 2;
    }
    if reset_hi >= 0x80 {
        score += 1;
    }
    let expected_nibble = if base == LOROM_HEADER_OFFSET {
        0x0
    } else {
        0x1
    };
    if mode_nibble == expected_nibble {
        score += 1;
    }
    Some(Candidate { base, score })
}

/// Parse a SNES header, first stripping a 512-byte copier header if
/// `raw.len() % 8192 == 512` (FR-CORE-010), then locating LoROM ($7FC0) vs
/// HiROM ($FFC0) by scoring both candidate locations.
///
/// Returns `Err` — never panics — for a too-short image, a location where
/// neither candidate looks plausible, an unsupported map mode (ExHiROM/
/// SA-1/S-DD1/SPC7110), or an unsupported enhancement chip (FR-CORE-013).
pub fn parse_snes_header(raw: &[u8]) -> Result<SnesHeader, CartError> {
    let had_copier_header = raw.len() % 8192 == COPIER_HEADER_LEN;
    let data = if had_copier_header {
        &raw[COPIER_HEADER_LEN..]
    } else {
        raw
    };

    let lo = score_candidate(data, LOROM_HEADER_OFFSET);
    let hi = score_candidate(data, HIROM_HEADER_OFFSET);

    let winner = match (lo, hi) {
        (Some(l), Some(h)) if h.score > l.score => h,
        (Some(l), _) => l,
        (None, Some(h)) => h,
        (None, None) => {
            return Err(CartError::Truncated {
                context: "SNES header (LoROM $7FC0 / HiROM $FFC0)",
                needed: LOROM_HEADER_OFFSET + HEADER_BLOCK_LEN,
                got: data.len(),
            });
        }
    };

    if winner.score == 0 {
        return Err(CartError::InvalidHeader(
            "no plausible SNES header found at $7FC0 or $FFC0 (checksum/reset-vector heuristic failed)"
                .to_string(),
        ));
    }

    let base = winner.base;
    let mode_byte = data[base + 0x15];
    let mode_nibble = mode_byte & 0x0F;
    let fast_rom = mode_byte & 0x10 != 0;
    let map_mode = match mode_nibble {
        0x0 => SnesMapMode::LoRom,
        0x1 => SnesMapMode::HiRom,
        _ => {
            return Err(CartError::UnsupportedChip {
                name: format!(
                    "{} (SNES map mode ${mode_byte:02X})",
                    map_mode_name(mode_nibble)
                ),
            });
        }
    };

    let chipset = data[base + 0x16];
    let hw = chipset & 0x0F;
    if hw >= 0x3 {
        return Err(CartError::UnsupportedChip {
            name: format!(
                "{} (SNES chipset ${chipset:02X})",
                coprocessor_name(chipset)
            ),
        });
    }
    let battery = hw == 0x2;

    let rom_size = kb_pow2(data[base + 0x17])?;
    let ram_size = kb_pow2(data[base + 0x18])?;
    let checksum = u16::from_le_bytes([data[base + 0x1E], data[base + 0x1F]]);
    let checksum_complement = u16::from_le_bytes([data[base + 0x1C], data[base + 0x1D]]);

    Ok(SnesHeader {
        map_mode,
        fast_rom,
        rom_size,
        ram_size,
        battery,
        checksum,
        checksum_complement,
        had_copier_header,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_checksum(data: &mut [u8], base: usize, checksum: u16) {
        let complement = checksum ^ 0xFFFF;
        data[base + 0x1C..base + 0x1E].copy_from_slice(&complement.to_le_bytes());
        data[base + 0x1E..base + 0x20].copy_from_slice(&checksum.to_le_bytes());
    }

    fn set_reset_vector(data: &mut [u8], base: usize, addr: u16) {
        let bytes = addr.to_le_bytes();
        data[base + RESET_VECTOR_OFFSET] = bytes[0];
        data[base + RESET_VECTOR_OFFSET + 1] = bytes[1];
    }

    /// A plausible, self-consistent LoROM image: 32 KB, correct checksum
    /// pair, sane reset vector, requested map-mode/chipset bytes.
    fn lorom_image(mode_byte: u8, chipset: u8) -> Vec<u8> {
        let mut data = vec![0u8; 0x8000];
        let base = LOROM_HEADER_OFFSET;
        data[base + 0x15] = mode_byte;
        data[base + 0x16] = chipset;
        data[base + 0x17] = 6; // 1<<6 KB = 64 KB
        data[base + 0x18] = 3; // 1<<3 KB = 8 KB
        set_checksum(&mut data, base, 0xBEEF);
        set_reset_vector(&mut data, base, 0x8000);
        data
    }

    /// A plausible, self-consistent HiROM image: 64 KB+.
    fn hirom_image(mode_byte: u8, chipset: u8) -> Vec<u8> {
        let mut data = vec![0u8; 0x10000];
        let base = HIROM_HEADER_OFFSET;
        data[base + 0x15] = mode_byte;
        data[base + 0x16] = chipset;
        data[base + 0x17] = 7; // 1<<7 KB = 128 KB
        data[base + 0x18] = 0;
        set_checksum(&mut data, base, 0xCAFE);
        set_reset_vector(&mut data, base, 0xC000);
        data
    }

    #[test]
    fn detects_lorom_via_checksum_and_reset_vector() {
        let rom = lorom_image(0x20, 0x00); // LoROM, slow, ROM only
        let header = parse_snes_header(&rom).expect("valid LoROM header");
        assert_eq!(header.map_mode, SnesMapMode::LoRom);
        assert!(!header.fast_rom);
        assert_eq!(header.rom_size, 64 * 1024);
        assert_eq!(header.ram_size, 8 * 1024);
        assert!(!header.battery);
        assert!(!header.had_copier_header);
    }

    #[test]
    fn detects_hirom_via_checksum_and_reset_vector() {
        let rom = hirom_image(0x31, 0x00); // HiROM, fast, ROM only
        let header = parse_snes_header(&rom).expect("valid HiROM header");
        assert_eq!(header.map_mode, SnesMapMode::HiRom);
        assert!(header.fast_rom);
        assert_eq!(header.rom_size, 128 * 1024);
    }

    #[test]
    fn battery_flag_set_for_rom_ram_battery_chipset() {
        let rom = lorom_image(0x20, 0x02); // ROM+RAM+Battery, no coprocessor
        let header = parse_snes_header(&rom).expect("valid header");
        assert!(header.battery);
    }

    #[test]
    fn copier_header_512_bytes_stripped_before_locating_header() {
        let mut rom = vec![0u8; COPIER_HEADER_LEN];
        rom.extend(lorom_image(0x20, 0x00));
        assert_eq!(
            rom.len() % 8192,
            512,
            "fixture must trip the copier-header heuristic"
        );

        let header = parse_snes_header(&rom).expect("valid header behind copier header");
        assert!(header.had_copier_header);
        assert_eq!(header.map_mode, SnesMapMode::LoRom);
        assert_eq!(header.rom_size, 64 * 1024);
    }

    #[test]
    fn unsupported_chip_sa1_reported_from_chipset_byte() {
        let rom = lorom_image(0x20, 0x35); // ROM+SA-1+RAM+Battery
        let err = parse_snes_header(&rom).unwrap_err();
        match &err {
            CartError::UnsupportedChip { name } => assert!(name.contains("SA-1"), "got: {name}"),
            other => panic!("expected UnsupportedChip, got {other:?}"),
        }
    }

    #[test]
    fn unsupported_chip_superfx_reported_from_chipset_byte() {
        let rom = lorom_image(0x20, 0x15); // ROM+Super FX+RAM+Battery
        let err = parse_snes_header(&rom).unwrap_err();
        match &err {
            CartError::UnsupportedChip { name } => {
                assert!(name.contains("Super FX"), "got: {name}")
            }
            other => panic!("expected UnsupportedChip, got {other:?}"),
        }
    }

    #[test]
    fn unsupported_exhirom_map_mode_reported_without_panicking() {
        let rom = lorom_image(0x25, 0x00); // ExHiROM map mode
        let err = parse_snes_header(&rom).unwrap_err();
        match &err {
            CartError::UnsupportedChip { name } => assert!(name.contains("ExHiROM"), "got: {name}"),
            other => panic!("expected UnsupportedChip, got {other:?}"),
        }
    }

    #[test]
    fn rejects_when_no_plausible_header_found() {
        // Uniform filler: bad checksum pair, low reset vector, and a
        // map-mode nibble (0x2) matching neither LoROM's (0x0) nor
        // HiROM's (0x1) expected value at either candidate location.
        let data = vec![0x02u8; 0x10000];
        let err = parse_snes_header(&data).unwrap_err();
        assert!(matches!(err, CartError::InvalidHeader(_)));
    }

    #[test]
    fn rejects_too_short_input_without_panicking() {
        let data = vec![0u8; 100];
        let err = parse_snes_header(&data).unwrap_err();
        assert!(matches!(err, CartError::Truncated { .. }));
    }

    #[test]
    fn rejects_empty_input_without_panicking() {
        let err = parse_snes_header(&[]).unwrap_err();
        assert!(matches!(err, CartError::Truncated { .. }));
    }
}
