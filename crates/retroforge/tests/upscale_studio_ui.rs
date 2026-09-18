//! Ticket W16-02: the Upscale Studio window, end to end against the stub
//! upscaler — open it, seed a capture, Run, approve one tile and reject
//! another, Write pack, and check the files landed.
//!
//! **One test function**, same reasoning as
//! `context_menu_and_game_settings.rs`: building the app touches
//! process-global state, so a second test in this binary would race it.
//!
//! No ROM or core thread is needed: the window opens and the pipeline
//! runs from captured tiles alone, so this test seeds the capture session
//! directly (`RetroForgeApp::upscale_studio_seed_for_test`) rather than
//! booting a fixture ROM and waiting for real frames — the same
//! test-only-accessor pattern `load_script_for_test` already uses to
//! sidestep something a headless harness cannot drive itself (there, a
//! native file dialog; here, a running PPU).

use eframe::egui;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::stepper::StudioTileCapture;

fn app() -> Harness<'static, RetroForgeApp> {
    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    harness.run();
    harness
}

/// One 8x8 solid-colour tile, all pixels index 1, `seed` perturbing a
/// pixel so distinct calls produce distinct asset hashes.
fn capture(x: i32, y: i32, colour: [u8; 4], seed: u8) -> StudioTileCapture {
    let mut indexed_pixels = [1u8; 64];
    indexed_pixels[0] = seed;
    let mut palette_rgba = [0u8; 16];
    palette_rgba[4..8].copy_from_slice(&colour);
    StudioTileCapture {
        tile: rf_enhance::hdpack::TileData::ChrRom(u32::from(seed)),
        mesen_palette: [0x0F, 0x00, 0x10, 0x20],
        layer: rf_enhance::hd_render::Layer::Background,
        x,
        y,
        indexed_pixels,
        palette_rgba,
    }
}

#[test]
fn upscale_studio_runs_against_the_stub_and_writes_only_approved_tiles() {
    let config_dir = std::env::temp_dir().join(format!(
        "retroforge_upscale_studio_ui_config_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&config_dir);
    std::fs::create_dir_all(&config_dir).expect("create scratch config dir");
    // SAFETY: first statement of the only test in this binary, before any
    // app, core thread or harness exists — no concurrent reader. Same
    // reasoning as `context_menu_and_game_settings.rs`'s identical
    // block: an empty, isolated config dir means no real ROM folders to
    // scan, so the library-scan spinner (`app.rs`'s "Scanning your ROM
    // folders…" repaint-forever path) settles within a couple of steps
    // instead of running indefinitely against the developer's own
    // library.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &config_dir);
    }

    let mut harness = app();

    // ---- open the window from the Enhance menu, same click sequence
    // `context_menu_and_game_settings.rs` uses for "Game settings…" ------
    assert!(!harness.state_mut().show_upscale_studio_for_test());
    harness
        .query_by_label("Enhance")
        .expect("Enhance menu")
        .click();
    harness.run_steps(2);
    harness
        .query_by_label("Upscale Studio\u{2026}")
        .expect("Enhance menu's Upscale Studio… command")
        .click();
    harness.run_steps(2);
    assert!(
        harness.state_mut().show_upscale_studio_for_test(),
        "the Enhance menu's Upscale Studio… must open the window"
    );

    // ---- seed two distinct captured tiles (no core thread needed) ------
    let tile_a = capture(10, 10, [255, 0, 0, 255], 1);
    let tile_b = capture(20, 10, [0, 255, 0, 255], 2);
    let hash_a = rf_ai::pack::asset_hash(&tile_a.indexed_pixels, &tile_a.palette_rgba);
    let hash_b = rf_ai::pack::asset_hash(&tile_b.indexed_pixels, &tile_b.palette_rgba);
    harness
        .state_mut()
        .upscale_studio_seed_for_test(&[tile_a, tile_b]);
    harness.run_steps(2);
    assert!(
        harness.query_by_label("2 tile(s) captured").is_some(),
        "the window must show the seeded capture count"
    );

    // ---- reject tile B before running, so the write step actually
    // exercises the decision rather than writing everything by default --
    harness.state_mut().upscale_studio_set_decision_for_test(
        &hash_b,
        retroforge::upscale_studio::Decision::Rejected,
    );

    // ---- Run, against the stub (no ONNX feature compiled in here) ------
    harness
        .query_by_label("Run upscaler")
        .expect("the Run upscaler button")
        .click();
    // The run happens on a worker thread; poll a few steps for it to land
    // rather than assuming one step is enough.
    let mut ran = false;
    for _ in 0..50 {
        harness.run_steps(1);
        if harness
            .query_all_by_label("(run to preview)")
            .next()
            .is_none()
        {
            ran = true;
            break;
        }
    }
    assert!(ran, "the stub Run must complete within 50 steps");

    // ---- Write pack, bypassing the native file dialog ------------------
    let dir = std::env::temp_dir().join(format!(
        "retroforge_upscale_studio_ui_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let ctx = harness.ctx.clone();
    harness
        .state_mut()
        .write_upscale_studio_pack_for_test(&ctx, dir.clone());
    harness.run_steps(2);

    assert_eq!(
        harness.state_mut().upscale_studio_write_dir_for_test(),
        Some(dir.clone())
    );
    assert!(
        dir.join("manifest.toml").exists(),
        "manifest.toml must exist"
    );
    assert!(dir.join("hires.txt").exists(), "hires.txt must exist");
    assert!(
        dir.join("upscaled.png").exists(),
        "the tileset PNG must exist"
    );

    // The Mesen round trip: `hires.txt` parses with `rf_enhance::hdpack`'s
    // own loader, the APPROVED tile is findable through it, and the
    // REJECTED one is not — a manifest that merely listed the hash would
    // pass a weaker assertion than an actual lookup does.
    let text = std::fs::read_to_string(dir.join("hires.txt")).unwrap();
    let loaded = rf_enhance::hdpack::parse_hires(&text).unwrap();
    let mesen_palette = [0x0F, 0x00, 0x10, 0x20];
    assert!(
        loaded
            .lookup(&rf_enhance::hdpack::TileData::ChrRom(1), &mesen_palette)
            .is_some(),
        "the APPROVED tile (hash {hash_a}) must be in the written pack"
    );
    assert!(
        loaded
            .lookup(&rf_enhance::hdpack::TileData::ChrRom(2), &mesen_palette)
            .is_none(),
        "the REJECTED tile (hash {hash_b}) must NOT be in the written pack"
    );

    // The STRICTER check: `parse_hires` alone only proves the syntax is
    // valid, not that the tileset image actually holds every rule's
    // region. `load_hd_pack` runs `hdpack::import`, which checks each
    // rule's `(x, y)` against the image's real decoded dimensions
    // (`Unsatisfied::RegionOutOfBounds`) — the same path a player's own
    // "Load HD pack…" goes through. A `Partial` import here would mean
    // this pack claims tiles it cannot actually deliver.
    harness.state_mut().load_hd_pack(&dir).expect(
        "the app's own HD-pack loader (hdpack::import) must accept what write_pack produced",
    );
    let summary = harness
        .state_mut()
        .hd_summary_for_test()
        .expect("a summary must be recorded")
        .to_string();
    assert!(
        summary.contains("all satisfied"),
        "the written pack must not be PARTIAL against the app's own loader: {summary}"
    );

    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_dir_all(&config_dir).ok();
}
