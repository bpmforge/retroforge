//! **Diorama tier two is reachable by a user** (ticket W16-06;
//! `docs/design/ENHANCEMENT_WAVE_16.md` §5).
//!
//! Drives `RetroForgeApp` with a tiny synthetic NROM + a matching
//! `metatile_screens` profile declaring `[decode.collision]` — built in
//! this test rather than borrowed from `profiles/**` (outside this
//! ticket's `write_scope`) or from a real fixture ROM (no shipped
//! fixture/commercial profile pairs a real identity match with a
//! collision table today, per W16-05's own closing note). What this
//! proves:
//!
//! - a profile-matched, collision-declaring ROM produces a decoded level
//!   with `LevelSession::has_collision() == true`;
//! - switching to Game-Aware mode and turning Diorama on makes the row
//!   `Available` and the status badge name it ("Diorama: walls") —
//!   before that (Accuracy, or Diorama off) the badge must NOT say so
//!   (FR-MODE-003's "never on silently", checked both ways).

use std::path::PathBuf;

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::game_settings::Mode;

const PRG_LEN: usize = 0x4000;
const CHR_LEN: usize = 0x2000;

/// A minimal one-bank NROM with a `metatile_screens` level (1x1 grid, one
/// metatile) whose single tile is declared solid via `[decode.collision]`
/// — just enough bytes for `rf_enhance::decode::metatile_screens::decode`
/// to succeed and `LevelSession::has_collision()` to read `true`. Layout
/// (offsets into the NORMALIZED image, i.e. right after the 16-byte iNES
/// header):
///   0x0000..0x0004  metatile table, one metatile, 4 tile ids (unused content)
///   0x0004          level_column_offset[0] = 0 (column 0 starts at data[0])
///   0x0005..0x0007  level_rle_data: (run=1, id=0) -- one row, metatile 0
///   0x0007          collision_table[0] = 0b1 (bit0 "solid", set)
fn synthetic_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 16 + PRG_LEN + CHR_LEN];
    rom[0..4].copy_from_slice(b"NES\x1a");
    rom[4] = 1; // 1 PRG bank (16KB)
    rom[5] = 1; // 1 CHR bank (8KB)
    let prg = &mut rom[16..16 + PRG_LEN];
    // metatile table (0x0000..0x0004) left at zero -- content is
    // unused by this test (no pixels are rendered/asserted).
    prg[0x0004] = 0x00; // level_column_offset[0]
    prg[0x0005] = 0x01; // run = 1
    prg[0x0006] = 0x00; // metatile id = 0
    prg[0x0007] = 0b1; // collision_table[0], bit0 "solid" set
    rom
}

fn profile_toml(sha256: &str) -> String {
    format!(
        r#"
[meta]
profile_version = "0.1"
title = "Diorama kittest fixture"
console = "nes"
region = "ntsc"
sources = ["synthetic, authored for ticket W16-06's kittest -- not a real game"]

[[identity]]
sha256 = "{sha256}"

[decode]
kind = "metatile_screens"

[decode.metatile]
table = 0
size = 4

[decode.screens]
width = 1
height = 1
order = "column_rle"

[decode.collision]
table = 7
bits = "solid"

[[rom_map]]
offset = 4
len = 1
label = "level_column_offset"
type = "table"
source = "synthetic test fixture (this test's own header comment)"

[[rom_map]]
offset = 5
len = 2
label = "level_rle_data"
type = "table"
source = "synthetic test fixture (this test's own header comment)"
"#
    )
}

fn run_frames(h: &mut Harness<'_, RetroForgeApp>, target: u64, deadline: std::time::Duration) {
    let start = std::time::Instant::now();
    while h.state().frame_count_for_test() < target && start.elapsed() < deadline {
        h.run_steps(1);
        std::thread::sleep(std::time::Duration::from_millis(4));
    }
}

#[test]
fn switching_to_game_aware_and_enabling_diorama_names_it_on_the_badge() {
    let rom = synthetic_rom();
    let hashes = match rf_cart::Cartridge::load(&rom).expect("synthetic NROM must be valid") {
        rf_cart::Cartridge::Nes { identity, .. } | rf_cart::Cartridge::Snes { identity, .. } => {
            identity.normalized
        }
    };

    let dir = std::env::temp_dir().join(format!("retroforge_diorama_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let profiles_dir = dir.join("profiles").join("test-diorama");
    std::fs::create_dir_all(&profiles_dir).expect("scratch profiles dir");
    std::fs::write(
        profiles_dir.join("profile.toml"),
        profile_toml(&hashes.sha256),
    )
    .expect("write synthetic profile");

    // SAFETY: first statements of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
        std::env::set_var("RETROFORGE_PROFILES_DIR", dir.join("profiles"));
    }

    let rom_path: PathBuf = dir.join("fixture.nes");
    std::fs::write(&rom_path, &rom).expect("write synthetic ROM");

    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    harness.state_mut().open_rom_path(&rom_path);
    harness.run_steps(2);

    assert!(
        harness.state().has_decoded_level_for_test(),
        "the synthetic profile must match the synthetic ROM's normalized sha256 and decode its \
         1x1 level"
    );

    // Fresh install / Accuracy: the badge must not mention Diorama even
    // though the profile matched and Diorama is nominally off (law 6).
    let accuracy_badge = harness.state().badge_text_for_test();
    assert!(
        !accuracy_badge.contains("Diorama"),
        "Accuracy must never mention an enhancement: {accuracy_badge}"
    );

    // Game-Aware, but Diorama still off: available (a profile with
    // collision matched) but not effective, so still not named.
    harness.state_mut().set_mode_for_test(Mode::GameAware);
    run_frames(&mut harness, 3, std::time::Duration::from_secs(5));
    let off_breakdown = harness.state().badge_breakdown_for_test();
    assert!(
        !off_breakdown.iter().any(|l| l.contains("Diorama: walls")),
        "Diorama is off; the breakdown must not claim it is active: {off_breakdown:?}"
    );

    // Game-Aware AND Diorama on: NOW the badge must name it.
    harness.state_mut().set_diorama_for_test(true);
    run_frames(&mut harness, 6, std::time::Duration::from_secs(5));

    let badge = harness.state().badge_text_for_test();
    assert!(
        badge.contains("Game-Aware"),
        "badge must name the active mode: {badge}"
    );
    let breakdown = harness.state().badge_breakdown_for_test();
    assert!(
        breakdown.iter().any(|l| l.contains("Diorama: walls")),
        "with a matching profile (collision declared), Game-Aware mode, and the toggle on, the \
         badge breakdown must name 'Diorama: walls': {breakdown:?}"
    );

    // And turning it back off removes the claim -- never on silently.
    harness.state_mut().set_diorama_for_test(false);
    run_frames(&mut harness, 3, std::time::Duration::from_secs(5));
    let after_off = harness.state().badge_breakdown_for_test();
    assert!(
        !after_off.iter().any(|l| l.contains("Diorama: walls")),
        "turning Diorama back off must stop the badge from naming it: {after_off:?}"
    );
}
