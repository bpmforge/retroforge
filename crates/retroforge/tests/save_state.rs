//! `.rfstate` container tests (ticket W2-04): the acceptance criteria that
//! live above `rf-nes`'s per-region encoding — the container roundtrip, the
//! golden fixture, the wrong-ROM refusal (FR-STATE-003), the
//! Enhanced-state-in-Accuracy rule (FR-STATE-007) and battery-SRAM
//! persistence (FR-CORE-012).
//!
//! The per-field encoding is tested inside `rf-nes`
//! (`system::tests::save_state`), including the anti-tamper check that
//! keeps its roundtrip from being vacuous. This file tests the layer above
//! it and does not duplicate that work.

use std::path::PathBuf;

use retroforge::save_state::{self, SaveStateError};
use retroforge::stepper::EmuStepper;
use rf_core_api::{CoreSink, PpuPixel};

/// The fixed timestamp every fixture and comparison in this file uses.
/// SAVE_STATES.md §2: the timestamp is metadata only, is excluded from any
/// state hash, and is caller-supplied precisely so a container is
/// byte-deterministic for a fixed input.
const FIXED_TIMESTAMP: u64 = 1_760_000_000;

struct NullSink;

impl CoreSink for NullSink {
    fn video_scanline(&mut self, _y: u16, _pixels: &[PpuPixel]) {}
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, _ev: rf_core_api::CoreEvent) {}
}

/// A tiny deterministic NROM image: enough CPU work that a state taken a
/// few frames in is non-trivial, with no dependency on a fetched ROM (this
/// suite must run in CI, where `roms/` does not exist -- NFR-006).
fn test_rom(seed: u8) -> Vec<u8> {
    let mut rom = vec![0u8; 16 + 0x4000 + 0x2000];
    rom[0..4].copy_from_slice(b"NES\x1a");
    rom[4] = 1; // 1 x 16 KiB PRG
    rom[5] = 1; // 1 x 8 KiB CHR
    rom[6] = 0; // mapper 0, horizontal mirroring

    // A loop that writes an incrementing value into WRAM and OAM so the
    // machine state actually moves: LDA #seed / STA $0000 / INC $0000 /
    // JMP back.
    let prg = &mut rom[16..16 + 0x4000];
    let code: [u8; 9] = [
        0xA9, seed, // LDA #seed
        0x8D, 0x00, 0x00, // STA $0000
        0xEE, 0x00, 0x00, // INC $0000
        0x4C, // JMP ...
    ];
    prg[..code.len()].copy_from_slice(&code);
    prg[code.len()] = 0x02; // low byte of $8002
    prg[code.len() + 1] = 0x80; // high byte
                                // Reset vector -> $8000.
    prg[0x3FFC] = 0x00;
    prg[0x3FFD] = 0x80;
    rom
}

fn advance(stepper: &mut EmuStepper, frames: u32) {
    let mut sink = NullSink;
    for _ in 0..frames {
        stepper.step_frame(&mut sink);
    }
}

/// FR-STATE-002 at the container level: encode, decode, apply, and the
/// machine must continue identically. The per-region encoding already has
/// its own roundtrip test in `rf-nes`; what this adds is the TLV envelope,
/// zstd, and the chunk table in between.
#[test]
fn container_roundtrip_continues_hash_identical() {
    let rom = test_rom(0x21);
    let mut stepper = EmuStepper::from_ines_bytes(&rom).expect("test rom loads");
    advance(&mut stepper, 6);

    let container = stepper.save_state(FIXED_TIMESTAMP).expect("save");
    let encoded = container.encode().expect("encode");

    advance(&mut stepper, 10);
    let live_hash = stepper.state_hash();

    // Restore into a fresh machine so nothing survives in place.
    let mut restored = EmuStepper::from_ines_bytes(&rom).expect("test rom loads");
    let (decoded, warnings) = rf_state::Container::decode_default(&encoded).expect("decode");
    assert!(
        warnings.is_empty(),
        "a self-written state warns about nothing"
    );
    restored.load_state(&decoded).expect("load");

    advance(&mut restored, 10);
    assert_eq!(
        restored.state_hash(),
        live_hash,
        "10 frames after a restore must reach the same full-machine hash as 10 frames after \
         the save"
    );
}

