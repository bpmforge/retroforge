//! **W42-07: polish.** The library status bar carries no play readouts with no game running; the
//! Original mode card says "nothing added" once; an art-less library card is a console-coloured
//! tile that carries the title; the Game settings window opens below the menu bar.
//!
//! One test function: `RETROFORGE_CONFIG_DIR` is process-global.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::library::{Console, EntryIdentity, LibraryView};
use retroforge::quick_menu::Section;

const NES: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

fn run_frames(h: &mut Harness<'_, RetroForgeApp>, target: u64) {
    let start = Instant::now();
    while h.state().frame_count_for_test() < target && start.elapsed() < Duration::from_secs(60) {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
}

fn write_fixture_rom(dir: &Path, name: &str) -> PathBuf {
    let mut data = vec![0u8; 16 + 0x4000 + 0x2000];
    data[0..4].copy_from_slice(b"NES\x1a");
    data[4] = 1;
    data[5] = 1;
    let prg = &mut data[16..16 + 0x4000];
    let code: &[u8] = &[
        0xA9, 0x1E, 0x8D, 0x01, 0x20, 0xEE, 0x00, 0x00, 0x4C, 0x05, 0x80,
    ];
    prg[..code.len()].copy_from_slice(code);
    prg[0x3FFC] = 0x00;
    prg[0x3FFD] = 0x80;
    let path = dir.join(name);
    std::fs::write(&path, &data).expect("write fixture rom");
    path
}

fn count_contains(h: &Harness<'_, RetroForgeApp>, needle: &str) -> usize {
    h.query_all_by_label_contains(needle).count()
}

#[test]
fn polish_library_status_original_card_and_placeholder_tile() {
    let dir = std::env::temp_dir().join(format!("retroforge_w42_07_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let games = dir.join("games");
    std::fs::create_dir_all(&games).expect("games");
    write_fixture_rom(&games, "Tile Test Game.nes");

    let mut h = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    h.run_steps(2);
    h.state_mut().set_library_roots_for_test(vec![games]);
    h.state_mut().set_library_view_for_test(LibraryView::Grid);
    h.run_steps(4);

    // ---- 1. Library, no game: no play readouts ------------------------
    assert!(h.query_by_label_contains("Tile Test Game").is_some());
    for needle in ["fps", "A/V", "a/v"] {
        assert_eq!(
            count_contains(&h, needle),
            0,
            "no {needle:?} readout in the library status bar"
        );
    }

    // ---- 3. The placeholder tile carries the title --------------------
    let ctx = egui::Context::default();
    let tokens = retroforge::theme::Tokens::dark();
    let mut drawn = String::new();
    let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
        let rect = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(180.0, 120.0));
        let identity = EntryIdentity::Recognized {
            console: Console::Nes,
            normalized_sha256: "0".repeat(64),
        };
        drawn = retroforge::theme::paint_placeholder_tile(
            ui.painter(),
            rect,
            &tokens,
            false,
            &identity,
            "Tile Test Game",
        );
    });
    assert!(drawn.contains("Tile Test Game"), "{drawn:?}");
    assert!(drawn.contains("NES"), "{drawn:?}");

    // ---- 2. Original mode: "Nothing added" at most once --------------
    let rom = Path::new(env!("CARGO_MANIFEST_DIR")).join(NES);
    h.state_mut().launch_rom(&rom);
    run_frames(&mut h, 10);
    h.state_mut()
        .open_quick_menu_for_test(Section::Enhancements);
    h.run_steps(4);
    assert!(
        count_contains(&h, "othing added") <= 1,
        "the Original card says it once"
    );
    assert!(
        !retroforge::game_settings::Mode::Accuracy
            .player_blurb()
            .to_lowercase()
            .contains("nothing added"),
        "the blurb leaves 'nothing added' to the count line"
    );

    // ---- 5. Game settings opens below the menu bar ---------------------
    h.key_press(egui::Key::Escape);
    h.run_steps(4);
    h.state_mut().open_game_settings_for_test();
    h.run_steps(4);
    let win = h
        .ctx
        .memory(|m| m.area_rect(egui::Id::new("game_settings_window")))
        .expect("Game settings window is open");
    assert!(win.top() >= 40.0, "below the menu bar, got {win:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
