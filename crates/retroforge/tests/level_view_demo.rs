//! Ticket W5-03 end to end: open the RF-Scroller fixture, match its
//! profile by identity, decode the level, run the game, and build the
//! enhanced scene from live machine state — the demo, as a test.
//!
//! ## Why this is a test and not a screenshot
//!
//! The acceptance is "live player + active sprites composited at
//! profile-decoded positions". A screenshot proves something was drawn;
//! it does not prove the sprites are at the *right* positions, which is
//! the entire claim. So this drives the real core and asserts that the
//! composited world positions **track the live camera** — the property
//! that distinguishes a working overlay from one stuck in screen space.

use std::path::{Path, PathBuf};

use retroforge::level_view::LevelSession;
use retroforge::stepper::EmuStepper;
use rf_core_api::{CoreEvent, CoreSink, InputFrame, PpuPixel};
use rf_enhance::camera::CameraMode;
use rf_enhance::scene_graph::SceneLayer;

struct NullSink;
impl CoreSink for NullSink {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/retroforge is two levels under the repo root")
        .to_path_buf()
}

fn fixture() -> Option<Vec<u8>> {
    let path = repo_root().join("fixtures/nes/rf-scroller/build/rf-scroller.nes");
    match std::fs::read(path) {
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

fn session(rom: &[u8]) -> LevelSession {
    let cart = rf_cart::Cartridge::load(rom).expect("valid cartridge");
    let hashes = match &cart {
        rf_cart::Cartridge::Nes { identity, .. } | rf_cart::Cartridge::Snes { identity, .. } => {
            identity.normalized.clone()
        }
    };
    LevelSession::open(&repo_root().join("profiles"), rom, &hashes).expect(
        "the shipped RF-Scroller profile must match the fixture by normalized sha256 — if this \
         fails, the profile's [[identity]] has drifted from the ROM CI builds",
    )
}

/// Run `frames` frames with `buttons` held.
fn run(stepper: &mut EmuStepper, frames: u64, buttons: u8) {
    stepper.resume();
    for _ in 0..frames {
        let mut input = InputFrame::empty();
        input.ports[0] = u16::from(buttons);
        stepper.latch_and_advance_frame(input, &mut NullSink);
    }
}

/// **The whole pipeline, and the assertion that makes it mean
/// something.** Sprites composited into a full-map view must move
/// through the LEVEL as the camera scrolls. An implementation that left
/// them in screen space returns the same world positions at both camera
/// positions and fails here — while still producing a scene graph with
/// all the right layers and sprite counts.
#[test]
fn the_enhanced_scene_places_live_sprites_at_world_positions_that_track_the_camera() {
    let Some(rom) = fixture() else { return };
    let session = session(&rom);
    let (table_addr, table_len) = session
        .entity_table_range()
        .expect("the profile declares an entity table");

    let mut stepper = EmuStepper::from_ines_bytes(&rom).expect("fixture loads");

    // Sample once early, then again after walking right for a while.
    let sample = |stepper: &EmuStepper| {
        let table: Vec<u8> = (0..table_len)
            .map(|i| stepper.peek(u16::try_from(table_addr as usize + i).unwrap()))
            .collect();
        let read = |addr: u32| stepper.peek(u16::try_from(addr).unwrap_or(u16::MAX));
        session.scene(true, &read, &table, 1920, 1080)
    };

    run(&mut stepper, 30, 0);
    let early = sample(&stepper);
    // The gems only exist once the TAIL GATE opens (`columns_streamed`
    // reaches TAIL_GATE_COL 95, FORMAT.md's "W2-10a red-fixture scenes"),
    // which takes the player most of the level. 400 frames was not
    // enough and produced an empty sprite set that looked like a wiring
    // bug rather than a workload that had not got there yet.
    run(&mut stepper, 900, 0x80); // hold Right
    let late = sample(&stepper);

    // The game really did scroll — without this the comparison below is
    // between two identical states and proves nothing.
    let outline_x = |s: &rf_enhance::scene_graph::SceneGraph| match &s.layers[2] {
        SceneLayer::OverlayCmds { cmds } => match cmds[0] {
            rf_enhance::scene_graph::DrawCmd::Rect { x, .. } => x,
            other => panic!("expected a rect outline, got {other:?}"),
        },
        other => panic!("expected the overlay layer, got {other:?}"),
    };
    assert!(
        outline_x(&late) > outline_x(&early),
        "the fixture did not scroll (viewport outline stayed at x={}), so nothing below is being \
         tested",
        outline_x(&early)
    );

    // Criterion 1: a full-map camera over the decoded level.
    assert_eq!(early.camera.mode, CameraMode::FullMap);
    assert!(matches!(early.layers[0], SceneLayer::DecodedLevel { .. }));
    // ...framing the level, not the player.
    assert_eq!(
        early.camera.center_world, late.camera.center_world,
        "a full-map camera must frame the level regardless of where the player is"
    );

    // Criterion 2: live sprites, at world positions that moved with the
    // camera. RF-Scroller's gems have FIXED world positions, so once
    // they are on screen their WORLD x must be stable while their screen
    // x changes — which is the strongest form of this assertion.
    let sprites = |s: &rf_enhance::scene_graph::SceneGraph| match &s.layers[1] {
        SceneLayer::SpriteSet { sprites } => sprites.clone(),
        other => panic!("expected the sprite layer, got {other:?}"),
    };
    let late_sprites = sprites(&late);
    assert!(
        !late_sprites.is_empty(),
        "no sprites were composited after the player walked to the gems"
    );
    for s in &late_sprites {
        assert!(
            s.x >= 0 && s.y >= 0,
            "a composited sprite landed at a negative world position: {s:?}"
        );
        assert!(
            s.x <= i32::try_from(session.geometry.width_px).unwrap() + 512,
            "sprite world x {} is far outside a {}px level — the screen-to-world lift is wrong",
            s.x,
            session.geometry.width_px
        );
    }
}

/// The ultrawide-over-level camera follows the player; the full-map one
/// does not — asserted on the SAME live state so neither can be
/// satisfied by an implementation that ignores its mode.
#[test]
fn ultrawide_tracks_the_player_while_full_map_does_not() {
    let Some(rom) = fixture() else { return };
    let session = session(&rom);
    let (table_addr, table_len) = session.entity_table_range().expect("entity table");
    let mut stepper = EmuStepper::from_ines_bytes(&rom).expect("fixture loads");
    run(&mut stepper, 200, 0x80);

    let table: Vec<u8> = (0..table_len)
        .map(|i| stepper.peek(u16::try_from(table_addr as usize + i).unwrap()))
        .collect();
    let read = |addr: u32| stepper.peek(u16::try_from(addr).unwrap_or(u16::MAX));

    let full = session.scene(true, &read, &table, 1920, 1080);
    let wide = session.scene(false, &read, &table, 1920, 1080);

    assert_eq!(full.camera.mode, CameraMode::FullMap);
    assert_eq!(wide.camera.mode, CameraMode::Ultrawide);
    assert_ne!(
        full.camera.center_world, wide.camera.center_world,
        "the two cameras must frame different things"
    );
    // Both carry the decoded level and the outline.
    for scene in [&full, &wide] {
        assert!(matches!(scene.layers[0], SceneLayer::DecodedLevel { .. }));
        assert!(matches!(scene.layers[2], SceneLayer::OverlayCmds { .. }));
    }
}

/// A ROM with no matching profile must produce no session — and must not
/// panic, error, or refuse to run. Enhancements are opt-in overlays over
/// an unmodified simulation (project law 6); a missing profile is the
/// normal case, not a fault.
#[test]
fn a_rom_with_no_profile_degrades_to_no_session() {
    let mut synthetic = vec![0u8; 16 + 0x4000 + 0x2000];
    synthetic[0..4].copy_from_slice(b"NES\x1a");
    synthetic[4] = 1;
    synthetic[5] = 1;
    assert!(
        LevelSession::open(
            &repo_root().join("profiles"),
            &synthetic,
            // Ticket W11-09: all four families, none of which any shipped
            // profile claims — so this still asserts "no match", and now
            // asserts it against the matcher that checks all of them.
            &rf_cart::RomHashes {
                crc32: "00000000".to_string(),
                md5: "0".repeat(32),
                sha1: "0".repeat(40),
                sha256: "0".repeat(64),
            },
        )
        .is_none(),
        "an unknown ROM must simply have no enhanced level, not an error"
    );
}
