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

/// How much evidence `score_candidate` must find, on top of the necessary
/// conditions it checks first, before a location is accepted as a SNES
/// header. See the check in [`parse_snes_header`] for why this is not zero.
const MINIMUM_SCORE: i32 = 2;

/// Highest country/region code fullsnes assigns ($00 Japan .. $14).
const MAX_COUNTRY_CODE: u8 = 0x14;

/// Highest cartridge revision treated as plausible. Commercial carts ship
/// $00 and revisions stay in single digits.
const MAX_ROM_VERSION: u8 = 0x0F;

/// Map-mode nibbles fullsnes names that this build does not run: $2
/// (S-DD1), $3 (SA-1), $5 (ExHiROM), $A (SPC7110). Used only to give a
/// cartridge-shaped refusal a chip name instead of "not a SNES image".
const KNOWN_UNSUPPORTED_MAP_MODES: [u8; 4] = [0x2, 0x3, 0x5, 0xA];
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
/// Does either candidate location hold a map mode this build recognizes by
/// name but cannot run? Reported as [`CartError::UnsupportedChip`] so the
/// user is told which chip, per FR-CORE-013 (ticket W14-05).
///
/// Only consulted once neither location has qualified as a candidate, so a
/// real LoROM or HiROM cartridge never reaches it.
fn unsupported_map_mode_at_either_location(data: &[u8]) -> Option<CartError> {
    for base in [LOROM_HEADER_OFFSET, HIROM_HEADER_OFFSET] {
        if data.len() < base + HEADER_BLOCK_LEN {
            continue;
        }
        // The same structural gate the candidate scorer applies, so this
        // fallback cannot re-admit the arbitrary data the ticket is about.
        if data[base + 0x19] > MAX_COUNTRY_CODE || data[base + 0x1B] > MAX_ROM_VERSION {
            continue;
        }
        let mode_byte = data[base + 0x15];
        let nibble = mode_byte & 0x0F;
        if KNOWN_UNSUPPORTED_MAP_MODES.contains(&nibble) {
            return Some(CartError::UnsupportedChip {
                name: format!("{} (SNES map mode ${mode_byte:02X})", map_mode_name(nibble)),
            });
        }
    }
    None
}

