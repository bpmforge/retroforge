//! Ticket W15-08: controller-first library navigation
//! (`docs/design/UX_WAVE_15.md` §9, §11).
//!
//! **One test function, on purpose.** `RETROFORGE_CONFIG_DIR` is
//! process-global and building the app reads *and writes* it, so a second
//! test in this binary would race the first — same reasoning as
//! `library_grid.rs`/`library_selection.rs`/`context_menu_and_game_settings.rs`.
//!
//! **Why `press_pad` is two frames, not a single injected event.**
//! `tests/gamepad_nav.rs` already proves `ui_nav.rs`'s own translation
//! (`GamepadNav::events_for`) in isolation, against a throwaway
//! `Harness<usize>`. This suite tests something that alone cannot: that a
//! real pad press also updates `RetroForgeApp`'s device tracker and
//! `pad_menu_requested`, exactly as `poll_input` would from a live
//! `PadBackend::poll` — which needs the `gamepad` feature and real
//! hardware neither this binary nor CI has. `mark_pad_active_for_test`
//! is the wire for those two effects; `press_pad`'s own doc explains why
//! it has to run AFTER the frame that processes the action's
//! `egui::Event`, not alongside it.
//!
//! **Why `is_focused()` stands in for `ctx.memory(|m| m.focused())`.**
//! AccessKit's `NodeId` is a stable hash of the underlying `egui::Id`,
//! not the same type, so a test cannot compare them directly without
//! reimplementing that hash. `kittest::Node::is_focused()` reads
//! `accesskit_node.is_focused()` — precisely the fact `ctx.memory(|m|
//! m.focused())` produces, translated into the accessibility tree by
//! egui's own AccessKit integration — so asserting it on the entry the
//! pad just selected proves the same claim acceptance 5 asks for: no
//! separate accessibility path exists, because AccessKit's focused node
//! IS egui's focused widget. `tests/gamepad_nav.rs`'s
//! `the_menu_surfaces_accessibility_nodes_that_can_be_focused` test
//! already established this same equivalence for a plain menu.

use std::path::{Path, PathBuf};

use eframe::egui;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::library::LibraryView;
use retroforge::theme;
use retroforge::ui_nav::{GamepadNav, InputDevice, NavAction};

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
    // Far from the code and the reset vector — only here so the two
    // fixtures hash differently (same trick `library_grid.rs` uses).
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

/// Inject one pad navigation action.
///
/// Two-phase, deliberately (see `mark_pad_active_for_test`'s own doc for
/// why): `action`'s own `egui::Event` (if it has one — `Menu`/`Back`
/// don't, `ui_nav.rs`'s doc explains) is pushed into the harness's
/// `RawInput` exactly as `tests/gamepad_nav.rs` does, and run FIRST —
/// this is the frame `library_grid`'s `key_pressed` check actually reads
/// it in, moving the selection and mirroring it into egui's real focus
/// (acceptance 5). ONLY THEN is the device marked Gamepad and a second
/// frame run: this is the frame the ring-width/type-scale assertions
/// below observe, and marking it any earlier would just have it
/// overwritten by `poll_input`'s own tracker seeing that same injected
/// key as ordinary keyboard input (there is no real `PadEvent` in this
/// test build to attribute it to a pad instead).
fn press_pad(harness: &mut Harness<'_, RetroForgeApp>, action: NavAction) {
    for event in GamepadNav::events_for(&[action]) {
        harness.input_mut().events.push(event);
    }
    harness.run_steps(2);
    harness.state_mut().mark_pad_active_for_test(&[action]);
    harness.run_steps(2);
}

