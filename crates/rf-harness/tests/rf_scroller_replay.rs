//! RF-Scroller 5-minute replay: the Tier-A real-game regression for the
//! project's own in-repo NES fixture (FR-CORE-026, ticket W2-10,
//! `docs/TESTING.md` §4's "RF-Scroller 5-min replay" row). RF-Scroller
//! (`fixtures/nes/rf-scroller`, D-001) is built from source, not fetched
//! -- its ROM binary never enters git (gitignored `build/`); only
//! `fixtures/nes/rf-scroller/rom.sha256` is checked in, and
//! `fixtures/nes/rf-scroller/build.sh` is the single command (used
//! identically here, in CI, and by a developer rebuilding locally) that
//! turns source into that exact hash.
//!
//! ## Shape, deliberately mirrored from `alter_ego_replay.rs`
//!
//! Same `NesBus::from_ines_bytes` + `Cpu::power_on` setup, the same
//! `run_frame` helper, the same `reachable_state_hash` (duplicated, not
//! shared -- see that file's module doc for why), the same
//! record-then-independently-replay `.rfreplay` round trip via
//! `rf_input`'s `ReplayRecorder`/`ReplayLog`/`ReplayPlayer`, the same
//! fast always-run anti-vacuity test plus a slow `#[ignore]`'d full
//! 5-minute replay. The one structural difference: RF-Scroller has no
//! `tests/rom-manifest.toml` entry by design (that file's own comment:
//! in-repo fixtures "have no entry here by design; see FR-CORE-026/
//! 035/037") -- [`resolve_rom`] below looks for the *built* ROM directly
//! rather than a fetched/manifest-verified archive.
//!
//! ## Anti-vacuity (the brief's central trap, and this fixture's own
//! history with it)
//!
//! A prior version of this fixture's runtime streamed content into VRAM
//! late enough that the HUD/playfield split visibly glitched on the
//! frames it happened -- see `fixtures/nes/rf-scroller/FORMAT.md`'s
//! "Known defects" section for the full, honest account (root cause,
//! measurements, and the residual that shipped anyway). That defect was
//! found by *driving the scripted input far enough to exercise real
//! wraparound*, not by a hash matching itself -- the exact anti-vacuity
//! discipline this test's own fast check exists to enforce going
//! forward: [`rf_scroller_scripted_input_actually_drives_the_game`]
//! asserts [`PLAYER_X_ADDR`], [`CAMERA_X_ADDR`], and
//! [`COLUMNS_STREAMED_ADDR`] all reach specific, non-boot values --
//! specifically `columns_streamed == 95`, which (per FORMAT.md's
//! "Streaming writes" section) is only reachable once a physical
//! nametable has been genuinely reused for new content. A do-nothing
//! input log leaves `columns_streamed == 63` (the initial two-nametable
//! preload only) forever -- verified by hand (conductor note): replacing
//! [`scripted_buttons`] with a constant `0` leaves `player_x == 16`
//! (spawn), `camera_x == 0`, and `columns_streamed == 63` at frame 900,
//! failing all three assertions below.
use rf_core_api::{CoreEvent, CoreSink, InputFrame, PpuPixel};
use rf_harness::{hash_frame_palette_indices, hex_sha256, FrameCapture};
use rf_input::{NesButton, ReplayHeader, ReplayLog, ReplayPlayer, ReplayRecorder, StartType};
use rf_nes::{Cpu, NesBus};
use std::path::PathBuf;

/// One NTSC frame at 60 fps, times 300 seconds = 5 minutes exactly (same
/// convention as `alter_ego_replay.rs::TOTAL_FRAMES`).
const TOTAL_FRAMES: u64 = 18_000;

/// `.rfreplay`'s periodic-hash cadence: 600 frames = 10 seconds, 30
/// checkpoints plus the final frame (same as `alter_ego_replay.rs`).
const HASH_INTERVAL: u64 = 600;

/// Golden-frame indices, chosen from the CONFIRMED CLEAN region of the
/// run (module doc / FORMAT.md "Known defects"): the residual
/// HUD/playfield-split-lands-late defect is correlated with
/// `stream_chunk()` actually doing chunk work, which only happens while
/// `columns_streamed < 95`. Every index below was individually verified
/// (via a throwaway instrumented run of the same ROM, comparing each
/// candidate frame's `ScrollWrite` placement against the vblank/
/// scanline-14-18 window) to land AFTER `columns_streamed` reaches 95
/// (streaming fully drained) and to be free of the defect at that exact
/// frame -- not merely "probably fine because streaming looks done".
/// `655` and `1200` are close to the streaming/idle boundary
/// specifically to prove the split is clean even shortly after
/// wraparound completes, not just deep in a settled idle tail.
const GOLDEN_FRAMES: [u64; 6] = [655, 1200, 2400, 5000, 10000, 15000];

