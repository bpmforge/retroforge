//! Acceptance criterion 2: "NROM loads via rf-cart". Builds synthetic
//! iNES images the same way `crates/rf-cart/src/nes.rs`'s own tests do,
//! then proves `NesBus`/`NesRom` slice and map them correctly — including
//! the "16 KiB PRG mirrors into both halves, 32 KiB is mapped straight
//! through" rule ([nesdev.org/wiki/NROM](https://www.nesdev.org/wiki/NROM)).
use super::build_nrom_ines;
use crate::cpu::CpuBus;
use crate::system::{NesBus, NesLoadError, NesRom};

#[test]
fn sixteen_kib_prg_mirrors_into_both_halves() {
    let raw = build_nrom_ines(1, 1, |i| i as u8);
    let mut bus = NesBus::from_ines_bytes(&raw).expect("valid 16 KiB NROM image");

    assert_eq!(bus.read(0x8000), 0x00);
    assert_eq!(bus.read(0xC000), 0x00, "second half must mirror the first");
    assert_eq!(bus.read(0x8001), 0x01);
    assert_eq!(bus.read(0xC001), 0x01);
    // Last byte of the 16 KiB bank (index 0x3FFF -> byte 0xFF, wrapping
    // u8) shows up at both $BFFF and the reset vector location $FFFF.
    assert_eq!(bus.read(0xBFFF), 0x3FFFu16 as u8);
    assert_eq!(bus.read(0xFFFF), 0x3FFFu16 as u8);
}

#[test]
fn thirty_two_kib_halves_are_independently_addressable() {
    // Distinguish the two halves with a fill function that differs across
    // the 16 KiB boundary, rather than relying on the wrapping `i as u8`
    // pattern (which repeats every 256 bytes and can't tell "mirrored"
    // from "coincidentally equal" at bank granularity).
    let raw = build_nrom_ines(2, 1, |i| if i < 16 * 1024 { 0xAA } else { 0xBB });
    let mut bus = NesBus::from_ines_bytes(&raw).expect("valid 32 KiB NROM image");
    assert_eq!(bus.read(0x8000), 0xAA, "first half ($8000) is bank 0");
    assert_eq!(
        bus.read(0xC000),
        0xBB,
        "second half ($C000) is bank 1, not a mirror"
    );
}

#[test]
fn sixteen_kib_halves_are_identical_not_independently_addressable() {
    let raw = build_nrom_ines(1, 1, |i| if i < 8 * 1024 { 0xAA } else { 0xBB });
    let mut bus = NesBus::from_ines_bytes(&raw).expect("valid 16 KiB NROM image");
    assert_eq!(bus.read(0x8000), 0xAA);
    assert_eq!(
        bus.read(0xC000),
        0xAA,
        "16 KiB image: $C000 mirrors $8000's bank, not bank-independent"
    );
    // Offset 0x2000 into the 16 KiB bank (past the 8 KiB 0xAA/0xBB split);
    // $A000 and its mirror $E000 (+0x4000) must both land here too.
    assert_eq!(bus.read(0xA000), 0xBB);
    assert_eq!(bus.read(0xE000), 0xBB);
}

// `unimplemented_mapper_is_rejected_even_though_rf_cart_parses_its_header`
// (the test that used to live here) was RETIRED, not retargeted, by ticket
// W2-03. It pinned "mapper 4 (MMC3) is in rf-cart's `nes::SUPPORTED_MAPPERS`
// but not rf-nes's `EMULATED_MAPPERS`" (itself retargeted there from mapper
// 1/MMC1 by W2-02, for the identical expired-premise reason). W2-03 makes
// MMC3 the fifth real implementor, so the two lists are now identical
// (`[0, 1, 2, 3, 4]`) and there is no mapper number left that exercises
// "rf-cart parses it, rf-nes refuses it" — `nes::SUPPORTED_MAPPERS` is a
// private `const` (correctly: rf-cart has no reason to expose it), so this
// crate cannot even assert the two lists' equality directly without
// widening rf-cart's own API surface for a test, which is out of this
// ticket's write_scope (`crates/rf-cart/**` is a different crate). The
// sibling case — "rf-cart refuses it outright" — stays covered below by
// `a_mapper_rf_cart_cannot_identify_is_refused_not_loaded` (mapper 66).
// Retarget a new test back into a real rejection assertion the moment
// EITHER list gains a mapper number the other one doesn't have yet.