#[test]
fn controller_first_library_navigation() {
    let dir = std::env::temp_dir().join(format!(
        "retroforge_library_controller_{}",
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
    write_fixture_rom(&games, "Alpha Quest.nes", 1);
    write_fixture_rom(&games, "Beta Racer.nes", 2);

    let mut harness = app();
    harness
        .state_mut()
        .set_library_roots_for_test(vec![games.clone()]);
    harness
        .state_mut()
        .set_library_view_for_test(LibraryView::Grid);
    harness.run_steps(2);

    // ---- before any pad input: mouse/keyboard defaults ------------------
    assert_eq!(
        harness.state().last_active_input_for_test(),
        InputDevice::Mouse,
        "a fresh app has never seen pad input"
    );
    assert_eq!(
        harness.state().library_focus_ring_width_for_test(),
        theme::FOCUS_RING_MOUSE,
        "the ring must be the thin, mouse-scale width before any pad input"
    );
    let body_before = harness
        .ctx
        .style_of(harness.ctx.theme())
        .text_styles
        .get(&egui::TextStyle::Body)
        .expect("Body text style is always set by apply_theme")
        .size;

    // ---- acceptance 1: Right moves the grid selection via the SAME pad
    // bridge — from no selection, Right lands on the first (alphabetical)
    // filtered card, exactly the ArrowRight contract `library_grid.rs`
    // proves for the keyboard ----------------------------------------------
    press_pad(&mut harness, NavAction::Right);
    assert!(
        harness.get_by_label("Alpha Quest").is_focused(),
        "pad Right from no selection must select and focus the first card"
    );

    // ---- acceptance 2/3: the pad is now the active device — thicker,
    // stronger-accent ring and the larger type scale -----------------------
    assert_eq!(
        harness.state().last_active_input_for_test(),
        InputDevice::Gamepad
    );
    assert_eq!(
        harness.state().library_focus_ring_width_for_test(),
        theme::FOCUS_RING_PAD,
        "the ring must thicken once the pad is the active device"
    );
    let body_pad = harness
        .ctx
        .style_of(harness.ctx.theme())
        .text_styles
        .get(&egui::TextStyle::Body)
        .expect("Body text style is always set by apply_theme")
        .size;
    assert!(
        body_pad > body_before,
        "body text must grow while the pad drives the UI \
         (was {body_before}, now {body_pad})"
    );

    // ---- acceptance 1, continued: Down also moves the SAME selection,
    // and acceptance 5: the newly selected card is egui's real focus, not
    // a bespoke model AccessKit cannot see (module doc explains why
    // `is_focused()` is the check for that) --------------------------------
    press_pad(&mut harness, NavAction::Down);
    assert!(
        harness.get_by_label("Beta Racer").is_focused(),
        "pad Down must move the selection off Alpha Quest onto Beta Racer, \
         and the new selection must be egui's real focused widget"
    );
    assert!(
        !harness.get_by_label("Alpha Quest").is_focused(),
        "the previously selected card must no longer be focused"
    );

    // ---- acceptance 4: Start opens the context menu on the focused CARD
    // (W15-03 already wired this for rows; this proves cards too) ---------
    assert!(
        harness.query_by_label("Game settings").is_none(),
        "the context menu must not be open yet"
    );
    press_pad(&mut harness, NavAction::Menu);
    assert!(
        harness.query_by_label("Game settings").is_some(),
        "Start must open the context menu on the focused card"
    );
    assert!(
        harness.query_by_label("Play in mode \u{23f5}").is_some(),
        "it must be the SAME context menu body rows and cards share"
    );

    // ---- acceptance 3, continued: real mouse activity reverts the type
    // scale and the ring, exactly as it does the active device. A single
    // `PointerMoved` is not enough to trip `PointerState::is_moving()` —
    // that needs a few frames of position history — so this is a real
    // click (move, press, release, one pass), the same shape
    // `context_menu_and_game_settings.rs`'s `right_click` helper uses,
    // which `any_pressed()`/`any_click()` (the OTHER mouse signals
    // `Self::track_input_device` checks) report the instant it happens.
    // Bottom-left corner: empty space, well clear of the menu bar and any
    // popup the previous step may have left open.
    let click_pos = egui::pos2(5.0, WINDOW_SIZE[1] - 5.0);
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(click_pos));
    harness.input_mut().events.push(egui::Event::PointerButton {
        pos: click_pos,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::default(),
    });
    harness.input_mut().events.push(egui::Event::PointerButton {
        pos: click_pos,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::default(),
    });
    harness.run_steps(2);
    assert_eq!(
        harness.state().last_active_input_for_test(),
        InputDevice::Mouse,
        "mouse activity must revert the tracked device"
    );
    assert_eq!(
        harness.state().library_focus_ring_width_for_test(),
        theme::FOCUS_RING_MOUSE,
        "the ring must revert to the thin width once the mouse resumes"
    );
    let body_after = harness
        .ctx
        .style_of(harness.ctx.theme())
        .text_styles
        .get(&egui::TextStyle::Body)
        .expect("Body text style is always set by apply_theme")
        .size;
    assert_eq!(
        body_after, body_before,
        "the type scale must revert exactly once the mouse resumes"
    );
}
