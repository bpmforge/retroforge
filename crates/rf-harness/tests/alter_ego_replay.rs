//! Alter Ego 5-minute replay: the Tier-A independent-proof regression
//! (FR-CORE-026, ticket W2-11, `docs/TESTING.md` §4's "Alter Ego 5-min
//! replay" row). Alter Ego (Shiru, D-001) is the one third-party
//! game-shaped fixture this project regresses on — see
//! `tests/rom-manifest.toml`'s comment block above the `alter-ego-rom`/
//! `alter-ego-src` artifacts for the provenance/licence notice this ticket
//! added.
//!
//! ## Why this lives in `tests/`, not `src/`
//!
//! `crates/rf-harness/src/lib.rs`'s own module doc says "do not add public
//! API here without a ticket in plan.json." Nothing outside this file
//! needs the zip-unpack helper, the scripted input, or the reachable-state
//! hash below, so all three stay test-local (dev-dependency-only `zip` and
//! `rf-input`, same shape `tests/golden_frame_gpu.rs` already uses for its
//! dev-dependency-only `rf-renderer`).
//!
//! ## The scripted input log (empirically derived and verified, not guessed)
//!
//! Alter Ego ships as `alter_ego.zip` (the author serves no raw `.nes`),
//! containing `Alter_Ego.nes` alongside label art, a manual PDF, and
//! `notes.txt` — [`extract_nes_rom`] finds the one NES image by sniffing
//! entry content through `rf_cart::Cartridge::load`, the same discipline
//! `crates/retroforge/src/rom_open.rs::resolve_rom_bytes` uses (never by
//! file name), reimplemented here rather than depending on the
//! `retroforge` app-shell crate — that would drag `eframe`/`rfd` into a
//! test harness for ~20 lines of logic.
//!
//! Alter Ego (`game.c`, in the fetched `alter_ego_src.zip`) has no debug
//! symbols in the shipped ROM, so every fact below was established by
//! **running the real, fetched ROM** through a scratch probe (not by
//! reading `game.c` alone) and cross-checking against it:
//!
//! - `credits_screen()`/`title_screen()` (`game.c`) both gate their
//!   `PAD_START` trigger on a rising edge relative to the *previous*
//!   `pad_trigger()`/`pad_poll()` call, and neither screen's exact
//!   frame-poll timing is discoverable without either counting every
//!   `ppu_waitnmi()` in the call graph by hand (fragile — one missed wait
//!   anywhere silently shifts every frame index after it) or driving the
//!   real ROM and watching for the transition. [`scripted_buttons`]'s
//!   frames `0..150` hold `Start` on a 2-frames-on/2-frames-off square
//!   wave rather than a hand-counted single pulse: this guarantees a
//!   rising edge lands within a few frames of wherever each screen's
//!   `pad_trigger()` loop actually starts polling, without needing
//!   frame-exact knowledge of when that is. Verified empirically: driving
//!   this exact pattern against the real ROM reaches live, controllable
//!   gameplay (confirmed both by a WRAM-diff experiment isolating the
//!   on-screen player-position byte, and by rendering frames to PNG and
//!   eyeballing real level 1 geometry, HUD stats, and "HELLO WORLD" title
//!   text — not just a passing hash).
//! - Gameplay becomes controllable (the game_loop's `start_delay` window
//!   elapses) well before frame 260 for this exact mash pattern — verified
//!   by holding `Right` in short probe windows and finding the first one
//!   that produces any WRAM change (transition observed between frames
//!   232 and 234 in one probe run; frame 260 leaves comfortable margin).
//! - [`PLAYER_X_ADDR`] (`$0619`) is an on-screen X-coordinate byte for the
//!   player sprite — identified empirically (not from a symbol table) by
//!   diffing full WRAM between an idle run and a run holding `Right` for a
//!   short window: `$0619` and the OAM shadow-buffer bytes at `$02F3`/
//!   `$02F7` (the "active ego" sprite pair's X byte, `OAM_PLAYER+0`/`+1` in
//!   `game.c`, 4 bytes/sprite, `[Y,tile,attr,X]` per entry) move by the
//!   identical delta on every experiment, which only happens if `$0619` is
//!   the same value `oam_spr` mirrors into the sprite buffer that frame.
//! - Boot/spawn value: `$0619 == 184` before any `Right` input reaches the
//!   game (confirmed both immediately post-boot-idle and via the
//!   all-zeros-log mutation test below). After holding `Right` for frames
//!   `260..290` (30 frames): `$0619 == 216` (settles by frame ~310 and
//!   stays there — the extra frames beyond the 30-frame hold are the
//!   in-flight 8-frame movement commit already started before release
//!   completing; `game.c`'s `DIR_RIGHT` handling does not re-check input
//!   mid-move). This is comfortably short of the wall the player would
//!   hit by continuing to hold `Right` (`$0619` saturates at 232) — **do
//!   not extend the hold window without re-verifying survival**: holding
//!   `Right` all the way to the wall and then idling there was tried
//!   first, and the run does NOT survive 5 minutes idle at that spot — by
//!   frame 9000 it has looped back to the title screen (confirmed by
//!   rendering that frame: "ALTER EGO … PRESS START"), almost certainly an
//!   enemy patrol eventually reaching the idle player and triggering
//!   `DONE_NOLUCK` enough times to exhaust `restart` (5) and hit
//!   `game_over_screen()`. Idling at `216` instead (short of the wall) was
//!   re-verified to survive the full 18,000 frames with `$0619` stable —
//!   this is the one actually shipped.
//! - No pause is used anywhere in the final script: an earlier draft
//!   paused (`Start`) immediately after the move and stayed paused for
//!   ~98% of the run to dodge the wall-death above, but that made nearly
//!   every golden-frame checkpoint the same frozen pause menu (cheap to
//!   check: two paused checkpoints hash identically, which is not what
//!   "golden frames" plural should mean) and does not match FR-CORE-026's
//!   "run … for a scripted 5-minute input log" — idling at 216 needed no
//!   such workaround once measured directly.
//!
//! ## The reachable-state hash: duplicated on purpose
//!
//! [`reachable_state_hash`] is byte-for-byte the same formula as
//! `crates/retroforge/src/stepper.rs::EmuStepper::state_hash` — same field
//! list, same order, same `"reachable-v1"` meaning (`.rfreplay`'s
//! `hash_kind`, `crates/rf-input/src/replay.rs` module doc). It is
//! **duplicated, not shared**: rf-harness cannot depend on the
//! `retroforge` app-shell crate (that would drag `eframe`/`rfd`/the GUI
//! stack into a test harness for one function), the same "second,
//! independent driver" discipline already established for
//! `blargg_evidence.rs`/`nestest_evidence.rs` versus their `rf-nes`-internal
//! peers. **Nothing mechanically enforces the two copies staying in
//! sync** — a future change to either `state_hash` needs to update both by
//! hand, or `hash_kind = "reachable-v1"` quietly stops meaning one thing
//! project-wide.
//!
//! ## Anti-vacuity (the brief's central trap)
//!
//! A final-state hash over 5 minutes proves nothing if the input log never
//! drives the game — an all-zero log produces a perfectly stable,
//! perfectly reproducible hash of a title screen that never even loads
//! level 1. Both tests below assert [`PLAYER_X_ADDR`] moved to a specific,
//! non-boot value (`216`, not merely "not 0") — mutation-verified by hand
//! (conductor note, not shipped as a separate test — see the ticket's
//! final report for the exact commands run):
//! - Replacing [`scripted_buttons`] with a constant `0` (all-zeros log):
//!   the game never leaves the credits screen, `$0619` stays `0` — the
//!   `216` assertion fails.
//! - Keeping the `Start` mash but deleting the `Right`-hold arm (script
//!   reaches live gameplay but never presses a direction): `$0619` stays
//!   `184` (the spawn X) — the `216` assertion fails on a *different*,
//!   more specific value than the all-zeros case, proving this checks
//!   that *directional* input reached the game, not merely "some input
//!   happened."
//!
//! ## Tier honesty (`docs/TESTING.md` §4's own rule)
//!
//! TESTING.md §4 states, in bold, that a tier label overstating coverage
//! is worse than an honest gap. This suite is real and self-verifying, but
//! by that section's own Tier A-local definition ("a suite whose … fixture
//! is a gitignored fetched artifact CI never has (NFR-006)") Alter Ego
//! qualifies for A-local treatment — except A-local also means "still a
//! hard release gate" via `docs/evidence/local-gate.json` +
//! `scripts/validate-evidence.mjs`'s staleness check, and wiring that (or
//! `.github/**`) is explicitly out of this ticket's scope (see the TESTING
//! §4 row this ticket edited and its explanatory note for exactly what is
//! and is not covered today).
use rf_core_api::{CoreEvent, CoreSink, InputFrame, PpuPixel};
use rf_harness::{hash_frame_palette_indices, hex_sha256, FrameCapture};
use rf_input::{NesButton, ReplayHeader, ReplayLog, ReplayPlayer, ReplayRecorder, StartType};
use rf_nes::{Cpu, NesBus};
use std::io::Read as _;
use std::path::PathBuf;

