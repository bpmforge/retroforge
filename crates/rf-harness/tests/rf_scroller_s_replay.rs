//! RF-Scroller-S 5-minute replay: the Tier-A regression for the
//! project's own in-repo **SNES** fixture (FR-CORE-037, ticket W6-05,
//! D-001).
//!
//! ## Shape, deliberately mirrored from `rf_scroller_replay.rs`
//!
//! Same record-then-independently-replay `.rfreplay` round trip, the
//! same fast always-run anti-vacuity test plus a slow `#[ignore]`'d full
//! 5-minute run, and the same rule about the fixture's ROM: it is built
//! from source and never enters git — only `rom.sha256` is checked in,
//! and `build.sh` is the single command that turns source into those
//! exact bytes.
//!
//! ## Anti-vacuity: `columns_streamed` is the hook
//!
//! A fixture that only ever renders its opening screen would pass a
//! replay hash forever while proving nothing about scrolling. RF-Scroller-S
//! streams a new tilemap column each time the camera advances a tile, and
//! the tilemap is only 32 columns wide — so a `columns_streamed` above 32
//! is only reachable once physical columns have been **reused for new
//! content**.
//!
//! That is the same trap RF-Scroller (NES) records in its own FORMAT.md,
//! and it is why the fast test below asserts specific non-boot values
//! rather than "the hash is stable": a do-nothing input log leaves
//! `player_x` at its spawn value, `camera_x` at 0 and `columns_streamed`
//! at 33 (the 32-column preload plus the first frame's), forever.

use rf_input::{ReplayHeader, ReplayLog, ReplayPlayer, ReplayRecorder, StartType};
use rf_snes::cpu::CpuBus;
use rf_snes::SnesSystem;
use std::path::PathBuf;

/// One NTSC frame at 60 fps x 300 seconds — 5 minutes exactly, the same
/// convention the NES replays use.
const TOTAL_FRAMES: u64 = 18_000;
/// `.rfreplay`'s periodic-hash cadence: 600 frames = 10 seconds.
const HASH_INTERVAL: u64 = 600;

/// Zero-page addresses from `fixtures/snes/rf-scroller-s/FORMAT.md`.
/// Declaration order decides this layout — see that file.
const FRAME_COUNTER: u32 = 0x0000;
const PLAYER_X: u32 = 0x0002;
const CAMERA_X: u32 = 0x0004;
const COLUMNS_STREAMED: u32 = 0x0006;

/// The SNES joypad word: Right is bit 8.
const BUTTON_RIGHT: u16 = 0x0100;

fn rom_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/snes/rf-scroller-s/build/rf-scroller-s.sfc")
}

fn load_rom() -> Option<Vec<u8>> {
    std::fs::read(rom_path()).ok()
}

/// The scripted log: hold Right for the whole run.
///
/// Deliberately simple. The point of this fixture is that *the emulator*
/// stays deterministic across five minutes of real scrolling and column
/// streaming, not that the input is interesting.
fn scripted_buttons(_frame: u64) -> u16 {
    BUTTON_RIGHT
}

fn read_word(s: &SnesSystem, addr: u32) -> u16 {
    u16::from(s.bus.peek(addr)) | (u16::from(s.bus.peek(addr + 1)) << 8)
}

/// Run exactly one frame with `buttons` held.
fn run_frame(s: &mut SnesSystem, buttons: u16) {
    let start = s.bus.timing.frame;
    let mut guard = 0u64;
    while s.bus.timing.frame == start {
        s.bus.joypads.ports[0] = buttons;
        s.step().expect("all 256 opcodes are implemented");
        guard += 1;
        assert!(guard <= 500_000, "frame {start} did not complete");
    }
}

/// A hash over the state a program can actually reach: work RAM plus the
/// CPU registers.
///
/// Deliberately NOT the whole machine — VRAM and CGRAM are outputs, and
/// including them would make this a frame hash under a different name.
/// Same reasoning the NES `reachable_state_hash` records.
fn reachable_state_hash(s: &SnesSystem) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(&s.bus.wram[..0x2000]);
    h.update(s.cpu.a.to_le_bytes());
    h.update(s.cpu.x.to_le_bytes());
    h.update(s.cpu.y.to_le_bytes());
    h.update(s.cpu.sp.to_le_bytes());
    h.update(s.cpu.pc.to_le_bytes());
    h.update([s.cpu.p, s.cpu.pbr, s.cpu.dbr, u8::from(s.cpu.e)]);
    use std::fmt::Write;
    h.finalize().iter().fold(String::new(), |mut acc, b| {
        let _ = write!(acc, "{b:02x}");
        acc
    })
}

/// **The anti-vacuity test.** Always runs; fast.
///
/// Asserts the scripted input actually drives the game, by requiring
/// three specific non-boot values. A hash-only test would pass on a
/// fixture that had frozen at its title screen.
#[test]
fn rf_scroller_s_scripted_input_actually_drives_the_game() {
    let Some(rom) = load_rom() else {
        eprintln!("SKIP: run fixtures/snes/rf-scroller-s/build.sh");
        return;
    };
    let mut s = SnesSystem::load(&rom).expect("the fixture is a plain LoROM cart");
    for f in 0..900u64 {
        run_frame(&mut s, scripted_buttons(f));
    }

    let frames = read_word(&s, FRAME_COUNTER);
    let player_x = read_word(&s, PLAYER_X);
    let camera_x = read_word(&s, CAMERA_X);
    let columns = read_word(&s, COLUMNS_STREAMED);

    assert!(frames > 800, "the game loop must be running: {frames}");
    assert!(
        player_x > 16,
        "the player must have moved off its spawn: {player_x}"
    );
    assert!(camera_x > 0, "the camera must have followed: {camera_x}");
    assert!(
        columns > 33,
        "columns_streamed must exceed the 32-column preload plus one, or no \
         physical tilemap column has been REUSED and the scrolling proves \
         nothing: {columns}"
    );
}

