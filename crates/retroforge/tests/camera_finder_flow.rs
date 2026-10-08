//! **Find the camera while you play** (ticket W27-02). A copy of the
//! RF-Scroller-S fixture no profile knows is played — Start, then Right
//! held — until the finder is sure; "Use it" saves the camera into a new
//! profile, and the address is the fixture's own `camera_x` (`$04`, two
//! bytes: `fixtures/snes/rf-scroller-s/FORMAT.md`'s RAM table).
//!
//! The SNES fixture, not the NES one: RF-Scroller's player only walks
//! right and its level scrolls 8 px every 8 frames, about a hundred
//! scrolling frames in all — under the finder's bar, by design (the
//! census found it, `$602B`, at 99 of 102, too few to be sure of).

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use retroforge::app::RetroForgeApp;

const FIXTURE: &str = "../../fixtures/snes/rf-scroller-s/build/rf-scroller-s.sfc";

fn key(h: &mut Harness<'_, RetroForgeApp>, key: egui::Key, pressed: bool) {
    h.input_mut().events.push(egui::Event::Key {
        key,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    });
}

fn frames(h: &mut Harness<'_, RetroForgeApp>, n: u64) {
    let target = h.state().frame_count_for_test() + n;
    let start = Instant::now();
    while h.state().frame_count_for_test() < target && start.elapsed() < Duration::from_secs(30) {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn the_camera_is_found_in_play_and_saved() {
    let dir = std::env::temp_dir().join(format!("retroforge_camfind_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
        std::env::set_var(
            "RETROFORGE_PROFILES_DIR",
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles"),
        );
    }
    let mut bytes =
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE)).expect("fixture");
    // The last byte (padding past the program): a dump no profile knows
    // that runs the same code.
    let last = bytes.len() - 1;
    bytes[last] ^= 0xFF;
    let rom = dir.join("rf-other.sfc");
    std::fs::write(&rom, &bytes).expect("write");

    let mut h = Harness::builder()
        .with_size(egui::vec2(1000.0, 900.0))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    h.run();
    h.state_mut().launch_rom(&rom);
    frames(&mut h, 10);
    assert_eq!(
        h.state().camera_search_for_test(),
        (Some(0), false),
        "looking, nothing yet"
    );

    // Start arms the finder (a title that scrolls must not count); then
    // walk right and keep walking.
    key(&mut h, egui::Key::Enter, true);
    frames(&mut h, 6);
    key(&mut h, egui::Key::Enter, false);
    key(&mut h, egui::Key::ArrowRight, true);
    let start = Instant::now();
    while !h.state().camera_search_for_test().1 && start.elapsed() < Duration::from_secs(60) {
        frames(&mut h, 30);
    }
    assert!(
        h.state().camera_search_for_test().1,
        "found: {:?}",
        h.state().camera_search_for_test()
    );
    h.get_by_label("Use it").click();
    h.run_steps(3);
    let text = std::fs::read_to_string(dir.join("retroforge/profiles/snes/rf-other/profile.toml"))
        .expect("the profile");
    let p = rf_profiles::load_str(&text).expect("loads").profile;
    let x = p.camera.and_then(|c| c.x).expect("a camera x");
    assert_eq!(
        (x.addr, x.ty.as_str()),
        (0x7E_0004, "u16"),
        "the fixture's camera_x: {text}"
    );
    assert_eq!(
        h.state().camera_search_for_test(),
        (None, false),
        "stopped looking"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
