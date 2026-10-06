//! Ticket W15-03: the per-game context menu and the one Game Settings
//! window (`docs/design/UX_WAVE_15.md` §3, §5, §11).
//!
//! **One test function, on purpose.** Same reasoning as
//! `library_selection.rs`/`library_filters.rs`: `RETROFORGE_CONFIG_DIR` is
//! process-global and building the app reads *and writes* it, so a second
//! test in this binary would race the first.

use std::path::{Path, PathBuf};

use eframe::egui;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};

fn write_fixture_rom(dir: &Path, name: &str) -> PathBuf {
    let mut data = vec![0u8; 16 + 0x4000 + 0x2000];
    data[0..4].copy_from_slice(b"NES\x1a");
    data[4] = 1;
    data[5] = 1;
    let prg = &mut data[16..16 + 0x4000];
    let code: &[u8] = &[
        0xA9, 0x1E, 0x8D, 0x01, 0x20, // LDA #$1E ; STA $2001 (rendering on)
        0xEE, 0x00, 0x00, // INC $0000
        0x4C, 0x05, 0x80, // JMP $8005
    ];
    prg[..code.len()].copy_from_slice(code);
    prg[0x3FFC] = 0x00;
    prg[0x3FFD] = 0x80;
    let path = dir.join(name);
    std::fs::write(&path, &data).expect("write fixture rom");
    path
}

/// The normalized sha256 for `write_fixture_rom`'s output: the file has no
/// per-fixture seed byte (unlike `library_filters.rs`'s fixtures), so this
/// is stable across the whole test, computed the same way `library.rs`'s
/// own scan does — by hashing the header-stripped image.
fn normalized_hash_of_fixture(path: &Path) -> String {
    let bytes = std::fs::read(path).expect("read fixture rom");
    match rf_cart::Cartridge::load(&bytes).expect("fixture parses as a cartridge") {
        rf_cart::Cartridge::Nes { identity, .. } | rf_cart::Cartridge::Snes { identity, .. } => {
            identity.normalized.sha256
        }
    }
}

fn app() -> Harness<'static, RetroForgeApp> {
    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    harness.run();
    harness
}

/// Send a real right-click at `pos` as one batch of raw events, same
/// reasoning as `library_selection.rs`'s `double_click` helper: pushing
/// press+release into `RawInput` directly and running exactly one step is
/// what a real click looks like to egui, rather than three separately
/// time-stepped calls through `Node::click`.
fn right_click(harness: &mut Harness<'_, RetroForgeApp>, pos: egui::Pos2) {
    let input = harness.input_mut();
    input.events.push(egui::Event::PointerMoved(pos));
    input.events.push(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Secondary,
        pressed: true,
        modifiers: egui::Modifiers::default(),
    });
    input.events.push(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Secondary,
        pressed: false,
        modifiers: egui::Modifiers::default(),
    });
    harness.step();
}

