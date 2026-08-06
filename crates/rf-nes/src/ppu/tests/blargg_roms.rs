//! Real-ROM cargo-test peer of `crates/rf-harness/src/blargg_evidence.rs`
//! (ticket W1-05b) — same "second, independent peer" relationship
//! `crate::system::tests::nestest` has to `rf-harness`'s
//! `nestest_evidence::run` (see that module's doc): this file independently
//! reimplements the tiny `$6000`/RAM-result protocol readers rather than
//! depending on `rf-harness` (which would invert the crate dependency
//! direction — `rf-harness` depends on `rf-nes`, never the reverse).
//!
//! ## ROM availability (gitignored `roms/`, NFR-006)
//!
//! Fetch with `scripts/fetch-test-roms.sh` (no arguments fetches every
//! artifact, including these). Same absence-skip discipline as
//! `crate::cpu::tests::vectors` and `crate::system::tests::nestest`:
//! `cargo test --workspace` must pass on a fresh checkout with these files
//! absent, so these tests print why and return rather than failing or
//! hanging.
//!
//! ## Why `ppu_vbl_nmi` doesn't assert 10/10 (ticket W1-05c)
//!
//! W1-05b left only 4 of the 10 real `ppu_vbl_nmi` sub-ROMs passing.
//! W1-05c's sub-CPU-cycle fix (`crate::ppu`'s module doc "Reachable and
//! unreachable races" section; `crate::system::NesBus::tick_master`'s
//! `nmi_level_latch` doc) brings that to 9/10 — every sub-ROM except
//! `10-even_odd_timing`, which fails for a *separate, independently
//! investigated* reason (a `$2001`-write-timing question, not the
//! `$2002`-read/NMI-edge race the other nine share — see
//! `crates/rf-harness/waivers.toml`'s one remaining entry for the specific
//! symptom and why W1-05c's fix cannot reach it). Asserting 10/10 here
//! would make `cargo test --workspace` permanently fail for every
//! contributor who fetches these ROMs, for a gap this ticket tracks as
//! evidence, not silently. This test instead asserts that the nine ROMs
//! known to pass keep passing (a regression there IS a real bug), and
//! reports `10-even_odd_timing`'s status without asserting on it.
use std::path::{Path, PathBuf};

use crate::cpu::Cpu;
use crate::system::NesBus;

const VALIDITY_SIGNATURE: [u8; 3] = [0xDE, 0xB0, 0x61];

/// Resolves a fetched-artifact path relative to this crate, `None` (the
/// absence-skip path) if it isn't a real file — same convention as
/// `crate::system::tests::nestest::resolve`, without the env-var override
/// (this suite never runs in CI, so there's no cache-path flexibility to
/// support).
fn resolve(default_rel: &str) -> Option<PathBuf> {
    let default = Path::new(env!("CARGO_MANIFEST_DIR")).join(default_rel);
    if default.is_file() {
        Some(default)
    } else {
        None
    }
}

