//! **A Mesen HD pack reaches the running game** (ticket W11-05).
//!
//! The pack machinery has been complete and tested since W9-06 —
//! `parse_hires`, `write_hires`, rule lookup, `PackBuilder`, and a suite
//! against a real pack from the wild. What did not exist was any way for
//! a user to load one and see it, and no unit test could have noticed:
//! every piece worked, and nothing assembled them.
//!
//! So this drives `RetroForgeApp` and asserts on **what the app draws**.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use rf_enhance::hdpack::{write_hires, HdPack, TileData, TileKey, TileRule};

const NES: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";
const SCALE: u32 = 2;

fn run_frames(h: &mut Harness<'_, RetroForgeApp>, target: u64, deadline: Duration) {
    let start = Instant::now();
    while h.state().frame_count_for_test() < target && start.elapsed() < deadline {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
}

/// A pack that replaces tile 0 whatever palette it is drawn with.
///
/// `default_tile` is the spec's own "matches the tile data but no rule
/// matches its palette" fallback, which is what makes this test
/// independent of whatever palette the fixture happens to use — pinning a
/// palette here would make the test fail for a reason that has nothing to
/// do with the plumbing under test.
fn write_pack(dir: &Path) {
    let rule = |tile: u32| TileRule {
        key: TileKey {
            tile: TileData::ChrRom(tile),
            palette: [0; 4],
        },
        img: 0,
        x: 0,
        y: 0,
        brightness: 1.0,
        default_tile: true,
        condition: None,
    };
    let pack = HdPack {
        version: 100,
        scale: SCALE,
        overscan: [0; 4],
        images: vec!["tiles.png".into()],
        // Tile 0 in BOTH pattern tables: a CHR-ROM index is the tile's
        // index across the whole of CHR, so the same tile in the $1000
        // table is 256 higher. Covering both keeps the test independent
        // of which table the fixture selects.
        tiles: vec![rule(0), rule(256)],
        conditions: Default::default(),
        options: Vec::new(),
        unevaluated: Vec::new(),
        unsupported: Default::default(),
    };
    std::fs::write(dir.join("hires.txt"), write_hires(&pack)).expect("write hires.txt");

    // One solid magenta tile at scale: 8 * SCALE square.
    let side = 8 * SCALE;
    let mut img = image::RgbaImage::new(side, side);
    for px in img.pixels_mut() {
        *px = image::Rgba([0xFF, 0x00, 0xFF, 0xFF]);
    }
    img.save(dir.join("tiles.png")).expect("write tiles.png");
}

#[test]
fn loading_a_pack_replaces_art_in_the_running_game() {
    let dir = std::env::temp_dir().join(format!("retroforge_hd_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }

    let nes = Path::new(env!("CARGO_MANIFEST_DIR")).join(NES);
    assert!(nes.exists(), "NES fixture missing: {}", nes.display());
    let rom: PathBuf = dir.join("rf-scroller.nes");
    std::fs::copy(&nes, &rom).expect("copy fixture");

    let pack_dir = dir.join("pack");
    std::fs::create_dir_all(&pack_dir).expect("pack dir");
    write_pack(&pack_dir);

    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    harness.state_mut().open_rom_path(&rom);
    harness.run_steps(2);
    harness.state_mut().resume_for_test();
    // **Run past the fixture's PPU warm-up.** rf-scroller, like real NES
    // software, waits several vblanks before enabling rendering — it
    // draws its first tile around frame 57. A test that loaded the pack
    // at frame 30 would find nothing to replace and blame the plumbing.
    run_frames(&mut harness, 90, Duration::from_secs(60));

    let before = harness
        .state()
        .frame_size_for_test()
        .expect("the game must be running before a pack is loaded");
    assert_eq!(before, (256, 240), "a fresh session draws the NES picture");
    assert!(
        harness.state().hd_report_for_test().is_none(),
        "nothing should have been composited before a pack was loaded"
    );

    // The user loads a pack.
    harness
        .state_mut()
        .load_hd_pack(&pack_dir)
        .expect("a pack this test just wrote must load");
    let summary = harness
        .state()
        .hd_summary_for_test()
        .expect("the import must report itself")
        .to_string();
    assert!(
        summary.contains("2"),
        "the import summary should account for both rules: {summary}"
    );

    let target = harness.state().frame_count_for_test() + 10;
    run_frames(&mut harness, target, Duration::from_secs(30));

    // 1. The picture is now scale times larger.
    let after = harness
        .state()
        .frame_size_for_test()
        .expect("frames continue");
    assert_eq!(
        after,
        (256 * SCALE as usize, 240 * SCALE as usize),
        "a scale-{SCALE} pack must make the drawn frame that much bigger"
    );
    let rgba = harness.state().last_frame_rgba_for_test().expect("a frame");
    assert_eq!(rgba.len(), after.0 * after.1 * 4, "buffer matches its size");

    // 2. Art was actually REPLACED, not merely upscaled. This is the
    //    assertion that separates "the pack applied" from "the frame got
    //    bigger", and only the second would pass on a no-op composite.
    let report = harness
        .state()
        .hd_report_for_test()
        .expect("a composited frame must report what it did");
    assert!(
        report.replaced > 0,
        "the pack replaces tile 0 with default_tile set, and this fixture \
         draws tile 0 — nothing was replaced: {report:?}"
    );
    assert!(
        rgba.chunks_exact(4)
            .any(|px| px[0] == 0xFF && px[1] == 0x00 && px[2] == 0xFF),
        "the pack's magenta tile must actually be visible in the drawn frame"
    );

    // Ticket W20-09: replaced art is visible, so the honesty badge names
    // it (before W20-09 it still read plain "Accuracy").
    assert!(
        harness.state().status_badge().contains("HD pack"),
        "badge must name the pack: {}",
        harness.state().status_badge()
    );

    // 3. Unloading restores the original picture exactly.
    harness.state_mut().clear_hd_pack();
    assert!(!harness.state().status_badge().contains("HD pack"));
    let target = harness.state().frame_count_for_test() + 10;
    run_frames(&mut harness, target, Duration::from_secs(30));
    assert_eq!(
        harness.state().frame_size_for_test(),
        Some((256, 240)),
        "unloading a pack must return to the accuracy-sized picture"
    );

    // 4. Ticket W20-09: a pack belongs to the game it was loaded for.
    //    Opening a game drops it, rather than leaving a summary on screen
    //    for a pack the new core never captures tiles for.
    harness
        .state_mut()
        .load_hd_pack(&pack_dir)
        .expect("reload the pack");
    assert!(harness.state().hd_summary_for_test().is_some());
    harness.state_mut().open_rom_path(&rom);
    harness.run_steps(2);
    assert!(
        harness.state().hd_summary_for_test().is_none(),
        "opening a game must drop the previous game's HD pack"
    );
    assert!(!harness.state().status_badge().contains("HD pack"));
}