/// One NTSC frame at 60 fps, times 300 seconds = 5 minutes exactly — also
/// `tests/rom-manifest.toml`'s `alter_ego` suite's already-correct
/// `frame_budget`.
const TOTAL_FRAMES: u64 = 18_000;

/// How often (in frames) [`ReplayRecorder::record_hash`] is called, per
/// `.rfreplay`'s own convention (`crates/rf-input/src/replay.rs`: "meant
/// to be emitted every `hash_interval` frames and always for the final
/// frame"). 600 frames = 10 seconds, giving 30 periodic checkpoints plus
/// the final frame.
const HASH_INTERVAL: u64 = 600;

/// Frame indices golden-framed (module doc: chosen well inside the live,
/// continuously-rendering part of the run — never during the credits/title
/// fades, which briefly disable rendering and would trip
/// [`hash_frame_palette_indices`]'s "complete frame" assertion).
const GOLDEN_FRAMES: [u64; 4] = [500, 6000, 12000, 17999];

/// Empirically identified on-screen player X-coordinate byte (module doc).
const PLAYER_X_ADDR: u16 = 0x0619;

/// Value at `$0619` once the scripted `Right`-hold (frames 260..290)
/// settles (module doc).
const PLAYER_X_AFTER_MOVE: u8 = 216;

