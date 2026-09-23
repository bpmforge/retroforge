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
//! most enhancement-chip carts (SA-1, Super FX, S-DD1, SPC7110, ...) are
//! explicitly deferred; `rf-cart` reports "unsupported chip: <name>" for
//! them rather than half-booting. DSP-1 was lifted out of that deferral by
//! ruling D-010 (`docs/DECISIONS.md`, SRS FR-CORE-038) once the plain
//! LoROM/HiROM accuracy gate was met — see [`Coprocessor`] and
//! [`DspWindow`].

use crate::error::CartError;
use std::ops::RangeInclusive;

const COPIER_HEADER_LEN: usize = 512;
const LOROM_HEADER_OFFSET: usize = 0x7FC0;
const HIROM_HEADER_OFFSET: usize = 0xFFC0;
/// ExHiROM header location: bank $40's mirror of the $FFC0 block (fullsnes
/// "SNES Memory Map" ExHiROM row: the header/vectors for the >4 MiB half of
/// an ExHiROM cartridge sit at $40FFC0, not $FFC0 — only used by
/// [`fallback_mapping_guess`]'s ExHiROM branch, ticket W14-52.
const EXHIROM_HEADER_OFFSET: usize = 0x40FFC0;
/// Images at or under this size cannot be ExHiROM (fullsnes: ExHiROM exists
/// specifically to address cartridges too large for a single bank of
/// mirrors — every real ExHiROM release is > 4 MiB); used only to decide
/// whether the fallback's third mapping guess is worth trying.
const EXHIROM_MIN_SIZE: usize = 4 * 1024 * 1024;
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

/// Map-mode nibbles fullsnes names that this build does not run: $5
/// (ExHiROM), $A (SPC7110). $3 (SA-1) and $2 (S-DD1) are no longer in this
/// list — ticket W17-01 lifted SA-1 out (D-013), ticket W19-03 lifts S-DD1
/// out the same way; both are now candidate map modes like LoROM/HiROM,
/// scored and accepted (or refused by chipset byte, same as before)
/// through the normal path. Ticket W14-53: a nibble in this list only
/// refuses when the chipset byte actually corroborates that chip — see
/// `parse_snes_header`'s `coprocessor_nibble_corroborated`. An
/// uncorroborated nibble is a mastering quirk, not evidence of real (if
/// unsupported) hardware, and falls back to the winning header location
/// instead.
const KNOWN_UNSUPPORTED_MAP_MODES: [u8; 2] = [0x5, 0xA];
/// RESET vector lives at file offset $FFFC/$7FFC, i.e. header_base + $3C.
const RESET_VECTOR_OFFSET: usize = 0x3C;

/// Which of the SNES memory maps this cart uses. ExHiROM and the
/// remaining coprocessor-carrying map modes (S-DD1, SPC7110) are still
/// detected but rejected — see module docs. SA-1 (map mode nibble $3) was
/// lifted out of that deferral by D-013 (ticket W17-01): its header
/// always sits at the LoROM location (fullsnes "SNES Cart SA-1": "Default
/// exception vectors (and cartridge header) are always in LoROM bank
/// 00h"), but the SNES-side memory map it describes is neither plain
/// LoROM nor plain HiROM, so it gets its own variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnesMapMode {
    LoRom,
    HiRom,
    /// Map mode $23 (fullsnes "SNES Cart SA-1"). See [`Coprocessor::Sa1`]
    /// for the board data rf-snes maps this against.
    Sa1,
    /// Map mode $22, "LoROM/32K Banks + S-DD1" (fullsnes "ROM Speed and Map
    /// Mode", `fullsnes.txt:6138`). Ticket W19-03, D-013's pattern applied
    /// to a second chip: like SA-1, the header still sits at the ordinary
    /// LoROM location and the base memory map is plain LoROM — only banks
    /// `$C0-$FF` are special (see [`Coprocessor::Sdd1`] and
    /// `rf_snes::mapping::sdd1_target`).
    Sdd1,
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
    /// Enhancement coprocessor named by the chipset byte, if any this
    /// build runs. See [`Coprocessor`] (D-010, SRS FR-CORE-038).
    pub coprocessor: Coprocessor,
    /// The DSP-1 register bus window, present iff `coprocessor` is
    /// [`Coprocessor::Dsp1`]. See [`DspWindow`].
    pub dsp_window: Option<DspWindow>,
    pub checksum: u16,
    pub checksum_complement: u16,
    /// Whether a 512-byte copier header was stripped before parsing.
    pub had_copier_header: bool,
    /// `Some` iff `score_candidate` found no plausible header at either
    /// location and [`fallback_mapping_guess`] had to name the mapping from
    /// the RESET vector instead (ticket W14-52). `None` means the header
    /// scored normally — real hardware never reads the header at all, so
    /// this is purely a diagnostic for the census/UI, never behavior.
    pub header_fallback: Option<HeaderFallback>,
}

/// Which mapping [`fallback_mapping_guess`] picked, and why, for a
/// cartridge whose header failed ordinary plausibility scoring (ticket
/// W14-52). Carried on [`SnesHeader`] purely as a diagnostic — the fallback
/// itself already committed to a `SnesMapMode` before this is attached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderFallback {
    /// The LoROM RESET vector (bank $00, file offset `vector - $8000`)
    /// pointed at a plausible 65816 reset prologue.
    LoRomResetVector,
    /// The HiROM RESET vector (bank $00's upper half, file offset ==
    /// vector) pointed at a plausible prologue.
    HiRomResetVector,
    /// The ExHiROM RESET vector (bank $00's upper half mapping into the
    /// cartridge's second 4 MiB, file offset == `$400000 + vector`)
    /// pointed at a plausible prologue. Only tried for images > 4 MiB.
    ExHiRomResetVector,
}

/// Which enhancement coprocessor (if any) the cartridge exposes.
///
/// Per D-010 (`docs/DECISIONS.md`, SRS FR-CORE-038), the header's
/// coprocessor nibble ($0, "DSP") cannot distinguish DSP-1 from DSP-2/3/4
/// — they share the same header signature. Rather than identify the game
/// by title to tell them apart (forbidden by law 5), `rf-cart` accepts
/// every nibble-0 DSP cart as DSP-1 and runs it through the DSP-1 HLE
/// core; the three known DSP-2/3/4 titles are named only in the
/// profile/rom-manifest layer (W14-19), recorded there as known-wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coprocessor {
    /// No coprocessor, or a chipset byte this build doesn't special-case
    /// (plain ROM / ROM+RAM / ROM+RAM+battery, `hw < 0x3`).
    None,
    /// Coprocessor nibble $0 ("DSP") with `hw` in 3..=5 (ROM+DSP,
    /// +RAM, +RAM+battery). HLE'd at command level, not LLE of the
    /// uPD7725 (D-010: the chip's program ROM is copyrighted firmware
    /// this project cannot ship).
    Dsp1,
    /// Coprocessor nibble $3 ("SA-1") with `hw` in 4..=5 (chipset $34
    /// ROM+SA-1+RAM, $35 +RAM+battery) under map mode $23. D-013, ticket
    /// W17-01: a second 65C816 (added in W17-02) with its own I-RAM,
    /// BW-RAM window, DMA, arithmetic unit and bit reader — never LLE'd
    /// against copyrighted firmware, because there is none: the SA-1 runs
    /// from the cartridge's own ROM, so this is ordinary clean-room
    /// hardware emulation, not HLE.
    Sa1(Sa1Board),
    /// Coprocessor nibble $1 ("Super FX / GSU-n") with `hw` in the
    /// documented $3..=$A range (chipset $13-$1A, fullsnes "SNES Cart
    /// GSU-n Cartridge Header": "[FFD6h]=13h..1Ah Chipset = GSUn (plus
    /// battery present/absent info)"). Ticket W18-01: clean-room hardware
    /// emulation of a second, cartridge-resident RISC CPU — like SA-1,
    /// never LLE'd against copyrighted firmware because there is none (the
    /// GSU runs from the cartridge's own ROM). This slice models detection
    /// and the SNES-side memory map/register window only; the GSU does not
    /// execute (W18-02+).
    SuperFx {
        version: SuperFxVersion,
        /// Expansion RAM, in KiB, from the extended header (see
        /// [`superfx_expansion_ram_kib`]'s doc for the address and its
        /// caveats).
        ram_kib: usize,
    },
    /// Coprocessor nibble $F ("custom coprocessor") with `hw=$3` (chipset
    /// $F3, "ROM+CustomChip", no battery/no sram) AND the extended
    /// header's `$FFBF` sub-type byte `=$10` (ticket W19-02, fullsnes "CX4
    /// Cartridge Header": `"[FFD6]=F3h ;ROM+CustomChip"`, `"[FFBF]=10h
    /// ;CustomChip=CX4"`). Nibble $F alone is not enough — fullsnes uses
    /// the same nibble for other "custom" chips (e.g. ST010/ST011's
    /// `$FFBF=$01`), so both the chipset byte AND the sub-type byte must
    /// agree, the same two-field disambiguation SA-1/GSU already apply to
    /// their own nibbles. Like SA-1 and GSU, this is clean-room hardware
    /// emulation, never LLE'd against copyrighted firmware — the CX4's
    /// program runs from the cartridge's own ROM, so there is no
    /// undisclosed firmware to reverse-engineer; what stops this project
    /// short of an opcode-level core is a *documentation* gap (see
    /// `rf_snes::cx4`'s module doc), not a legal one.
    Cx4,

    /// Coprocessor nibble $2 ("OBC1") with `hw` $5 (chipset $25,
    /// ROM+OBC1+RAM+battery — the only chipset byte fullsnes or any board
    /// database assigns to this chip; unlike DSP/SA-1/GSU there is no
    /// documented $3/$4 variant to also accept). Ticket W19-01, fullsnes
    /// "SNES Cart OBC1 (OBJ Controller)": one retail title (Metal Combat:
    /// Falcon's Revenge). Clean-room hardware emulation — the chip is a
    /// pure address-remapper over the cart's own 8 KiB battery-backed
    /// SRAM, not a firmware coprocessor, so there is nothing to LLE or HLE
    /// against copyrighted code.
    Obc1,

    /// Coprocessor nibble $4 ("S-DD1") with `hw` in `{3, 5}` (chipset $43
    /// ROM+S-DD1, $45 ROM+S-DD1+RAM+Battery — fullsnes's own "in practice"
    /// list gives only these two; there is no documented $4 "+RAM,
    /// no battery" variant). Ticket W19-03, fullsnes "SNES Cart S-DD1
    /// (Data Decompressor)": two retail titles (Street Fighter Alpha 2,
    /// Star Ocean). Clean-room hardware emulation — the chip has no CPU
    /// and runs no firmware of its own (see `rf_snes::sdd1`'s module doc),
    /// so there is nothing to LLE or HLE against copyrighted code; the
    /// whole decompression pipeline is transcribed from fullsnes's own
    /// published pseudocode.
    Sdd1,
}

/// Which physical GSU chip a cartridge carries. fullsnes "SNES Cart GSU-n
/// List of Games, Chips, and PCB versions" documents no header field that
/// names this directly ("There is no info in the header (nor extended
/// header) whether the game uses a GSU1 or GSU2."); it gives instead a
/// heuristic: "Games with 2MByte ROM are typically using GSU2 (though that
/// rule doesn't always match: Star Fox 2 is only 1MByte)." This build
/// applies that heuristic literally — ROM > 1 MiB selects GSU2 — which
/// means Star Fox 2 (a real, 1 MiB GSU2 title) is knowingly mis-detected
/// as GSU1 by this rule, exactly as fullsnes's own caveat predicts. Chosen
/// over hardcoding a per-title exception (law 5: no title-keyed behavior)
/// since fullsnes states no other distinguishing signal exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuperFxVersion {
    /// GSU-1/GSU-1A/MC1 ("Mario Chip 1"), 10.74MHz.
    Gsu1,
    /// GSU-2/GSU-2-SP1, 21.4MHz-capable.
    Gsu2,
}

/// The SA-1 board data a parsed cartridge carries: sizes rf-snes needs to
/// allocate the SNES-side memory map (fullsnes "SNES Cart SA-1", the
/// memory-map overview and "Misc" sections).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sa1Board {
    /// Cartridge ROM length in bytes, from the header's size byte (up to
    /// 8 MiB — fullsnes: "Addressable ROM up to 8MByte (64MBits)").
    pub rom_len: usize,
    /// BW-RAM length in bytes, from the header's RAM size byte (fullsnes:
    /// "Optional external backup/work BW-RAM up to 2MByte"; boards in the
    /// local library stay far under that).
    pub bwram_len: usize,
    /// I-RAM length in bytes: always 2 KiB, on-chip and fixed (fullsnes
    /// "Misc": "2Kbytes internal I-RAM (work ram/stack)").
    pub iram_len: usize,
}

/// The SA-1's fixed on-chip I-RAM size (fullsnes "SNES Cart SA-1", Misc).
pub const SA1_IRAM_LEN: usize = 2048;

