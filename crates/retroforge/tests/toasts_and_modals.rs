//! Ticket W15-04: themed modals (`egui::Modal`) and toasts replacing the
//! save-state overwrite window, quit confirmation and the script-error
//! status string (`docs/design/UX_WAVE_15.md` §5).
//!
//! Two acceptance-4 scenarios, in the `egui_kittest` harness pattern
//! `tests/state_slots.rs` and `tests/library_selection.rs` already use:
//!
//! 1. A modal (the save-state overwrite confirmation) blocks a click on
//!    the app behind it and dismisses on an outside click — proved
//!    together, since a click that lands on the backdrop instead of the
//!    button it is drawn over demonstrates both at once.
//! 2. A toast (state saved) appears, then disappears once its duration
//!    elapses on the harness's own advancing clock, without pausing the
//!    core thread — proved by the core's frame counter still climbing
//!    across the toast's lifetime.
//!
//! **One test function.** `RETROFORGE_CONFIG_DIR` is process-global and
//! `RetroForgeApp::new` reads it, so a second test in this binary setting
//! it while this one reads it would race — every other file that opens a
//! real app reaches the same conclusion (`library_home.rs`,
//! `library_selection.rs`, `ui_smoke.rs`).

use std::path::{Path, PathBuf};
use std::time::Duration;

use eframe::egui;
use eframe::egui::accesskit::Role;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::state_slots::{self, SlotId};
use retroforge::stepper::EmuStepper;

/// A tiny deterministic NROM that actually runs, so the core thread has
/// frames to report — `tests/script_reaches_the_app.rs` and
/// `tests/library_selection.rs` both build the same shape for the same
/// reason.
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

/// A real `.rfstate` container, built through the shipped save path
/// (`EmuStepper::save_state`) rather than hand-rolled bytes —
/// `tests/state_slots.rs`'s `real_container` does the same, for the same
/// reason: what the manager reads should be what the app actually
/// writes.
fn real_container_bytes() -> Vec<u8> {
    let mut rom = vec![0u8; 16 + 0x4000 + 0x2000];
    rom[0..4].copy_from_slice(b"NES\x1a");
    rom[4] = 1;
    rom[5] = 1;
    rom[16 + 0x3FFC] = 0x00;
    rom[16 + 0x3FFD] = 0x80;
    let stepper = EmuStepper::from_ines_bytes(&rom).expect("synthetic rom loads");
    stepper
        .save_state(1_700_000_000)
        .expect("saves")
        .encode()
        .expect("encodes")
}

#[test]
fn overwrite_modal_blocks_the_app_behind_it_and_a_toast_expires_without_pausing_the_core() {
    let dir = std::env::temp_dir().join(format!(
        "retroforge_toasts_and_modals_{}",
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

    let rom = write_fixture_rom(&dir, "fixture.nes");

    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    harness.run();

    harness.state_mut().open_rom_path(&rom);
    harness.run_steps(2);
    harness.state_mut().resume_for_test();

    // A fixed hash rather than whatever this synthetic ROM identified
    // as: the core thread does not care what `current_game_hash` says
    // (it is already running against `rom`'s real bytes), and a fixed
    // value lets the pre-written slot file's directory be computed
    // without reading the hash back out of the app first.
    let hash = "b".repeat(64);
    harness
        .state_mut()
        .set_game_hash_for_test(Some(hash.clone()));

    // ---- scenario 1: the overwrite modal blocks and outside-dismisses -
    //
    // Put a real state in slot 1 BEFORE the modal ever opens, so its very
    // first render already shows an occupied slot — this is what makes
    // the next "Save Slot 1" click go through `pending_overwrite` instead
    // of saving immediately (`states_modal`'s own branch on
    // `info.saved.is_some()`).
    let states_dir = state_slots::slots_dir(&dir, &hash);
    state_slots::save(
        &states_dir,
        SlotId::Numbered(1),
        &real_container_bytes(),
        None,
    )
    .expect("pre-seed slot 1");

    harness.state_mut().open_states_modal();
    harness.run_steps(2);

    harness
        .get_by_role_and_label(Role::Button, "Save Slot 1")
        .click();
    harness.run_steps(1);
    assert!(
        harness.state().pending_overwrite_for_test(),
        "a Save click on an OCCUPIED slot must open the overwrite-confirmation modal, not save \
         immediately"
    );

    // The modal's backdrop sits at `Order::Foreground`, above the plain
    // `states_modal` window underneath it (`egui::Modal`'s own doc:
    // "blocks input to the rest of the UI"). Clicking exactly where
    // "Load Slot 1" is drawn should therefore hit the BACKDROP, not the
    // button — proving the click was blocked from reaching the app
    // behind the modal, and (since a backdrop click is
    // `ModalResponse::should_close`'s outside-click case) dismissing the
    // modal in the same motion.
    let status_before = harness.state().status().to_string();
    harness
        .get_by_role_and_label(Role::Button, "Load Slot 1")
        .click();
    harness.run_steps(1);
    assert!(
        !harness.state().pending_overwrite_for_test(),
        "a click outside the modal's own frame must dismiss it"
    );
    assert_eq!(
        harness.state().status(),
        status_before,
        "the click must have been consumed by the backdrop, not reached \"Load Slot 1\" \
         underneath it — a status change here would mean the load actually ran"
    );

    // ---- scenario 2: a toast appears and expires without pausing the --
    // ---- core -----------------------------------------------------------
    assert!(
        !harness.state().has_visible_toast_for_test(),
        "no toast should be up yet — nothing has been saved this test"
    );

    // Slot 2 is empty, so this Save goes straight through (no
    // confirmation needed) and fires the "state saved" toast directly —
    // scenario 1 already covers the occupied-slot path.
    harness
        .get_by_role_and_label(Role::Button, "Save Slot 2")
        .click();
    harness.run_steps(1);
    assert!(
        harness.state().has_visible_toast_for_test(),
        "saving a state must raise a toast"
    );

    let frame_before = harness.state().frame_count_for_test();

    // Advance the HARNESS's own clock (`egui::Context::time`, which
    // `ToastStack` keys expiry off — see that module's doc for why it is
    // NOT wall-clock `Instant::now()`) well past the toast's
    // duration-plus-fade, with a little real sleep between steps so the
    // core thread — a genuinely separate OS thread — has wall-clock time
    // to actually step frames while this runs, the same pattern
    // `script_reaches_the_app.rs`'s `run_frames` uses.
    for _ in 0..40 {
        harness.step();
        std::thread::sleep(Duration::from_millis(20));
    }

    let frame_after = harness.state().frame_count_for_test();
    assert!(
        frame_after > frame_before,
        "the core must keep stepping frames while a toast is up and after it expires — a toast \
         must never pause emulation (FRONTEND_UI §1 principle 3)"
    );
    assert!(
        !harness.state().has_visible_toast_for_test(),
        "a toast must disappear once its duration has elapsed"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
