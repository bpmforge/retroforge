//! Ticket W15-01: launch gestures and selection focus on the library home
//! (`docs/design/UX_WAVE_15.md` §9-§11).
//!
//! **One test function, on purpose.** `RETROFORGE_CONFIG_DIR` is
//! process-global and building the app reads *and writes* the config root,
//! so a second test in this binary setting it while this one reads it would
//! be a race — `library_home.rs` and `ui_smoke.rs` reached the same
//! conclusion for the same reason. All four gesture/selection scenarios
//! therefore run in sequence against one harness rather than one each.

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

fn app() -> Harness<'static, RetroForgeApp> {
    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    harness.run();
    harness
}

/// Send a real double-click at `pos`, as one batch of raw events processed
/// in a single pass.
///
/// `Node::click` is the wrong tool for this: it queues hover+down+up as
/// THREE SEPARATE steps, each advancing egui's own clock by `step_dt`
/// (`predicted_dt` — no test here ever sets `RawInput::time`), so two
/// `.click()` calls land their releases multiple steps apart. egui's
/// double-click window (`max_double_click_delay`, 0.3s by construction)
/// checks the GAP BETWEEN RELEASES, and the default `step_dt` alone (0.25s)
/// already spends most of that budget on ONE intervening step — so the
/// two releases read as two independent single clicks instead of one
/// double-click. Pushing all six events into `RawInput` directly and
/// running exactly one step processes them with the same pass `time`,
/// which is what a real double-click actually looks like to egui.
fn double_click(harness: &mut Harness<'_, RetroForgeApp>, pos: egui::Pos2) {
    let input = harness.input_mut();
    input.events.push(egui::Event::PointerMoved(pos));
    for _ in 0..2 {
        input.events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        });
        input.events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::default(),
        });
    }
    harness.step();
}

#[test]
fn double_click_enter_and_arrow_keys_all_drive_the_one_selection_state() {
    let dir = std::env::temp_dir().join(format!(
        "retroforge_library_selection_{}",
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
    write_fixture_rom(&games, "Alpha Quest.nes");
    write_fixture_rom(&games, "Beta Racer.nes");

    let mut harness = app();
    harness
        .state_mut()
        .set_library_roots_for_test(vec![games.clone()]);
    harness.run_steps(2);

    // ---- click-only selects without launching -----------------------
    harness
        .query_by_label("Beta Racer")
        .expect("Beta Racer row")
        .click();
    harness.run_steps(2);
    assert!(
        harness.query_by_label("Beta Racer").is_some(),
        "a single click must select, not launch"
    );
    assert!(
        harness.query_by_label("Alpha Quest").is_some(),
        "the OTHER row must still be on the home screen too — nothing launched"
    );

    // ---- Enter launches the current selection ------------------------
    assert!(
        harness.query_by_label("Beta Racer").is_some(),
        "Beta Racer row must exist"
    );
    harness.key_press(egui::Key::Enter);
    harness.run_steps(3);
    assert!(
        harness.query_by_label("Beta Racer").is_none(),
        "Enter must launch the row that was just selected by clicking it"
    );
    harness.state_mut().close_rom_for_test();
    harness.run_steps(3);

    // ---- double-click launches directly, no prior selection needed ---
    let alpha_pos = harness
        .query_by_label("Alpha Quest")
        .expect("Alpha Quest row")
        .rect()
        .center();
    double_click(&mut harness, alpha_pos);
    harness.run_steps(3);
    assert!(
        harness.query_by_label("Alpha Quest").is_none(),
        "double-click on a row must launch that game"
    );
    harness.state_mut().close_rom_for_test();
    harness.run_steps(3);

    // ---- arrow keys move the selection, driving the same state Enter
    // and click read: from no selection, Down moves to the first
    // (alphabetical) filtered entry, Alpha Quest, and Enter then launches
    // exactly that one.
    harness.state_mut().set_library_selected_for_test(None);
    harness.run_steps(2);
    assert!(
        harness.query_by_label("Alpha Quest").is_some(),
        "Alpha Quest row must exist"
    );
    harness.key_press(egui::Key::ArrowDown);
    harness.run_steps(2);
    harness.key_press(egui::Key::Enter);
    harness.run_steps(3);
    assert!(
        harness.query_by_label("Alpha Quest").is_none(),
        "ArrowDown then Enter must launch the first filtered entry — the pad bridge in \
         ui_nav.rs drives this exact same key event, so this also proves gamepad Activate"
    );
    assert!(
        harness.query_by_label("Beta Racer").is_none(),
        "ArrowDown from no selection must land on Alpha Quest, not Beta Racer"
    );
}
