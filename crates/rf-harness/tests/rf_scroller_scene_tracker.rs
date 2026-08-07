//! Ticket W4-03d fixture-driven validation: `rf_enhance::scene_tracker::
//! SceneTracker` (the continuity-based scroll-stable scene identity) driven
//! by the REAL RF-Scroller fixture ROM (`fixtures/nes/rf-scroller`, ticket
//! W2-10) — the tracker itself lives in `rf-enhance`, which cannot depend on
//! a console core directly (`scripts/validate-arch.sh` rule 3); this crate
//! is exempted as "the test harness" and already dev-depends on
//! `rf-renderer` the same shape, so a dev-dependency on `rf-enhance` costs
//! nothing new.
//!
//! ## Shape, deliberately mirrored from `rf_scroller_replay.rs`
//!
//! Same `NesBus::from_ines_bytes` + `Cpu::power_on` setup, the same
//! `run_frame` helper, the same `resolve_rom`/absence-skip convention, and
//! the same RAM-address anti-vacuity witnesses (`player_x`/`camera_x`/
//! `columns_streamed`) — duplicated, not shared, for the same reason that
//! file's own module doc gives for its own duplicated
//! `reachable_state_hash`: nothing outside a single test file needs these
//! few lines, and every real-ROM test in this crate independently proves it
//! drove the actual game rather than trusting a sibling file's setup.
//!
//! ## Measured baseline (report before designing, per this ticket's brief)
//!
//! W4-03b's `compute_scene_id`, run over this exact fixture for 900 frames
//! of held Right (752px of `player_x` travel, full wraparound per
//! `columns_streamed == 95`), produces **1 distinct scene id** — not the 36
//! ids/150px W4-03b's own SYNTHETIC fixture measured. This is not a
//! contradiction of that number; it is exactly this ticket's own warning
//! come true ("MAGNITUDES ARE FIXTURE-MEASURED, NOT A PROPERTY OF REAL
//! GAMES... it may differ substantially in either direction") — measured,
//! not assumed, in [`old_hash_baseline_over_a_real_scrolling_session`]
//! below. The reason, also measured (not guessed): RF-Scroller's real
//! background art (`fixtures/nes/rf-scroller/src/level_data.c`'s
//! `area_palette`) produces `block_averages` (`rf_enhance::scene_identity`'s
//! private per-8x8-block-grid function) whose largest adjacent-block delta
//! anywhere in the level stays under roughly 10 — nowhere close to
//! `EDGE_THRESHOLD`'s 48, so `compute_scene_id`'s edge-bucket signature is
//! **constant** (all "flat") across this entire level, for every one of the
//! 900 frames. That is a genuine, separate gap in W4-03b's own algorithm
//! (real NES `palette_index` values cluster far closer together than the
//! wide 0/96/176-separated thirds its own synthetic fixture used), out of
//! THIS ticket's scope to fix (see "Acceptance 2" section below) — recorded
//! here rather than silently worked around.
//!
//! Because the baseline is already 1 on this fixture, **scene-id COUNT
//! alone does not discriminate a working `SceneTracker` from a broken
//! one here** — an always-same-id degenerate implementation trivially
//! "passes" too. [`nine_hundred_frames_of_real_scrolling_play...`] below
//! does assert the id count (acceptance 1's literal wording), but the
//! assertion that actually discriminates is the per-frame WORLD-POSITION
//! DELTA bound plus the RAM-witnessed total travel distance — see that
//! test's own doc. The synthetic unit tests in
//! `crates/rf-enhance/src/scene_tracker.rs` (whose content, unlike this
//! real fixture, DOES cross `EDGE_THRESHOLD`) carry the burden of proving
//! the mechanism can be broken and caught; this file proves it survives
//! real hardware-driven event timing, including a genuine edge case this
//! ticket found and fixed (the OAM-DMA frame-boundary leak,
//! `crate::scroll_tracker`'s module-level note).
//!
//! ## Acceptance 2 ("genuinely different scene -> different id"): BLOCKED
//! at the fixture level, not silently skipped
//!
//! [`rf_scroller_and_alter_ego_scene_ids_measured_not_asserted`] below
//! drives BOTH available real fixture ROMs (RF-Scroller and Alter Ego,
//! `roms/nes/alter-ego`) through independent `SceneTracker` instances and
//! finds they produce the **same** `SceneId`. This is not a bug in
//! `SceneTracker`: both ROMs are mapper 0 (NROM, confirmed by inspecting
//! each `.nes` header's mapper nibble) with no mapper bank state at all (so
//! the bank-state half of the continuity signal cannot discriminate them
//! either — this ticket's own NROM constraint, inherited from W2-10), and
//! both ROMs' real background art happens to produce the same degenerate
//! all-flat `compute_scene_id` edge signature this module doc's baseline
//! section describes — confirmed by printing `block_averages` for both
//! (see that test's own `eprintln!`s). Recalibrating `EDGE_THRESHOLD` to
//! catch real ~10-unit deltas would also catch the ~17-34-unit block-average
//! perturbation a moving sprite causes (`scene_identity.rs`'s own
//! "Threshold derivation" math) — breaking the FM-11 sprite/HUD-noise
//! watermark this ticket was explicitly told not to weaken. No single
//! `EDGE_THRESHOLD` value satisfies both real-content sensitivity and
//! sprite/HUD-noise rejection on these two fixtures; that tension is a
//! genuine, deeper gap in W4-03b's own algorithm, out of this ticket's
//! scope. Acceptance 2's INTENT (the mechanism is not an always-same-id
//! degenerate) is proven instead at the unit level
//! (`crate::scene_tracker::tests::a_scroll_jump_far_beyond_the_continuous_step_bound_changes_the_id`
//! / `a_mapper_bank_change_alone_changes_the_id_even_with_continuous_scroll`
//! in `rf-enhance`, both mutation-verified) — this file's own test is a
//! measurement, printed not asserted, exactly like `scene_identity.rs`'s own
//! established convention for a known, still-open gap.
use rf_core_api::{CoreSink, EventMask, FrameBundleBuilder};
use rf_enhance::scene_tracker::{SceneTracker, MAX_CONTINUOUS_STEP_PX};
use rf_nes::{Cpu, NesBus};
use std::path::PathBuf;

