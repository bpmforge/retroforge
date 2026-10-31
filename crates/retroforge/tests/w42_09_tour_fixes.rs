//! **W42-09: tour fixes.** The Controls diagram fits the Settings sheet and speaks player words
//! ("Left", "L"), not config names ("DpadLeft", "LeftShoulder").
//!
//! One test function: `RETROFORGE_CONFIG_DIR` is process-global.

use eframe::egui;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use retroforge::app::RetroForgeApp;

#[test]
fn controls_diagram_fits_and_uses_player_words() {
    let dir = std::env::temp_dir().join(format!("retroforge_w42_09_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let screen = egui::vec2(1100.0, 900.0);
    let mut h = Harness::builder()
        .with_size(screen)
        .build_eframe(|cc| RetroForgeApp::new(cc));
    h.run();
    h.state_mut().open_controls_settings();
    h.run_steps(3);
    assert!(h.state().settings_on_controls_for_test());
    // SNES controller: the diagram with the most labels.
    h.state_mut().set_controls_for_test(true, true);
    h.run_steps(3);

    for bad in [
        "DpadUp",
        "DpadLeft",
        "DpadRight",
        "LeftShoulder",
        "RightShoulder",
    ] {
        assert!(h.query_by_label_contains(bad).is_none(), "{bad} shown");
    }
    let names = [
        "Up", "Down", "Left", "Right", "L", "R", "Select", "Start", "A", "B", "X", "Y",
    ];
    let mut union: Option<egui::Rect> = None;
    let mut rects = Vec::new();
    for n in names {
        let needle = format!("{n}: ");
        let node = h
            .query_all_by_label_contains(&needle)
            .next()
            .unwrap_or_else(|| panic!("no control button {n}"));
        let r = node.rect();
        union = Some(union.map_or(r, |u| u.union(r)));
        rects.push((n, r));
    }
    let union = union.expect("buttons");
    let sheet = h.get_by_label("Restore defaults").rect();
    assert!(
        union.width() <= 440.0,
        "diagram {:?} wider than the sheet",
        union
    );
    assert!(union.left() >= 0.0 && union.right() <= screen.x);
    // The pad words are what the player sees for the D-pad and shoulders.
    for spoken in [
        "Left: Left",
        "Right: Right",
        "Up: Up",
        "Down: Down",
        "L: L",
        "R: R",
    ] {
        assert!(h.query_by_label(spoken).is_some(), "missing {spoken}");
    }
    // No two controls overlap (the old DpadRight label sat on SELECT).
    for (i, (a, ra)) in rects.iter().enumerate() {
        for (b, rb) in &rects[i + 1..] {
            assert!(!ra.intersects(*rb), "{a} overlaps {b}");
        }
    }
    // The diagram sits inside the sheet's horizontal band: Restore defaults is in the same sheet.
    assert!(union.left() >= sheet.left() - 60.0 && union.right() <= sheet.right() + 460.0);
}