struct NullSink;
impl CoreSink for NullSink {
    fn video_scanline(&mut self, _y: u16, _pixels: &[PpuPixel]) {}
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, _ev: CoreEvent) {}
}

/// Resolves the fetched `alter_ego.zip` (gitignored, NFR-006): `env_var` if
/// set and it names a real file, else the default relative-to-this-crate
/// path if that is a real file, else `None` (the absence-skip path —
/// same convention `crates/rf-nes/src/system/tests/nestest.rs::resolve`
/// uses).
fn resolve_zip() -> Option<PathBuf> {
    if let Ok(configured) = std::env::var("RF_ALTER_EGO_ZIP") {
        let path = PathBuf::from(configured);
        return if path.is_file() { Some(path) } else { None };
    }
    let default =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../roms/nes/alter-ego/alter_ego.zip");
    if default.is_file() {
        Some(default)
    } else {
        None
    }
}

/// Find the single NES ROM inside `zip_bytes` by sniffing entry content
/// (module doc). Unlike `rom_open.rs::resolve_rom_bytes` (untrusted,
/// user-picked input), this archive's bytes are already SHA-256-pinned by
/// `tests/rom-manifest.toml` and verified at fetch time
/// ([`crate::fetch::fetch_artifact`]), so this skips that function's
/// defensive per-entry/cumulative read caps — there is nothing left to be
/// untrusted about by the time this runs.
///
/// # Errors
/// A message naming why: the archive didn't parse, held no NES image, or
/// held more than one.
fn extract_nes_rom(zip_bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes))
        .map_err(|e| format!("alter_ego.zip did not parse as a zip archive: {e}"))?;
    let mut found: Option<(String, Vec<u8>)> = None;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("could not read alter_ego.zip entry {i}: {e}"))?;
        if !entry.is_file() {
            continue;
        }
        let name = entry.name().to_string();
        let mut buf = Vec::new();
        entry
            .read_to_end(&mut buf)
            .map_err(|e| format!("could not read alter_ego.zip entry {name}: {e}"))?;
        if matches!(
            rf_cart::Cartridge::load(&buf),
            Ok(rf_cart::Cartridge::Nes { .. })
        ) {
            if let Some((prev, _)) = &found {
                return Err(format!(
                    "alter_ego.zip contains more than one NES image ({prev}, {name})"
                ));
            }
            found = Some((name, buf));
        }
    }
    found
        .map(|(_, bytes)| bytes)
        .ok_or_else(|| "alter_ego.zip contains no recognizable NES image".to_string())
}