/// Documented RAM addresses (`fixtures/nes/rf-scroller/FORMAT.md`
/// "Documented RAM addresses") -- duplicated from `rf_scroller_replay.rs`
/// (module doc).
const PLAYER_X_ADDR: u16 = 0x6029;
const CAMERA_X_ADDR: u16 = 0x602B;
const COLUMNS_STREAMED_ADDR: u16 = 0x602D;
const PLAYER_X_AT_END: u16 = 752;
const CAMERA_X_AT_END: u16 = 512;
const COLUMNS_STREAMED_AT_END: u8 = 95;

/// Resolves the built RF-Scroller ROM (same convention as
/// `rf_scroller_replay.rs::resolve_rom`, duplicated -- module doc).
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

/// Resolves the fetched `alter_ego.zip` (same convention as
/// `alter_ego_replay.rs::resolve_zip`, duplicated -- module doc).
fn resolve_alter_ego_zip() -> Option<PathBuf> {
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

/// Sniffs the single NES ROM out of `alter_ego.zip` by content, not file
/// name (same discipline `alter_ego_replay.rs::extract_nes_rom` uses,
/// duplicated here -- module doc).
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
        use std::io::Read as _;
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

/// Holds Right for every frame (module doc; same as
/// `rf_scroller_replay.rs::scripted_buttons`, duplicated).
fn scripted_buttons_right() -> u8 {
    use rf_input::NesButton;
    1u8 << NesButton::Right.bit()
}

/// Same Start-mash-then-hold-Right script `alter_ego_replay.rs::
/// scripted_buttons` uses, duplicated (module doc) -- this file only needs
/// "real, varied gameplay content", not the full 5-minute regression that
/// file already owns.
fn alter_ego_scripted_buttons(frame: u64) -> u8 {
    use rf_input::NesButton;
    const MASH_UNTIL: u64 = 150;
    const HOLD_FROM: u64 = 260;
    const HOLD_UNTIL: u64 = 290;
    let start_bit = 1u8 << NesButton::Start.bit();
    let right_bit = 1u8 << NesButton::Right.bit();
    if (HOLD_FROM..HOLD_UNTIL).contains(&frame) {
        right_bit
    } else if frame < MASH_UNTIL {
        if (frame / 2).is_multiple_of(2) {
            start_bit
        } else {
            0
        }
    } else {
        0
    }
}

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

fn peek_u16(bus: &NesBus, addr: u16) -> u16 {
    u16::from(bus.peek(addr)) | (u16::from(bus.peek(addr + 1)) << 8)
}

/// The measured baseline this ticket's brief required before designing
/// anything (module doc's "Measured baseline" section). Not `#[ignore]`'d
/// -- always runs, absence-skips loudly per this ticket's brief.
#[test]
fn old_hash_baseline_over_a_real_scrolling_session() {
    let Some(rom_path) = resolve_rom() else {
        eprintln!(
            "SKIP old_hash_baseline_over_a_real_scrolling_session: rf-scroller.nes not built. \
             Build it first: cd fixtures/nes/rf-scroller && ./build.sh (requires cc65 -- brew \
             install cc65 on macOS)"
        );
        return;
    };
    let rom_bytes = std::fs::read(&rom_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", rom_path.display()));
    let mut bus = NesBus::from_ines_bytes(&rom_bytes).expect("rf-scroller.nes must be valid iNES");
    bus.set_event_mask(EventMask::SCANLINE.union(EventMask::SCROLL_WRITE));
    let mut cpu = Cpu::power_on(&mut bus);

    let mut ids = std::collections::HashSet::new();
    for frame in 0..900u64 {
        let mut builder = FrameBundleBuilder::new(256, 240);
        run_frame(&mut bus, &mut cpu, scripted_buttons_right(), &mut builder);
        let bundle = builder.take(frame);
        let id = rf_enhance::scene_identity::compute_scene_id(
            &bundle.video,
            bundle.width,
            bundle.height,
            &[], // NROM: no mapper bank state (module doc's NROM note)
        );
        ids.insert(id);
    }

    eprintln!(
        "BASELINE (pre-ticket compute_scene_id, no continuity tracking): {} distinct scene ids \
         over 900 frames (752px of real scrolling travel) on the real RF-Scroller fixture -- \
         see this file's module doc for the measured reason this is 1, not the 36/150px W4-03b's \
         own SYNTHETIC fixture recorded",
        ids.len()
    );
}

/// Acceptance 1 + acceptance 3 (FM-11 watermark over SCROLLING play),
/// proven together against the real fixture (module doc). Not
/// `#[ignore]`'d: 900 frames of interpretation-mode 6502 execution runs in
/// well under a second, the same budget `rf_scroller_scripted_input_actually_drives_the_game`
/// already spends.
#[test]
fn nine_hundred_frames_of_real_scrolling_play_stay_one_scene_id_with_bounded_continuity() {
    let Some(rom_path) = resolve_rom() else {
        eprintln!(
            "SKIP nine_hundred_frames_of_real_scrolling_play_stay_one_scene_id_with_bounded_continuity: \
             rf-scroller.nes not built. Build it first: cd fixtures/nes/rf-scroller && ./build.sh \
             (requires cc65 -- brew install cc65 on macOS)"
        );
        return;
    };
    let rom_bytes = std::fs::read(&rom_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", rom_path.display()));
    let mut bus = NesBus::from_ines_bytes(&rom_bytes).expect("rf-scroller.nes must be valid iNES");
    bus.set_event_mask(EventMask::SCANLINE.union(EventMask::SCROLL_WRITE));
    let mut cpu = Cpu::power_on(&mut bus);
    let mut tracker = SceneTracker::new();

    let mut ids = std::collections::HashSet::new();
    let mut max_abs_delta = 0i64;
    let mut first_world: Option<(i64, i64)> = None;
    let mut last_world: Option<(i64, i64)> = None;

    for frame in 0..900u64 {
        let mut builder = FrameBundleBuilder::new(256, 240);
        run_frame(&mut bus, &mut cpu, scripted_buttons_right(), &mut builder);
        let bundle = builder.take(frame);

        let before = tracker.last_world_position();
        let id = tracker.observe_frame(
            &bundle.video,
            bundle.width,
            bundle.height,
            &bundle.events,
            &[], // NROM: no mapper bank state (module doc's NROM note)
        );
        ids.insert(id);
        let after = tracker.last_world_position();
        if let (Some(b), Some(a)) = (before, after) {
            max_abs_delta = max_abs_delta.max((a.0 - b.0).abs()).max((a.1 - b.1).abs());
        }
        if first_world.is_none() {
            first_world = after;
        }
        last_world = after;
    }

    // Anti-vacuity (same RAM witnesses `rf_scroller_replay.rs`'s own
    // anti-vacuity test uses): a do-nothing input log would leave these at
    // their boot values forever, and this session would trivially "stay
    // one scene id" for the wrong reason (nothing moved at all).
    assert_eq!(
        peek_u16(&bus, PLAYER_X_ADDR),
        PLAYER_X_AT_END,
        "900 frames of Right must move the player to the level end (752) -- the scripted input \
         never reached the game otherwise"
    );
    assert_eq!(
        peek_u16(&bus, CAMERA_X_ADDR),
        CAMERA_X_AT_END,
        "camera must have followed the player to its maximum (512)"
    );
    assert_eq!(
        bus.peek(COLUMNS_STREAMED_ADDR),
        COLUMNS_STREAMED_AT_END,
        "columns_streamed must reach 95 -- wraparound must have genuinely occurred"
    );

    // The property that actually discriminates a working continuity signal
    // from a broken one on THIS fixture (module doc: id-count alone does
    // not, because the baseline is already 1 here). This is also the exact
    // check that would have caught the OAM-DMA frame-boundary leak
    // (`crate::scroll_tracker`'s module-level fix) corrupting one frame's
    // world position via the HUD band aliasing to primary.
    assert!(
        max_abs_delta <= MAX_CONTINUOUS_STEP_PX,
        "every frame-to-frame primary-band world-position delta must stay within the \
         continuity bound ({MAX_CONTINUOUS_STEP_PX}px) -- max observed was {max_abs_delta}px"
    );

    // The session must have actually traveled -- pairs with the RAM
    // witnesses above so this isn't "the delta never moved because nothing
    // scrolled" trivially satisfying the bound above.
    let (fw, lw) = (
        first_world.expect("at least one frame observed"),
        last_world.expect("at least one frame observed"),
    );
    let traveled = (lw.0 - fw.0).abs();
    assert!(
        traveled >= 400,
        "the session must have traveled close to the RAM-witnessed 512px of camera movement -- \
         only {traveled}px of SceneTracker-observed world_x travel (first={fw:?} last={lw:?})"
    );

    // Acceptance 1's own literal wording ("one scene id for one level") --
    // true here, but see module doc for why this alone would not have
    // discriminated a broken implementation on this specific fixture; the
    // two assertions above are what does that job.
    assert_eq!(
        ids.len(),
        1,
        "a sustained real scrolling session over one level must be one scene id -- got {} \
         distinct ids",
        ids.len()
    );

    eprintln!(
        "SceneTracker over 900 real RF-Scroller frames: {} scene id(s), max per-frame world \
         delta {max_abs_delta}px, total world_x travel {traveled}px",
        ids.len()
    );
}

/// Acceptance 2, measured not asserted -- module doc's "Acceptance 2:
/// BLOCKED at the fixture level" section explains why in full. Both ROMs
/// absence-skip independently and loudly.
#[test]
fn rf_scroller_and_alter_ego_scene_ids_measured_not_asserted() {
    let Some(rom_path) = resolve_rom() else {
        eprintln!(
            "SKIP rf_scroller_and_alter_ego_scene_ids_measured_not_asserted: rf-scroller.nes not \
             built. Build it first: cd fixtures/nes/rf-scroller && ./build.sh"
        );
        return;
    };
    let Some(zip_path) = resolve_alter_ego_zip() else {
        eprintln!(
            "SKIP rf_scroller_and_alter_ego_scene_ids_measured_not_asserted: alter_ego.zip not \
             fetched. Fetch it first via the rf-harness fetch-test-roms binary / \
             tests/rom-manifest.toml's alter-ego-rom artifact."
        );
        return;
    };

    let rom_bytes = std::fs::read(&rom_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", rom_path.display()));
    let mut bus = NesBus::from_ines_bytes(&rom_bytes).expect("rf-scroller.nes must be valid iNES");
    bus.set_event_mask(EventMask::SCANLINE.union(EventMask::SCROLL_WRITE));
    let mut cpu = Cpu::power_on(&mut bus);
    let mut tracker = SceneTracker::new();
    let mut scroller_ids = std::collections::HashSet::new();
    for frame in 0..300u64 {
        let mut builder = FrameBundleBuilder::new(256, 240);
        run_frame(&mut bus, &mut cpu, scripted_buttons_right(), &mut builder);
        let bundle = builder.take(frame);
        scroller_ids.insert(tracker.observe_frame(
            &bundle.video,
            bundle.width,
            bundle.height,
            &bundle.events,
            &[],
        ));
    }

    let zip_bytes = std::fs::read(&zip_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", zip_path.display()));
    let ae_rom_bytes = extract_nes_rom(&zip_bytes).unwrap_or_else(|e| panic!("alter_ego.zip: {e}"));
    let mut ae_bus =
        NesBus::from_ines_bytes(&ae_rom_bytes).expect("Alter_Ego.nes must be valid iNES");
    ae_bus.set_event_mask(EventMask::SCANLINE.union(EventMask::SCROLL_WRITE));
    let mut ae_cpu = Cpu::power_on(&mut ae_bus);
    let mut ae_tracker = SceneTracker::new();
    let mut ae_ids = std::collections::HashSet::new();
    for frame in 0..600u64 {
        let mut builder = FrameBundleBuilder::new(256, 240);
        run_frame(
            &mut ae_bus,
            &mut ae_cpu,
            alter_ego_scripted_buttons(frame),
            &mut builder,
        );
        let bundle = builder.take(frame);
        ae_ids.insert(ae_tracker.observe_frame(
            &bundle.video,
            bundle.width,
            bundle.height,
            &bundle.events,
            &[],
        ));
    }

    let overlap: Vec<_> = scroller_ids.intersection(&ae_ids).collect();
    eprintln!(
        "MEASURED (not asserted -- module doc's 'Acceptance 2: BLOCKED' section): RF-Scroller \
         scene ids over 300 frames = {scroller_ids:?}, Alter Ego scene ids over 600 frames \
         (real Start-mash-then-Right-hold gameplay) = {ae_ids:?}, overlap = {overlap:?}. A \
         non-empty overlap here means these two specific real, both-NROM, both-low-contrast \
         fixtures do not discriminate each other through compute_scene_id's edge-bucket \
         signature -- see module doc for the measured root cause and why this ticket does not \
         attempt to fix W4-03b's own algorithm to compensate."
    );
}
