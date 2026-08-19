//! Ticket W5-02b's acceptance, run against the real fixture: the offline
//! `metatile_screens` decode must equal the nametable bytes the ROM
//! itself streams into VRAM, byte for byte, across the whole level.
//!
//! See `rf_harness::level_decode_evidence`'s module doc for why this
//! compares VRAM rather than screenshots, and why it takes two snapshots.

use std::path::{Path, PathBuf};

use rf_enhance::decode::metatile_screens;
use rf_harness::level_decode_evidence as ev;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/rf-harness is two levels under the repo root")
        .to_path_buf()
}

fn fixture_path() -> PathBuf {
    repo_root().join("fixtures/nes/rf-scroller/build/rf-scroller.nes")
}

/// Skip locally, **fail on CI** — same rule as `gpu_or_skip` (W3-06) and
/// as `rf-enhance`'s offline decode suite. CI builds the fixture in its
/// own step before testing, so an absent ROM there means that step broke,
/// and a suite that passed having asserted nothing is the failure mode
/// this file exists to prevent.
fn rom_or_skip(reason: &str) -> Option<Vec<u8>> {
    match std::fs::read(fixture_path()) {
        Ok(bytes) => Some(bytes),
        Err(_) => {
            assert!(
                std::env::var_os("CI").is_none(),
                "{reason}: CI builds the fixture before testing, so its absence means that step \
                 failed and this suite would otherwise pass having asserted nothing"
            );
            eprintln!("SKIP: {reason} — run fixtures/nes/rf-scroller/build.sh (needs cc65)");
            None
        }
    }
}