/// The DSP-1 memory-mapped register bus window: which cartridge banks the
/// chip's DR (data/command) and SR (status) registers are visible in, and
/// the address range within a mapped bank each occupies.
///
/// Two sources were checked independently and they disagree on exact bank
/// counts, so both are cited:
/// - snes.nesdev.org/wiki/DSP-1 documents the narrower window specified
///   for the chip: LoROM ("Mode 20") banks $30-$3F/$B0-$BF, DR
///   $8000-$BFFF, SR $C000-$FFFF; HiROM ("Mode 21") banks $00-$0F/$80-$8F,
///   DR $6000-$6FFF, SR $7000-$7FFF.
/// - snes9x's `memmap.cpp` `map_DSP()` (github.com/snes9xgit/snes9x) — the
///   shipping HLE this ticket follows — decodes a wider superset that real
///   boards accept: LoROM <=1 MiB banks $20-$3F/$A0-$BF (same offsets);
///   LoROM >1 MiB ("DSP-1B" boards, selected by the header's declared ROM
///   size, never by title) banks $60-$6F/$E0-$EF, DR $0000-$3FFF, SR
///   $4000-$7FFF; HiROM banks $00-$1F/$80-$9F (same offsets as snesdev —
///   only the upper bank bound, $1F vs $0F, differs between the sources).
///
/// This type implements the snes9x superset: the snesdev-documented range
/// sits entirely inside it, and snes9x's ranges are what has shipped
/// against every real DSP-1 title for decades. `rf-snes` (W14-19) is the
/// consumer of this data; nothing here decides chip behavior, only the
/// address shape of its bus window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DspWindow {
    /// The two mirrored bank ranges the window is visible in.
    pub banks: [RangeInclusive<u8>; 2],
    /// Data/command register address range within a mapped bank.
    pub dr: RangeInclusive<u16>,
    /// Status register address range within a mapped bank.
    pub sr: RangeInclusive<u16>,
}

/// Highest LoROM size, in bytes, that still uses the plain DSP-1 window;
/// larger than this selects the DSP-1B board layout (snes9x
/// `M_DSP1_LOROM_L`), per the header's declared ROM size — never by title
/// (law 5, D-010).
const DSP1B_LOROM_THRESHOLD_BYTES: usize = 1024 * 1024;

