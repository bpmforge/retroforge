//! Ticket W15-02: recently played, favourites, and sort on the library
//! toolbar (`docs/design/UX_WAVE_15.md` §3, §11).
//!
//! **One test function, on purpose.** Same reasoning as
//! `library_home.rs`/`library_selection.rs`: `RETROFORGE_CONFIG_DIR` is
//! process-global and building the app reads *and writes* it, so a second
//! test in this binary would race the first.

use std::path::{Path, PathBuf};

use eframe::egui;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};

/// `seed` lands in an unused PRG byte so two fixtures never share a
/// normalized hash — required here (unlike `library_selection.rs`, which
/// only needs distinct titles) because `library_meta` and the Recently
/// played/Favourites filters are keyed by that hash, not by title.
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
    // fixtures hash differently.
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

/// The acceptance scenario in W15-02's ticket text: launch a game, close
/// it, and see it — and only it — under Recently played. A second game
/// that was never launched must not appear once that chip is active,
/// which is the same fact the pure `filter_and_sort` unit tests already
/// cover over a synthetic list; this proves the live app wires the two
/// together (a launch really does update `library_meta`, and the chip
/// really does read it).
#[test]
fn a_launched_and_closed_game_shows_up_first_under_recently_played() {
    let dir =
        std::env::temp_dir().join(format!("retroforge_library_filters_{}", std::process::id()));
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
    write_fixture_rom(&games, "Never Played.nes", 1);
    let played_path = write_fixture_rom(&games, "Played Once.nes", 2);

    let mut harness = app();
    harness
        .state_mut()
        .set_library_roots_for_test(vec![games.clone()]);
    harness.run_steps(2);

    // Before any launch, neither the Recently played filter nor a
    // favourite exists — both games are ordinary and unplayed.
    assert!(harness.query_by_label("Never Played").is_some());
    assert!(harness.query_by_label("Played Once").is_some());

    // ---- launch, then close ------------------------------------------
    harness.state_mut().open_rom_path(&played_path);
    harness.run_steps(3);
    assert!(
        harness.query_by_label("Played Once").is_none(),
        "opening a ROM must replace the home with the play view"
    );
    harness.state_mut().close_rom_for_test();
    harness.run_steps(3);
    // `query_all`: since W20-20 the just-played game is ALSO the home's
    // Continue hero, so its title appears twice on the unfiltered home.
    assert!(
        harness.query_all_by_label("Played Once").count() > 0,
        "closing the ROM must return to the library home"
    );

    // ---- select the Recently played chip ------------------------------
    harness
        .query_by_label("Recently played")
        .expect("Recently played chip")
        .click();
    harness.run_steps(2);

    assert!(
        harness.query_by_label("Played Once").is_some(),
        "the just-launched game must show up under Recently played"
    );
    assert!(
        harness.query_by_label("Never Played").is_none(),
        "a game that was never launched must not show up under Recently played"
    );

    // ---- Recently played and Favourites are radio-like -----------------
    // Selecting Favourites must clear Recently played (and vice versa):
    // neither game is a favourite yet, so the list goes empty.
    harness
        .query_by_label("Favourites")
        .expect("Favourites chip")
        .click();
    harness.run_steps(2);
    assert!(
        harness.query_by_label("Played Once").is_none(),
        "Favourites replaces Recently played rather than combining with it"
    );
    assert!(harness
        .query_by_label("No games match this search.")
        .is_some());

    // Clear the filter chip (click Favourites again to toggle it off) and
    // confirm both games are back — proves the toggle-off behaviour, not
    // just the toggle-on.
    harness
        .query_by_label("Favourites")
        .expect("Favourites chip")
        .click();
    harness.run_steps(2);
    assert!(harness.query_by_label("Never Played").is_some());
    // Back on the unfiltered home: the list row AND the Continue hero
    // (W20-20) both name it.
    assert!(harness.query_all_by_label("Played Once").count() > 0);
}