/// Documented RAM addresses (`FORMAT.md` "Documented RAM addresses").
const PLAYER_X_ADDR: u16 = 0x6029;
const CAMERA_X_ADDR: u16 = 0x602B;
const COLUMNS_STREAMED_ADDR: u16 = 0x602D;

/// Level-end / wraparound-complete values (`FORMAT.md`): `player_x`
/// saturates at `MAX_PLAYER_X` (752), `camera_x` at `MAX_CAMERA_X` (512),
/// and `columns_streamed` reaches 95 (the level's last raw tile column)
/// only once every raw column -- including the ones that required
/// physical-nametable reuse -- has been streamed.
const PLAYER_X_AT_END: u16 = 752;
const CAMERA_X_AT_END: u16 = 512;
const COLUMNS_STREAMED_AT_END: u8 = 95;

struct NullSink;
impl CoreSink for NullSink {
    fn video_scanline(&mut self, _y: u16, _pixels: &[PpuPixel]) {}
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, _ev: CoreEvent) {}
}

/// Resolves the built (never committed, gitignored `build/`) ROM:
/// `RF_SCROLLER_ROM` env var if set and it names a real file, else the
/// default relative-to-this-crate path if that is a real file, else
/// `None` (the absence-skip path, same convention
/// `alter_ego_replay.rs::resolve_zip` and
/// `crates/rf-nes/src/system/tests/nestest.rs::resolve` use).
fn resolve_rom() -> Option<PathBuf> {
    if let Ok(configured) = std::env::var("RF_SCROLLER_ROM") {
        let path = PathBuf::from(configured);
        return if path.is_file() { Some(path) } else { None };
    }
    let default = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/nes/rf-scroller/build/rf-scroller.nes");
    if default.is_file() {
        Some(default)
    } else {
        None
    }
}

/// Reads `fixtures/nes/rf-scroller/rom.sha256`'s pinned hash (the
/// checked-in half of D-001's "hash in git, binary never in git" split).
/// This is a RAW-FILE-BYTES sha256 (`build.sh`'s own convention: `shasum
/// -a 256` over the built `.nes`), deliberately checked with
/// `hex_sha256(&rom_bytes)` below rather than `rf_cart::hash::
/// identity_nes` -- `identity_nes` computes a normalized hash over the
/// parsed cartridge (used for the `.rfreplay` header's `rom_sha256`
/// field below, where header-byte-insensitivity is the point) and does
/// NOT agree with a raw-file shasum; comparing the two would be
/// comparing different things by name coincidence alone (confirmed
/// empirically: they produced different hashes for the same file when
/// this test was first written).
fn expected_rom_sha256() -> String {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/nes/rf-scroller/rom.sha256");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
    text.split_whitespace()
        .next()
        .unwrap_or_else(|| panic!("{} is empty", path.display()))
        .to_string()
}

/// Holds Right for every frame -- this fixture has exactly one input
/// (`FORMAT.md`'s "What this ticket's runtime does NOT do": no Left, no
/// jump). `main.c`'s own movement gate (`frame_counter & 0x07u`, 8px
/// every 8 frames) is what turns a constant "Right held" input into
/// discrete, budget-paced movement -- this function does not need to
/// replicate that pacing itself.
fn scripted_buttons(_frame: u64) -> u8 {
    1u8 << NesButton::Right.bit()
}

/// Latch `buttons` into port 0 and run exactly one frame, streaming video
/// to `sink` (same shape as `alter_ego_replay.rs::run_frame`).
fn run_frame(bus: &mut NesBus, cpu: &mut Cpu, buttons: u8, sink: &mut dyn CoreSink) {
    bus.set_controller_buttons(0, buttons);
    let start = bus.frame_count();
    let mut guard = 0u64;
    while bus.frame_count() == start {
        cpu.step(bus);
        bus.drain_video(sink);
        guard += 1;
        assert!(
            guard <= 400_000,
            "frame did not complete within guard cycles at frame {start}"
        );
    }
}

