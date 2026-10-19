//! Ticket W30-06 part B: the HD-2D preset's gating and settings, and the
//! guarantee that painting a height table never reaches Accuracy mode.
//! Ticket W34-04 (D-023): the preset needs Enhanced (or Game-Aware) and
//! nothing else -- no profile, no painted table -- because the heights are
//! learned from play; it no longer tells the player to paint one.
//!
//! Same shape as `diorama_live_heights.rs`: the app is driven through its
//! test hooks (`set_live_heights_for_test` gives it a table the way a matched
//! profile would). No ROM is opened, so no emulator runs.

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::game_settings::Mode;
use retroforge::live_heights::HeightTable;

#[test]
fn the_hd2d_preset_is_gated_on_a_height_table_sets_the_look_and_restores_it() {
    let dir = std::env::temp_dir().join(format!("retroforge_hd2d_preset_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch config dir");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();

    // Accuracy: unavailable, and a click changes nothing (law 6).
    let before = harness.state().game_settings_for_test().clone();
    assert!(harness.state().hd2d_unavailable_for_test().is_some());
    assert!(!harness.state_mut().toggle_hd2d_for_test());
    assert_eq!(harness.state().game_settings_for_test(), &before);

    // Accuracy says which mode to use, and no longer asks for a painted table.
    let why = harness.state().hd2d_unavailable_for_test().unwrap();
    assert_eq!(why, "Needs Enhanced mode.");

    // Enhanced with no table and no decoded level: available (D-023).
    harness.state_mut().set_mode_for_test(Mode::Enhanced);
    harness.run_steps(1);
    assert_eq!(harness.state().hd2d_unavailable_for_test(), None);

    // Painting even Accuracy mode's table must not make anything render.
    harness
        .state_mut()
        .set_live_heights_for_test(HeightTable::new(1, 8, [(0x0A4, 16)].into_iter().collect()));
    harness.state_mut().set_mode_for_test(Mode::Accuracy);
    harness.run_steps(1);
    assert!(!harness.state().diorama_wanted_for_test());
    assert_eq!(harness.state().diorama_rgba_hash_for_test(), None);
    assert!(!harness.state_mut().toggle_hd2d_for_test());

    // Game-Aware plus a table: available. Give the player their own look first.
    harness.state_mut().set_mode_for_test(Mode::GameAware);
    harness.state_mut().edit_game_settings_for_test(|s| {
        s.lens_bloom = true;
        s.lens_bloom_pct = 90;
        s.lens_grade = true;
        s.diorama_pitch = 70;
        s.diorama_zoom = 300;
    });
    harness.run_steps(1);
    assert_eq!(harness.state().hd2d_unavailable_for_test(), None);
    let mine = harness.state().game_settings_for_test().clone();

    assert!(harness.state_mut().toggle_hd2d_for_test());
    harness.run_steps(1);
    let s = harness.state().game_settings_for_test();
    assert!(retroforge::hd2d_preset::is_applied(s));
    assert_eq!(
        (s.lens_bloom_pct, s.lens_tiltshift_pct, s.lens_shadows_pct),
        (35, 50, 50)
    );
    assert_eq!(
        (s.diorama_pitch, s.diorama_zoom, s.diorama_follow),
        (40, 160, true)
    );
    assert!(s.diorama && !s.lens_grade);
    assert!(harness.state().diorama_wanted_for_test(), "Diorama is on");

    // Off again: the player's own values are back, Diorama included.
    assert!(harness.state_mut().toggle_hd2d_for_test());
    harness.run_steps(1);
    assert_eq!(harness.state().game_settings_for_test(), &mine);
    assert!(!harness.state().diorama_wanted_for_test());
}