/// The golden fixture (acceptance criterion 3): a `.rfstate` written by this
/// build, checked in, and loaded on every run. Its purpose is to fail when a
/// future change alters the format or the encoding without anyone deciding
/// to -- SAVE_STATES.md §2's "Golden `.rfstate` fixtures from each release
/// are kept in the test suite; CI loads all of them."
///
/// Regenerate deliberately (never to make this test go green) with:
/// `cargo test -p retroforge --test save_state -- --ignored regenerate`
#[test]
fn golden_fixture_loads_and_drives_the_machine_identically() {
    let rom = test_rom(0x21);
    let bytes = std::fs::read(fixture_path()).expect(
        "golden fixture missing -- regenerate with: cargo test -p retroforge --test save_state \
         -- --ignored regenerate",
    );
    let (container, warnings) =
        rf_state::Container::decode_default(&bytes).expect("golden decodes");
    assert!(warnings.is_empty(), "golden fixture warns: {warnings:?}");

    let mut restored = EmuStepper::from_ines_bytes(&rom).expect("test rom loads");
    restored.load_state(&container).expect("golden loads");
    assert_eq!(
        restored.frame_count(),
        6,
        "the fixture was taken six frames in"
    );

    // And it must still DRIVE: re-running from the fixture reproduces the
    // same hash a live machine reaches at the same frame.
    let mut live = EmuStepper::from_ines_bytes(&rom).expect("test rom loads");
    advance(&mut live, 6);
    advance(&mut live, 10);
    advance(&mut restored, 10);
    assert_eq!(restored.state_hash(), live.state_hash());
}

/// Rewrites the golden fixture. Ignored so it never runs as part of the
/// gate: a fixture that regenerates itself proves nothing.
#[test]
#[ignore = "regenerates the checked-in golden .rfstate; run deliberately, never to fix a red test"]
fn regenerate_golden_fixture() {
    let rom = test_rom(0x21);
    let mut stepper = EmuStepper::from_ines_bytes(&rom).expect("test rom loads");
    advance(&mut stepper, 6);
    let bytes = stepper
        .save_state(FIXED_TIMESTAMP)
        .expect("save")
        .encode()
        .expect("encode");
    std::fs::create_dir_all(fixture_path().parent().unwrap()).expect("fixture dir");
    std::fs::write(fixture_path(), bytes).expect("write fixture");
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nrom-frame6.rfstate")
}

/// FR-STATE-003: a state saved against one ROM must be refused against
/// another, with a diagnostic rather than a silent misload.
#[test]
fn a_state_from_a_different_rom_is_refused_with_a_diagnostic() {
    let mut original = EmuStepper::from_ines_bytes(&test_rom(0x21)).expect("rom a");
    advance(&mut original, 3);
    let container = original.save_state(FIXED_TIMESTAMP).expect("save");

    let mut other = EmuStepper::from_ines_bytes(&test_rom(0x99)).expect("rom b");
    let err = other
        .load_state(&container)
        .expect_err("a state from another ROM must be refused");
    let message = format!("{err}");
    assert!(
        message.to_lowercase().contains("rom"),
        "the refusal must name the ROM mismatch, got: {message}"
    );

    // ...and the refused load must not have half-applied anything.
    let untouched = EmuStepper::from_ines_bytes(&test_rom(0x99)).expect("rom b");
    assert_eq!(
        other.state_hash(),
        untouched.state_hash(),
        "a refused state must leave the machine exactly as it was"
    );
}