/// **The half of acceptance criterion 2 that is fully proven: all 64
/// preload columns, byte-exact, from a quiescent machine.**
///
/// `init_video()` fills raw columns 0-63 and then nothing streams until
/// the camera moves, so this is the one moment VRAM is genuinely still —
/// no tearing, no recycled slots, no race. Every one of those 64 columns
/// must equal the offline decode exactly.
#[test]
fn the_offline_decode_equals_what_the_rom_preloads_into_vram() {
    let Some(raw) = rom_or_skip("fixture ROM not built") else {
        return;
    };
    let spec = ev::spec_from_shipped_profile(&repo_root()).expect("profile drives the decoder");
    // Offsets are into the NORMALIZED image (rf_enhance::decode's module
    // doc): strip the 16-byte iNES header.
    let level = metatile_screens::decode(&raw[16..], &spec).expect("the fixture level decodes");

    let report = ev::verify_every_column(&raw, &level, &spec, 4_000).expect("the level streams");

    let preload: Vec<usize> = (0..=ev::PRELOAD_LAST_RAW_COLUMN).collect();
    let missing: Vec<usize> = preload
        .iter()
        .copied()
        .filter(|c| !report.verified.contains(c))
        .collect();
    let failed: Vec<&String> = report
        .problems
        .iter()
        .filter(|p| {
            p.split_whitespace()
                .nth(2)
                .and_then(|t| t.parse::<usize>().ok())
                .is_some_and(|c| c <= ev::PRELOAD_LAST_RAW_COLUMN)
        })
        .collect();
    assert!(
        failed.is_empty(),
        "the offline decoder disagrees with the running game on preloaded columns:\n{}",
        failed
            .iter()
            .map(|p| p.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );

    // Anti-vacuity: an empty problem list is also what a harness that
    // compared nothing produces. All 64 must have been reached.
    assert!(
        missing.is_empty(),
        "only {} of 64 preloaded raw columns were verified; missing {missing:?}",
        64 - missing.len()
    );
}

/// **The full-level claim, which does NOT hold yet — `#[ignore]`d so the
/// gap is visible and rerunnable rather than quietly dropped.**
///
/// 81 of the level's 96 raw tile columns verify byte-exact against the
/// running game. The other 15 are documented in
/// `rf_harness::level_decode_evidence`'s "The streamed tail" section:
/// six are byte-identical to the occupant they replace (so the frame
/// their write lands is unobservable), and nine — metatile columns 43-47,
/// which include W2-10a's ladder columns — are torn in the fixture's own
/// final VRAM, matching neither their own content nor their predecessor's.
///
/// This is deliberately not weakened to "at least 81 columns". A test
/// asserting the number that currently passes would be fitting the target
/// to the arrow; the criterion says the whole level, and until it does,
/// this is a known-failing test rather than a passing one with a smaller
/// claim.
#[test]
#[ignore = "15 of 96 raw columns unresolved — see the module doc; run to reproduce"]
fn the_offline_decode_equals_what_the_rom_streams_across_the_whole_level() {
    let Some(raw) = rom_or_skip("fixture ROM not built") else {
        return;
    };
    let spec = ev::spec_from_shipped_profile(&repo_root()).expect("profile drives the decoder");
    let level = metatile_screens::decode(&raw[16..], &spec).expect("the fixture level decodes");
    let report = ev::verify_every_column(&raw, &level, &spec, 4_000).expect("the level streams");

    assert!(
        report.problems.is_empty(),
        "{} of {} raw columns disagree:\n{}",
        report.problems.len(),
        spec.width * 2,
        report.problems.join("\n")
    );
    assert_eq!(report.verified.len(), spec.width as usize * 2);
}

/// **Mutation, run rather than argued.** The test above is only worth
/// having if a wrong decode fails it. This corrupts one metatile
/// definition in the ROM image handed to the DECODER only — the emulator
/// still runs the real ROM — so the two must disagree.
///
/// It targets the metatile table rather than the level data because that
/// is the subtler failure: every column still decodes, sums correctly and
/// has valid ids; only the tiles they expand to are wrong. A test that
/// merely checked the grid's shape would not notice.
#[test]
fn a_corrupted_metatile_table_makes_the_comparison_fail() {
    let Some(raw) = rom_or_skip("fixture ROM not built") else {
        return;
    };
    let spec = ev::spec_from_shipped_profile(&repo_root()).expect("profile drives the decoder");

    let mut corrupted = raw.clone();
    // Metatile 1 is the ground block; flip its top-left tile id.
    let byte = 16 + spec.metatile_table as usize + spec.metatile_size as usize;
    corrupted[byte] ^= 0xFF;

    let level =
        metatile_screens::decode(&corrupted[16..], &spec).expect("still structurally valid");
    let report = ev::verify_every_column(&raw, &level, &spec, 4_000).expect("runs");

    assert!(
        !report.problems.is_empty(),
        "one flipped metatile tile id went undetected — the VRAM comparison is not actually \
         comparing tile content"
    );
}

/// **The `[[identity]]` drift gate owed by W5-01.**
///
/// W5-01 could not host this: its write_scope was
/// `profiles/nes/rf-scroller/**` only. Without it, a fixture rebuild that
/// changed the ROM would leave the profile claiming an identity nothing
/// has — and profiles are keyed on exactly that hash, so every match
/// would silently stop happening.
#[test]
fn the_shipped_profile_still_identifies_the_fixture_ci_builds() {
    let Some(raw) = rom_or_skip("fixture ROM not built") else {
        return;
    };
    let path = repo_root().join("profiles/nes/rf-scroller/profile.toml");
    let profile = rf_profiles::load_file(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .profile;

    let cart = rf_cart::Cartridge::load(&raw).expect("the fixture is a valid cartridge");
    let identity = match &cart {
        rf_cart::Cartridge::Nes { identity, .. } | rf_cart::Cartridge::Snes { identity, .. } => {
            identity
        }
    };

    assert!(
        !profile.identity.is_empty(),
        "the profile declares no [[identity]] block at all"
    );
    let normalized = &identity.normalized.sha256;
    assert!(
        profile
            .identity
            .iter()
            .any(|i| i.sha256.as_deref() == Some(normalized.as_str())),
        "profiles/nes/rf-scroller/profile.toml claims sha256 {:?}, but the fixture CI builds \
         normalizes to {normalized}. Profiles are keyed on this hash: a mismatch means the \
         profile silently stops matching its own game.",
        profile
            .identity
            .iter()
            .map(|i| i.sha256.clone())
            .collect::<Vec<_>>()
    );
}
