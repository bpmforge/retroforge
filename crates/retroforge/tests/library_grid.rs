//! Ticket W15-05: the card grid (`docs/design/UX_WAVE_15.md` §3, §11).
//!
//! **One test function, on purpose.** `RETROFORGE_CONFIG_DIR` is
//! process-global and building the app reads *and writes* the config
//! root, so a second test in this binary would race the first —
//! `library_selection.rs`/`library_home.rs`/`library_filters.rs` all give
//! the same reasoning.
//!
//! Two fixture ROMs, neither with a save state or a first-frame capture
//! yet, so both must show as **placeholder** cards (acceptance 4) — this
//! test never touches `crate::thumbnail`'s decode path, only that a card
//! with nothing to show never claims otherwise and still carries the
//! title as its accessible label, which is what lets it be found and
//! driven by `query_by_label` exactly like a list row.

use std::path::{Path, PathBuf};

use eframe::egui;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::library::LibraryView;

fn write_fixture_rom(dir: &Path, name: &str, seed: u8) -> PathBuf {
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
    // Far from the code and the reset vector: only here to make two
    // fixtures hash differently (same trick `library_filters.rs` uses).
    prg[0x1000] = seed;
    prg[0x3FFC] = 0x00;
    prg[0x3FFD] = 0x80;
    let path = dir.join(name);
    std::fs::write(&path, &data).expect("write fixture rom");
    path
}

fn app() -> Harness<'static, RetroForgeApp> {
    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    harness.run();
    harness
}

#[test]
fn grid_toggle_shows_placeholder_cards_and_arrow_plus_enter_launches() {
    let dir = std::env::temp_dir().join(format!("retroforge_library_grid_{}", std::process::id()));
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
    write_fixture_rom(&games, "Alpha Quest.nes", 1);
    write_fixture_rom(&games, "Beta Racer.nes", 2);

    let mut harness = app();
    harness
        .state_mut()
        .set_library_roots_for_test(vec![games.clone()]);
    harness.run_steps(2);

    // ---- List is the default; both titles are reachable there --------
    assert!(
        harness.query_by_label("Alpha Quest").is_some(),
        "the list view (the default) must show both fixtures before the toggle"
    );

    // ---- toggling to Grid keeps both titles visible as cards ----------
    harness
        .state_mut()
        .set_library_view_for_test(LibraryView::Grid);
    harness.run_steps(2);
    assert!(
        harness.query_by_label("Alpha Quest").is_some(),
        "Grid view must still show Alpha Quest, now as a card"
    );
    assert!(
        harness.query_by_label("Beta Racer").is_some(),
        "Grid view must still show Beta Racer, now as a card"
    );

    // ---- selecting with a click, then Enter, launches -----------------
    harness
        .query_by_label("Beta Racer")
        .expect("Beta Racer card")
        .click();
    harness.run_steps(2);
    assert!(
        harness.query_by_label("Beta Racer").is_some(),
        "a single click on a card must select, not launch"
    );
    harness.key_press(egui::Key::Enter);
    harness.run_steps(3);
    assert!(
        harness.query_by_label("Beta Racer").is_none(),
        "Enter must launch the card that was just selected by clicking it"
    );
    harness.state_mut().close_rom_for_test();
    harness.run_steps(3);

    // ---- arrow keys move the card selection ---------------------------
    // From no selection, ArrowRight lands on the first (alphabetical)
    // filtered entry, Alpha Quest — the same "no selection -> first
    // entry" contract `library_selection.rs` proves for ArrowDown in the
    // list view, extended to Grid's ArrowRight (§11 acceptance 2).
    harness.state_mut().set_library_selected_for_test(None);
    harness.run_steps(2);
    assert!(
        harness.query_by_label("Alpha Quest").is_some(),
        "Alpha Quest card must exist"
    );
    harness.key_press(egui::Key::ArrowRight);
    harness.run_steps(2);
    harness.key_press(egui::Key::Enter);
    harness.run_steps(3);
    assert!(
        harness.query_by_label("Alpha Quest").is_none(),
        "ArrowRight then Enter must launch the first filtered card"
    );
    assert!(
        harness.query_by_label("Beta Racer").is_none(),
        "ArrowRight from no selection must land on Alpha Quest, not Beta Racer"
    );
}
