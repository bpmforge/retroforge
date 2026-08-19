//! `docs/MVP.md`'s boundary checkbox, at the scale it actually asks for:
//! **identical per-frame core state hashes over a scripted 5000-frame
//! RF-Scroller run** (ticket W5-08).
//!
//! ## Why this is not another `CORPUS` entry
//!
//! `crate::mode_invariant::CORPUS` is a `const` of hermetic synthetic
//! ROMs, and its `run()` feeds `InputFrame::empty()` — the corpus ROMs
//! self-drive. RF-Scroller does neither: it is gitignored build output
//! that CI produces in its own step, and it does not scroll unless
//! something holds Right. "Scripted" in the MVP line means exactly that
//! input, so this test carries its own loop.
//!
//! ## `#[ignore]`d, and run explicitly in CI
//!
//! 10 000 emulated frames plus 10 000 full-machine SHA-256 hashes is a
//! wall-clock test, not a hermetic one — the same call `determinism.rs`
//! makes for its 10k double-run. CI runs it in release. Absent the
//! fixture it skips locally and FAILS on CI, because a suite that passed
//! having asserted nothing is the failure mode this file exists to
//! prevent.

use std::path::{Path, PathBuf};

use retroforge::mode_invariant::hash_indexed_video;
use retroforge::stepper::EmuStepper;
use rf_core_api::InputFrame;

/// The MVP line says 5000 frames. At NTSC that is roughly 83 seconds of
/// play — long enough for the camera to cross the whole level, the tail
/// gate to open, and the gem rotation and blink cadence to run many
/// times over.
const FRAMES: u64 = 5_000;

/// Bit 7 is Right in `Controller`'s documented order. Held for the whole
/// run: an idle RF-Scroller never scrolls, never streams, and never
/// reaches the tail scenes, so a 5000-frame run with no input would be
/// 5000 frames of a title screen and would prove nothing at any length.
const RIGHT: u8 = 0x80;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/retroforge is two levels under the repo root")
        .to_path_buf()
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

struct Frame {
    state: String,
    video: String,
}

/// Run the fixture `FRAMES` frames with Right held, hashing every frame.
///
/// `enhanced` turns on the one enhancement that touches the core path at
/// all — the sprite-limit-bypass overlay. That is the whole point of the
/// boundary check: an enhancement is an *overlay* over an unmodified
/// simulation (project law 6), so the core's state must be
/// indistinguishable with it on.
fn run(rom: &[u8], enhanced: bool) -> Vec<Frame> {
    let mut stepper = EmuStepper::from_ines_bytes(rom).expect("fixture is a valid iNES image");
    let mut builder = rf_core_api::FrameBundleBuilder::new(
        rf_renderer::frame::NES_WIDTH as u16,
        rf_renderer::frame::NES_HEIGHT as u16,
    );
    stepper.resume();
    let mut out = Vec::with_capacity(FRAMES as usize);
    for _ in 0..FRAMES {
        if enhanced {
            // Re-asserted every frame, matching ARCHITECTURE §8's main
            // loop where `enhancement.on_frame` runs per frame — a
            // once-at-startup toggle would not exercise the same path.
            stepper.set_sprite_overlay_enabled(true);
        }
        let mut input = InputFrame::empty();
        input.ports[0] = u16::from(RIGHT);
        stepper.latch_and_advance_frame(input, &mut builder);
        let bundle = builder.take(stepper.frame_count());
        out.push(Frame {
            state: stepper.state_hash(),
            video: hash_indexed_video(&bundle.video),
        });
    }
    out
}

/// **The MVP boundary checkbox.** 5000 frames, both modes, every frame's
/// core state hash compared.
#[test]
#[ignore = "5000-frame wall-clock run; CI runs it in release"]
fn accuracy_and_enhanced_agree_on_core_state_over_5000_scripted_frames() {
    let Some(rom) = fixture() else { return };

    let accuracy = run(&rom, false);
    let enhanced = run(&rom, true);
    assert_eq!(accuracy.len(), FRAMES as usize);
    assert_eq!(enhanced.len(), FRAMES as usize);

    for (i, (a, e)) in accuracy.iter().zip(&enhanced).enumerate() {
        assert_eq!(
            a.state, e.state,
            "core state diverged at frame {i} of {FRAMES}. An enhancement is an overlay over an \
             UNMODIFIED simulation (project law 6); a difference here means the enhanced path \
             changed the machine, not just what is drawn from it."
        );
    }

    // **Anti-vacuity, and this is the assertion that makes the 5000
    // above mean something.** Two runs of a game that never did anything
    // would also agree at every frame. The video hashes must MOVE —
    // otherwise this is 5000 comparisons of one static screen.
    let distinct: std::collections::BTreeSet<&String> = accuracy.iter().map(|f| &f.video).collect();
    assert!(
        distinct.len() > 100,
        "only {} distinct frames over {FRAMES} — the scripted input is not driving the game, so \
         agreeing at every frame proves nothing",
        distinct.len()
    );
    // ...and the machine must actually have advanced, not looped.
    assert_ne!(
        accuracy.first().map(|f| &f.state),
        accuracy.last().map(|f| &f.state),
        "the machine ended in the state it started in"
    );

    eprintln!(
        "MVP boundary: {FRAMES} scripted frames, Accuracy vs Enhanced core state identical at \
         every frame; {} distinct video frames observed",
        distinct.len()
    );
}
