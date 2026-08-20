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
//!
//! ## The `.rfreplay` gate needed W7-02 first
//!
//! This fixture is why `rf-input` has a `SnesButton` type at all. The
//! format was NES-only, so the scripted "hold Right" log serialised to
//! nothing and replayed as an idle pad — a lossy log masquerading as a
//! determinism failure. W7-02 fixed the format; the gate below is what
//! proves it.

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

/// The full 5-minute replay, through a real `.rfreplay` round trip.
///
/// Run 2 uses a FRESH machine that Run 1 never touched, driven entirely
/// from the parsed log — that is the determinism proof, not a re-read of
/// Run 1's own result.
///
/// ## This test could not exist until W7-02
///
/// `.rfreplay` was NES-only: `PortLogKey::buttons` was a
/// `Vec<NesButton>`, so a SNES button outside the NES 8-bit set — Right
/// is bit 8 — had no log-key entry and was **silently dropped** on
/// serialisation. The first attempt at this test diverged at frame 600
/// because run 2 received `buttons == 0`. The emulator was deterministic
/// throughout; the log was lossy. W7-02 added `SnesButton` and made the
/// log key console-directed, and this is the gate that proves it end to
/// end rather than in a unit test.
#[test]
#[ignore = "5 minutes of emulated time; run via scripts/local-gate.sh"]
fn rf_scroller_s_five_minute_replay_is_deterministic() {
    let Some(rom) = load_rom() else {
        eprintln!("SKIP: run fixtures/snes/rf-scroller-s/build.sh");
        return;
    };
    let rom_sha256 = rf_cart::hash::identity_snes(&rom).normalized.sha256;

    // --- Run 1: record ---
    let header = ReplayHeader {
        console: "snes".to_string(),
        rom_sha256,
        emu_version: env!("CARGO_PKG_VERSION").to_string(),
        core_config: "accuracy".to_string(),
        start_type: StartType::PowerOn,
        hash_kind: "reachable-v1".to_string(),
        hash_interval: HASH_INTERVAL,
    };
    let mut recorder = ReplayRecorder::new(header);
    let mut rec = SnesSystem::load(&rom).expect("loads");
    let mut rec_hashes: Vec<(u64, String)> = Vec::new();

    for frame in 0..TOTAL_FRAMES {
        let buttons = scripted_buttons(frame);
        recorder.record_frame(rf_core_api::InputFrame {
            ports: [buttons, 0, 0, 0],
        });
        run_frame(&mut rec, buttons);
        if (frame + 1).is_multiple_of(HASH_INTERVAL) || frame + 1 == TOTAL_FRAMES {
            rec_hashes.push((frame + 1, reachable_state_hash(&rec)));
        }
    }
    let log = recorder.finish();

    // --- serialize / parse round trip: proves the format is exercised,
    // not merely constructed and discarded ---
    let text = log.to_string();
    let parsed = ReplayLog::parse(&text).expect("the recorded replay must parse");
    assert_eq!(
        text,
        parsed.to_string(),
        "serialize -> parse must round trip"
    );
    assert!(
        text.contains("P1:B,Y,Select,Start,Up,Down,Left,Right,A,X,L,R\n"),
        "the log must carry the SNES button table, not the NES one"
    );

    // --- Run 2: independent replay on a fresh machine ---
    let mut player = ReplayPlayer::new(&parsed);
    let mut play = SnesSystem::load(&rom).expect("loads");
    let mut frame = 0u64;
    let mut checked = 0usize;
    while let Some(input) = player.next_frame() {
        assert_eq!(
            input.ports[0],
            scripted_buttons(frame),
            "frame {frame}: the log must replay the buttons it recorded — a \
             mismatch here means the format dropped bits again"
        );
        run_frame(&mut play, input.ports[0]);
        frame += 1;
        if frame.is_multiple_of(HASH_INTERVAL) || frame == TOTAL_FRAMES {
            let want = &rec_hashes[checked];
            assert_eq!(want.0, frame, "checkpoint frames must line up");
            assert_eq!(
                reachable_state_hash(&play),
                want.1,
                "divergence at frame {frame}: the same input produced different \
                 reachable state on an independent run"
            );
            checked += 1;
        }
    }

    assert_eq!(
        frame, TOTAL_FRAMES,
        "the replay must cover the full 5 minutes"
    );
    assert_eq!(
        checked,
        rec_hashes.len(),
        "every checkpoint must be compared"
    );
    assert_eq!(checked, 30, "30 ten-second checkpoints over five minutes");

    // And the run must have gone somewhere.
    let columns = read_word(&play, COLUMNS_STREAMED);
    assert!(
        columns > 1000,
        "five minutes of scrolling should stream far more than the preload; got {columns}"
    );
}