/// `camera_x`/`player_x` are little-endian `unsigned int` in `main.c`
/// (`FORMAT.md`'s RAM address table) -- read both bytes.
fn peek_u16(bus: &NesBus, addr: u16) -> u16 {
    u16::from(bus.peek(addr)) | (u16::from(bus.peek(addr + 1)) << 8)
}

/// The "reachable-v1" state hash -- byte-for-byte the same formula as
/// `alter_ego_replay.rs::reachable_state_hash` and
/// `crates/retroforge/src/stepper.rs::EmuStepper::state_hash`, duplicated
/// here for the same reason that file's module doc gives (rf-harness
/// cannot depend on the `retroforge` app-shell crate).
fn reachable_state_hash(bus: &NesBus, cpu: &Cpu) -> String {
    let mut buf = Vec::with_capacity(0x0800 + 256 + 0x2000 + 32);
    for addr in 0x0000u16..=0x07FF {
        buf.push(bus.peek(addr));
    }
    buf.extend_from_slice(bus.oam());
    buf.extend_from_slice(bus.prg_ram());
    buf.push(cpu.a);
    buf.push(cpu.x);
    buf.push(cpu.y);
    buf.push(cpu.s);
    buf.push(cpu.p);
    buf.extend_from_slice(&cpu.pc.to_le_bytes());
    buf.push(u8::from(cpu.jammed));
    buf.extend_from_slice(&bus.master_cycle().to_le_bytes());
    buf.extend_from_slice(&bus.frame_count().to_le_bytes());
    hex_sha256(&buf)
}

/// Frozen 2026-08-07: golden-frame hashes for [`GOLDEN_FRAMES`], SHA-256
/// over `palette_index` only (`hash_frame_palette_indices`), in frame
/// order, against ROM `sha256=aa3b08c2ee7b0d9213203e15ae555ced009215121f
/// 28b52d4d6ee460a30b96f0` (`rom.sha256`). **Verified before freezing**:
/// (1) this test's own recording run and independent replay run agree on
/// all six byte-exact (the assertion using this constant, which fails
/// loudly on any divergence -- see the golden-frame comparison below);
/// (2) each of the six frame indices was independently confirmed, via a
/// throwaway instrumented harness run over the exact same ROM hash, to
/// be free of the residual split-timing defect FORMAT.md documents (the
/// `ScrollWrite` events for that frame all land inside vblank or the
/// scanline-14-18 split window) -- these are not merely "the hash
/// matched itself" goldens, and they were deliberately NOT chosen from
/// the streaming-active window (frames ~100-650) where FORMAT.md records
/// the split can land late; (3) `columns_streamed` is `95` at every one
/// of these frame indices (all six are at or after wraparound
/// completion), independently confirmed via the same instrumented run.
const GOLDEN_HASHES: [&str; 6] = [
    "1257b381c4dcbb4f0fc6cda8f1e35520099628efeb4e12654b828f340257f1e0",
    "cba5b17fd0b5eb6711b3c6d65232e2e97e3c5ac1d8f461e917ac7291e27d9bdc",
    "cb09aae208921fd90a59f9b1d7711f874a70d5b2d595dcbf6256a86677e0c27f",
    "658165a4e7df562482ead82095f4b514959458ae9c41a05fdd872b15d4fd9c5e",
    "80ab136a64b9ca62e9929ccd96be5af550830bcd2d082ad3c7d7658e76b912ac",
    "4f7a24f715eee2c7e9091236da9f946f34d21273fce36d92b60dc173004d3a46",
];