/// Select the DSP-1 bus window for a parsed cartridge. See [`DspWindow`]
/// for the cited ranges.
fn dsp_window_for(map_mode: SnesMapMode, rom_size: usize) -> DspWindow {
    match map_mode {
        SnesMapMode::HiRom => DspWindow {
            banks: [0x00..=0x1F, 0x80..=0x9F],
            dr: 0x6000..=0x6FFF,
            sr: 0x7000..=0x7FFF,
        },
        SnesMapMode::LoRom if rom_size > DSP1B_LOROM_THRESHOLD_BYTES => DspWindow {
            banks: [0x60..=0x6F, 0xE0..=0xEF],
            dr: 0x0000..=0x3FFF,
            sr: 0x4000..=0x7FFF,
        },
        SnesMapMode::LoRom => DspWindow {
            banks: [0x20..=0x3F, 0xA0..=0xBF],
            dr: 0x8000..=0xBFFF,
            sr: 0xC000..=0xFFFF,
        },
        // Never reached: `parse_snes_header` only calls this for
        // `Coprocessor::Dsp1`, which its own branching only produces when
        // `map_mode` is neither `Sa1` (ticket W17-01) nor `Sdd1` (ticket
        // W19-03) — each of the three chips claims its own map mode.
        SnesMapMode::Sa1 => unreachable!("DSP-1 window requested for an SA-1 map mode"),
        SnesMapMode::Sdd1 => unreachable!("DSP-1 window requested for an S-DD1 map mode"),
    }
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

/// ROM size above which [`SuperFxVersion::Gsu2`] is picked over `Gsu1` —
/// see [`SuperFxVersion`]'s doc for the fullsnes citation and its known
/// Star Fox 2 mismatch.
const GSU2_ROM_THRESHOLD_BYTES: usize = 1024 * 1024;

/// Read the GSU cartridge's expansion RAM size (ticket W18-01), fullsnes
/// "SNES Cart GSU-n Cartridge Header": "[FFBDh]=05h..06h Expansion RAM
/// Size (32Kbyte and 64Kbyte exist)" — "always use the Expansion entry"
/// rather than the standard RAM-size byte, which the same section
/// documents as always `$00` for a GSU cart ($FFD8h).
///
/// fullsnes's `$FFBD` is written in the chapter's own canonical
/// `$FFC0`-relative header notation (every other field in the same
/// section — `$FFD5`, `$FFD6`, `$FFD8` — is that base plus the field's
/// offset in this module, e.g. `base + 0x15/0x16/0x18`); `$FFBD` is
/// `$FFC0 - 3`, i.e. `base - 3`, three bytes before the header block this
/// module already anchors LoROM ($7FC0) and HiROM ($FFC0) headers to. That
/// puts it in the SNES "extended header" area (maker code, game code,
/// expansion RAM size, special version, sub-number) that snes.nesdev.org
/// documents immediately before the standard header.
///
/// fullsnes's own caution — "Starfox/Star Wing, Powerslide, and Starfox 2
/// do not have extended headers" for some dumps — names an unpopulated
/// extended header (observed as a run of `$FF` bytes in this project's own
/// library dumps, including a real Star Fox (USA) image) as a REAL,
/// documented board condition, not an absence of information to shrug off:
/// the very next sentence in the same chapter states the fact for exactly
/// this case — "RAM Size for Starfox/Starwing is 32Kbytes" — and the
/// chapter's general RAM note elsewhere ("Game Pak RAM with mirrors
/// (64Kbyte max?, usually 32K)") independently backs 32 KiB as the
/// ordinary size, not a per-title guess. Ticket W18-04's Star Fox trace
/// found the previous `0` fallback here was the actual boot-regression
/// root cause: with no GSU RAM at all, `LMS R14,($0062)` (Star Fox's own
/// decompressor priming its ROM read pointer from a value the SNES DMAs
/// into cartridge RAM before GO) reads back nothing but zero, so the
/// decoder walks off into the ROM's own vector-table bytes instead of the
/// real, SNES-supplied base address, and never converges within the census
/// window. This is a flat default for the "extended header absent" case
/// generally — the code does not branch on title (law 5's actual
/// constraint) — it is simply cited to fullsnes for two independent
/// reasons (the specific Starfox/Starwing fact and the general "usually
/// 32K" rule) rather than asserted from nothing. A present-but-implausible
/// exponent (outside `kb_pow2`'s valid range) is a different, genuinely
/// ambiguous case fullsnes gives no fallback for, and still reports `0`
/// rather than guessing.
fn superfx_expansion_ram_kib(data: &[u8], base: usize) -> usize {
    if base < 3 {
        return 0;
    }
    let raw = data[base - 3];
    if raw == 0xFF {
        return 32;
    }
    kb_pow2(raw).map(|bytes| bytes / 1024).unwrap_or(0)
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

/// Cartridges whose header names the generic "DSP" chipset (coprocessor
/// nibble $0, hw $3-$5) but whose actual chip is a DSP variant other than
/// DSP-1, keyed by the header checksum rather than by title text (law 5:
/// no copyrighted title strings in engine code; a checksum is an opaque
/// 16-bit fingerprint, not the title itself, and is exactly how
/// bsnes/higan's and snes9x's own per-board databases resolve the same
/// ambiguity — fullsnes and snes.nesdev.org both note the header's
/// chipset byte cannot distinguish DSP-1/2/3/4).
///
/// `0x5327` is Top Gear 3000 (USA)'s header checksum: DSP-4 (fullsnes
/// "SNES Add-on Chips" / snes.nesdev.org "DSP-4" — the only commercially
/// released DSP-4 title). This build implements only DSP-1 (ticket
/// W14-19); returning its real chip here routes it to
/// [`CartError::UnsupportedChip`] instead of the DSP-1 HLE, which used to
/// accept it and then hang forever on a status byte no DSP-1 command
/// produces (ticket W14-43).
fn known_non_dsp1_checksum(checksum: u16) -> Option<&'static str> {
    match checksum {
        0x5327 => Some("DSP-4"),
        _ => None,
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
/// real LoROM or HiROM cartridge never reaches it — including all five of
/// W14-53's uncorroborated-nibble carts, whose LoROM location always wins
/// outright on score. This fallback has no "winning location" to defer to
/// (by definition neither location scored), so it does not apply W14-53's
/// chipset-corroboration check; it still reports the nibble by name.
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

/// A plausible 65816 reset prologue's first opcode byte, per the WDC 65C816
/// datasheet's reset behavior (emulation mode, M/X=1, PC loaded from
/// $FFFC/$FFFD — snes.nesdev.org "65c816 reference" Reset) and the
/// idioms every SNES boot ROM this project has traced uses immediately
/// after: `SEI` ($78, disable IRQs first), `CLC`/`SEC` ($18/$38, set the
/// carry ahead of `XCE`), `XCE` ($FB, swap emulation/native mode — the
/// standard `CLC`/`XCE` idiom that leaves emulation mode), `JMP`/`JML`
/// ($4C/$5C, some titles vector straight to a real init routine), `REP`/
/// `SEP` ($C2/$E2, set the 8/16-bit register widths before touching A/X/Y),
/// `LDA #imm` ($A9), or `STZ` ($9C, common as the very first instruction
/// when the boot code zeroes a hardware register before anything else).
/// This is a heuristic, not a disassembler: it looks at the first four
/// bytes only, and a title-keyed table of "the" prologue would violate law
/// 5 even if one existed, so this stays a generic opcode-shape check.
fn looks_like_reset_prologue(bytes: &[u8]) -> bool {
    const PLAUSIBLE_FIRST_BYTES: [u8; 10] =
        [0x78, 0x18, 0xFB, 0x38, 0x4C, 0x5C, 0xC2, 0xE2, 0xA9, 0x9C];
    if bytes.is_empty() {
        return false;
    }
    if PLAUSIBLE_FIRST_BYTES.contains(&bytes[0]) {
        return true;
    }
    // CLC/XCE as a pair starting at byte 0 is already covered by CLC
    // ($18) above; this also catches a leading NOP/other filler byte
    // immediately followed by the CLC/XCE idiom within the first four
    // bytes, which several beta/proto dumps in the population this
    // ticket was filed for actually do.
    bytes.windows(2).any(|w| w == [0x18, 0xFB])
}

/// When neither header location scores as plausible, guess the mapping
/// from the one thing every cartridge that boots actually has: a RESET
/// vector that points at real code (ticket W14-52). Real hardware never
/// reads $7FC0/$FFC0 at all — the header is a scoring convenience for
/// emulators, not something the SNES CPU consults — so a cartridge with a
/// corrupted checksum, garbage map-mode byte, or blanked-out title still
/// boots on real hardware provided its PCB is wired LoROM or HiROM and its
/// RESET vector is intact, which this fallback checks directly instead of
/// the header fields that failed scoring.
///
/// Tries LoROM, then HiROM, then (for images > 4 MiB) ExHiROM. For each:
/// the RESET vector at that mapping's header block must decode to an
/// address `>= $8000` (WRAM/hardware registers live below that in every
/// one of these maps, so a vector below it cannot be pointing at mapped
/// ROM) and the bytes at the file offset that address maps to must look
/// like a 65816 reset prologue ([`looks_like_reset_prologue`]). Among
/// mappings that qualify, prefers one whose OWN map-mode byte nibble
/// agrees with the mapping being tried, then (if LoROM and HiROM both
/// qualify) the one whose header title is majority-printable ASCII —
/// exactly the two extra signals `score_candidate` already uses, just
/// applied as a tie-break instead of a score.
/// Ticket W14-55: does the reset-vector target this guess just validated
/// fall INSIDE the very header block the guess is evaluating? Real code a
/// RESET vector points at is always outside the 64-byte header/vector-table
/// block (fullsnes "SNES Cartridge ROM Header" — the block is fixed-layout
/// data, never executable), so a vector that resolves back into its own
/// header is definitionally not a real reset target, whatever bytes happen
/// to sit there. This is the fix for The Lion King (USA) (Beta 3) (v.21):
/// its LoROM header block ($7FC0-$7FFF) is wholesale `$FF` filler,
/// including the vector field itself (`$7FFC`/`$7FFD` = `$FF $FF`, i.e. the
/// vector IS the filler value $FFFF, not a surviving real address) — which
/// decodes to file offset `$7FFF`, the last byte of that same header block.
/// The byte at `$7FFF` is `$FF` (still filler) and `looks_like_reset_
/// prologue` only accepts it because the CLC/XCE pair it finds at
/// `$7FFF+2..+4` (`$18 $FB`) actually belongs to the FOLLOWING bank's real
/// content (which happens to be HiROM's own genuine reset prologue at
/// `$8000`) — a one-byte-offset coincidence at the seam between the header
/// block and the next bank, not code this candidate's own vector reaches.
/// Rejecting any guess whose 4-byte read window overlaps its own header
/// block closes exactly this coincidence without needing a title-keyed
/// exception (law 5).
fn resolves_into_own_header_block(base: usize, file_offset: usize) -> bool {
    let header_end = base + HEADER_BLOCK_LEN;
    let read_end = file_offset + 4;
    file_offset < header_end && read_end > base
}

fn fallback_mapping_guess(data: &[u8]) -> Option<(SnesMapMode, usize, HeaderFallback)> {
    struct Guess {
        mode: SnesMapMode,
        base: usize,
        kind: HeaderFallback,
        expected_nibble: u8,
    }
    let mut guesses = vec![Guess {
        mode: SnesMapMode::LoRom,
        base: LOROM_HEADER_OFFSET,
        kind: HeaderFallback::LoRomResetVector,
        expected_nibble: 0x0,
    }];
    guesses.push(Guess {
        mode: SnesMapMode::HiRom,
        base: HIROM_HEADER_OFFSET,
        kind: HeaderFallback::HiRomResetVector,
        expected_nibble: 0x1,
    });
    if data.len() > EXHIROM_MIN_SIZE {
        guesses.push(Guess {
            mode: SnesMapMode::HiRom,
            base: EXHIROM_HEADER_OFFSET,
            kind: HeaderFallback::ExHiRomResetVector,
            expected_nibble: 0x5,
        });
    }

    let mut best: Option<(i32, usize, SnesMapMode, usize, HeaderFallback)> = None;
    for (order, g) in guesses.into_iter().enumerate() {
        if data.len() < g.base + HEADER_BLOCK_LEN {
            continue;
        }
        let reset_vector = u16::from_le_bytes([
            data[g.base + RESET_VECTOR_OFFSET],
            data[g.base + RESET_VECTOR_OFFSET + 1],
        ]);
        if reset_vector < 0x8000 {
            continue;
        }
        let file_offset = match g.kind {
            // LoROM bank $00: file offset 0 is CPU $8000 (fullsnes "SNES
            // Memory Map" LoROM row).
            HeaderFallback::LoRomResetVector => usize::from(reset_vector) - 0x8000,
            // HiROM bank $00's upper half mirrors bank $C0, whose file
            // offset equals the address directly.
            HeaderFallback::HiRomResetVector => usize::from(reset_vector),
            // ExHiROM bank $00's upper half maps into the cartridge's
            // SECOND 4 MiB (fullsnes "SNES Memory Map" ExHiROM row) — the
            // same half the $40FFC0 header itself lives in.
            HeaderFallback::ExHiRomResetVector => EXHIROM_MIN_SIZE + usize::from(reset_vector),
        };
        if file_offset + 4 > data.len() {
            continue;
        }
        // Ticket W14-55: a vector that resolves back into the header block
        // it came from is not pointing at real code, whatever the bytes
        // there happen to look like — see `resolves_into_own_header_block`'s
        // doc (The Lion King (Beta 3) shape).
        if resolves_into_own_header_block(g.base, file_offset) {
            continue;
        }
        if !looks_like_reset_prologue(&data[file_offset..file_offset + 4]) {
            continue;
        }
        let mode_byte = data[g.base + 0x15];
        let mut score = 0i32;
        if mode_byte & 0x0F == g.expected_nibble {
            score += 1;
        }
        let printable = data[g.base..g.base + 0x15]
            .iter()
            .filter(|&&b| (0x20..=0x7E).contains(&b))
            .count();
        if printable >= 12 {
            score += 1;
        }
        // Ticket W14-55, rule B: a candidate whose OWN header block is
        // internally sane outranks a bare prologue match at the other
        // location — the same "location wins" spirit `score_candidate`
        // already applies, just scored here instead of assumed from
        // prologue alone. A plausible ROM-size exponent (`score_candidate`'s
        // own $08-$0D bracket) and a map-mode byte with a VALID speed bit
        // (fullsnes "ROM Speed and Map Mode (FFD5h)": "Bit7-6 Always 0",
        // "Bit5 Always 1" — a real board's mode byte always has this exact
        // `mode_byte & 0xE0 == 0x20` shape, whatever its speed bit or low
        // nibble say) are both fields a wholesale-filler ($00 or $FF)
        // header block fails, unlike a location that merely happens to have
        // a plausible-looking byte or two.
        if (0x08..=0x0D).contains(&data[g.base + 0x17]) {
            score += 1;
        }
        if mode_byte & 0xE0 == 0x20 {
            score += 1;
        }
        // Order acts as the final tie-break (LoROM, then HiROM, then
        // ExHiROM), so only a STRICTLY better score displaces the earlier
        // guess.
        let candidate = (score, usize::MAX - order, g.mode, g.base, g.kind);
        if best.as_ref().is_none_or(|b| candidate.0 > b.0) {
            best = Some(candidate);
        }
    }
    best.map(|(_, _, mode, base, kind)| (mode, base, kind))
}

/// Ticket W14-55, rule A: once [`fallback_mapping_guess`] has picked a
/// mapping from the RESET vector alone, should the chipset byte AT THAT
/// LOCATION still be trusted to name a coprocessor?
///
/// W14-52 shipped a blanket "coprocessor is always `None`" rule for a
/// fallback-loaded cartridge, reasoning that a chipset byte found this way
/// "has no more credibility than the map-mode byte that just failed" — true
/// in general, but the population this ticket was filed over shows real
/// counter-examples: the three Star Fox 2 betas carry a fully-corroborated
/// GSU chipset (`$15`: coprocessor nibble $1, hw $5, squarely in fullsnes's
/// documented `$13-$1A` GSU-n range) at a LoROM location whose map-mode
/// NIBBLE independently agrees ($20, nibble $0 = LoROM) — the header failed
/// scoring only because its revision byte is $FF (fails `MAX_ROM_VERSION`)
/// and its checksum/complement both read `$FFFF` (fails the "not literally
/// 0/summed" checks), fields that say nothing about the chipset byte's own
/// trustworthiness. Stripping SuperFx here is what leaves the betas' CPU
/// spinning forever on a "wait for GSU ready" WRAM flag that no chip ever
/// sets (`docs/TESTING.md`'s W14-54 re-triage).
///
/// This function re-checks the SAME chipset ranges `parse_snes_header`'s
/// scored path already trusts (GSU $13-$1A; SA-1 $33-$35, gated on the
/// mode-mode nibble also reading $3, same W14-53 "both fields must agree"
/// shape; S-DD1 $43/$45, gated on the map-mode nibble reading $2; OBC1
/// $25; Cx4 $F3 with the `$FFBF` sub-type byte $10; DSP $03-$05 via
/// `known_non_dsp1_checksum`) and returns `Coprocessor::None` for anything
/// else — a chipset byte this build has no chip for (including ST010's
/// $F6: W19-04 is unimplemented as of this ticket, so nothing here can
/// "honour" it yet; deliberately NOT special-cased so a future W19-04 arm
/// can be added without colliding with this one) is exactly as garbage as
/// before, and NEVER refuses the cartridge outright — a chip this build
/// cannot run yet must still fall through to `None` here, not
/// `UnsupportedChip`, because doing otherwise would flip a title that
/// currently boots (uniform screen or better) into a fresh refusal, which
/// is a regression this ticket's acceptance does not ask for (Zool (Beta)'s
/// chipset $53 — coprocessor nibble $5 "S-RTC", hw $3 — matches none of
/// these ranges and stays `None`, exactly as the ticket names it).
///
/// Returns `(coprocessor, battery, dsp_window, map_mode_override)` — SA-1
/// and S-DD1 also need their own `SnesMapMode` (the memory map, not just
/// the chip), same as the scored path.
fn fallback_chipset_coprocessor(
    data: &[u8],
    base: usize,
    mode_nibble: u8,
    rom_size: usize,
    ram_size: usize,
    checksum: u16,
) -> (Coprocessor, bool, Option<DspWindow>, Option<SnesMapMode>) {
    let chipset = data[base + 0x16];
    let hw = chipset & 0x0F;
    let coprocessor_nibble = (chipset & 0xF0) >> 4;

    if coprocessor_nibble == 0x1 && (0x3..=0xA).contains(&hw) {
        // GSU-n, fullsnes "SNES Cart GSU-n Cartridge Header" — same
        // decision `parse_snes_header`'s scored path makes, see there for
        // the battery-bit citation.
        let version = if rom_size > GSU2_ROM_THRESHOLD_BYTES {
            SuperFxVersion::Gsu2
        } else {
            SuperFxVersion::Gsu1
        };
        let ram_kib = superfx_expansion_ram_kib(data, base);
        return (
            Coprocessor::SuperFx { version, ram_kib },
            matches!(hw, 0x5 | 0xA),
            None,
            None,
        );
    }
    if mode_nibble == 0x3 && coprocessor_nibble == 0x3 && (0x3..=0x5).contains(&hw) {
        // SA-1, W17-01/W14-53's "both fields must agree" shape: the
        // map-mode nibble at this SAME location must also read $3, not
        // just the chipset byte.
        return (
            Coprocessor::Sa1(Sa1Board {
                rom_len: rom_size,
                bwram_len: ram_size,
                iram_len: SA1_IRAM_LEN,
            }),
            hw == 0x5,
            None,
            Some(SnesMapMode::Sa1),
        );
    }
    if mode_nibble == 0x2 && coprocessor_nibble == 0x4 && (0x3..=0x5).contains(&hw) {
        // S-DD1, same "both fields must agree" shape (W19-03/W14-53).
        return (Coprocessor::Sdd1, hw == 0x5, None, Some(SnesMapMode::Sdd1));
    }
    if coprocessor_nibble == 0x2 && hw == 0x5 {
        // OBC1: chipset $25 is the only assigned combination.
        return (Coprocessor::Obc1, true, None, None);
    }
    if coprocessor_nibble == 0xF && hw == 0x3 && base >= 1 && data[base - 1] == 0x10 {
        // CX4: chipset $F3 with extended-header sub-type $10.
        return (Coprocessor::Cx4, false, None, None);
    }
    if coprocessor_nibble == 0x0
        && (0x3..=0x5).contains(&hw)
        && known_non_dsp1_checksum(checksum).is_none()
    {
        // DSP-1 (not a known DSP-2/3/4 checksum, W14-43).
        let map_mode = if base == LOROM_HEADER_OFFSET {
            SnesMapMode::LoRom
        } else {
            SnesMapMode::HiRom
        };
        return (
            Coprocessor::Dsp1,
            hw == 0x5,
            Some(dsp_window_for(map_mode, rom_size)),
            None,
        );
    }
    (Coprocessor::None, chipset == 0x02, None, None)
}

/// Build a [`SnesHeader`] once [`fallback_mapping_guess`] has already
/// confirmed a mapping from the RESET vector (ticket W14-52). The header
/// FIELDS at `base` are, by construction, the ones that just failed
/// plausibility scoring — so this trusts them far less than the scored
/// path does:
///
/// - `rom_size` comes from the ACTUAL image length, not the header's size
///   byte, which is exactly the field this ticket's population shows
///   corrupted most often (garbage exponents that would overflow
///   `kb_pow2`, or a value nowhere near the real dump size). The file's own
///   length is strictly more trustworthy than a byte inside the same
///   header block that just failed every other check.
/// - `ram_size` still reads the header byte through `kb_pow2`, falling
///   back to 0 (no RAM) on an implausible exponent rather than failing the
///   whole cartridge over a field that, worst case, only affects save
///   support.
/// - the coprocessor is read as `Coprocessor::None` UNLESS the chipset byte
///   at this location names a chip this build runs AND (where W14-53
///   requires it) the location's own map-mode nibble corroborates it —
///   ticket W14-55, [`fallback_chipset_coprocessor`]. A chipset byte this
///   build cannot run is treated exactly like a garbage one (`None`, never
///   a fresh refusal) rather than routed through a chip HLE on unreliable
///   evidence.
/// - `ExHiROM` is detected (so FR-CORE-013 can name it) but still refused:
///   `rf-snes` has no ExHiROM memory map regardless of how the header was
///   found, so accepting it here would just move the half-boot this ticket
///   exists to prevent from "bad header" to "bad map".
fn build_header_from_fallback(
    data: &[u8],
    mode: SnesMapMode,
    base: usize,
    kind: HeaderFallback,
    had_copier_header: bool,
) -> Result<SnesHeader, CartError> {
    if matches!(kind, HeaderFallback::ExHiRomResetVector) {
        return Err(CartError::UnsupportedChip {
            name: "ExHiROM (header-fallback RESET vector)".to_string(),
        });
    }

    let mode_byte = data[base + 0x15];
    let fast_rom = mode_byte & 0x10 != 0;
    let rom_size = data.len();
    // `kb_pow2` only refuses an exponent that overflows `usize` outright
    // (>=54 or so on a 64-bit build) — nowhere near tight enough for a
    // BYTE this ticket's whole premise is that we cannot trust. A raw
    // exponent like Beta F-Zero's real $24 (36) decodes "successfully" to
    // 64 TiB and downstream code allocates it. Real cartridge RAM never
    // exceeds a few hundred KiB (SA-1's 2 MiB BW-RAM is a different field
    // entirely, read only once the SA-1 chipset byte itself is trusted,
    // never through this fallback) so anything past the same plausible
    // ceiling `score_candidate` uses for ROM size ($0D, 8 MiB) is treated
    // as corrupt and defaulted to "no RAM" rather than allocated.
    let ram_size_byte = data[base + 0x18];
    let ram_size = if ram_size_byte <= 0x0D {
        kb_pow2(ram_size_byte).unwrap_or(0)
    } else {
        0
    };
    let checksum = u16::from_le_bytes([data[base + 0x1E], data[base + 0x1F]]);
    let checksum_complement = u16::from_le_bytes([data[base + 0x1C], data[base + 0x1D]]);
    let mode_nibble = mode_byte & 0x0F;

    let (coprocessor, battery, dsp_window, map_mode_override) =
        fallback_chipset_coprocessor(data, base, mode_nibble, rom_size, ram_size, checksum);
    let map_mode = map_mode_override.unwrap_or(mode);

    Ok(SnesHeader {
        map_mode,
        fast_rom,
        rom_size,
        ram_size,
        battery,
        coprocessor,
        dsp_window,
        checksum,
        checksum_complement,
        had_copier_header,
        header_fallback: Some(kind),
    })
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
            // Ticket W14-52: `score_candidate`'s necessary conditions
            // (country code, revision byte) failed at BOTH locations here
            // — a corrupted header can clear those too, e.g. a filler byte
            // over $14 in the country field — but that still says nothing
            // about whether the cartridge boots. Try the RESET-vector
            // fallback before giving up; see its doc and the call site
            // below for the full rationale.
            if let Some((mode, base, kind)) = fallback_mapping_guess(data) {
                return build_header_from_fallback(data, mode, base, kind, had_copier_header);
            }
            return Err(CartError::InvalidHeader(
                "no plausible SNES header at $7FC0 or $FFC0: neither location has a \
                 map mode matching it, an assigned country code and a plausible \
                 revision, and no RESET-vector fallback mapping produced a \
                 plausible reset prologue either"
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
        // Ticket W14-52: real hardware never reads $7FC0/$FFC0 at all —
        // scoring is an emulator convenience, not something a physical
        // cartridge needs to pass. Before giving up, check whether the
        // RESET vector alone (the one thing every cartridge that boots
        // truly has) picks out a mapping. A survey of this project's own
        // 74-title no-plausible-header population found the RESET-vector
        // prologue intact — and pointing at real code — for the large
        // majority of them: corrupted checksums, garbage map-mode bytes,
        // and blanked-out titles from betas/protos/pirates all still leave
        // the vector table alone.
        if let Some((mode, base, kind)) = fallback_mapping_guess(data) {
            return build_header_from_fallback(data, mode, base, kind, had_copier_header);
        }
        return Err(CartError::InvalidHeader(format!(
            "no plausible SNES header at $7FC0 or $FFC0: best candidate scored \
             {} of {} (checksum/complement, reset vector, map mode, title), and \
             no RESET-vector fallback mapping produced a plausible reset prologue \
             either",
            winner.score, MINIMUM_SCORE
        )));
    }

    let base = winner.base;
    let mode_byte = data[base + 0x15];
    let mode_nibble = mode_byte & 0x0F;
    let fast_rom = mode_byte & 0x10 != 0;
    // W14-36: nibble $0/$1 name LoROM/HiROM, but `score_candidate`'s own doc
    // already records that the nibble is evidence, not a requirement,
    // because a real dump's mode byte can disagree with WHERE its header
    // sits — `WWF Super WrestleMania (USA)` ships nibble $1 ("HiROM") at
    // the LoROM header location ($7FC0: valid checksum/complement pair,
    // legible title, reset vector $FF51 into bank 0's upper half). Before
    // this fix the nibble alone picked `SnesMapMode::HiRom` regardless of
    // `base`, so `mapping.rs` addressed a physically-LoROM cartridge as
    // HiROM: the reset-vector read at $00:FFFC resolved to the wrong file
    // offset, came back as $0000, and the CPU booted straight into WRAM
    // executing `BRK`/open-bus `$FF` in a loop that walked SP down until it
    // crashed into the vector table — the exact "$00:0003 BRK #$00 /
    // $00:FFFF SBC $000000,X" shape `docs/TESTING.md`'s W14-34 re-triage
    // named for this title. The winning LOCATION (`base`, already decided
    // by the checksum/reset-vector/title scoring above) is what actually
    // governs the memory map on real hardware — a LoROM board's header
    // lives at $7FC0 no matter what its mode byte happens to say — so it
    // takes priority over the nibble for $0/$1; only $2/$3 (S-DD1/SA-1,
    // which fullsnes documents as always headered at the LoROM location
    // too) and the explicitly-unsupported nibbles keep reading the nibble
    // itself.
    //
    // W14-52: this survey's population turned up the SAME shape for
    // nibbles fullsnes never assigns at all (`Super Adventure Island` ships
    // $44, `HAL's Hole in One Golf` ships $46 — both otherwise
    // excellent-scoring LoROM headers: valid checksum/complement, a
    // legible 21-byte title, a reset vector into real code). An
    // UNASSIGNED nibble can never legitimately name a real board — unlike
    // $5/$A, which name real (if unsupported) hardware this build must
    // still refuse honestly per FR-CORE-013 — so it gets the same
    // location-wins treatment as $0/$1 rather than an "unrecognized map
    // mode" refusal. `KNOWN_UNSUPPORTED_MAP_MODES` ($5 ExHiROM, $A SPC7110)
    // and $2/$3 (S-DD1/SA-1, both gated on the chipset byte separately
    // below) are deliberately EXCLUDED from this: those nibbles collide
    // with named chips this build cannot run (or, for $2/$3, can only run
    // when the chipset byte corroborates), and `Contra III`/`The Duel`/
    // `Krusty's Super Fun House`/`Space Football` in this same population
    // happen to hit exactly the SA-1/ExHiROM collisions (a map-mode nibble
    // with a chipset byte that does NOT corroborate the chip) — see
    // `docs/TESTING.md`'s W14-52 section for why loosening THAT specific
    // check is a separate call this ticket does not make unilaterally: the
    // SA-1 corroboration requirement is W17-01's own deliberate "both must
    // agree" ruling, pinned by
    // `sa1_map_mode_without_sa1_chipset_still_refuses`, and ticket W19-03
    // gives S-DD1 the identical treatment (see
    // `sdd1_map_mode_without_sdd1_chipset_still_refuses` below).
    //
    // Needed before the map-mode decision itself now (moved up from its
    // previous read point below, W14-53): the chipset byte is what tells
    // $2/$3/$5/$A ("SNES Cartridge ROM Header" §"ROM Speed and Map Mode
    // (FFD5h)": S-DD1, SA-1, ExHiROM, SPC7110) apart from a plain
    // LoROM/HiROM cartridge whose mode byte simply names the wrong chip.
    let checksum = u16::from_le_bytes([data[base + 0x1E], data[base + 0x1F]]);
    let checksum_complement = u16::from_le_bytes([data[base + 0x1C], data[base + 0x1D]]);
    let chipset = data[base + 0x16];
    let hw = chipset & 0x0F;
    let coprocessor_nibble = (chipset & 0xF0) >> 4;

    // W14-53: `Contra III - The Alien Wars (USA)` (and its Virtual Console
    // dump), `Krusty's Super Fun House (USA)`, `The Duel - Test Drive II
    // (USA)` and `Space Football - One on One (USA)` all reproduce the
    // SAME shape W14-52 named but declined to fix: an otherwise
    // excellent-scoring LoROM header ($7FC0: valid checksum/complement,
    // legible title, reset vector into a real `SEI`/`CLC`/`XCE` prologue —
    // confirmed against real archive dumps, see `docs/TESTING.md`'s W14-53
    // section for the printed bytes) whose map-mode nibble names a
    // coprocessor mapping ($3 SA-1 for both Contra III dumps, $2 S-DD1 for
    // The Duel, $5 ExHiROM for Krusty's and Space Football) while the
    // chipset byte at $FFD6/$7FD6 is plain `$00` (fullsnes: "00h ROM") —
    // no coprocessor nibble at all. fullsnes documents no mechanism by
    // which real hardware reads the map-mode nibble either (SNES memory
    // decoding is wired into the cartridge's PCB, not read from the ROM at
    // boot) — the field, like the whole header, exists for humans and
    // emulators, not the CPU — so a nibble the chipset byte does not
    // corroborate is exactly as untrustworthy here as it is for the
    // unassigned-nibble case above: a mastering quirk, not a hardware
    // fact, and the winning header LOCATION governs the real memory map
    // (W14-36) rather than the nibble.
    //
    // "Corroborated" means the chipset byte's own coprocessor nibble/hw
    // pair actually names that chip, per the same chapter's Chipset
    // (FFD6h) table:
    //   - $3 (SA-1): coprocessor nibble $3 with hw $3-$5 ("x3h
    //     ROM+Co-processor" .. "x5h ROM+Co-processor+RAM+Battery"; in
    //     practice $34/$35, "Dragon Ball Z - Hyper Dimension"/generic).
    //   - $2 (S-DD1): coprocessor nibble $4 with hw $3-$5 (same x3h-x5h
    //     shape; in practice $43/$45).
    //   - $A (SPC7110): chipset $F5 or $F9 ("Fxh.00h Co-processor is
    //     Custom (SPC7110)") with the $FFBF sub-type byte ($FFC0-1,
    //     `base - 1` in this crate's arithmetic, same as the CX4 check
    //     below) equal to $00.
    //   - $5 (ExHiROM) has NO coprocessor nibble of its own at all — it
    //     is a bigger memory map, not a chip — so nothing in the chipset
    //     byte can corroborate it. fullsnes's own note ("ExHiROM is used
    //     only by 'Dai Kaiju Monogatari 2' and 'Tales of Phantasia'")
    //     together with its ROM-size table shows every real ExHiROM
    //     release exceeds 4 MiB — the whole reason the map mode exists —
    //     which is the same `EXHIROM_MIN_SIZE` threshold the RESET-vector
    //     fallback already uses, applied here as the corroborating fact
    //     instead.
    //
    // When a nibble IS corroborated, nothing changes from before this
    // ticket: SA-1 is accepted (below), and ticket W19-03 now accepts
    // S-DD1 the same way (see the `Sdd1` arm of the coprocessor decision
    // below); ExHiROM/SPC7110 are still refused by name — this build has
    // no memory map or bus wiring for either of them. Only the
    // UNCORROBORATED case is new.
    let coprocessor_nibble_corroborated = match mode_nibble {
        0x3 => coprocessor_nibble == 0x3 && (0x3..=0x5).contains(&hw),
        0x2 => coprocessor_nibble == 0x4 && (0x3..=0x5).contains(&hw),
        0x5 => data.len() > EXHIROM_MIN_SIZE,
        0xA => matches!(chipset, 0xF5 | 0xF9) && base >= 1 && data[base - 1] == 0x00,
        // Not a chip-naming nibble ($0/$1 or unassigned) — corroboration
        // doesn't apply, so this must not gate anything below.
        _ => true,
    };

    let map_mode = match mode_nibble {
        0x0 | 0x1 => {
            if base == LOROM_HEADER_OFFSET {
                SnesMapMode::LoRom
            } else {
                SnesMapMode::HiRom
            }
        }
        0x2 if coprocessor_nibble_corroborated => SnesMapMode::Sdd1,
        0x3 if coprocessor_nibble_corroborated => SnesMapMode::Sa1,
        nibble
            if KNOWN_UNSUPPORTED_MAP_MODES.contains(&nibble) && coprocessor_nibble_corroborated =>
        {
            return Err(CartError::UnsupportedChip {
                name: format!(
                    "{} (SNES map mode ${mode_byte:02X})",
                    map_mode_name(mode_nibble)
                ),
            });
        }
        // W14-53: $2/$3/$5/$A without chipset corroboration, and every
        // still-unassigned nibble (W14-52) — location wins.
        _ => {
            if base == LOROM_HEADER_OFFSET {
                SnesMapMode::LoRom
            } else {
                SnesMapMode::HiRom
            }
        }
    };

    let rom_size = kb_pow2(data[base + 0x17])?;
    let ram_size = kb_pow2(data[base + 0x18])?;

    // The header's "DSP" chipset byte ($03/$04/$05, coprocessor nibble $0)
    // is the same for DSP-1, DSP-2, DSP-3 and DSP-4 — fullsnes and
    // snes.nesdev.org both note the byte alone cannot tell the variants
    // apart, which is why every emulator that supports more than one of
    // them (bsnes/higan, snes9x) disambiguates from a per-title board
    // database keyed by the header checksum, not the chipset byte
    // (W14-43, below).
    // D-010 / FR-CORE-038: coprocessor nibble $0 ("DSP") with hw in 3..=5
    // (ROM+coprocessor / +RAM / +RAM+battery — fullsnes's three assigned
    // "DSP" hw values) is lifted out of the refusal below and accepted as
    // Coprocessor::Dsp1 instead. hw=6..=15 is not an assigned combination
    // for any chipset and still refuses exactly as before, same as every
    // other coprocessor nibble at hw>=3.
    //
    // W14-43: EXCEPT when the checksum matches a known non-DSP-1 board —
    // see `known_non_dsp1_checksum`'s doc. Before this, a $03 "DSP"
    // chipset ROM whose real chip is DSP-4 (Top Gear 3000: checksum
    // $5327) was silently accepted as `Coprocessor::Dsp1` and routed
    // through the DSP-1 HLE (`crate::dsp1`, ticket W14-19), which answers
    // every idle/unmatched read with $80 (fullsnes DSP-1: "idle reads
    // return $80") — a value the game's `LDA $308000` polling loop, which
    // is waiting on a DSP-4 status byte no DSP-1 command ever produces,
    // never sees change, so the title hung forever instead of failing
    // honestly. `docs/TESTING.md`'s W14-34 re-triage named this as a
    // "ROM-mirror mismatch"; tracing it with `title_probe`'s `PROBE_PEEK`
    // showed the polled byte was the DSP-1 HLE's own $80 sentinel, not a
    // ROM-mirroring bug at all.
    let (coprocessor, battery) = if map_mode == SnesMapMode::Sdd1 {
        // Ticket W19-03, corrected by W14-53: map mode $22 is only reached
        // here (see `coprocessor_nibble_corroborated` above) when the
        // chipset byte already names S-DD1 (fullsnes "SNES Cartridge ROM
        // Header", Chipset (FFD6h): coprocessor nibble $4, hw $3-$5, same
        // generic "x3h ROM+Co-processor" .. "x5h ROM+Co-processor+RAM+
        // Battery" shape SA-1 uses), so this check must match
        // `coprocessor_nibble_corroborated`'s `0x2` arm exactly ($3..=$5,
        // not just the two chipset bytes seen in the wild) — a narrower
        // range here would let hw=$4 corroborate the map-mode decision
        // above and then hit the `else` below as dead, mislabelled code.
        // The "in practice" chipset list gives only two combinations: $43
        // (ROM+S-DD1, no RAM/battery) and $45 (ROM+S-DD1+RAM+Battery); hw=$4
        // ($44) is an assigned-shape combination fullsnes's own table
        // allows but no known cart ships, same "in practice" gap SA-1
        // documents for its own range. A header naming map mode $22
        // WITHOUT a corroborating chipset byte no longer reaches this
        // branch at all: `map_mode` itself falls back to the winning
        // header LOCATION instead, the same location-wins treatment
        // W14-36 gives $0/$1 collisions. This inner check is genuinely
        // defense in depth (always true when reached), same shape as the
        // DSP-1 hw check below.
        if coprocessor_nibble == 0x4 && (0x3..=0x5).contains(&hw) {
            (Coprocessor::Sdd1, hw == 0x5)
        } else {
            return Err(CartError::UnsupportedChip {
                name: format!(
                    "{} (SNES chipset ${chipset:02X})",
                    coprocessor_name(chipset)
                ),
            });
        }
    } else if map_mode == SnesMapMode::Sa1 {
        // D-013 / ticket W17-01, corrected by W14-53: map mode $23 is only
        // reached here (see `coprocessor_nibble_corroborated` above) when
        // the chipset byte already names SA-1 (fullsnes "SNES Cartridge
        // ROM Header", Chipset (FFD6h): coprocessor nibble $3, hw $3-$5,
        // "x3h ROM+Co-processor" .. "x5h ROM+Co-processor+RAM+Battery"; in
        // practice $34/$35), so this check is always true when reached —
        // kept as defense in depth, same shape as the DSP-1 hw check below.
        // A header naming map mode $23 WITHOUT a corroborating chipset
        // byte no longer reaches this branch at all: `map_mode` itself
        // falls back to the winning header LOCATION instead (W14-53), the
        // same location-wins treatment W14-36 gives $0/$1 collisions.
        if coprocessor_nibble == 0x3 && (0x3..=0x5).contains(&hw) {
            (
                Coprocessor::Sa1(Sa1Board {
                    rom_len: rom_size,
                    bwram_len: ram_size,
                    iram_len: SA1_IRAM_LEN,
                }),
                hw == 0x5,
            )
        } else {
            return Err(CartError::UnsupportedChip {
                name: format!(
                    "{} (SNES chipset ${chipset:02X})",
                    coprocessor_name(chipset)
                ),
            });
        }
    } else if (0x3..=0x5).contains(&hw)
        && coprocessor_nibble == 0x0
        && known_non_dsp1_checksum(checksum).is_none()
    {
        (Coprocessor::Dsp1, hw == 0x5)
    } else if (0x3..=0x5).contains(&hw) && coprocessor_nibble == 0x0 {
        // Matched a known non-DSP-1 checksum above: refuse honestly,
        // naming the real chip, instead of misrouting through the DSP-1
        // HLE (see the doc note above this `if`/`else` chain).
        return Err(CartError::UnsupportedChip {
            name: format!(
                "{} (SNES chipset ${chipset:02X}, not DSP-1)",
                known_non_dsp1_checksum(checksum).unwrap_or("unknown DSP variant")
            ),
        });
    } else if coprocessor_nibble == 0x1 && (0x3..=0xA).contains(&hw) {
        // Ticket W18-01 / fullsnes "SNES Cart GSU-n Cartridge Header":
        // "[FFD6h]=13h..1Ah Chipset = GSUn (plus battery present/absent
        // info)" — the full documented range. Battery presence is not
        // spelled out bit-by-bit, so this reads it off the same chapter's
        // own PCB table ("List of Games, Chips, and PCB versions"): boards
        // whose row names "Battery" (`SHVC-1CA6B-01` Stunt Race FX hw=$A,
        // `SHVC-1CB5B-01`/`-20` Yoshi's Island hw=$5) against boards whose
        // row does not (`SHVC-1CA0N5S-01`/`-1CA0N6S-01` Dirt Racer/Vortex/
        // Dirt Trax FX hw=$4, `SHVC-1CB0N7S-01` Doom hw=$4) — confirmed by
        // reading real archive headers in this project's own ROM library
        // (Star Fox hw=$3 no RAM, Dirt Trax FX/Doom/Vortex hw=$4 RAM no
        // battery, Yoshi's Island/Star Fox 2 hw=$5, Stunt Race FX hw=$A,
        // all RAM+battery) rather than trusting an unwritten bit rule.
        let version = if rom_size > GSU2_ROM_THRESHOLD_BYTES {
            SuperFxVersion::Gsu2
        } else {
            SuperFxVersion::Gsu1
        };
        let ram_kib = superfx_expansion_ram_kib(data, base);
        (
            Coprocessor::SuperFx { version, ram_kib },
            matches!(hw, 0x5 | 0xA),
        )
    } else if coprocessor_nibble == 0xF && hw == 0x3 && base >= 1 && data[base - 1] == 0x10 {
        // Ticket W19-02 / fullsnes "CX4 Cartridge Header": "[FFD6]=F3h
        // ;ROM+CustomChip (no battery, no sram)" together with
        // "[FFBF]=10h ;CustomChip=CX4" — the extended-header sub-type byte
        // is what distinguishes the CX4 from every other chip nibble $F
        // ("custom") covers (e.g. ST010/ST011's own $FFBF=$01, documented
        // in the neighbouring DSP-n/ST010/ST011 chapter). `base - 1` is
        // `$FFC0 - 1 = $FFBF` in the chapter's own `$FFC0`-relative
        // notation, the same arithmetic `superfx_expansion_ram_kib` uses
        // for `$FFBD` (`base - 3`).
        (Coprocessor::Cx4, false)
    } else if coprocessor_nibble == 0x2 && hw == 0x5 {
        // Ticket W19-01 / fullsnes "SNES Cart OBC1": chipset $25 only —
        // the sole assigned OBC1 combination (ROM+OBC1+RAM+battery).
        // Always battery-backed (the chapter: "8Kbyte battery-backed
        // SRAM").
        (Coprocessor::Obc1, true)
    } else if hw >= 0x3 {
        return Err(CartError::UnsupportedChip {
            name: format!(
                "{} (SNES chipset ${chipset:02X})",
                coprocessor_name(chipset)
            ),
        });
    } else {
        (Coprocessor::None, hw == 0x2)
    };
    let dsp_window = match coprocessor {
        Coprocessor::Dsp1 => Some(dsp_window_for(map_mode, rom_size)),
        Coprocessor::None
        | Coprocessor::Sa1(_)
        | Coprocessor::SuperFx { .. }
        | Coprocessor::Cx4 => None,

        Coprocessor::Obc1 => None,
        Coprocessor::Sdd1 => None,
    };

    Ok(SnesHeader {
        map_mode,
        fast_rom,
        rom_size,
        ram_size,
        battery,
        coprocessor,
        dsp_window,
        checksum,
        checksum_complement,
        had_copier_header,
        header_fallback: None,
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
    /// W14-36 regression: `WWF Super WrestleMania (USA)`'s real dump has a
    /// valid, self-consistent header at the LoROM location ($7FC0) —
    /// checksum/complement XOR to $FFFF, a legible 21-byte title, and a
    /// reset vector ($FF51) into bank 0's upper half — but its map-mode
    /// byte is $41, whose low nibble ($1) names HiROM. Before this fix
    /// `map_mode` was read straight off that nibble regardless of where
    /// the header actually won, so this cartridge was addressed as HiROM:
    /// `$00:FFFC`'s reset-vector read resolved to the wrong file offset,
    /// came back $0000 instead of $FF51, and the CPU booted into WRAM
    /// executing BRK/open-bus in a loop that crashed into the vector
    /// table — the exact shape `docs/TESTING.md`'s W14-34 re-triage named
    /// for this title. The winning LOCATION must govern the map mode, not
    /// the disagreeing nibble.
    fn nibble_disagreeing_with_the_winning_location_defers_to_location() {
        let rom = lorom_image(0x41, 0x00); // WWF Super WrestleMania's real mode byte
        let header = parse_snes_header(&rom).expect("valid LoROM header despite the nibble");
        assert_eq!(
            header.map_mode,
            SnesMapMode::LoRom,
            "the LoROM header location must win over a disagreeing nibble"
        );
        assert!(
            !header.fast_rom,
            "$41's bit 0x10 is clear, so this is SlowROM"
        );
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
    fn detects_superfx_gsu1_from_chipset_byte() {
        // Ticket W18-01: chipset $15 (ROM+Super FX+RAM+Battery) is now
        // accepted, not refused — 64 KB ROM stays under the GSU2
        // threshold, so this is GSU1.
        let rom = lorom_image(0x20, 0x15);
        let header = parse_snes_header(&rom).expect("GSU cart accepted");
        assert!(header.battery, "hw=$5 is a battery board");
        match header.coprocessor {
            Coprocessor::SuperFx { version, .. } => {
                assert_eq!(version, SuperFxVersion::Gsu1);
            }
            other => panic!("expected Coprocessor::SuperFx, got {other:?}"),
        }
    }

    #[test]
    fn detects_superfx_gsu2_from_rom_size() {
        // fullsnes's own heuristic: >1 MiB ROM selects GSU2.
        let mut data = vec![0u8; 0x8000];
        let base = LOROM_HEADER_OFFSET;
        data[base + 0x15] = 0x20;
        data[base + 0x16] = 0x15;
        data[base + 0x17] = 0x0B; // 1<<11 KB = 2048 KB = 2 MiB
        data[base + 0x18] = 0;
        set_checksum(&mut data, base, 0xBEEF);
        set_reset_vector(&mut data, base, 0x8000);
        let header = parse_snes_header(&data).expect("GSU cart accepted");
        match header.coprocessor {
            Coprocessor::SuperFx { version, .. } => {
                assert_eq!(version, SuperFxVersion::Gsu2);
            }
            other => panic!("expected Coprocessor::SuperFx, got {other:?}"),
        }
    }

    #[test]
    fn detects_cx4_from_chipset_and_extended_subtype() {
        // Ticket W19-02: chipset $F3 (ROM+CustomChip) AND extended-header
        // $FFBF=$10 together name the Cx4 (fullsnes "CX4 Cartridge
        // Header").
        let mut rom = lorom_image(0x20, 0xF3);
        let base = LOROM_HEADER_OFFSET;
        rom[base - 1] = 0x10;
        let header = parse_snes_header(&rom).expect("Cx4 cart accepted");
        assert_eq!(header.coprocessor, Coprocessor::Cx4);
        assert!(
            !header.battery,
            "fullsnes: chipset $F3 is no battery, no sram"
        );
    }

    #[test]
    fn chipset_f3_without_cx4_subtype_is_refused() {
        // Same chipset byte, but the extended-header sub-type does NOT
        // name Cx4 (e.g. an unassigned/other custom-chip value) — this
        // build refuses it honestly rather than guessing.
        let mut rom = lorom_image(0x20, 0xF3);
        let base = LOROM_HEADER_OFFSET;
        rom[base - 1] = 0x01; // ST010/ST011's own sub-type, not Cx4
        let err = parse_snes_header(&rom).unwrap_err();
        match &err {
            CartError::UnsupportedChip { name } => {
                assert!(name.contains("custom coprocessor"), "got: {name}");
            }
            other => panic!("expected UnsupportedChip, got {other:?}"),
        }
    }

    #[test]
    fn other_custom_coprocessor_hw_values_are_refused() {
        // Nibble $F ("custom") at a hw other than $3 is not a documented
        // Cx4 chipset byte at all.
        let mut rom = lorom_image(0x20, 0xF5);
        let base = LOROM_HEADER_OFFSET;
        rom[base - 1] = 0x10;
        let err = parse_snes_header(&rom).unwrap_err();
        assert!(matches!(err, CartError::UnsupportedChip { .. }));
    }

    #[test]
    fn superfx_no_battery_no_ram_chipset() {
        // hw=$3 (Star Fox's real chipset byte): no RAM, no battery.
        let rom = lorom_image(0x20, 0x13);
        let header = parse_snes_header(&rom).expect("GSU cart accepted");
        assert!(!header.battery);
        match header.coprocessor {
            Coprocessor::SuperFx { ram_kib, .. } => assert_eq!(ram_kib, 0),
            other => panic!("expected Coprocessor::SuperFx, got {other:?}"),
        }
    }

    #[test]
    fn superfx_ram_no_battery_chipset() {
        // hw=$4 (Doom/Vortex/Dirt Trax FX's real chipset byte): RAM, no
        // battery. Extended-header expansion RAM byte set to $05 (32 KiB),
        // 3 bytes before the LoROM header base.
        let mut rom = lorom_image(0x20, 0x14);
        rom[LOROM_HEADER_OFFSET - 3] = 0x05;
        let header = parse_snes_header(&rom).expect("GSU cart accepted");
        assert!(!header.battery);
        match header.coprocessor {
            Coprocessor::SuperFx { ram_kib, .. } => assert_eq!(ram_kib, 32),
            other => panic!("expected Coprocessor::SuperFx, got {other:?}"),
        }
    }

    #[test]
    fn superfx_extended_header_absent_reports_32kib_ram() {
        // A real dump with no extended header (fullsnes's own caution,
        // reproduced here rather than only asserted): the bytes preceding
        // the header are the flash-erase fill value $FF, not a real size
        // exponent — this is Star Fox (USA)'s actual header shape.
        // fullsnes states the fact directly for this exact case: "RAM Size
        // for Starfox/Starwing is 32Kbytes" (ticket W18-04: the previous
        // `0` here was the Star Fox boot regression's real root cause —
        // no GSU RAM meant the decompressor's own RAM-sourced ROM pointer
        // read back zero instead of the SNES-supplied base address).
        let mut rom = lorom_image(0x20, 0x13);
        rom[LOROM_HEADER_OFFSET - 3] = 0xFF;
        let header = parse_snes_header(&rom).expect("GSU cart accepted");
        match header.coprocessor {
            Coprocessor::SuperFx { ram_kib, .. } => assert_eq!(ram_kib, 32),
            other => panic!("expected Coprocessor::SuperFx, got {other:?}"),
        }
    }

    #[test]
    fn superfx_battery_chipset_variant_1a() {
        // hw=$A (Stunt Race FX's real chipset byte): RAM+battery, the
        // alternate encoding fullsnes's "13h..1Ah" range documents.
        let rom = lorom_image(0x20, 0x1A);
        let header = parse_snes_header(&rom).expect("GSU cart accepted");
        assert!(header.battery);
        assert!(matches!(header.coprocessor, Coprocessor::SuperFx { .. }));
    }

    #[test]
    fn superfx_out_of_range_hw_still_refuses() {
        // hw=$B is outside the documented $3..=$A range: still an
        // unsupported chip, not silently accepted.
        let rom = lorom_image(0x20, 0x1B);
        let err = parse_snes_header(&rom).unwrap_err();
        match &err {
            CartError::UnsupportedChip { name } => {
                assert!(name.contains("Super FX"), "got: {name}")
            }
            other => panic!("expected UnsupportedChip, got {other:?}"),
        }
    }

    #[test]
    fn non_gsu_cart_unaffected_by_superfx_detection() {
        // A plain ROM+RAM+battery cart (chipset $02, coprocessor nibble
        // $0's sibling `hw` values are DSP-only) still parses as
        // `Coprocessor::None` — W18-01 only touches nibble $1.
        let rom = lorom_image(0x20, 0x02);
        let header = parse_snes_header(&rom).expect("plain cart accepted");
        assert_eq!(header.coprocessor, Coprocessor::None);
        assert!(header.battery);
    }

    #[test]
    /// Ticket W14-05: filler whose nibble happens to name a real map mode
    /// is still refused — with the chip's name, which is what FR-CORE-013
    /// asks for, rather than as an unreadable file.
    ///
    /// Filler value $05 (ExHiROM), not $02: ticket W19-03 lifted S-DD1's
    /// nibble ($2) out of `KNOWN_UNSUPPORTED_MAP_MODES` (the same move
    /// W17-01 made for SA-1's nibble $3) — plain repeated-byte filler with
    /// no corroborating chipset byte now falls through to the ordinary
    /// candidate-scoring path and is refused as an implausible header
    /// (`CartError::InvalidHeader`, exercised by
    /// `sdd1_map_mode_without_sdd1_chipset_still_refuses` below via a
    /// properly-shaped-but-uncorroborated header instead), not shortcut-
    /// named here. ExHiROM ($5) stays in the always-refuse list, so it is
    /// still the right filler value for this test's original intent.
    fn filler_matching_a_named_map_mode_is_refused_by_name() {
        let data = vec![0x05u8; 0x10000];
        match parse_snes_header(&data).unwrap_err() {
            CartError::UnsupportedChip { name } => {
                assert!(name.contains("ExHiROM"), "got: {name}");
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
    fn unsupported_chip_obc1_reported_from_chipset_byte() {
        // hw=3 (ROM+coprocessor), nibble 2 = OBC1, but only hw=5 (chipset
        // $25) is a real assigned combination (ticket W19-01) — hw=3 stays
        // refused, unaffected by the DSP carve-out (D-010 only touches
        // nibble 0) and by W19-01 (which only accepts hw=5).
        let rom = lorom_image(0x20, 0x23);
        let err = parse_snes_header(&rom).unwrap_err();
        match &err {
            CartError::UnsupportedChip { name } => assert!(name.contains("OBC1"), "got: {name}"),
            other => panic!("expected UnsupportedChip, got {other:?}"),
        }
    }

    #[test]
    /// Ticket W19-01: chipset $25 (nibble 2 "OBC1", hw 5) is Metal Combat:
    /// Falcon's Revenge's real header byte (verified against the archive
    /// dump: `$7FC0+0x16 == 0x25`), and is the only chipset byte fullsnes
    /// assigns to this chip.
    fn chipset_0x25_detected_as_obc1_with_battery() {
        let rom = lorom_image(0x20, 0x25);
        let header = parse_snes_header(&rom).expect("valid OBC1 header");
        assert_eq!(header.coprocessor, Coprocessor::Obc1);
        assert!(header.battery, "OBC1 is always battery-backed SRAM");
        assert_eq!(header.dsp_window, None);
    }

    #[test]
    /// W14-43: a $03 "DSP" chipset ROM whose header checksum matches Top
    /// Gear 3000's ($5327, DSP-4) must be refused by name, not accepted
    /// as `Coprocessor::Dsp1` — the checksum, not the chipset byte, is
    /// what disambiguates DSP-1 from DSP-4 (see `known_non_dsp1_checksum`'s
    /// doc). Everything else about the image is the same shape
    /// `dsp1_lorom_cart_parses_with_bus_window` below accepts, so this
    /// isolates the checksum as the only thing that must change the
    /// outcome.
    fn known_dsp4_checksum_is_refused_honestly_not_run_as_dsp1() {
        let mut rom = lorom_image(0x20, 0x03);
        set_checksum(&mut rom, LOROM_HEADER_OFFSET, 0x5327);
        let err = parse_snes_header(&rom).expect_err("DSP-4 must be refused, not run as DSP-1");
        match err {
            CartError::UnsupportedChip { name } => assert!(
                name.contains("DSP-4"),
                "refusal must name the real chip, got: {name}"
            ),
            other => panic!("expected UnsupportedChip, got {other:?}"),
        }
    }

    #[test]
    /// D-010 / FR-CORE-038: nibble 0 ("DSP") at hw=3 (ROM+coprocessor, no
    /// RAM/battery) now parses instead of refusing, with the LoROM <=1 MiB
    /// window (lorom_image's default 64 KB size byte).
    fn dsp1_lorom_cart_parses_with_bus_window() {
        let rom = lorom_image(0x20, 0x03);
        let header = parse_snes_header(&rom).expect("DSP-1 LoROM cart must parse");
        assert_eq!(header.coprocessor, Coprocessor::Dsp1);
        assert!(!header.battery, "hw=3 has no battery");
        assert_eq!(
            header.dsp_window,
            Some(DspWindow {
                banks: [0x20..=0x3F, 0xA0..=0xBF],
                dr: 0x8000..=0xBFFF,
                sr: 0xC000..=0xFFFF,
            })
        );
    }

    #[test]
    /// hw=5 (ROM+DSP+RAM+battery) sets battery, matching the existing
    /// hw==2 rule for the non-coprocessor case.
    fn dsp1_cart_with_hw5_sets_battery() {
        let rom = lorom_image(0x20, 0x05);
        let header = parse_snes_header(&rom).expect("DSP-1+RAM+battery cart must parse");
        assert_eq!(header.coprocessor, Coprocessor::Dsp1);
        assert!(header.battery);
    }

    #[test]
    /// DSP-1B boards (LoROM > 1 MiB) get the wider snes9x window, selected
    /// by the header's declared ROM size, never by title (law 5).
    fn dsp1_lorom_over_1mib_gets_dsp1b_window() {
        let mut rom = lorom_image(0x20, 0x03);
        rom[LOROM_HEADER_OFFSET + 0x17] = 11; // 1<<11 KB = 2048 KB = 2 MiB
        let header = parse_snes_header(&rom).expect("DSP-1B LoROM cart must parse");
        assert_eq!(header.rom_size, 2 * 1024 * 1024);
        assert_eq!(
            header.dsp_window,
            Some(DspWindow {
                banks: [0x60..=0x6F, 0xE0..=0xEF],
                dr: 0x0000..=0x3FFF,
                sr: 0x4000..=0x7FFF,
            })
        );
    }

    #[test]
    fn dsp1_hirom_cart_parses_with_bus_window() {
        let rom = hirom_image(0x21, 0x03);
        let header = parse_snes_header(&rom).expect("DSP-1 HiROM cart must parse");
        assert_eq!(header.coprocessor, Coprocessor::Dsp1);
        assert_eq!(
            header.dsp_window,
            Some(DspWindow {
                banks: [0x00..=0x1F, 0x80..=0x9F],
                dr: 0x6000..=0x6FFF,
                sr: 0x7000..=0x7FFF,
            })
        );
    }

    #[test]
    /// Nibble 0 ("DSP") but hw=9 is not one of the three assigned DSP hw
    /// values (3/4/5) — D-010 only lifts those, so this must still refuse
    /// exactly as before, naming "DSP".
    fn dsp_nibble_with_unassigned_hw_value_still_refuses() {
        let rom = lorom_image(0x20, 0x09);
        let err = parse_snes_header(&rom).unwrap_err();
        match &err {
            CartError::UnsupportedChip { name } => assert!(name.contains("DSP"), "got: {name}"),
            other => panic!("expected UnsupportedChip, got {other:?}"),
        }
    }

    #[test]
    /// D-013 / ticket W17-01: chipset $34 (ROM+SA-1+RAM, hw=4) under map
    /// mode $23 parses instead of refusing, carrying the board data
    /// rf-snes needs (ROM/BW-RAM sizes from `lorom_image`'s default size
    /// bytes: 64 KiB ROM, 8 KiB RAM; I-RAM fixed at 2 KiB).
    fn sa1_cart_chipset_34_parses_with_board_data() {
        let rom = lorom_image(0x23, 0x34);
        let header = parse_snes_header(&rom).expect("SA-1 cart must parse");
        assert_eq!(header.map_mode, SnesMapMode::Sa1);
        assert_eq!(
            header.coprocessor,
            Coprocessor::Sa1(Sa1Board {
                rom_len: 64 * 1024,
                bwram_len: 8 * 1024,
                iram_len: SA1_IRAM_LEN,
            })
        );
        assert!(!header.battery, "hw=4 has no battery");
        assert_eq!(header.dsp_window, None, "SA-1 is not the DSP-1");
    }

    #[test]
    /// Chipset $35 (+battery) sets the battery flag, matching the DSP-1
    /// hw==5 rule.
    fn sa1_cart_chipset_35_sets_battery() {
        let rom = lorom_image(0x23, 0x35);
        let header = parse_snes_header(&rom).expect("SA-1+battery cart must parse");
        assert!(header.battery);
        match header.coprocessor {
            Coprocessor::Sa1(board) => assert_eq!(board.bwram_len, 8 * 1024),
            other => panic!("expected Coprocessor::Sa1, got {other:?}"),
        }
    }

    #[test]
    /// SA-1's own I-RAM size is fixed by hardware, not the header — it
    /// must be 2 KiB regardless of the declared ROM/RAM sizes.
    fn sa1_cart_iram_is_always_2kib() {
        let mut rom = lorom_image(0x23, 0x34);
        rom[LOROM_HEADER_OFFSET + 0x17] = 11; // 2 MiB ROM
        rom[LOROM_HEADER_OFFSET + 0x18] = 8; // 256 KiB BW-RAM
        let header = parse_snes_header(&rom).expect("SA-1 cart must parse");
        match header.coprocessor {
            Coprocessor::Sa1(board) => {
                assert_eq!(board.rom_len, 2 * 1024 * 1024);
                assert_eq!(board.bwram_len, 256 * 1024);
                assert_eq!(board.iram_len, SA1_IRAM_LEN);
            }
            other => panic!("expected Coprocessor::Sa1, got {other:?}"),
        }
    }

    #[test]
    /// W14-53 (D-013's "both must agree" ruling, CORRECTED): map mode $23
    /// without a matching SA-1 chipset byte is no longer refused — it is
    /// the exact shape `Contra III - The Alien Wars (USA)` ships (real
    /// dump: LoROM header at $7FC0, checksum/complement valid, mode byte
    /// $53, chipset $00 plain ROM; see `docs/TESTING.md`'s W14-53 section
    /// for the full printed bytes of all five retail carts this rule
    /// fixes). fullsnes documents no mechanism by which real hardware
    /// reads the map-mode nibble at all, so an uncorroborated nibble is a
    /// mastering quirk and the winning header LOCATION governs, same as
    /// W14-36's $0/$1 collisions and W14-52's unassigned nibbles.
    fn sa1_nibble_without_sa1_chipset_defers_to_location_not_refused() {
        let rom = lorom_image(0x23, 0x00); // map mode SA-1, chipset plain ROM
        let header =
            parse_snes_header(&rom).expect("uncorroborated SA-1 nibble must defer to location");
        assert_eq!(header.map_mode, SnesMapMode::LoRom);
        assert_eq!(header.coprocessor, Coprocessor::None);
    }

    #[test]
    /// One test per shape named in W14-53's acceptance: a HiROM-location
    /// header (checksum/complement valid, reset vector into bank $C0's
    /// upper half) whose mode byte's low nibble names SA-1 ($3) but whose
    /// chipset byte is $02 (ROM+RAM+Battery, no coprocessor) — still not
    /// corroborated, so HiROM (the winning location) governs, not SA-1 nor
    /// a refusal.
    fn hirom_location_with_sa1_nibble_and_plain_chipset_is_hirom() {
        let rom = hirom_image(0x23, 0x02);
        let header = parse_snes_header(&rom).expect("must parse as plain HiROM");
        assert_eq!(header.map_mode, SnesMapMode::HiRom);
        assert_eq!(header.coprocessor, Coprocessor::None);
        assert!(header.battery, "chipset $02 is ROM+RAM+Battery");
    }

    #[test]
    /// Second shape: LoROM location, mode nibble $2 (S-DD1), chipset $00
    /// (plain ROM) — `The Duel - Test Drive II (USA)`'s real shape.
    /// Uncorroborated, so LoROM wins.
    fn lorom_location_with_sdd1_nibble_and_plain_chipset_is_lorom() {
        let rom = lorom_image(0x22, 0x00);
        let header = parse_snes_header(&rom).expect("must parse as plain LoROM");
        assert_eq!(header.map_mode, SnesMapMode::LoRom);
        assert_eq!(header.coprocessor, Coprocessor::None);
    }

    #[test]
    /// Third shape: LoROM location, mode nibble $5 (ExHiROM), a 1 MiB image
    /// — `Krusty's Super Fun House (USA)`/`Space Football - One on One
    /// (USA)`'s real shape (both 512 KiB in the actual dumps; 1 MiB used
    /// here to also exercise a non-trivial `rom_size`). ExHiROM has no
    /// chipset-byte corroboration of its own (fullsnes: it names a bigger
    /// memory map, not a coprocessor) — the corroborating fact is size:
    /// every real ExHiROM release exceeds 4 MiB (fullsnes: "ExHiROM is
    /// used only by 'Dai Kaiju Monogatari 2' and 'Tales of Phantasia'"),
    /// and this image is nowhere near that, so LoROM wins.
    fn lorom_location_with_exhirom_nibble_under_4mib_is_lorom() {
        let mut data = vec![0u8; 1024 * 1024];
        let base = LOROM_HEADER_OFFSET;
        data[base + 0x15] = 0x45; // fast, ExHiROM nibble
        data[base + 0x16] = 0x00; // plain ROM
        data[base + 0x17] = 0x0A; // 1 MiB
        data[base + 0x18] = 0x00;
        set_checksum(&mut data, base, 0xBEEF);
        set_reset_vector(&mut data, base, 0x8000);

        let header = parse_snes_header(&data).expect("must parse as plain LoROM");
        assert_eq!(header.map_mode, SnesMapMode::LoRom);
        assert_eq!(header.coprocessor, Coprocessor::None);
        assert_eq!(header.rom_size, 1024 * 1024);
    }

    #[test]
    /// Fourth shape: a GENUINE SA-1 header — nibble $3 with a chipset byte
    /// that actually names SA-1 (fullsnes "SNES Cartridge ROM Header",
    /// Chipset (FFD6h): coprocessor nibble $3, hw $3, "x3h
    /// ROM+Co-processor" — the lower end of the same x3h..x5h range
    /// `sa1_cart_chipset_34_parses_with_board_data`/`_35_sets_battery`
    /// already cover) — still detects SA-1, exactly as before this ticket.
    /// This is the corroborated case W14-53 leaves unchanged.
    fn sa1_cart_chipset_33_still_detects_sa1() {
        let rom = lorom_image(0x23, 0x33);
        let header = parse_snes_header(&rom).expect("corroborated SA-1 header must parse");
        assert_eq!(header.map_mode, SnesMapMode::Sa1);
        assert!(!header.battery, "hw=3 has no battery");
        assert!(matches!(header.coprocessor, Coprocessor::Sa1(_)));
    }

    #[test]
    /// An SA-1 chipset byte under a plain LoROM map mode (not $23) still
    /// refuses exactly as before this ticket — real SA-1 boards always
    /// declare map mode $23, so this combination is not a shape any real
    /// cartridge uses.
    fn sa1_chipset_under_plain_lorom_map_mode_still_refuses() {
        let rom = lorom_image(0x20, 0x34);
        let err = parse_snes_header(&rom).unwrap_err();
        match &err {
            CartError::UnsupportedChip { name } => assert!(name.contains("SA-1"), "got: {name}"),
            other => panic!("expected UnsupportedChip, got {other:?}"),
        }
    }

    #[test]
    /// Ticket W19-03, fullsnes "SNES Cart S-DD1" `43h ROM+S-DD1`: no
    /// RAM/battery.
    fn sdd1_cart_chipset_43_parses_without_battery() {
        let rom = lorom_image(0x22, 0x43);
        let header = parse_snes_header(&rom).expect("S-DD1 cart must parse");
        assert_eq!(header.map_mode, SnesMapMode::Sdd1);
        assert_eq!(header.coprocessor, Coprocessor::Sdd1);
        assert!(!header.battery, "hw=3 has no battery");
        assert_eq!(header.dsp_window, None, "S-DD1 is not the DSP-1");
    }

    #[test]
    /// fullsnes "SNES Cart S-DD1": `45h ROM+S-DD1+RAM+Battery`.
    fn sdd1_cart_chipset_45_sets_battery() {
        let rom = lorom_image(0x22, 0x45);
        let header = parse_snes_header(&rom).expect("S-DD1+battery cart must parse");
        assert!(header.battery);
        assert_eq!(header.coprocessor, Coprocessor::Sdd1);
    }

    #[test]
    /// Merge note (W19-03 into main, reconciled with W14-53): chipset $44
    /// (coprocessor nibble $4, hw $4) is in the assigned x3h-x5h shape
    /// `coprocessor_nibble_corroborated`'s `0x2` arm accepts — same generic
    /// range SA-1 corroborates on — even though fullsnes's "in practice"
    /// list names only $43/$45 as real S-DD1 boards. Pinned here so the
    /// map-mode decision's corroboration gate and the coprocessor decision's
    /// inner chipset check can never drift apart again: if `map_mode`
    /// decides $44 corroborates S-DD1, the branch below MUST accept it
    /// rather than treat it as unreachable dead code.
    fn sdd1_cart_chipset_44_accepted_no_battery() {
        let rom = lorom_image(0x22, 0x44);
        let header = parse_snes_header(&rom).expect("assigned-shape S-DD1 chipset must parse");
        assert_eq!(header.map_mode, SnesMapMode::Sdd1);
        assert_eq!(header.coprocessor, Coprocessor::Sdd1);
        assert!(!header.battery, "hw=4 has no battery");
    }

    #[test]
    /// D-013's "both must agree" ruling, applied to S-DD1: map mode $22
    /// without the corroborating chipset byte no longer refuses outright —
    /// merged with W14-53's location-wins rule, `map_mode` itself falls
    /// back to the winning header LOCATION instead (same flip W14-53 gave
    /// `sa1_map_mode_without_sa1_chipset_still_refuses`, renamed
    /// `sa1_nibble_without_sa1_chipset_defers_to_location_not_refused`
    /// above). This is the exact same rom shape as
    /// `lorom_location_with_sdd1_nibble_and_plain_chipset_is_lorom` above;
    /// kept as its own test, under W19-03's original name pattern, as a
    /// belt-and-suspenders regression check on the "both must agree" rule
    /// specifically.
    fn sdd1_nibble_without_sdd1_chipset_defers_to_location_not_refused() {
        let rom = lorom_image(0x22, 0x00); // map mode S-DD1, chipset plain ROM
        let header =
            parse_snes_header(&rom).expect("uncorroborated S-DD1 nibble must defer to location");
        assert_eq!(header.map_mode, SnesMapMode::LoRom);
        assert_eq!(header.coprocessor, Coprocessor::None);
    }

    #[test]
    /// An S-DD1 chipset byte under a plain LoROM map mode (not $22) still
    /// refuses — real S-DD1 boards always declare map mode $22, so this
    /// combination is not a shape any real cartridge uses (same reasoning
    /// as `sa1_chipset_under_plain_lorom_map_mode_still_refuses`).
    fn sdd1_chipset_under_plain_lorom_map_mode_still_refuses() {
        let rom = lorom_image(0x20, 0x43);
        let err = parse_snes_header(&rom).unwrap_err();
        match &err {
            CartError::UnsupportedChip { name } => assert!(name.contains("S-DD1"), "got: {name}"),
            other => panic!("expected UnsupportedChip, got {other:?}"),
        }
    }

    #[test]
    fn non_dsp_cart_has_no_coprocessor_or_window() {
        let rom = lorom_image(0x20, 0x00);
        let header = parse_snes_header(&rom).expect("valid header");
        assert_eq!(header.coprocessor, Coprocessor::None);
        assert_eq!(header.dsp_window, None);
    }

    #[test]
    /// W14-53 originally named this the fifth shape its ruling asked for: a
    /// GENUINE S-DD1 header (nibble $2, chipset $43/$45) refusing by name
    /// since this build had no S-DD1 bus wiring yet. Ticket W19-03 lifts
    /// that deferral (D-013's pattern applied a second time, same as SA-1
    /// in W17-01) — a corroborated S-DD1 header is now accepted as
    /// `Coprocessor::Sdd1` instead of refused. This flips the assertion
    /// accordingly; `sdd1_cart_chipset_43_parses_without_battery` and
    /// `sdd1_cart_chipset_45_sets_battery` above already cover the same
    /// two chipset bytes individually — this test keeps W14-53's original
    /// "both chipset bytes in one shape" framing as a belt-and-suspenders
    /// check.
    fn genuine_sdd1_header_accepted_as_of_w19_03() {
        for (chipset, battery) in [(0x43u8, false), (0x45u8, true)] {
            let rom = lorom_image(0x22, chipset);
            let header = parse_snes_header(&rom)
                .unwrap_or_else(|e| panic!("chipset ${chipset:02X}: must parse, got {e:?}"));
            assert_eq!(header.map_mode, SnesMapMode::Sdd1);
            assert_eq!(header.coprocessor, Coprocessor::Sdd1);
            assert_eq!(
                header.battery, battery,
                "chipset ${chipset:02X}: battery mismatch"
            );
        }
    }

    #[test]
    /// A genuine ExHiROM header (nibble $5, image > 4 MiB — the only
    /// corroborating fact ExHiROM has, fullsnes having no coprocessor
    /// nibble for it) still refuses by name: this build has no ExHiROM
    /// memory map at all, corroborated or not.
    fn genuine_exhirom_header_over_4mib_still_refuses_by_name() {
        let mut data = vec![0u8; EXHIROM_MIN_SIZE + 0x10000];
        let base = LOROM_HEADER_OFFSET;
        data[base + 0x15] = 0x45;
        data[base + 0x16] = 0x00;
        data[base + 0x17] = 0x0D; // 8 MiB exponent, plausible for the real size
        set_checksum(&mut data, base, 0xBEEF);
        set_reset_vector(&mut data, base, 0x8000);

        let err = parse_snes_header(&data).unwrap_err();
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

    // ---- W14-52: RESET-vector fallback for headers that fail scoring -----

    /// A LoROM image whose header block scores below `MINIMUM_SCORE` (bad
    /// checksum/complement, disagreeing map-mode nibble, implausible ROM
    /// size, unprintable title — the shape this ticket's population survey
    /// found repeatedly among corrupted beta/proto/pirate dumps) but whose
    /// LoROM RESET vector points at a real 65816 reset prologue
    /// (`SEI`/`CLC`/`XCE`, $78 $18 $FB) at file offset 0.
    #[test]
    fn lorom_with_corrupted_checksum_loads_as_lorom_via_reset_vector_fallback() {
        let mut data = vec![0u8; 0x8000];
        let base = LOROM_HEADER_OFFSET;
        data[0] = 0x78; // SEI
        data[1] = 0x18; // CLC
        data[2] = 0xFB; // XCE
        data[base + 0x15] = 0xFF; // map-mode nibble disagrees with LoROM ($F)
        data[base + 0x16] = 0x00; // plain ROM chipset
        data[base + 0x17] = 0xFF; // implausible size exponent (no score point)
        data[base + 0x18] = 0x00;
        set_checksum(&mut data, base, 0x0000); // checksum stays 0 -> no xor point
        set_reset_vector(&mut data, base, 0x8000); // -> file offset 0

        let header = parse_snes_header(&data).expect("RESET-vector fallback must accept this");
        assert_eq!(header.map_mode, SnesMapMode::LoRom);
        assert_eq!(
            header.header_fallback,
            Some(HeaderFallback::LoRomResetVector)
        );
        assert_eq!(header.coprocessor, Coprocessor::None);
        assert_eq!(
            header.rom_size,
            data.len(),
            "fallback must size ROM from the real image length, not the corrupted byte"
        );
    }

    /// A HiROM image whose map-mode byte is garbage (so `score_candidate`'s
    /// nibble-agreement point is lost) and whose checksum/complement are
    /// zeroed, but whose HiROM RESET vector points at a plausible prologue.
    #[test]
    fn hirom_with_garbage_map_mode_byte_loads_as_hirom_via_reset_vector_fallback() {
        let mut data = vec![0u8; 0x10000];
        let base = HIROM_HEADER_OFFSET;
        let target = 0xC000usize; // an ordinary HiROM RESET target
        data[target] = 0xC2; // REP #imm
        data[target + 1] = 0x30;
        data[base + 0x15] = 0x77; // garbage map-mode byte (nibble $7)
        data[base + 0x16] = 0x00;
        data[base + 0x17] = 0xFF; // implausible size exponent
        data[base + 0x18] = 0x00;
        set_checksum(&mut data, base, 0x0000);
        set_reset_vector(&mut data, base, target as u16);

        let header = parse_snes_header(&data).expect("RESET-vector fallback must accept this");
        assert_eq!(header.map_mode, SnesMapMode::HiRom);
        assert_eq!(
            header.header_fallback,
            Some(HeaderFallback::HiRomResetVector)
        );
    }

    /// The fallback runs on the copier-header-stripped image, same as
    /// ordinary scoring — a 512-byte copier header in front of an otherwise
    /// fallback-only LoROM image must still be stripped before the RESET
    /// vector is read.
    #[test]
    fn copier_headered_lorom_loads_via_reset_vector_fallback() {
        let mut inner = vec![0u8; 0x8000];
        let base = LOROM_HEADER_OFFSET;
        inner[0] = 0x38; // SEC
        inner[1] = 0xFB; // XCE (SEC/XCE: the emulation-mode-entry idiom)
        inner[base + 0x15] = 0xFF;
        inner[base + 0x16] = 0x00;
        inner[base + 0x17] = 0xFF;
        inner[base + 0x18] = 0x00;
        set_checksum(&mut inner, base, 0x0000);
        set_reset_vector(&mut inner, base, 0x8000);

        let mut rom = vec![0u8; COPIER_HEADER_LEN];
        rom.extend(inner);
        assert_eq!(
            rom.len() % 8192,
            512,
            "fixture must trip the copier heuristic"
        );

        let header =
            parse_snes_header(&rom).expect("copier-headered fallback image must still load");
        assert!(header.had_copier_header);
        assert_eq!(header.map_mode, SnesMapMode::LoRom);
        assert_eq!(
            header.header_fallback,
            Some(HeaderFallback::LoRomResetVector)
        );
    }

    /// Regression: real Beta F-Zero (1991-05-13) carries a garbage RAM-size
    /// byte ($24, exponent 36) at the fallback location — `kb_pow2` does
    /// not treat that as an error (it only overflows past exponent ~54 on
    /// a 64-bit `usize`), so the un-clamped fallback tried to report 64 TiB
    /// of cartridge RAM and a downstream allocation aborted the process.
    /// Pinned here so the RAM-size clamp in `build_header_from_fallback`
    /// cannot regress silently.
    #[test]
    fn fallback_clamps_an_implausible_ram_size_exponent_to_zero() {
        let mut data = vec![0u8; 0x8000];
        let base = LOROM_HEADER_OFFSET;
        data[0] = 0x78; // SEI
        data[1] = 0x18; // CLC
        data[2] = 0xFB; // XCE
        data[base + 0x15] = 0xEC; // garbage map-mode byte, real F-Zero-beta value
        data[base + 0x16] = 0x14;
        data[base + 0x17] = 0xEC; // implausible ROM size exponent too
        data[base + 0x18] = 0x24; // 1<<36 KiB if trusted -- 64 TiB
        set_checksum(&mut data, base, 0x0000);
        set_reset_vector(&mut data, base, 0x8000);

        let header = parse_snes_header(&data).expect("fallback must still accept this cart");
        assert_eq!(
            header.ram_size, 0,
            "an implausible RAM-size exponent must clamp to 0, not allocate it"
        );
    }

    /// Uniformly random-shaped data must still be refused: nothing in it
    /// resembles a header, AND its RESET vector at both candidate locations
    /// reads as $0000 — below $8000, i.e. WRAM/registers, not mapped ROM —
    /// which the fallback's own necessary condition rejects before it ever
    /// looks at prologue bytes. This is what actually keeps the fallback
    /// from re-admitting the all-zero-filler shape W14-05 was filed over,
    /// so it is pinned as a test rather than assumed.
    #[test]
    fn random_bytes_image_still_refused_by_reset_vector_fallback() {
        // All-zero filler: `score_candidate`'s necessary conditions (country
        // $00, revision $00) both pass, but there is no checksum/complement
        // pair, no legible title, and the reset vector at both candidate
        // locations is $0000 -- below $8000, so the fallback's own "must
        // point into mapped ROM" check rejects both before ever reaching
        // `looks_like_reset_prologue`.
        let data = vec![0x00u8; 0x20000];
        let err = parse_snes_header(&data).unwrap_err();
        assert!(
            matches!(
                err,
                CartError::InvalidHeader(_) | CartError::UnsupportedChip { .. }
            ),
            "non-header data must still be refused even with the fallback in place, got {err:?}"
        );
    }

    /// Ticket W14-55, rule A — the Star Fox 2 (Beta) shape: a LoROM location
    /// whose header fails scoring on the revision byte alone (real dumps
    /// carry `$FF`, past `MAX_ROM_VERSION`) but whose chipset byte ($15:
    /// coprocessor nibble $1, hw $5) squarely names GSU-n, and whose own
    /// map-mode byte ($20, nibble $0) agrees with the LoROM location. Before
    /// this ticket, W14-52's blanket fallback rule reported `Coprocessor::
    /// None` here — the actual bug docs/TESTING.md's W14-54 re-triage
    /// found: the betas' CPU spins forever on a "GSU ready" WRAM flag no
    /// chip ever sets. The chipset byte must now be honoured instead.
    #[test]
    fn fallback_honours_corroborated_gsu_chipset_star_fox_2_beta_shape() {
        let mut data = vec![0u8; 0x8000];
        let base = LOROM_HEADER_OFFSET;
        data[0] = 0x78; // SEI
        data[1] = 0x18; // CLC
        data[2] = 0xFB; // XCE
        data[base + 0x15] = 0x20; // LoROM, nibble agrees with location
        data[base + 0x16] = 0x15; // GSU-n chipset (nibble $1, hw $5)
        data[base + 0x17] = 0x0A; // plausible ROM size exponent
        data[base + 0x1B] = 0xFF; // revision byte past MAX_ROM_VERSION
        set_checksum(&mut data, base, 0x0000); // checksum stays 0 -> no xor point
        set_reset_vector(&mut data, base, 0x8000); // -> file offset 0

        let header = parse_snes_header(&data).expect("RESET-vector fallback must accept this");
        assert_eq!(
            header.header_fallback,
            Some(HeaderFallback::LoRomResetVector)
        );
        assert_eq!(header.map_mode, SnesMapMode::LoRom);
        assert_eq!(
            header.coprocessor,
            Coprocessor::SuperFx {
                version: SuperFxVersion::Gsu1,
                ram_kib: 0,
            },
            "a corroborated GSU chipset byte must survive the header fallback"
        );
        assert!(header.battery, "hw=$5 GSU chipset carries a battery");
    }

    /// Ticket W14-55, rule A — the Zool (USA) (Beta) shape: a LoROM location
    /// whose header fails scoring on the country byte (real dump carries
    /// $57, past `MAX_COUNTRY_CODE`) and whose chipset byte ($53:
    /// coprocessor nibble $5 "S-RTC", hw $3) does NOT match any chip this
    /// build runs — a garbage/irrelevant chipset byte, not a corroborated
    /// one, so the fallback must still report `Coprocessor::None` exactly
    /// as W14-52 shipped, per this ticket's own acceptance.
    #[test]
    fn fallback_uncorroborated_chipset_zool_beta_shape_stays_none() {
        let mut data = vec![0u8; 0x8000];
        let base = LOROM_HEADER_OFFSET;
        data[0x18] = 0x78; // SEI
        data[0x19] = 0x18; // CLC
        data[0x1A] = 0xFB; // XCE
        data[base + 0x15] = 0x4F; // unassigned map-mode nibble ($F)
        data[base + 0x16] = 0x53; // chipset naming a chip this build has no arm for
        data[base + 0x19] = 0x57; // country byte past MAX_COUNTRY_CODE
        set_checksum(&mut data, base, 0x0000);
        set_reset_vector(&mut data, base, 0x8018); // -> file offset 0x18

        let header = parse_snes_header(&data).expect("RESET-vector fallback must accept this");
        assert_eq!(
            header.header_fallback,
            Some(HeaderFallback::LoRomResetVector)
        );
        assert_eq!(
            header.coprocessor,
            Coprocessor::None,
            "an uncorroborated/unimplemented chipset byte must not be trusted"
        );
        assert!(!header.battery);
    }

    /// Ticket W14-55, rule B — the Lion King (USA) (Beta 3) (v.21) shape:
    /// the LoROM header block is wholesale `$FF` filler, including the
    /// vector field itself (so the "RESET vector" is just the filler value
    /// $FFFF, not a survived real address), and the byte immediately AFTER
    /// that vector's decoded file offset happens to be the start of a real
    /// `SEI CLC XCE JML` prologue — but that prologue belongs to the HiROM
    /// candidate's own, genuinely-set RESET vector ($8000), one byte over
    /// from where LoROM's coincidental math landed. Before this ticket the
    /// LoROM guess (tried first) won outright; `resolves_into_own_header_
    /// block` must now reject it, leaving HiROM as the only candidate.
    #[test]
    fn fallback_prefers_hirom_over_lorom_seam_coincidence_lion_king_beta3_shape() {
        // Both header blocks start as wholesale `$FF` filler, matching the
        // real dump exactly; only the two genuinely-differing bytes (HiROM's
        // RESET vector) and the shared prologue bytes at file offset $8000
        // are overwritten below.
        let mut data = vec![0xFFu8; 0x10000];
        // The real prologue: SEI/CLC/XCE/JML at file offset $8000. LoROM's
        // spurious vector ($FFFF - $8000 = $7FFF) reads this location's
        // first three bytes one position late, with the header block's own
        // trailing $FF filler as its bogus leading byte.
        data[0x8000] = 0x78; // SEI
        data[0x8001] = 0x18; // CLC
        data[0x8002] = 0xFB; // XCE
        data[0x8003] = 0x5C; // JML
                             // HiROM's own RESET vector: the only non-filler bytes in its header
                             // block, per the real dump.
        let hirom_base = HIROM_HEADER_OFFSET;
        data[hirom_base + RESET_VECTOR_OFFSET] = 0x00;
        data[hirom_base + RESET_VECTOR_OFFSET + 1] = 0x80;

        let header = parse_snes_header(&data).expect("RESET-vector fallback must accept this");
        assert_eq!(
            header.header_fallback,
            Some(HeaderFallback::HiRomResetVector),
            "the LoROM candidate's vector resolves into its own header block and must be rejected"
        );
        assert_eq!(header.map_mode, SnesMapMode::HiRom);
    }
}