#[test]
fn chr_rom_is_sliced_after_prg_rom() {
    let raw = build_nrom_ines(1, 1, |i| i as u8);
    let rom = NesRom::from_ines_bytes(&raw).expect("valid NROM image");
    assert_eq!(rom.prg_rom().len(), 16 * 1024);
    assert_eq!(rom.chr_rom().len(), 8 * 1024);
    assert!(!rom.chr_is_ram());
}

#[test]
fn zero_chr_rom_size_means_chr_ram() {
    let raw = build_nrom_ines(1, 0, |i| i as u8);
    let rom = NesRom::from_ines_bytes(&raw).expect("valid NROM image with CHR RAM");
    assert!(rom.chr_is_ram());
    assert!(
        !rom.chr_rom().is_empty(),
        "CHR RAM must still have a backing store"
    );
}

// ---------------------------------------------------------------------
// Ticket W2-02 conductor fix: the PRODUCTION load path must actually
// reach the mappers.
//
// `NesBus::new`'s mapper-selection `match` was implemented for 0/1/2/3,
// but `NesRom::from_ines_bytes`'s own gate still read `!= 0` — so every
// real MMC1/UxROM/CNROM `.nes` file was rejected before dispatch, making
// the whole implementation dead code from the only path a user can take.
// The implementing agent found and reported this (that file was outside
// its write scope) and noted the dispatch `match` had ZERO coverage.
// These tests close both holes at once: they go through
// `NesBus::from_ines_bytes`, the same entry point `EmuStepper` and the
// app use, not through a test-only constructor.
// ---------------------------------------------------------------------

/// Build a synthetic iNES image declaring `mapper` in flags6/flags7.
/// iNES puts the low nibble in flags6 bits 4-7 and the high nibble in
/// flags7 bits 4-7 (nesdev.org/wiki/INES).
pub(super) fn ines_with_mapper(mapper: u8, prg_banks: u8, chr_banks: u8) -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(&rf_cart::nes::INES_MAGIC);
    data.push(prg_banks);
    data.push(chr_banks);
    data.push((mapper & 0x0F) << 4); // flags6: mapper low nibble
    data.push(mapper & 0xF0); // flags7: mapper high nibble
    data.extend_from_slice(&[0u8; 8]); // 8 reserved
    data.extend(vec![0xEAu8; prg_banks as usize * 16 * 1024]); // NOP fill
    data.extend(vec![0u8; chr_banks as usize * 8 * 1024]);
    data
}

#[test]
fn every_emulated_mapper_loads_through_the_production_path() {
    // (mapper id, prg banks, chr banks) — CNROM/MMC1/MMC3 need real CHR to
    // bank; MMC3 needs at least 2 x 16 KiB (4 x 8 KiB) PRG banks for its
    // fixed/switchable windows to be distinguishable at all.
    // AxROM (7) needs 2 x 32 KiB = 4 x 16 KiB PRG banks for its window to
    // be distinguishable at all, and declares CHR-RAM (0 CHR banks).
    for (mapper, prg, chr) in [
        (0u8, 1u8, 1u8),
        (1, 2, 2),
        (2, 2, 0),
        (3, 1, 2),
        (4, 2, 1),
        (7, 4, 0),
    ] {
        let raw = ines_with_mapper(mapper, prg, chr);
        let rom = NesRom::from_ines_bytes(&raw)
            .unwrap_or_else(|e| panic!("mapper {mapper} must load through the real path, got {e}"));
        assert_eq!(rom.header().mapper, u16::from(mapper));
        // Constructing the bus exercises `NesBus::new`'s dispatch arm,
        // which previously could not be reached for 1/2/3 at all.
        let bus = super::super::NesBus::new(rom);
        assert_eq!(bus.rom().mapper, u16::from(mapper));
    }
}

