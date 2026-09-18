//! Ticket W15-09, ruling D-011 (`docs/design/UX_WAVE_15.md` §4 item 4):
//! turning the "Fetch box art from the internet" toggle on, with a fake
//! `ArtClient`, must mark the fetched card with the network indicator —
//! and with the toggle left off (the default), the fake client must never
//! be called at all.
//!
//! **One test function, on purpose** — same reasoning as
//! `library_grid.rs`'s own module doc: `RETROFORGE_CONFIG_DIR` is
//! process-global, and building the app reads/writes the config root, so
//! a second test in this binary would race the first.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use eframe::egui;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::art::test_support::FakeArtClient;
use retroforge::library::LibraryView;

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

/// A tiny valid PNG (1x1, opaque red) — the fake client's canned response
/// must decode through the real `image::load_from_memory` path
/// `library_thumbnail_texture` uses, exactly like a real fetch would.
fn tiny_png() -> Vec<u8> {
    rf_renderer::png::encode_rgba(&[255, 0, 0, 255], 1, 1)
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
fn toggle_off_makes_zero_requests_and_on_shows_the_network_indicator() {
    let dir = std::env::temp_dir().join(format!(
        "retroforge_library_grid_network_art_{}",
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
    write_fixture_rom(&games, "Net Art Quest.nes");

    let mut harness = app();
    harness
        .state_mut()
        .set_library_roots_for_test(vec![games.clone()]);
    harness
        .state_mut()
        .set_library_view_for_test(LibraryView::Grid);
    harness.run_steps(2);
    assert!(
        harness.query_by_label("Net Art Quest").is_some(),
        "the fixture must appear as a placeholder card before any fetch runs"
    );

    // ---- acceptance 1: toggle off, fake client never called -----------
    let off_client = Arc::new(FakeArtClient::new(Ok(tiny_png())));
    harness
        .state_mut()
        .set_art_client_for_test(off_client.clone());
    // `fetch_art` is off by default (a fresh install, D-011); several
    // frames of the grid drawing the same card must not call the client.
    harness.run_steps(5);
    assert_eq!(
        off_client.call_count(),
        0,
        "the toggle is off by default; nothing may call the art client"
    );
    assert!(
        !harness
            .state()
            .thumbnail_is_fetched_for_test("Net Art Quest"),
        "no card may show the network indicator while the toggle is off"
    );

    // ---- acceptance 2/3: toggle on, one fetch, indicator appears -------
    let on_client = Arc::new(FakeArtClient::new(Ok(tiny_png())));
    harness
        .state_mut()
        .set_art_client_for_test(on_client.clone());
    harness.state_mut().set_fetch_art_for_test(true);
    harness.run_steps(2);

    // The fetch runs on a background thread; give it a moment to land
    // and the waker to wake the harness's own repaint, then keep
    // stepping until the indicator shows or a generous timeout elapses.
    let start = std::time::Instant::now();
    loop {
        harness.run_steps(1);
        if harness
            .state()
            .thumbnail_is_fetched_for_test("Net Art Quest")
        {
            break;
        }
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "network indicator never appeared after enabling the toggle"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(
        on_client.call_count(),
        1,
        "exactly one fetch for the one entry with no local thumbnail"
    );

    // A further round of frames must not re-request it — cached now.
    harness.run_steps(10);
    assert_eq!(
        on_client.call_count(),
        1,
        "a cached fetch must never be re-requested"
    );
}
