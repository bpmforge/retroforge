//! **Screenshots of the Rebuild card** (ticket W37-03): the Enhancements panel
//! with Rebuild on, for Super Mario Bros. (NES) and Super Mario World (SNES).
//!
//! Not a correctness test (`w37_03_rebuild_card.rs` is). Real commercial ROMs
//! never live in git (law 5): only the rendered PNGs are committed, to
//! `docs/screens/w37/`. Each shot is the whole window as the player sees it,
//! after the game was played for a while with Rebuild on, then the Quick Menu
//! opened on Enhancements. The verification line shows whatever record the
//! lockstep tools stored for that ROM in the per-ROM cache; nothing is seeded
//! here (run the W37-02 census with `RF_W37_RECORD` first to have one).
//!
//! ```text
//! cargo test --release -p retroforge --test w37_03_screens -- --ignored --nocapture
//! ```
//!
//! One test per binary: `RETROFORGE_CONFIG_DIR` is process-global.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};
use retroforge::core_thread::CoreCommand;
use retroforge::game_settings::Mode;
use retroforge::quick_menu::Section;
use retroforge::rebuild::RebuildStatus;

type H = Harness<'static, RetroForgeApp>;

fn rom(console: &str, file: &str) -> Option<PathBuf> {
    let root = std::env::var("RF_SCREENS_ROMS_DIR")
        .unwrap_or_else(|_| "/Users/bmatthews/Games/Roms".to_string());
    let p = Path::new(&root).join(console).join(file);
    p.exists().then_some(p)
}

fn run_n(h: &mut H, n: usize) {
    for _ in 0..n {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
}

fn run_frames(h: &mut H, target: u64, deadline: Duration) {
    let start = Instant::now();
    while h.state().frame_count_for_test() < target && start.elapsed() < deadline {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
}

fn key_event(h: &mut H, key: egui::Key, pressed: bool) {
    h.input_mut().events.push(egui::Event::Key {
        key,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    });
}

fn tap(h: &mut H, key: egui::Key, frames: u64) {
    key_event(h, key, true);
    let t = h.state().frame_count_for_test() + frames;
    run_frames(h, t, Duration::from_secs(60));
    key_event(h, key, false);
    let t = h.state().frame_count_for_test() + 8;
    run_frames(h, t, Duration::from_secs(60));
}

/// Advance `frames` core frames with pacing off.
fn fast(h: &mut H, frames: u64) {
    h.state()
        .send_core_command_for_test(CoreCommand::SetPacingEnabled(false));
    let end = h.state().frame_count_for_test() + frames;
    run_frames(h, end, Duration::from_secs(300));
    h.state()
        .send_core_command_for_test(CoreCommand::SetPacingEnabled(true));
}

fn shoot(h: &mut H, name: &str) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/screens/w37");
    std::fs::create_dir_all(&dir).expect("screens dir");
    let img = h.render().expect("renders (needs a GPU)");
    let path = dir.join(format!("{name}.png"));
    img.save(&path).expect("write png");
    println!("SHOT {}", path.display());
}

/// Play with Rebuild on, then open Enhancements and photograph the card.
fn card_shot(h: &mut H, name: &str, play: impl Fn(&mut H)) {
    h.state_mut().set_mode_for_test(Mode::Enhanced);
    h.state_mut().set_rebuild_for_test(true);
    play(h);
    let t = Instant::now();
    while !matches!(h.state().rebuild_status_for_test(), RebuildStatus::On(_))
        && t.elapsed() < Duration::from_secs(60)
    {
        run_n(h, 2);
    }
    // The grow job runs five seconds of play after the first install.
    fast(h, 900);
    h.key_press(egui::Key::Escape);
    run_n(h, 3);
    h.get_by_label(&Section::Enhancements.rail_text()).click();
    run_n(h, 3);
    h.get_by_role_and_label(egui::accesskit::Role::CheckBox, "Rebuild")
        .scroll_to_me();
    run_n(h, 4);
    println!("{name}: {:?}", h.state().rebuild_status_for_test());
    shoot(h, name);
    h.key_press(egui::Key::Escape);
    run_n(h, 3);
}

#[test]
#[ignore = "needs the ROM library (RF_SCREENS_ROMS_DIR) and a GPU"]
fn rebuild_card_for_smb_and_smw() {
    let dir = std::env::temp_dir().join(format!("retroforge_w37_03_shots_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let mut h: H = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0] + 200.0, WINDOW_SIZE[1] + 380.0))
        .renderer(egui_kittest::wgpu::WgpuTestRenderer::default())
        .build_eframe(|cc| RetroForgeApp::new(cc));
    h.run();
    let want = std::env::var("RF_SCREENS_ONLY").unwrap_or_default();

    if let (true, Some(path)) = (
        "smb".contains(&want),
        rom("nes", "Super Mario Bros. + Duck Hunt (USA).zip"),
    ) {
        h.state_mut().open_rom_path(&path);
        run_n(&mut h, 3);
        h.state_mut().resume_for_test();
        run_frames(&mut h, 20, Duration::from_secs(60));
        card_shot(&mut h, "rebuild-card-smb", |h| {
            fast(h, 200);
            tap(h, egui::Key::Enter, 6);
            fast(h, 200);
            tap(h, egui::Key::Enter, 6);
            fast(h, 300);
            key_event(h, egui::Key::ArrowRight, true);
            fast(h, 400);
            key_event(h, egui::Key::ArrowRight, false);
        });
    }

    if let (true, Some(path)) = (
        "smw".contains(&want),
        rom("snes", "Super Mario World (USA).zip"),
    ) {
        h.state_mut().open_rom_path(&path);
        run_n(&mut h, 3);
        h.state_mut().resume_for_test();
        run_frames(&mut h, 20, Duration::from_secs(60));
        card_shot(&mut h, "rebuild-card-smw", |h| {
            fast(h, 200);
            for i in 0..6 {
                tap(
                    h,
                    if i % 2 == 0 {
                        egui::Key::Enter
                    } else {
                        egui::Key::X
                    },
                    6,
                );
                fast(h, 100);
            }
            fast(h, 300);
        });
    }
}