fn score_candidate(data: &[u8], base: usize) -> Option<Candidate> {
    if data.len() < base + HEADER_BLOCK_LEN {
        return None;
    }

    // ---- necessary conditions -------------------------------------------
    //
    // A candidate that fails any of these is not scored at all, and the
    // distinction matters: these are things every SNES header HAS, however
    // badly the rest of it is filled in, so failing one is evidence the
    // location is not a header rather than evidence of a poor dump. The
    // prototypes this had to keep loading — blanked titles, zero checksum,
    // zero size byte — all satisfy every one of them.

    // Country/region code: fullsnes documents $00-$14. Regions above that
    // are not assigned.
    if data[base + 0x19] > MAX_COUNTRY_CODE {
        return None;
    }
    // ROM version. Real cartridges ship $00, and revisions stay in single
    // digits; a byte above that is noise, not a twentieth revision.
    if data[base + 0x1B] > MAX_ROM_VERSION {
        return None;
    }

    // ---- evidence --------------------------------------------------------
    let checksum = u16::from_le_bytes([data[base + 0x1E], data[base + 0x1F]]);
    let complement = u16::from_le_bytes([data[base + 0x1C], data[base + 0x1D]]);
    let mut score = 0;
    // The one strong signal. fullsnes: the checksum and its complement are
    // stored so that they XOR to $FFFF — a 1-in-65536 coincidence on data
    // that is not a SNES header, which is why it is worth two points.
    if checksum ^ complement == 0xFFFF && checksum != 0 {
        score += 2;
    }
    // A reset vector into the upper half of the bank, where mapped ROM
    // lives. True of every cartridge that boots.
    if data[base + RESET_VECTOR_OFFSET + 1] >= 0x80 {
        score += 1;
    }
    // The 21-byte title (fullsnes: "Cartridge Title, 21 bytes, ASCII").
    // A MAJORITY rather than all of it, because real dumps pad with $00 as
    // well as with spaces — and this point is never load-bearing for a
    // cartridge whose checksum is intact, which is what keeps Shift-JIS
    // titles (high bytes, not ASCII) loading.
    let printable = data[base..base + 0x15]
        .iter()
        .filter(|&&b| (0x20..=0x7E).contains(&b))
        .count();
    if printable >= 12 {
        score += 1;
    }
    // The map-mode nibble agreeing with WHERE this candidate is. Evidence
    // and NOT a requirement, which was tried first and was wrong: real
    // dumps exist whose header sits at one location while its mode byte
    // names the other, and `WWF Super WrestleMania (USA)` is one of them.
    let expected_nibble = if base == LOROM_HEADER_OFFSET {
        0x0
    } else {
        0x1
    };
    if data[base + 0x15] & 0x0F == expected_nibble {
        score += 1;
    }
    // A plausible ROM size exponent: $08 is 256 KiB and $0D is 8 MiB, which
    // brackets every commercial SNES cartridge ever made.
    if (0x08..=0x0D).contains(&data[base + 0x17]) {
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
            // Ticket W14-05: before reporting "not a SNES image", check
            // whether a location holds a map mode this build simply does
            // not support. `score_candidate` requires the mode nibble to
            // agree with the location, so ExHiROM, SA-1 and SPC7110
            // headers are not candidates at all — and saying "no plausible
            // header" about a perfectly good ExHiROM cartridge would be a
            // worse answer than the one FR-CORE-013 asks for, which is a
            // diagnostic naming the chip. Twelve titles in a real library
            // land here.
            if let Some(err) = unsupported_map_mode_at_either_location(data) {
                return Err(err);
            }
            // "Too short to hold a header" and "long enough but nothing
            // there looks like one" are different answers, and this arm
            // used to give the first for both. It only became reachable
            // the other way once `score_candidate` gained structural
            // conditions it can fail on a long file (ticket W14-05).
            if data.len() < LOROM_HEADER_OFFSET + HEADER_BLOCK_LEN {
                return Err(CartError::Truncated {
                    context: "SNES header (LoROM $7FC0 / HiROM $FFC0)",
                    needed: LOROM_HEADER_OFFSET + HEADER_BLOCK_LEN,
                    got: data.len(),
                });
            }
            return Err(CartError::InvalidHeader(
                "no plausible SNES header at $7FC0 or $FFC0: neither location has a \
                 map mode matching it, an assigned country code and a plausible \
                 revision"
                    .to_string(),
            ));
        }
    };

    // Ticket W14-05. A SNES header carries NO magic number — unlike iNES,
    // which is why only this half of the loader had this problem — so
    // "does this location look like a header?" is the whole defence, and
    // until 2026-09-15 a single accidental match passed it: pointed at a
    // real 681-archive Game Boy folder, `Cartridge::load` called **130 of
    // them** SNES cartridges.
    //
    // Two things carry the weight now. `score_candidate` first applies
    // NECESSARY conditions — an assigned country code and a plausible
    // revision, fields every real header fills and arbitrary data clears
    // about once in 200 — and then requires TWO points of positive
    // evidence on top, which no single accident supplies.
    if winner.score < MINIMUM_SCORE {
        // Same courtesy as the no-candidate arm above: if a location holds
        // a map mode we can name, name it (FR-CORE-013) instead of calling
        // the file unreadable.
        if let Some(err) = unsupported_map_mode_at_either_location(data) {
            return Err(err);
        }
        return Err(CartError::InvalidHeader(format!(
            "no plausible SNES header at $7FC0 or $FFC0: best candidate scored \
             {} of {} (checksum/complement, reset vector, map mode, title)",
            winner.score, MINIMUM_SCORE
        )));
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
    fn tie_break_prefers_lorom_by_policy_when_both_candidates_are_plausible() {
        // Construct an image where *both* header locations pass every
        // heuristic check (valid checksum/complement pair, sane RESET
        // vector, self-consistent map-mode nibble) — a rare coincidence
        // for a malformed/adversarial image, but the only situation where
        // the tie-break actually matters. EMULATION_CORES.md §3.5
        // specifies the scoring signals but not a tie-break; breaking
        // ties toward LoROM is this crate's own policy, not a hardware
        // fact, hence no external citation for the choice itself.
        let mut data = vec![0u8; 0x10000];

        let lo_base = LOROM_HEADER_OFFSET;
        data[lo_base + 0x15] = 0x20; // LoROM, slow
        set_checksum(&mut data, lo_base, 0x1111);
        set_reset_vector(&mut data, lo_base, 0x8000);

        let hi_base = HIROM_HEADER_OFFSET;
        data[hi_base + 0x15] = 0x21; // HiROM, slow
        set_checksum(&mut data, hi_base, 0x2222);
        set_reset_vector(&mut data, hi_base, 0xC000);

        let lo_score = score_candidate(&data, lo_base).unwrap().score;
        let hi_score = score_candidate(&data, hi_base).unwrap().score;
        assert_eq!(lo_score, hi_score, "fixture must produce an actual tie");

        let header = parse_snes_header(&data).expect("valid header");
        assert_eq!(
            header.map_mode,
            SnesMapMode::LoRom,
            "ties break toward LoROM by policy"
        );
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
    /// Ticket W14-05: filler whose nibble happens to name a real map mode
    /// is still refused — with the chip's name, which is what FR-CORE-013
    /// asks for, rather than as an unreadable file.
    fn filler_matching_a_named_map_mode_is_refused_by_name() {
        let data = vec![0x02u8; 0x10000];
        match parse_snes_header(&data).unwrap_err() {
            CartError::UnsupportedChip { name } => {
                assert!(name.contains("S-DD1"), "got: {name}");
            }
            other => panic!("expected UnsupportedChip, got {other:?}"),
        }
    }

    #[test]
    /// Ticket W14-05, the regression this ticket exists for. This byte
    /// pattern is a Game Boy ROM's opening bytes followed by filler that
    /// clears the OLD heuristic — a reset vector high byte over $80 was
    /// worth a point, and one point was the whole bar. 130 of 681 real
    /// Game Boy archives passed that way.
    fn data_that_passed_the_old_single_signal_bar_is_refused() {
        let mut data = vec![0x00u8; 0x10000];
        // The only signal the old scorer would have found: a high reset
        // vector at the LoROM location. Country ($FFD9 here) and version
        // are left at $00, which are legal values — so this is refused by
        // the map-mode nibble disagreeing with the location, not by luck.
        data[LOROM_HEADER_OFFSET + RESET_VECTOR_OFFSET + 1] = 0x80;
        data[LOROM_HEADER_OFFSET + 0x15] = 0x07; // not LoROM, not a named mode
        let err = parse_snes_header(&data).unwrap_err();
        assert!(
            matches!(err, CartError::InvalidHeader(_)),
            "a lone reset vector must no longer mint a cartridge, got {err:?}"
        );
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
        // map-mode nibble ($E) that is neither a location's expected value
        // nor any map mode fullsnes names — so it is not a candidate and
        // not a nameable chip either. ($02 filler is a DIFFERENT case and
        // has its own test below: $2 is ExLoROM, a real map mode.)
        let data = vec![0x0Eu8; 0x10000];
        let err = parse_snes_header(&data).unwrap_err();
        assert!(
            matches!(err, CartError::InvalidHeader(_)),
            "long-but-implausible must not be reported as truncated, got {err:?}"
        );
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
