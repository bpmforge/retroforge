//! Ticket W5-03's scene construction, tested as a pure function of a
//! decoded level plus the handful of live bytes the profile names — no
//! renderer, no running core.
//!
//! ## The vacuity trap here is coordinate space
//!
//! Every assertion in a scene-graph test is tempting to write as "a
//! SpriteSet layer exists and has 12 entries", which passes against
//! sprites placed at entirely the wrong coordinates. A full-map overlay
//! with screen-space coordinates does not crash and does not look
//! broken — the sprites simply stick to the viewport while the level
//! scrolls beneath them. So the tests below pin **positions against a
//! moved camera**, which is the one thing that distinguishes world space
//! from screen space.

use std::path::{Path, PathBuf};

use rf_enhance::camera::CameraMode;
use rf_enhance::decode::metatile_screens::{self, DecodedLevel};
use rf_enhance::level_view::{self, LevelGeometry, LiveCamera, ORIGINAL_VIEWPORT};
use rf_enhance::scene_graph::{DrawCmd, LevelId, SceneLayer};
use rf_profiles::schema::Profile;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/rf-enhance is two levels under the repo root")
        .to_path_buf()
}

fn profile() -> Profile {
    let path = repo_root().join("profiles/nes/rf-scroller/profile.toml");
    rf_profiles::load_file(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .profile
}

/// Skip locally, fail on CI — the fixture is built by its own CI step.
fn level() -> Option<DecodedLevel> {
    let path = repo_root().join("fixtures/nes/rf-scroller/build/rf-scroller.nes");
    let Ok(raw) = std::fs::read(path) else {
        assert!(
            std::env::var_os("CI").is_none(),
            "fixture ROM missing on CI: its build step failed and this suite would pass having \
             asserted nothing"
        );
        eprintln!("SKIP: fixture ROM not built — run fixtures/nes/rf-scroller/build.sh");
        return None;
    };
    let spec = metatile_screens::spec_from_profile(&profile()).expect("profile declares decode");
    Some(metatile_screens::decode(&raw[16..], &spec).expect("decodes"))
}

/// A synthetic OAM shadow: 12 gems at slots 2-13, all sharing one Y,
/// spaced 8px apart — the shape `main.c`'s `update_gems()` writes.
fn oam_with_gems(y: u8) -> [u8; 256] {
    let mut oam = [0xFFu8; 256];
    for i in 0..12u8 {
        let slot = 2 + usize::from(i);
        oam[slot * 4] = y;
        oam[slot * 4 + 1] = 0x0E; // tile
        oam[slot * 4 + 2] = 0x00; // attributes
        oam[slot * 4 + 3] = 16 + i * 8; // screen X
    }
    oam
}

/// The profile's entity table starts at $0208 — slot 2 of the OAM shadow
/// at $0200. Slice accordingly, the way the shell will.
fn entity_table(oam: &[u8; 256]) -> &[u8] {
    &oam[8..]
}

#[test]
fn geometry_comes_from_the_metatile_size_not_an_assumed_16() {
    let Some(level) = level() else { return };
    let geo = LevelGeometry::from_level(&level, 8);
    assert_eq!(geo.metatile_px, 16, "a size-4 metatile is 2x2 tiles of 8px");
    assert_eq!(
        geo.width_px,
        48 * 16,
        "RF-Scroller is 48 metatile columns wide"
    );
    assert_eq!(geo.height_px, 14 * 16);
}

/// **The coordinate-space test.** Move the camera and the same OAM must
/// produce sprites that moved with it. A screen-space implementation
/// returns identical positions for both cameras and fails here — and
/// would pass any assertion about layer counts or sprite counts.
#[test]
fn sprites_are_lifted_into_world_space_by_the_live_camera() {
    let profile = profile();
    let oam = oam_with_gems(99);
    let table = entity_table(&oam);

    let at_origin = level_view::sprites_in_world(&profile, table, LiveCamera { x: 0, y: 0 });
    let scrolled = level_view::sprites_in_world(&profile, table, LiveCamera { x: 400, y: 0 });

    assert_eq!(at_origin.len(), 12, "all twelve gems are active");
    assert_eq!(scrolled.len(), 12);
    assert_eq!(at_origin[0].x, 16, "screen X 16 at camera 0 is world X 16");
    assert_eq!(
        scrolled[0].x, 416,
        "the SAME screen X at camera 400 must be world X 416 — equal values here would mean the \
         overlay sticks to the viewport while the level scrolls under it"
    );
    // Y is unscrolled in this fixture, and must not drift.
    assert_eq!(at_origin[0].y, 99);
    assert_eq!(scrolled[0].y, 99);
    // Spacing must survive the lift.
    assert_eq!(scrolled[1].x - scrolled[0].x, 8);
}

/// With `offscreen_valid = false` and no `active` field, parked sprites
/// (Y >= 0xEF) must be dropped — a full-map view that drew them would
/// stack a dozen enemies in one corner of the level.
#[test]
fn parked_sprites_are_dropped_when_the_profile_says_offscreen_is_not_valid() {
    let profile = profile();
    assert!(
        !profile.entities.as_ref().unwrap().offscreen_valid,
        "this test is about the offscreen_valid = false path"
    );
    let oam = oam_with_gems(0xF0); // parked
    let sprites = level_view::sprites_in_world(&profile, entity_table(&oam), LiveCamera::default());
    assert!(
        sprites.is_empty(),
        "sprites parked off-screen must not be composited, got {}",
        sprites.len()
    );

    // ...and the on-screen case must still produce them, or the test
    // above would pass against a function that returns nothing at all.
    let live = oam_with_gems(0x60);
    assert_eq!(
        level_view::sprites_in_world(&profile, entity_table(&live), LiveCamera::default()).len(),
        12
    );
}

/// `[camera].x` is a little-endian u16 in this profile; reading only the
/// low byte would work for the first 256 pixels of the level and then
/// silently wrap — which is exactly the kind of bug that survives a demo.
#[test]
fn the_live_camera_reads_a_full_u16_not_just_the_low_byte() {
    let profile = profile();
    // camera_x lives at $602B (W5-01's RAM map).
    let read = |addr: u32| -> u8 {
        match addr {
            0x602B => 0x2C, // low
            0x602C => 0x01, // high  -> 0x012C = 300
            0x602E => 24,   // camera_y
            _ => 0,
        }
    };
    let camera = level_view::live_camera(&profile, &read);
    assert_eq!(
        camera.x, 300,
        "a low-byte-only read would report 44 and wrap every 256 pixels"
    );
    assert_eq!(camera.y, 24);
}

/// Acceptance criteria 1-3 in one scene: a full-map camera over the
/// decoded level, sprites at world positions, and the original-viewport
/// outline.
#[test]
fn a_full_map_scene_carries_level_sprites_and_the_viewport_outline() {
    let Some(level) = level() else { return };
    let profile = profile();
    let geo = LevelGeometry::from_level(&level, 8);
    let oam = oam_with_gems(99);
    let camera = LiveCamera { x: 320, y: 0 };
    let sprites = level_view::sprites_in_world(&profile, entity_table(&oam), camera);

    let scene = level_view::full_map_scene(LevelId(1), geo, sprites, camera, 1920, 1080, 3);

    assert_eq!(scene.camera.mode, CameraMode::FullMap);
    // The full map frames the LEVEL, not the player — a full-map view
    // that followed the camera would defeat its own purpose.
    assert_eq!(
        scene.camera.center_world,
        (i64::from(geo.width_px / 2), i64::from(geo.height_px / 2))
    );

    assert!(matches!(scene.layers[0], SceneLayer::DecodedLevel { .. }));
    let SceneLayer::SpriteSet { sprites } = &scene.layers[1] else {
        panic!("layer 1 must be the sprite set, got {:?}", scene.layers[1]);
    };
    assert_eq!(sprites.len(), 12);
    assert_eq!(sprites[0].x, 336, "world X = camera 320 + screen 16");

    let SceneLayer::OverlayCmds { cmds } = &scene.layers[2] else {
        panic!("layer 2 must be the overlay, got {:?}", scene.layers[2]);
    };
    assert_eq!(
        cmds[0],
        DrawCmd::Rect {
            x: 320,
            y: 0,
            width: ORIGINAL_VIEWPORT.0,
            height: ORIGINAL_VIEWPORT.1,
            color_index: 3,
        },
        "the outline must sit at the LIVE camera and be exactly one console viewport"
    );
}

/// The ultrawide-over-level camera follows the player; the full-map one
/// does not. Asserted together so neither can be satisfied by a single
/// implementation that ignores its mode.
#[test]
fn ultrawide_follows_the_camera_while_full_map_frames_the_level() {
    let Some(level) = level() else { return };
    let geo = LevelGeometry::from_level(&level, 8);
    let near = LiveCamera { x: 0, y: 0 };
    let far = LiveCamera { x: 500, y: 0 };

    let uw_near = level_view::ultrawide_scene_over_level(LevelId(1), vec![], near, 1920, 1080, 3);
    let uw_far = level_view::ultrawide_scene_over_level(LevelId(1), vec![], far, 1920, 1080, 3);
    assert_ne!(
        uw_near.camera.center_world, uw_far.camera.center_world,
        "an ultrawide camera must track the player"
    );
    assert_eq!(uw_far.camera.mode, CameraMode::Ultrawide);

    let fm_near = level_view::full_map_scene(LevelId(1), geo, vec![], near, 1920, 1080, 3);
    let fm_far = level_view::full_map_scene(LevelId(1), geo, vec![], far, 1920, 1080, 3);
    assert_eq!(
        fm_near.camera.center_world, fm_far.camera.center_world,
        "a full-map camera must NOT track the player"
    );

    // ...but the outline must move in both, or the user cannot tell where
    // the console viewport is.
    let outline = |s: &rf_enhance::scene_graph::SceneGraph| match &s.layers[2] {
        SceneLayer::OverlayCmds { cmds } => cmds[0],
        other => panic!("expected overlay, got {other:?}"),
    };
    assert_ne!(outline(&fm_near), outline(&fm_far));
    assert_ne!(outline(&uw_near), outline(&uw_far));
}
