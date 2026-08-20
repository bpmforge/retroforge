//! Boot the LoROM/HiROM mirror-map fixtures and read their result block
//! (ticket W6-02a, owed by W6-06; FR-CORE-035, `docs/TESTING.md` §5).
//!
//! ## Why this test is the point of the ticket
//!
//! W6-06 built these two ROMs and could only assert *structural* things
//! about them — that they build deterministically, carry a valid
//! cartridge header, and differ only in the mapping — because `rf-snes`
//! was a 15-line stub and nothing could run them. Its FORMAT.md says so
//! plainly and records the gap against this ticket.
//!
//! This closes it. The ROMs ask the mapping questions from *inside* the
//! machine: read the same byte through every address a correct mapping
//! aliases together, write through one WRAM view and read through
//! another, and confirm the write did not spill into ROM space. A unit
//! test of [`rf_snes::map`] asserts what I believe the mapping is; this
//! asserts what the mapping actually does to real 65816 code.
//!
//! ## The protocol
//!
//! Result block at `$7E:0000` — magic `'R'`,`'F'` (written LAST), checks
//! run, checks passed, then one byte per check. Documented in
//! `fixtures/snes/mirror-map/FORMAT.md` and deliberately re-stated in the
//! assertions below, so a drift between ROM and harness fails loudly.

use rf_cart::SnesMapMode;
use rf_snes::cpu::CpuBus;
use rf_snes::SnesSystem;

const CHECKS: u8 = 4;
/// Generous: the fixture is a few hundred instructions. Bounded so a ROM
/// that never finishes fails rather than hangs.
const MAX_INSTRUCTIONS: u64 = 200_000;

fn fixture(name: &str) -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/snes/mirror-map/build/"
    );
    std::fs::read(format!("{path}{name}")).ok()
}

/// Run one image and return its result block.
fn boot(name: &str) -> Option<[u8; 16]> {
    let rom = fixture(name)?;
    let mut system = SnesSystem::load(&rom).expect("fixture must load through rf-cart");
    system
        .run_until(MAX_INSTRUCTIONS, None)
        .expect("all 256 opcodes are implemented as of W6-01b");

    let mut block = [0u8; 16];
    for (i, b) in block.iter_mut().enumerate() {
        *b = system.bus.peek(0x007E_0000 + i as u32);
    }
    Some(block)
}

fn assert_result_block(name: &str, block: [u8; 16]) {
    // Magic FIRST, and before trusting anything under it. The ROM writes
    // it last precisely so that its presence certifies the counts came
    // from a run that reached the end rather than from WRAM that happened
    // to look plausible.
    assert_eq!(
        (block[0], block[1]),
        (b'R', b'F'),
        "{name}: result-block magic absent — the ROM did not reach the end of its checks. \
         Block: {block:02X?}"
    );
    assert_eq!(block[2], CHECKS, "{name}: expected {CHECKS} checks to run");
    assert_eq!(
        block[3],
        CHECKS,
        "{name}: {} of {CHECKS} checks passed; per-check results {:?} \
         (0 = mapping check FAILED, and the index is the check number in FORMAT.md)",
        block[3],
        &block[4..4 + CHECKS as usize]
    );
    for (i, &r) in block[4..4 + CHECKS as usize].iter().enumerate() {
        assert_eq!(r, 1, "{name}: check {i} failed");
    }
}

#[test]
fn lorom_fixture_reports_all_mapping_checks_passing() {
    let Some(block) = boot("lorom.sfc") else {
        eprintln!("SKIP: build the fixtures with fixtures/snes/mirror-map/build.sh");
        return;
    };
    assert_result_block("lorom.sfc", block);
}

#[test]
fn hirom_fixture_reports_all_mapping_checks_passing() {
    let Some(block) = boot("hirom.sfc") else {
        eprintln!("SKIP: build the fixtures with fixtures/snes/mirror-map/build.sh");
        return;
    };
    assert_result_block("hirom.sfc", block);
}

/// The two images must be recognised as DIFFERENT mappings.
///
/// Without this, both passing would be consistent with the loader
/// defaulting to one mode and the other fixture happening to work — which
/// is exactly the failure a "one source, two mappings" fixture pair is
/// built to expose.
#[test]
fn the_two_fixtures_load_as_different_map_modes() {
    let (Some(lo), Some(hi)) = (fixture("lorom.sfc"), fixture("hirom.sfc")) else {
        eprintln!("SKIP: fixtures not built");
        return;
    };
    let lo = rf_cart::Cartridge::load(&lo).expect("loads");
    let hi = rf_cart::Cartridge::load(&hi).expect("loads");
    let mode = |c: rf_cart::Cartridge| match c {
        rf_cart::Cartridge::Snes { header, .. } => header.map_mode,
        rf_cart::Cartridge::Nes { .. } => panic!("fixture detected as NES"),
    };
    assert_eq!(mode(lo), SnesMapMode::LoRom);
    assert_eq!(mode(hi), SnesMapMode::HiRom);
}