/// The counter-test for the one above: with no input, nothing moves.
///
/// Without this, the assertions above could be satisfied by a fixture
/// that scrolled on its own regardless of the pad — which would make the
/// replay a test of nothing.
#[test]
fn rf_scroller_s_does_nothing_without_input() {
    let Some(rom) = load_rom() else {
        eprintln!("SKIP");
        return;
    };
    let mut s = SnesSystem::load(&rom).expect("loads");
    for _ in 0..900u64 {
        run_frame(&mut s, 0);
    }
    assert_eq!(read_word(&s, PLAYER_X), 16, "no input, no movement");
    assert_eq!(read_word(&s, CAMERA_X), 0, "no input, no scrolling");
    assert_eq!(
        read_word(&s, COLUMNS_STREAMED),
        33,
        "only the preload plus the first frame's column"
    );
}

/// The full 5-minute run, twice, on two independent machines.
///
/// ## Why this does NOT go through a `.rfreplay` round trip
///
/// It cannot yet, and the reason is a real gap rather than a shortcut.
/// `rf-input`'s replay format is **NES-only**: `PortLogKey::buttons` is a
/// `Vec<NesButton>`, `canonical_log_key()` always writes the `$4016` read
/// order, and there is no `SnesButton` type in the crate at all. A SNES
/// button outside the NES 8-bit set — such as Right, at bit 8 — has no
/// entry in the log key and is **silently dropped** on serialisation.
///
/// That is not a theory. Recording this fixture's scripted "hold Right"
/// log and replaying it produced a state divergence at the very first
/// checkpoint (frame 600), because run 2 received `buttons == 0`. The
/// emulator was deterministic throughout; the log was lossy.
///
/// So this test delivers the half that is achievable and honest — the
/// scripted five minutes runs without faults, and two independent
/// machines fed the same input reach bit-identical reachable state at
/// every checkpoint. The `.rfreplay` gating half is recorded as a
/// HANDOFF on W6-05.
#[test]
#[ignore = "5 minutes of emulated time; run via scripts/local-gate.sh"]
fn rf_scroller_s_five_minutes_is_deterministic_across_independent_runs() {
    let Some(rom) = load_rom() else {
        eprintln!("SKIP: run fixtures/snes/rf-scroller-s/build.sh");
        return;
    };

    let mut a = SnesSystem::load(&rom).expect("loads");
    let mut b = SnesSystem::load(&rom).expect("loads");
    let mut checkpoints = 0usize;

    for frame in 0..TOTAL_FRAMES {
        let buttons = scripted_buttons(frame);
        run_frame(&mut a, buttons);
        run_frame(&mut b, buttons);
        if (frame + 1) % HASH_INTERVAL == 0 || frame + 1 == TOTAL_FRAMES {
            assert_eq!(
                reachable_state_hash(&a),
                reachable_state_hash(&b),
                "divergence at frame {}: the same input produced different \
                 reachable state on two independent machines",
                frame + 1
            );
            checkpoints += 1;
        }
    }

    assert_eq!(
        checkpoints, 30,
        "30 ten-second checkpoints over five minutes"
    );
    // And the run must have gone somewhere: five minutes of scrolling
    // streams far more than the 32-column preload.
    let columns = read_word(&a, COLUMNS_STREAMED);
    assert!(
        columns > 1000,
        "five minutes of scrolling should stream far more than the preload; got {columns}"
    );
}

/// Pins the `.rfreplay` gap itself, so it cannot be quietly forgotten.
///
/// If someone adds SNES support to `rf-input`, this test starts failing
/// and points at the work that becomes possible — which is a better
/// reminder than a comment.
#[test]
fn the_replay_format_still_cannot_represent_snes_buttons() {
    let header = ReplayHeader {
        console: "snes".to_string(),
        rom_sha256: "0".repeat(64),
        emu_version: env!("CARGO_PKG_VERSION").to_string(),
        core_config: "accuracy".to_string(),
        start_type: StartType::PowerOn,
        hash_kind: "reachable-v1".to_string(),
        hash_interval: HASH_INTERVAL,
    };
    let mut recorder = ReplayRecorder::new(header);
    recorder.record_frame(rf_core_api::InputFrame {
        ports: [BUTTON_RIGHT, 0, 0, 0],
    });
    let log = recorder.finish();
    let parsed = ReplayLog::parse(&log.to_string()).expect("parses");
    let mut player = ReplayPlayer::new(&parsed);
    let got = player.next_frame().expect("one frame").ports[0];

    assert_eq!(
        got, 0,
        "SNES bit 8 survived a .rfreplay round trip — rf-input has gained \
         SNES support, so RF-Scroller-S's replay gate (W6-05 criterion 3) \
         can now be completed. Delete this test and wire it up."
    );
}