/// FR-STATE-007: an Accuracy-mode session loads a state saved in Enhanced
/// mode, and the enhancement chunks are skipped WITH A WARNING -- not
/// silently, and not fatally.
#[test]
fn an_enhanced_mode_state_loads_in_accuracy_with_a_warning_and_identical_machine_state() {
    let rom = test_rom(0x21);
    let mut stepper = EmuStepper::from_ines_bytes(&rom).expect("test rom loads");
    advance(&mut stepper, 4);
    let mut container = stepper.save_state(FIXED_TIMESTAMP).expect("save");

    // Stand in for what an Enhanced-mode session adds: rf-enhance's ENHC
    // chunk and rf-profiles' PROF chunk (SAVE_STATES.md §2's table). This
    // build has no consumer for either.
    container
        .add_chunk(*b"ENHC", 1, vec![0xDE, 0xAD, 0xBE, 0xEF])
        .expect("enhc");
    container
        .add_chunk(*b"PROF", 1, vec![0x01, 0x02])
        .expect("prof");

    advance(&mut stepper, 7);
    let live_hash = stepper.state_hash();

    let mut accuracy = EmuStepper::from_ines_bytes(&rom).expect("test rom loads");
    let warnings = accuracy
        .load_state(&container)
        .expect("enhanced state loads");
    assert_eq!(
        warnings.len(),
        2,
        "both enhancement chunks must be reported"
    );
    let reported: Vec<String> = warnings.iter().map(|w| format!("{w:?}")).collect();
    assert!(
        reported.iter().any(|w| w.contains("ENHC")) || reported.iter().any(|w| w.contains("69")),
        "the warnings must name the skipped chunks, got: {reported:?}"
    );

    advance(&mut accuracy, 7);
    assert_eq!(
        accuracy.state_hash(),
        live_hash,
        "skipping enhancement chunks must not perturb machine state"
    );
}

/// FR-CORE-012: battery SRAM survives a round trip through disk, is keyed
/// by the normalized ROM hash, and a wrong-sized `.sav` is refused.
#[test]
fn battery_sram_persists_to_disk_and_reloads() {
    let dir = std::env::temp_dir().join(format!(
        "rf-w2-04-sram-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let rom = test_rom(0x21);
    let hex = rf_cart::hash::identity_nes(&rom).normalized.sha256;

    let mut stepper = EmuStepper::from_ines_bytes(&rom).expect("test rom loads");
    stepper.poke_bus(0x6000, 0xC3);
    stepper.poke_bus(0x7FFF, 0x3C);

    let path = save_state::write_battery_ram(stepper.bus_for_battery(), &dir, &hex).expect("write");
    assert_eq!(path, save_state::battery_ram_path(&dir, &hex));

    let mut fresh = EmuStepper::from_ines_bytes(&rom).expect("test rom loads");
    assert_eq!(fresh.peek(0x6000), 0x00, "a fresh machine starts blank");
    let loaded =
        save_state::read_battery_ram(fresh.bus_for_battery_mut(), &dir, &hex).expect("read");
    assert!(loaded, "the .sav exists, so it must report a load");
    assert_eq!(fresh.peek(0x6000), 0xC3);
    assert_eq!(fresh.peek(0x7FFF), 0x3C);

    // No file for an unknown ROM is not an error -- it is first boot.
    let missing = save_state::read_battery_ram(
        fresh.bus_for_battery_mut(),
        &dir,
        "0000000000000000000000000000000000000000000000000000000000000000",
    )
    .expect("absent .sav is not an error");
    assert!(!missing);

    // A truncated .sav is refused, not padded.
    std::fs::write(save_state::battery_ram_path(&dir, &hex), [0u8; 16]).expect("truncate");
    let err = save_state::read_battery_ram(fresh.bus_for_battery_mut(), &dir, &hex)
        .expect_err("a truncated .sav must be refused");
    assert!(
        matches!(err, SaveStateError::Core(_)),
        "a size mismatch is the core's refusal, got: {err}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Per-process unique suffix for the temp dir above — libtest runs a
/// binary's tests in parallel threads sharing one pid, so a pid-only key
/// races (the exact flake W1-03 fixed in `rf-harness`).
static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