/// Drives `rom_path` through the real reset vector and polls `$6000+`
/// frame-by-frame (via `NesBus::frame_count`) until the protocol reports a
/// final status or `max_frames` elapses. Returns `Some(status_byte)` (the
/// blargg readme's `$00-$7F` = "completed and gave that result code";
/// `$00` = passed) or `None` on timeout.
fn run_six_thousand(rom_path: &Path, max_frames: u32) -> Option<u8> {
    let rom_bytes = std::fs::read(rom_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", rom_path.display()));
    let bus = NesBus::from_ines_bytes(&rom_bytes)
        .unwrap_or_else(|e| panic!("invalid rom image {}: {e}", rom_path.display()));
    run_six_thousand_on(bus, max_frames)
}

/// [`run_six_thousand`], but for the two MMC3 sub-ROMs
/// (`mmc3_test_2/6-MMC3_alt.nes`, `mmc3_irq_tests/5.MMC3_rev_A.nes`) that
/// require `Mmc3Revision::A` rather than `NesBus::from_ines_bytes`'s
/// default `Mmc3Revision::B` to pass — see `crate::mappers::mmc3`'s module
/// doc "Which revision does a real cartridge get?" section: both ROMs'
/// headers are byte-identical to their revision-B-requiring siblings, so
/// there is no way to detect this from the file itself.
fn run_six_thousand_mmc3_revision_a(rom_path: &Path, max_frames: u32) -> Option<u8> {
    let rom_bytes = std::fs::read(rom_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", rom_path.display()));
    let bus = NesBus::from_ines_bytes_forcing_mmc3_revision_a(&rom_bytes)
        .unwrap_or_else(|e| panic!("invalid rom image {}: {e}", rom_path.display()));
    run_six_thousand_on(bus, max_frames)
}

fn run_six_thousand_on(mut bus: NesBus, max_frames: u32) -> Option<u8> {
    let mut cpu = Cpu::power_on(&mut bus);

    for _ in 0..max_frames {
        let start_frame = bus.frame_count();
        while bus.frame_count() == start_frame {
            cpu.step(&mut bus);
        }
        let region = bus.prg_ram();
        if region[1..4] != VALIDITY_SIGNATURE {
            continue;
        }
        let status = region[0];
        if status != 0x80 {
            return Some(status);
        }
    }
    None
}

/// Drives `rom_path` for exactly `max_frames`, then reads the RAM byte at
/// `result_addr` — see `rf_harness::blargg_evidence::run_ram_result`'s doc
/// for why this protocol has no earlier-completion signal to poll for.
fn run_ram_result(rom_path: &Path, max_frames: u32, result_addr: u16) -> u8 {
    let rom_bytes = std::fs::read(rom_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", rom_path.display()));
    let mut bus = NesBus::from_ines_bytes(&rom_bytes)
        .unwrap_or_else(|e| panic!("invalid rom image {}: {e}", rom_path.display()));
    let mut cpu = Cpu::power_on(&mut bus);

    for _ in 0..max_frames {
        let start_frame = bus.frame_count();
        while bus.frame_count() == start_frame {
            cpu.step(&mut bus);
        }
    }
    bus.peek(result_addr)
}

#[test]
fn sprite_hit_tests_all_eleven_pass() {
    let names = [
        "01.basics",
        "02.alignment",
        "03.corners",
        "04.flip",
        "05.left_clip",
        "06.right_edge",
        "07.screen_bottom",
        "08.double_height",
        "09.timing_basics",
        "10.timing_order",
        "11.edge_timing",
    ];

    let mut missing = 0;
    let mut failures = Vec::new();
    for name in names {
        let rel = format!("../../roms/nes/sprite_hit_tests_2005.10.05/{name}.nes");
        let Some(rom_path) = resolve(&rel) else {
            missing += 1;
            continue;
        };
        let value = run_ram_result(&rom_path, 600, 0x00F8);
        if value != 1 {
            failures.push(format!("{name}: result byte = {value} (1 = pass)"));
        }
    }

    if missing == names.len() {
        eprintln!(
            "SKIP sprite_hit_tests_all_eleven_pass: sprite_hit_tests_2005.10.05/*.nes not found. \
             Fetch them first: scripts/fetch-test-roms.sh"
        );
        return;
    }
    assert!(
        missing == 0,
        "found some but not all 11 sprite_hit_tests ROMs (partial fetch?) -- {missing} missing"
    );
    assert!(
        failures.is_empty(),
        "sprite_hit_tests failures:\n{}",
        failures.join("\n")
    );
}

#[test]
fn ppu_vbl_nmi_known_good_roms_still_pass() {
    // (name, frame_budget, "must currently pass" per this ticket's real,
    // investigated evidence -- see this file's module doc).
    let roms: [(&str, u32, bool); 10] = [
        ("01-vbl_basics", 3600, true),
        ("02-vbl_set_time", 3600, true),
        ("03-vbl_clear_time", 3600, true),
        ("04-nmi_control", 3600, true),
        ("05-nmi_timing", 3600, true),
        ("06-suppression", 3600, true),
        ("07-nmi_on_timing", 3600, true),
        ("08-nmi_off_timing", 3600, true),
        ("09-even_odd_frames", 3600, true),
        ("10-even_odd_timing", 3600, false),
    ];

    let mut missing = 0;
    let mut regressions = Vec::new();
    for (name, frame_budget, must_pass) in roms {
        let rel = format!("../../roms/nes/ppu_vbl_nmi/rom_singles/{name}.nes");
        let Some(rom_path) = resolve(&rel) else {
            missing += 1;
            continue;
        };
        let status = run_six_thousand(&rom_path, frame_budget);
        let passed = status == Some(0);
        eprintln!(
            "ppu_vbl_nmi/{name}: {}",
            match status {
                Some(0) => "PASS".to_string(),
                Some(code) => format!("FAIL (code ${code:02X})"),
                None => "TIMEOUT".to_string(),
            }
        );
        if must_pass && !passed {
            regressions.push(format!(
                "{name}: expected to pass (previously verified) but got {status:?}"
            ));
        }
    }

    if missing == roms.len() {
        eprintln!(
            "SKIP ppu_vbl_nmi_known_good_roms_still_pass: ppu_vbl_nmi/rom_singles/*.nes not \
             found. Fetch them first: scripts/fetch-test-roms.sh"
        );
        return;
    }
    assert!(
        missing == 0,
        "found some but not all 10 ppu_vbl_nmi rom_singles (partial fetch?) -- {missing} missing"
    );
    assert!(
        regressions.is_empty(),
        "ppu_vbl_nmi regressions in previously-passing ROMs:\n{}",
        regressions.join("\n")
    );
}

/// SEQUENCING (ticket W2-03 pre-flight): run this sub-ROM first and alone.
/// It isolates A12 edge detection (the PPU/bus seam this ticket's guardrail
/// note calls out) from the counter-reload and IRQ-delivery questions the
/// other five sub-ROMs also exercise.
#[test]
fn mmc3_test_2_3_a12_clocking_passes() {
    let Some(rom_path) = resolve("../../roms/nes/mmc3_test_2/rom_singles/3-A12_clocking.nes")
    else {
        eprintln!(
            "SKIP mmc3_test_2_3_a12_clocking_passes: mmc3_test_2/rom_singles/3-A12_clocking.nes \
             not found. Fetch it first: scripts/fetch-test-roms.sh"
        );
        return;
    };
    let status = run_six_thousand(&rom_path, 600);
    eprintln!(
        "mmc3_test_2/3-A12_clocking: {}",
        match status {
            Some(0) => "PASS".to_string(),
            Some(code) => format!("FAIL (code ${code:02X})"),
            None => "TIMEOUT".to_string(),
        }
    );
    assert_eq!(status, Some(0), "3-A12_clocking must pass");
}

/// Acceptance criterion 1 (ticket W2-03): all 6 fetched `mmc3_test_2`
/// sub-ROMs pass, asserted the same way [`sprite_hit_tests_all_eleven_pass`]/
/// [`ppu_vbl_nmi_known_good_roms_still_pass`] already are.
/// `6-MMC3_alt` needs [`Mmc3Revision::A`](crate::mappers::Mmc3Revision)
/// (`run_six_thousand_mmc3_revision_a`) — see that function's doc.
#[test]
fn mmc3_test_2_all_six_sub_roms_pass() {
    let names_and_revision_a = [
        ("1-clocking", false),
        ("2-details", false),
        ("3-A12_clocking", false),
        ("4-scanline_timing", false),
        ("5-MMC3", false),
        ("6-MMC3_alt", true),
    ];

    let mut missing = 0;
    let mut failures = Vec::new();
    for (name, revision_a) in names_and_revision_a {
        let rel = format!("../../roms/nes/mmc3_test_2/rom_singles/{name}.nes");
        let Some(rom_path) = resolve(&rel) else {
            missing += 1;
            continue;
        };
        let status = if revision_a {
            run_six_thousand_mmc3_revision_a(&rom_path, 600)
        } else {
            run_six_thousand(&rom_path, 600)
        };
        eprintln!(
            "mmc3_test_2/{name}: {}",
            match status {
                Some(0) => "PASS".to_string(),
                Some(code) => format!("FAIL (code ${code:02X})"),
                None => "TIMEOUT".to_string(),
            }
        );
        if status != Some(0) {
            failures.push(format!("{name}: expected Some(0), got {status:?}"));
        }
    }

    if missing == names_and_revision_a.len() {
        eprintln!(
            "SKIP mmc3_test_2_all_six_sub_roms_pass: mmc3_test_2/rom_singles/*.nes not found. \
             Fetch them first: scripts/fetch-test-roms.sh"
        );
        return;
    }
    assert!(
        missing == 0,
        "found some but not all 6 mmc3_test_2 rom_singles (partial fetch?) -- {missing} missing"
    );
    assert!(
        failures.is_empty(),
        "mmc3_test_2 failures:\n{}",
        failures.join("\n")
    );
}

// `mmc3_irq_tests` (ticket W2-03) is wired into `tests/rom-manifest.toml`
// and verified fetchable (`scripts/fetch-test-roms.sh
// mmc3-irq-tests-*` — six `OK`s, hash-verified, this ticket's session) but
// deliberately has NO in-crate assertion here, unlike `mmc3_test_2` above.
// A `run_six_thousand`-based attempt was tried first and every one of its
// six sub-ROMs timed out — not an MMC3 behavior gap (the identical ground
// mmc3_test_2 covers passes 6/6 above), but a PROTOCOL mismatch: this
// suite's source (`mmc3_irq_tests/source/*.asm`, its own readme: "runs on
// a custom devcart and assembler") uses Shay Green's older devcart result
// format — a zero-page `result` byte and an infinite text-printing loop at
// `report_final_result_` — not the `$6000`/`$6001-$6003`
// "DE B0 61"-signature protocol `run_six_thousand` reads (confirmed by
// grepping that suite's `validation.asm`: zero references to `$6000` or
// the signature bytes anywhere). Building a second reader (poll a
// zero-page address plus detect the infinite loop, or read rendered
// nametable text) is a real, separate harness feature, not a quick fix,
// and out of what this ticket's acceptance actually requires (the manifest
// entry, done above) — blocked-with-evidence, not silently skipped.