/// Fast anti-vacuity check (NOT `#[ignore]`'d -- runs on every
/// `cargo test --workspace` once the ROM is built): drives 900 frames of
/// constant Right and asserts `player_x`, `camera_x`, and
/// `columns_streamed` all reach their level-end values -- see module doc
/// for what this catches and how it was verified.
#[test]
fn rf_scroller_scripted_input_actually_drives_the_game() {
    let Some(rom_path) = resolve_rom() else {
        eprintln!(
            "SKIP rf_scroller_scripted_input_actually_drives_the_game: \
             rf-scroller.nes not built. Build it first: \
             cd fixtures/nes/rf-scroller && ./build.sh (requires cc65 -- \
             brew install cc65 on macOS)"
        );
        return;
    };
    let rom_bytes = std::fs::read(&rom_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", rom_path.display()));

    let mut bus = NesBus::from_ines_bytes(&rom_bytes).expect("rf-scroller.nes must be valid iNES");
    let mut cpu = Cpu::power_on(&mut bus);
    let mut sink = NullSink;
    for frame in 0..900u64 {
        run_frame(&mut bus, &mut cpu, scripted_buttons(frame), &mut sink);
    }

    assert_eq!(
        peek_u16(&bus, PLAYER_X_ADDR),
        PLAYER_X_AT_END,
        "900 frames of Right must move the player to the level end (752) -- \
         16 means the scripted input never reached the game (module doc's \
         anti-vacuity section)"
    );
    assert_eq!(
        peek_u16(&bus, CAMERA_X_ADDR),
        CAMERA_X_AT_END,
        "camera must have followed the player to its maximum (512)"
    );
    assert_eq!(
        bus.peek(COLUMNS_STREAMED_ADDR),
        COLUMNS_STREAMED_AT_END,
        "columns_streamed must reach 95 -- this is only reachable once a \
         physical nametable has been genuinely reused for new content \
         (FORMAT.md's wraparound witness); 63 means streaming past the \
         initial two-nametable preload never happened"
    );
}

/// The Tier-A real-game regression itself (FR-CORE-026): records a
/// scripted 5-minute constant-Right run through a real `.rfreplay` round
/// trip, then replays it through a **second, independent** `Cpu`/`NesBus`
/// pair, checking every periodic + final reachable-state hash and every
/// golden-frame hash agree between the two runs and against the frozen
/// constants.
#[test]
#[ignore = "5-minute double-run (record + independent replay); same \
            release-only discipline as alter_ego_replay.rs's ignored \
            test. Run directly with: cargo test --release -p rf-harness \
            --test rf_scroller_replay -- --ignored"]
fn rf_scroller_five_minute_replay_final_hash_and_golden_frames() {
    let Some(rom_path) = resolve_rom() else {
        eprintln!(
            "SKIP rf_scroller_five_minute_replay_final_hash_and_golden_frames: \
             rf-scroller.nes not built. Build it first: \
             cd fixtures/nes/rf-scroller && ./build.sh (requires cc65 -- \
             brew install cc65 on macOS)"
        );
        return;
    };
    let rom_bytes = std::fs::read(&rom_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", rom_path.display()));
    assert_eq!(
        hex_sha256(&rom_bytes),
        expected_rom_sha256(),
        "the built ROM's raw-file hash does not match \
         fixtures/nes/rf-scroller/rom.sha256 -- see build.sh's own mismatch \
         diagnostics / FORMAT.md's toolchain provenance note; do not loosen \
         this check to work around a mismatch"
    );
    // Normalized identity hash for the .rfreplay header's rom_sha256 field
    // (header-byte-insensitive matching -- see expected_rom_sha256()'s doc
    // for why this is deliberately a DIFFERENT hash from the check above).
    let rom_sha256 = rf_cart::hash::identity_nes(&rom_bytes).normalized.sha256;

    // --- Run 1: record ---
    let header = ReplayHeader {
        console: "nes".to_string(),
        rom_sha256: rom_sha256.clone(),
        emu_version: env!("CARGO_PKG_VERSION").to_string(),
        core_config: "accuracy".to_string(),
        start_type: StartType::PowerOn,
        hash_kind: "reachable-v1".to_string(),
        hash_interval: HASH_INTERVAL,
    };
    let mut recorder = ReplayRecorder::new(header);
    let mut rec_bus =
        NesBus::from_ines_bytes(&rom_bytes).expect("rf-scroller.nes must be valid iNES");
    let mut rec_cpu = Cpu::power_on(&mut rec_bus);
    let mut rec_goldens: Vec<(u64, String)> = Vec::new();
    let mut null = NullSink;

    for frame in 0..TOTAL_FRAMES {
        let buttons = scripted_buttons(frame);
        if GOLDEN_FRAMES.contains(&frame) {
            let mut cap = FrameCapture::new();
            run_frame(&mut rec_bus, &mut rec_cpu, buttons, &mut cap);
            rec_goldens.push((frame, hash_frame_palette_indices(cap.scanlines(), 240)));
        } else {
            run_frame(&mut rec_bus, &mut rec_cpu, buttons, &mut null);
        }
        recorder.record_frame(InputFrame {
            ports: [buttons as u16, 0, 0, 0],
        });
        let is_final = frame + 1 == TOTAL_FRAMES;
        if frame.is_multiple_of(HASH_INTERVAL) || is_final {
            recorder.record_hash(frame, reachable_state_hash(&rec_bus, &rec_cpu));
        }
    }

    assert_eq!(
        peek_u16(&rec_bus, PLAYER_X_ADDR),
        PLAYER_X_AT_END,
        "recording run: scripted input must have actually driven the game \
         to the level end (module doc's anti-vacuity section)"
    );
    assert_eq!(
        rec_bus.peek(COLUMNS_STREAMED_ADDR),
        COLUMNS_STREAMED_AT_END,
        "recording run: wraparound must have completed"
    );

    let log = recorder.finish();
    log.verify_rom_sha256(&rom_sha256)
        .expect("recorded log's own rom_sha256 must match the built ROM");

    // --- serialize / parse round trip (proves the .rfreplay format is
    // actually exercised, not merely constructed and discarded) ---
    let text = log.to_string();
    let parsed = ReplayLog::parse(&text).expect("recorded replay text must parse");
    assert_eq!(
        text,
        parsed.to_string(),
        "serialize -> parse -> serialize must be byte-exact"
    );
    assert_eq!(log, parsed);

    // --- Run 2: independent replay, a FRESH Cpu/NesBus never touched by
    // Run 1 -- this is the determinism proof, not a re-read of Run 1's
    // own result. ---
    let mut player = ReplayPlayer::new(&parsed);
    let mut play_bus =
        NesBus::from_ines_bytes(&rom_bytes).expect("rf-scroller.nes must be valid iNES");
    let mut play_cpu = Cpu::power_on(&mut play_bus);
    let mut play_goldens: Vec<(u64, String)> = Vec::new();
    let mut frame_no = 0u64;
    let mut hashes_checked = 0usize;

    while let Some(input) = player.next_frame() {
        let buttons = input.ports[0] as u8;
        if GOLDEN_FRAMES.contains(&frame_no) {
            let mut cap = FrameCapture::new();
            run_frame(&mut play_bus, &mut play_cpu, buttons, &mut cap);
            play_goldens.push((frame_no, hash_frame_palette_indices(cap.scanlines(), 240)));
        } else {
            run_frame(&mut play_bus, &mut play_cpu, buttons, &mut null);
        }
        if let Some(expected) = player.expected_hash(frame_no) {
            assert_eq!(
                reachable_state_hash(&play_bus, &play_cpu),
                expected,
                "independent replay's state hash at frame {frame_no} diverged from \
                 the recorded run -- first divergence, per FAILURE_MODES.md FM-08"
            );
            hashes_checked += 1;
        }
        frame_no += 1;
    }
    assert_eq!(
        hashes_checked,
        log.hashes.len(),
        "every recorded hash must be checked"
    );
    assert_eq!(
        frame_no, TOTAL_FRAMES,
        "the independent replay must consume exactly the scripted 5 minutes"
    );
    assert_eq!(
        peek_u16(&play_bus, PLAYER_X_ADDR),
        PLAYER_X_AT_END,
        "independent replay: scripted input must have actually driven the game, \
         reproduced from the log alone (module doc's anti-vacuity section)"
    );
    assert_eq!(
        play_bus.peek(COLUMNS_STREAMED_ADDR),
        COLUMNS_STREAMED_AT_END,
        "independent replay: wraparound must have completed, reproduced from \
         the log alone"
    );

    // --- golden frames: recorded run vs. independent replay must match
    // byte-exact (the "captured from a verified run" requirement) ---
    assert_eq!(
        rec_goldens, play_goldens,
        "golden-frame hashes diverged between the recording run and the \
         independent replay -- a golden that only one of the two runs agrees \
         with is not a verified golden"
    );

    // --- and against the frozen constants ---
    let expected: Vec<(u64, String)> = GOLDEN_FRAMES
        .iter()
        .zip(GOLDEN_HASHES.iter())
        .map(|(&f, &h)| (f, h.to_string()))
        .collect();
    assert_eq!(
        rec_goldens, expected,
        "golden-frame hashes changed from the frozen constants -- if this is an \
         intentional core/renderer/ROM change, regenerate GOLDEN_HASHES from a \
         run re-verified the same way (module doc); if not, this is the \
         regression these goldens exist to catch"
    );
}