/// The scripted 5-minute input log's port-0 byte for `frame` (module doc's
/// "The scripted input log" section for the full derivation). Uses
/// [`NesButton::bit`] rather than a hand-rolled bit literal so this can
/// never silently drift from the one authoritative `$4016` bit layout.
fn scripted_buttons(frame: u64) -> u8 {
    const MASH_UNTIL: u64 = 150;
    const HOLD_FROM: u64 = 260;
    const HOLD_UNTIL: u64 = 290;

    let start_bit = 1u8 << NesButton::Start.bit();
    let right_bit = 1u8 << NesButton::Right.bit();

    if (HOLD_FROM..HOLD_UNTIL).contains(&frame) {
        right_bit
    } else if frame < MASH_UNTIL {
        // 2-on/2-off square wave: guarantees a rising Start edge lands
        // within a few frames of wherever credits_screen()/title_screen()
        // actually poll, without needing frame-exact knowledge of when
        // that is (module doc).
        if (frame / 2).is_multiple_of(2) {
            start_bit
        } else {
            0
        }
    } else {
        0
    }
}

/// Latch `buttons` into port 0 and run exactly one frame, streaming video
/// to `sink` — the same "latch before advancing" shape
/// `crates/retroforge/src/stepper.rs::EmuStepper::latch_and_advance_frame`
/// uses, reimplemented locally for the same reason
/// [`reachable_state_hash`] is (module doc).
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

/// The "reachable-v1" state hash (module doc's "The reachable-state hash"
/// section — duplicated from `EmuStepper::state_hash`, not shared).
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
/// order. **Verified before freezing, not merely computed once and
/// trusted**: (1) this test's own recorded run and independent replay run
/// agreed on all four byte-exact (the assertion above this constant's use,
/// which failed loudly against placeholder values before these were
/// filled in — confirmed empirically, not assumed); (2) the same four
/// frame indices were separately rendered to RGBA (via a scratch
/// `rf_renderer::FrameBuffer`, outside this test) and eyeballed: each
/// shows real level-1 geometry (the ladder/platform layout), the HUD
/// "life"/"exchange" stat sprites top-left, the "HELLO WORLD" level-name
/// text top-right, and a patrolling enemy sprite at a *different* position
/// in each of the four — ruling out "all four are the same frozen image"
/// (the failure mode an earlier pause-based script draft actually hit,
/// see module doc). All four values differ from each other, consistent
/// with the enemy/decoration animation continuing throughout an idle,
/// fully-live run.
const GOLDEN_HASHES: [&str; 4] = [
    "003bdae35fb94ffcc56d6c0d77dcbd86eefef67526a8c5e685dff574e464a628",
    "1ff2913b9fe712099278cda68ad41af74ee0460a4398413598dbf0e3975e43b3",
    "31e48479acaea3e4be9521e4767e5f76af8b9803a70cffe5c8792d86712c4686",
    "b99e39c2d01c87414a27a1726ff52677872ab993102edeac76a72d332f85ce66",
];