#[test]
fn context_menu_and_one_game_settings_window() {
    let dir = std::env::temp_dir().join(format!(
        "retroforge_context_menu_game_settings_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    // SAFETY: first statement of the only test in this binary, before any
    // app, core thread or harness exists — no concurrent reader.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }

    let games = dir.join("games");
    std::fs::create_dir_all(&games).expect("create games root");
    let rom_path = write_fixture_rom(&games, "Alpha Quest.nes");
    let hash = normalized_hash_of_fixture(&rom_path);

    let mut harness = app();
    harness
        .state_mut()
        .set_library_roots_for_test(vec![games.clone()]);
    harness.run_steps(2);

    // ---- acceptance 1: right-click opens `Response::context_menu`, and
    // "Favourite" from it toggles the star -----------------------------
    let title_pos = harness
        .query_by_label("Alpha Quest")
        .expect("Alpha Quest row")
        .rect()
        .center();
    assert!(
        harness.query_by_label("Favourite").is_some(),
        "the unfavourited star (accessible name \"Favourite\") must be showing before the right-click"
    );
    right_click(&mut harness, title_pos);
    harness.run_steps(2);
    // Two nodes are named "Favourite" now (ticket W20-05 gave the row's
    // icon-only star button a real accessible name): the row's star and
    // the context menu's item. The menu opened last, so its node is last.
    harness
        .query_all_by_label("Favourite")
        .last()
        .expect("the context menu's Favourite item")
        .click();
    harness.run_steps(2);
    assert!(
        harness.query_by_label("Unfavourite").is_some(),
        "the row's star must flip to favourited (accessible name \"Unfavourite\") after the \
         context menu's Favourite item"
    );

    // ---- acceptance 3/4: "Game settings…" from the Enhance menu opens
    // the one window, targeting the RUNNING game (`None`) ----------------
    assert!(!harness.state_mut().show_game_settings_for_test());
    harness
        .query_by_label("Enhance")
        .expect("Enhance menu")
        .click();
    harness.run_steps(2);
    harness
        .query_by_label("Game settings\u{2026}")
        .expect("Enhance menu's Game settings… command")
        .click();
    harness.run_steps(2);
    assert!(
        harness.state_mut().show_game_settings_for_test(),
        "the Enhance menu's Game settings… must open the window"
    );
    assert_eq!(
        harness.state_mut().game_settings_target_hash_for_test(),
        None,
        "opened with no ROM running, so the target is \"the running game\", i.e. None"
    );
    harness.state_mut().close_game_settings_for_test();
    harness.run_steps(2);

    // ---- the SAME window, opened from the overlay menu instead ----------
    harness.key_press(egui::Key::Escape);
    harness.run_steps(2);
    // Ticket W20-10: the overlay menu is the Quick Menu now; Game
    // settings… lives in its Enhancements section.
    harness
        .query_by_label_contains("Enhancements")
        .expect("the Quick Menu's Enhancements rail entry")
        .click();
    harness.run_steps(2);
    harness
        .query_by_label("Mode and settings\u{2026}")
        .expect("the Quick Menu's Mode and settings… command (formerly \"Mode…\", then \"Game settings…\")")
        .click();
    harness.run_steps(2);
    assert!(
        harness.state_mut().show_game_settings_for_test(),
        "the overlay menu's Game settings… must open the SAME window field"
    );
    assert_eq!(
        harness.state_mut().game_settings_target_hash_for_test(),
        None,
        "the overlay menu also targets \"the running game\" — same field, same meaning"
    );
    harness.state_mut().close_game_settings_for_test();
    // Ticket W20-10: leaving the Quick Menu for a window already closed
    // it (the game stays paused), so there is no menu left to dismiss —
    // an Esc here would OPEN it again.
    assert!(!harness.state().running_and_menu_for_test().1);
    harness.run_steps(2);

    // ---- acceptance 3: "Game settings" from the library's context menu
    // targets THAT library entry (by hash), and a Mode change there
    // persists to its per-game settings file --------------------------
    right_click(&mut harness, title_pos);
    harness.run_steps(2);
    harness
        .query_by_label("Game settings")
        .expect("the context menu's Game settings item")
        .click();
    harness.run_steps(2);
    assert!(harness.state_mut().show_game_settings_for_test());
    assert_eq!(
        harness.state_mut().game_settings_target_hash_for_test(),
        Some(hash.clone()),
        "the context menu's Game settings must target the ROW that was right-clicked"
    );

    // A `ComboBox` exposes its current selection through AccessKit's
    // `value` (`WidgetInfo::current_text_value`), not its `label` — the
    // combo box itself carries no separate accessible name here (no
    // `ComboBox::new` label argument is used), so `query_by_value` is the
    // correct query, not `query_by_label`.
    harness
        .query_by_value("Accuracy")
        .expect("the Mode combo box, showing the default preset")
        .click();
    harness.run_steps(2);
    harness
        .query_by_label("Enhanced")
        .expect("the Enhanced option in the open Mode combo box")
        .click();
    harness.run_steps(2);

    let persisted = retroforge::game_settings::load(&dir, &hash);
    assert_eq!(
        persisted.mode,
        retroforge::game_settings::Mode::Enhanced,
        "changing Mode in the Game Settings window must persist immediately, no Apply button"
    );
}
