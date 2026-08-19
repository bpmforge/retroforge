//! `docs/MVP.md`'s enhancement-demo requirement that the demos be
//! "recorded + reproducible via replay files in-repo", for the
//! **un-profiled scroller** case (ticket W5-08).
//!
//! ## "Un-profiled" is a property of the SESSION, not of the ROM
//!
//! The stitcher, the fog mask and scene identity never consult a profile
//! — that is precisely what makes them the generic fallback for a game
//! nobody has written a profile for. So a faithful un-profiled session is
//! one driven **without loading a profile**, which is what this does with
//! RF-Scroller.
//!
//! The alternative the MVP line names first, Alter Ego, is fetch-only
//! with no redistribution grant (Brad's ruling at W0-03), so no replay of
//! it could be checked in and the "in-repo" half of the requirement could
//! not be met with it at all. Using a ROM we build deterministically and
//! simply declining to give it a profile is both reproducible and honest
//! — and it is stated here rather than left for a reader to assume the
//! ROM is unprofiled.
//!
//! ## What the replay is for
//!
//! A `.rfreplay` makes the demo *reproducible*: the same inputs, applied
//! to the same ROM, produce the same canvas growth on any machine. The
//! recording step below regenerates it, and the assertion step replays
//! the checked-in file — so a change that broke stitching fails here
//! rather than being noticed the next time somebody looks at a screen.

use std::path::{Path, PathBuf};

use retroforge::canvas_accum::CanvasAccumulator;
use retroforge::stepper::EmuStepper;
use rf_core_api::InputFrame;
use rf_input::replay::{ReplayHeader, ReplayLog, ReplayRecorder, StartType};

/// Long enough for the camera to cross several screens' worth of level,
/// which is what "the canvas grows during play" needs in order to be
/// observable at all.
const FRAMES: u64 = 900;
const RIGHT: u16 = 0x80;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/retroforge is two levels under the repo root")
        .to_path_buf()
}

fn replay_path() -> PathBuf {
    repo_root().join("fixtures/replays/unprofiled-scroller.rfreplay")
}

fn fixture() -> Option<Vec<u8>> {
    match std::fs::read(repo_root().join("fixtures/nes/rf-scroller/build/rf-scroller.nes")) {
        Ok(b) => Some(b),
        Err(_) => {
            assert!(
                std::env::var_os("CI").is_none(),
                "fixture ROM missing on CI: its build step failed and this suite would pass \
                 having asserted nothing"
            );
            eprintln!("SKIP: fixture ROM not built — run fixtures/nes/rf-scroller/build.sh");
            None
        }
    }
}

/// The scripted session: hold Right throughout.
fn scripted_inputs() -> Vec<InputFrame> {
    (0..FRAMES)
        .map(|_| {
            let mut f = InputFrame::empty();
            f.ports[0] = RIGHT;
            f
        })
        .collect()
}

/// Regenerate `fixtures/replays/unprofiled-scroller.rfreplay`.
///
/// `#[ignore]`d: it WRITES into the repository, and a test that rewrites
/// a checked-in artefact on every `cargo test` would make every run dirty
/// the working tree. Run deliberately with
/// `cargo test -p rf-harness --test unprofiled_scroller_replay -- --ignored record`
/// when the ROM changes.
#[test]
#[ignore = "regenerates a checked-in artefact; run deliberately"]
fn record_unprofiled_scroller_replay() {
    let Some(rom) = fixture() else { return };
    let rom_sha256 = rf_cart::hash::identity_nes(&rom).normalized.sha256;

    let mut recorder = ReplayRecorder::new(ReplayHeader {
        console: "nes".to_string(),
        rom_sha256: rom_sha256.clone(),
        emu_version: env!("CARGO_PKG_VERSION").to_string(),
        // No profile: this is the generic, un-profiled path.
        core_config: "accuracy".to_string(),
        start_type: StartType::PowerOn,
        hash_kind: "full-v1".to_string(),
        hash_interval: 60,
    });
    for input in scripted_inputs() {
        recorder.record_frame(input);
    }
    let log: ReplayLog = recorder.finish();

    let path = replay_path();
    std::fs::create_dir_all(path.parent().expect("has a parent")).expect("create dir");
    std::fs::write(&path, log.to_string()).expect("write replay");
    eprintln!("wrote {} ({FRAMES} frames)", path.display());
}

/// **The assertion the MVP checkbox is about.** Replay the checked-in
/// file and watch the stitched canvas grow while scene identity holds.
#[test]
fn the_checked_in_replay_grows_a_canvas_under_one_stable_scene() {
    let Some(rom) = fixture() else { return };
    let text = std::fs::read_to_string(replay_path()).expect(
        "fixtures/replays/unprofiled-scroller.rfreplay must be checked in — MVP.md requires the \
         enhancement demos be reproducible via replay files IN-REPO. Regenerate with the \
         `record` test if the fixture ROM changed.",
    );
    let log = ReplayLog::parse(&text).expect("the checked-in replay must parse");

    // The replay is bound to the ROM it was recorded against; a fixture
    // rebuild that changed the ROM must fail loudly here rather than
    // replaying stale inputs into a different game.
    let actual = rf_cart::hash::identity_nes(&rom).normalized.sha256;
    log.verify_rom_sha256(&actual).expect(
        "the checked-in replay was recorded against a different ROM — regenerate it with the \
         `record` test",
    );

    let mut stepper = EmuStepper::from_ines_bytes(&rom).expect("fixture loads");
    stepper.resume();
    // `None` cache: this test is about the canvas GROWING during one
    // session, which is a different claim from W4-03b's cross-restart
    // persistence and is gated there.
    let mut accum = CanvasAccumulator::new(actual, None);
    let mut builder = rf_core_api::FrameBundleBuilder::new(
        rf_renderer::frame::NES_WIDTH as u16,
        rf_renderer::frame::NES_HEIGHT as u16,
    );

    // `SceneId` is not `Ord`, so collect and dedup rather than using a set.
    let mut scene_ids: Vec<rf_enhance::scene_identity::SceneId> = Vec::new();
    let mut sizes = Vec::new();
    for (i, input) in log.frames.iter().enumerate() {
        stepper.latch_and_advance_frame(*input, &mut builder);
        let bundle = builder.take(stepper.frame_count());
        let id = accum.observe_frame(&bundle, &[]);
        if !scene_ids.contains(&id) {
            scene_ids.push(id);
        }
        if i % 100 == 0 {
            sizes.push(accum.current_canvas().map_or(0, |c| c.width()));
        }
    }

    // FR-ENH-004's demo claim: the canvas GROWS as the player explores.
    let first = *sizes.first().expect("sampled");
    let last = *sizes.last().expect("sampled");
    assert!(
        last > first,
        "the stitched canvas did not grow over {FRAMES} frames of scrolling ({first} -> {last}) \
         — the demo this replay exists to reproduce is not happening"
    );
    assert!(
        last as u64 > rf_renderer::frame::NES_WIDTH as u64,
        "the canvas ({last}px) never exceeded one console viewport, so there is nothing \
         'beyond the original viewport' to show"
    );

    // W4-03d's property, re-asserted over this session: one level is one
    // scene, not one scene per few block-widths.
    assert_eq!(
        scene_ids.len(),
        1,
        "a single continuous scroll produced {} scene ids; scene identity is not stable and the \
         canvas would be fragmented across them",
        scene_ids.len()
    );
}