/// Fast anti-vacuity check (NOT `#[ignore]`'d — cheap, ~330 frames, runs on
/// every `cargo test --workspace` where the ROM is fetched): drives only
/// the scripted script's opening (skip credits/title, then the 30-frame
/// `Right` hold) and asserts [`PLAYER_X_ADDR`] reaches
/// [`PLAYER_X_AFTER_MOVE`] — see module doc's "Anti-vacuity" section for
/// what this catches and how it was mutation-verified.
#[test]
fn alter_ego_scripted_input_actually_drives_the_game() {
    let Some(zip_path) = resolve_zip() else {
        eprintln!(
            "SKIP alter_ego_scripted_input_actually_drives_the_game: alter_ego.zip \
             not found. Set RF_ALTER_EGO_ZIP or fetch it at the default path: \
             scripts/fetch-test-roms.sh alter-ego-rom"
        );
        return;
    };
    let zip_bytes = std::fs::read(&zip_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", zip_path.display()));
    let rom_bytes =
        extract_nes_rom(&zip_bytes).expect("alter_ego.zip must yield exactly one NES image");

    let mut bus =
        NesBus::from_ines_bytes(&rom_bytes).expect("Alter Ego must be a valid iNES image");
    let mut cpu = Cpu::power_on(&mut bus);
    let mut sink = NullSink;
    for frame in 0..330u64 {
        run_frame(&mut bus, &mut cpu, scripted_buttons(frame), &mut sink);
    }

    assert_eq!(
        bus.peek(PLAYER_X_ADDR),
        PLAYER_X_AFTER_MOVE,
        "scripted Right-hold (frames 260..290) must move the player from spawn \
         (184) to {PLAYER_X_AFTER_MOVE} -- 0 means the log never left the \
         credits/title screens, 184 means it reached gameplay but Right never \
         registered (module doc's mutation-test notes)"
    );
}

/// The Tier-A independent-proof regression itself (FR-CORE-026): records
/// the scripted 5-minute log through a real `.rfreplay` round trip, then
/// replays it through a **second, independent** `Cpu`/`NesBus` pair,
/// checking every periodic + final reachable-state hash and every
/// golden-frame hash agree between the two runs and against the frozen
/// constants (module doc's "second independent run" + "golden frames…
/// verified" requirements).
#[test]
#[ignore = "5-minute double-run (record + independent replay), ~18s/pass release, \
            >120s/pass debug (measured -- exceeded a 2-minute budget), same \
            release-only discipline as retroforge's determinism.rs 10k-frame \
            suite (ticket W1-08). Run directly with: \
            cargo test --release -p rf-harness --test alter_ego_replay -- --ignored"]
fn alter_ego_five_minute_replay_final_hash_and_golden_frames() {
    let Some(zip_path) = resolve_zip() else {
        eprintln!(
            "SKIP alter_ego_five_minute_replay_final_hash_and_golden_frames: \
             alter_ego.zip not found. Set RF_ALTER_EGO_ZIP or fetch it at the \
             default path: scripts/fetch-test-roms.sh alter-ego-rom"
        );
        return;
    };
    let zip_bytes = std::fs::read(&zip_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", zip_path.display()));
    let rom_bytes =
        extract_nes_rom(&zip_bytes).expect("alter_ego.zip must yield exactly one NES image");
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
        NesBus::from_ines_bytes(&rom_bytes).expect("Alter Ego must be a valid iNES image");
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
        rec_bus.peek(PLAYER_X_ADDR),
        PLAYER_X_AFTER_MOVE,
        "recording run: scripted input must have actually driven the game \
         (module doc's anti-vacuity section)"
    );

    let log = recorder.finish();
    log.verify_rom_sha256(&rom_sha256)
        .expect("recorded log's own rom_sha256 must match the extracted ROM");

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
    // Run 1 -- this is the determinism proof (module doc), not a re-read
    // of Run 1's own result. ---
    let mut player = ReplayPlayer::new(&parsed);
    let mut play_bus =
        NesBus::from_ines_bytes(&rom_bytes).expect("Alter Ego must be a valid iNES image");
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
        play_bus.peek(PLAYER_X_ADDR),
        PLAYER_X_AFTER_MOVE,
        "independent replay: scripted input must have actually driven the game, \
         reproduced from the log alone (module doc's anti-vacuity section)"
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
         intentional core/renderer change, regenerate GOLDEN_HASHES from a run \
         re-verified the same way (module doc); if not, this is the regression \
         these goldens exist to catch"
    );
}