/// FR-CORE-013 must keep working for a mapper rf-cart cannot even name —
/// widening the emulated set must not turn a clear diagnostic into a
/// panic or a silent accept.
///
/// This is now the ONLY reachable refusal path: W2-03 made
/// `EMULATED_MAPPERS` and rf-cart's `SUPPORTED_MAPPERS` identical
/// (`[0,1,2,3,4]`), so rf-cart rejects everything rf-nes would have
/// rejected, and `NesLoadError::UnimplementedMapper` is currently
/// unreachable from parsing. That variant is deliberately kept — it
/// becomes reachable the instant either list moves — and its *message* is
/// covered directly by
/// `unimplemented_mapper_message_names_the_mapper_and_what_is_emulated`
/// below, which is the half a user actually reads.
#[test]
fn a_mapper_rf_cart_cannot_identify_is_refused_not_loaded() {
    // 13 = CPROM: absent from rf-cart's SUPPORTED_MAPPERS entirely. (This
    // used 66, 64 and 5 until W14-12, W14-14 and W14-16 emulated them.)
    let raw = ines_with_mapper(13, 1, 1);
    assert!(
        NesRom::from_ines_bytes(&raw).is_err(),
        "an unknown mapper must be refused, never loaded"
    );
}

/// The `UnimplementedMapper` message is what a user sees when their ROM is
/// refused, and W2-16 fixed it after it went stale ("NROM/mapper 0 only",
/// false the moment W2-02 landed three more mappers). The variant is
/// currently unreachable from parsing (see above), so this constructs it
/// directly rather than leaving the message — the part that faces the
/// user — with no coverage at all.
#[test]
fn unimplemented_mapper_message_names_the_mapper_and_what_is_emulated() {
    let shown = NesLoadError::UnimplementedMapper(7).to_string();
    assert!(
        shown.contains('7'),
        "the message must name the offending mapper, got: {shown}"
    );
    // Derived from EMULATED_MAPPERS, not restated in prose — so this also
    // fails if someone reintroduces a hard-coded list that can drift.
    for emulated in [0, 1, 2, 3, 4] {
        assert!(
            shown.contains(&emulated.to_string()),
            "the message must list emulated mapper {emulated}, got: {shown}"
        );
    }
}

/// Every mapper this crate says it emulates must actually load (ticket
/// W14-04).
///
/// **The bug this exists for cost a detour and would have cost a release.**
/// There are TWO gates, in two crates: `rf_cart`'s `SUPPORTED_MAPPERS`
/// decides whether a file is a cartridge at all — which is what the library
/// scanner asks — and `EMULATED_MAPPERS` here decides whether a session can
/// start. W14-04 added four mappers to the first and not the second, so the
/// library listed 116 newly-playable games and every one of them refused to
/// run. Unit tests for the mappers themselves all passed: they never went
/// through either gate.
///
/// This closes the gap from the `rf-nes` side without needing `rf_cart` to
/// export its list: `NesRom::from_ines_bytes` passes through BOTH, so a
/// mapper that loads here is agreed on by both crates.
#[test]
fn every_emulated_mapper_loads_through_both_gates() {
    for &mapper in crate::system::cartridge::EMULATED_MAPPERS {
        // iNES flags 6 and 7 carry the mapper number, low nibble then
        // high; mapper 206 needs the high nibble, which is why this is
        // built by hand rather than with `build_nrom_ines`.
        assert!(
            mapper <= 0xFF,
            "this builder writes an 8-bit mapper number; {mapper} needs NES 2.0"
        );
        let mut data = Vec::new();
        data.extend_from_slice(&rf_cart::nes::INES_MAGIC);
        data.push(2); // 32 KiB PRG: enough for every banked mapper here
        data.push(1); // 8 KiB CHR
        data.push(((mapper as u8) & 0x0F) << 4);
        data.push((mapper as u8) & 0xF0);
        data.extend_from_slice(&[0u8; 8]);
        data.extend(vec![0u8; 2 * 16 * 1024]);
        data.extend(vec![0u8; 8 * 1024]);

        let rom = NesRom::from_ines_bytes(&data)
            .unwrap_or_else(|e| panic!("mapper {mapper} is listed as emulated but failed: {e}"));
        assert_eq!(rom.header().mapper, mapper, "header round-trip");

        // ...and a bus can be built from it, which is what catches a
        // mapper that passed both gates and then hit the dispatch's
        // `unreachable!`.
        let _bus = NesBus::new(rom);
    }
}
