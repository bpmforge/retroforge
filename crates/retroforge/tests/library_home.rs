//! **The library is the app's home screen** (ticket W10-03;
//! `docs/design/FRONTEND_UI.md` §3.1, and §2's information architecture,
//! which puts Library at the root: *Library (home) → Play view →
//! Workspaces*).
//!
//! Until W10-03 the app booted to an empty play area with a single line
//! of text in it, and the library was a checkbox-toggled floating window
//! — an application whose home screen was a void. The scanning, the hash
//! identity and the three first-run states all already existed and are
//! unchanged; this ticket moved *where they live*.
//!
//! ## What is actually asserted here
//!
//! That the home is reachable **with no clicks**, that the round trip
//! works (open a game, close it, land back on the library without a
//! rescan), and that §3.1's three first-run states stay distinct — the
//! rule design review G-21 forced, because "no folders configured" and
//! "folders with nothing in them" are different facts and an empty grid
//! that says neither is the bug.
//!
//! `crate::library::first_run_state` is unit-tested in the library module
//! itself; what those tests cannot see is whether the app ever *renders*
//! the branch. That is this file's job.

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

/// **One test per binary, on purpose.** `RETROFORGE_CONFIG_DIR` is
/// process-global and building the app reads *and writes* the config
/// root, so a second test in this binary setting it while this one reads
/// it is a race — or worse, a run against the developer's real config
/// directory. `ui_smoke.rs` and `hud_fits.rs` reached the same conclusion.
#[test]
fn the_library_is_the_home_screen_and_the_round_trip_works() {
    let dir = std::env::temp_dir().join(format!("retroforge_library_home_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    // SAFETY: first statement of the only test in this binary, before any
    // app, core thread or harness exists — no concurrent reader.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }

    // ---- 1. no folders configured ----------------------------------
    //
    // Reachable with ZERO clicks: this is what boot looks like. Before
    // W10-03 you had to find a checkbox in a menu to see any of it.
    let mut harness = app();
    assert!(
        harness.query_by_label("No ROM folders yet").is_some(),
        "with no roots configured the home must say so and offer the folder picker. \
         Tree: {:?}",
        labels(&harness)
    );
    assert!(
        harness.query_by_label("Add a ROM folder\u{2026}").is_some(),
        "the empty home must offer the one action that fixes it"
    );

    // ---- 2. folders configured, nothing in them --------------------
    //
    // G-21's distinction. This must NOT render the same thing as state 1:
    // "you have not set this up" and "you set it up and it found nothing"
    // send the user to completely different places.
    let empty_root = dir.join("empty-folder");
    std::fs::create_dir_all(&empty_root).expect("create empty root");
    harness
        .state_mut()
        .set_library_roots_for_test(vec![empty_root.clone()]);
    harness.run_steps(2);
    assert!(
        harness.query_by_label("No ROMs found").is_some(),
        "a configured folder with no ROMs must say THAT, not `No ROM folders yet`. \
         Tree: {:?}",
        labels(&harness)
    );
    assert!(
        harness.query_by_label("No ROM folders yet").is_none(),
        "the two empty states must not both render — that is the G-21 bug"
    );
    assert!(
        labels(&harness)
            .iter()
            .any(|l| l.contains("0 ROMs found in") && l.contains("empty-folder")),
        "the folder that came up empty must be NAMED — `0 ROMs found` without a path is \
         indistinguishable from a scan that never ran. Tree: {:?}",
        labels(&harness)
    );

    // ---- 3. populated ----------------------------------------------
    let games = dir.join("games");
    std::fs::create_dir_all(&games).expect("create games root");
    write_fixture_rom(&games, "Alpha Quest.nes");
    write_fixture_rom(&games, "Beta Racer.nes");
    harness
        .state_mut()
        .set_library_roots_for_test(vec![games.clone()]);
    harness.run_steps(2);
    for title in ["Alpha Quest", "Beta Racer"] {
        assert!(
            harness.query_by_label(title).is_some(),
            "`{title}` is missing from the populated home. Tree: {:?}",
            labels(&harness)
        );
    }

    // ---- 4. the search box actually filters ------------------------
    //
    // Asserting the OTHER entry disappears, not merely that the matching
    // one is still there — a filter that returns everything would pass the
    // weaker check.
    harness.state_mut().set_library_search_for_test("beta");
    harness.run_steps(2);
    assert!(
        harness.query_by_label("Beta Racer").is_some(),
        "the matching game must survive the filter"
    );
    assert!(
        harness.query_by_label("Alpha Quest").is_none(),
        "a search that leaves non-matching games on screen is not a filter"
    );
    harness.state_mut().set_library_search_for_test("");
    harness.run_steps(2);

    // ---- 5. the round trip -----------------------------------------
    //
    // The acceptance criterion this ticket turns on: opening a game
    // replaces the home, closing returns to it, and the return costs no
    // rescan. Without `Close ROM` the home is reachable exactly once per
    // process and is really just a launcher.
    // The DELTA across the round trip, not the absolute count: the setup
    // above legitimately scanned three times (one lazy scan at boot, then
    // once per `set_library_roots_for_test`). Asserting a total would be
    // asserting the shape of this test rather than the behaviour of the
    // app, and would go red the moment the setup changed.
    let scans_before = harness.state().library_scan_count_for_test();
    let rom = games.join("Alpha Quest.nes");
    harness.state_mut().open_rom_path(&rom);
    harness.run_steps(3);
    assert!(
        harness.query_by_label("Alpha Quest").is_none(),
        "opening a ROM must replace the home with the play view"
    );

    harness.state_mut().close_rom_for_test();
    harness.run_steps(3);
    assert!(
        harness.query_by_label("Alpha Quest").is_some(),
        "closing the ROM must return to the library home. Tree: {:?}",
        labels(&harness)
    );
    assert_eq!(
        harness.state().library_scan_count_for_test(),
        scans_before,
        "the play-then-close round trip re-scanned the library. Going back should be free — \
         playing a game does not change what is in the user's folders, and re-walking them \
         on every return is what makes going back feel expensive."
    );
}

fn labels(harness: &Harness<'_, RetroForgeApp>) -> Vec<String> {
    use egui_kittest::kittest::NodeT as _;
    harness
        .root()
        .children_recursive()
        .filter_map(|n| {
            let a = n.accesskit_node();
            a.label().or_else(|| a.value())
        })
        .collect()
}
