//! **Ticket W33-07: "Find scout settings for this game" saves to the PLAYER's
//! profile, and only there.**
//!
//! The finder's answer is offered on a card; "Save scout settings" writes the
//! `[scout]` table. A game whose profile ships with the app (here the
//! RF-Scroller-S fixture's, in the repo's `profiles/`) must not be touched:
//! the table goes into a copy in the player's own profile folder, which the
//! loader searches first. A game with no profile gets a new one.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use retroforge::app::RetroForgeApp;
use retroforge::quick_menu::Section;
use rf_enhance::scout_finder::{Row, Skeleton, SOURCE};

const FIXTURE: &str = "../../fixtures/snes/rf-scroller-s/build/rf-scroller-s.sfc";

fn frames(h: &mut Harness<'_, RetroForgeApp>, n: u64) {
    let target = h.state().frame_count_for_test() + n;
    let start = Instant::now();
    while h.state().frame_count_for_test() < target && start.elapsed() < Duration::from_secs(30) {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn skeleton() -> Skeleton {
    let row = |role, addr, value| Row {
        role,
        addr,
        value,
        notes: "found".into(),
        confidence: 0.9,
    };
    Skeleton {
        strategy: "player_move",
        step_px: 4,
        frame_budget: 3000,
        stall_frames: 60,
        settle_frames: 60,
        dirs: vec!["right", "left"],
        rows: vec![
            row("camera_x", 0x7E_0004, None),
            row("player_x", 0x7E_0010, None),
            row("hold", 0x7E_0012, Some(40)),
        ],
        confidence: 0.9,
    }
}

#[test]
fn the_scout_table_goes_to_the_players_profile_never_the_shipped_one() {
    let dir = std::env::temp_dir().join(format!("retroforge_scoutsave_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    let shipped_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
        std::env::set_var("RETROFORGE_PROFILES_DIR", &shipped_root);
    }
    let shipped = shipped_root.join("snes/rf-scroller-s/profile.toml");
    let before = std::fs::read(&shipped).expect("the shipped profile");
    let rom = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);

    let mut h = Harness::builder()
        .with_size(egui::vec2(1000.0, 900.0))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    h.run();
    h.state_mut().launch_rom(&rom);
    frames(&mut h, 10);
    assert_eq!(h.state().scout_find_for_test(), (false, false));

    // The action is in Enhancements, starts the finder and stops it again.
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    h.get_by_label(&Section::Enhancements.rail_text()).click();
    h.run_steps(3);
    h.get_by_label("Find scout settings for this game").click();
    h.run_steps(3);
    assert_eq!(h.state().scout_find_for_test(), (true, false), "looking");
    h.get_by_label("Stop").click();
    h.run_steps(3);
    assert_eq!(h.state().scout_find_for_test(), (false, false), "stopped");
    h.key_press(egui::Key::Escape);
    h.run_steps(2);

    h.state_mut().offer_scout_for_test(skeleton());
    h.run_steps(3);
    assert_eq!(h.state().scout_find_for_test(), (false, true));
    h.get_by_label("Save scout settings").click();
    h.run_steps(3);

    // The shipped file is untouched...
    assert_eq!(
        std::fs::read(&shipped).expect("still there"),
        before,
        "the shipped profile was written to"
    );
    // ...and the player's copy has the table, with every row's source.
    let user = dir.join("retroforge/profiles/snes/rf-scroller-s/profile.toml");
    let text = std::fs::read_to_string(&user).expect("the player's profile");
    let scout = rf_profiles::load_str(&text)
        .expect("loads")
        .profile
        .scout
        .expect("a [scout] table");
    assert_eq!(scout.strategy, "player_move");
    assert_eq!(scout.addr.len(), 3);
    assert!(scout
        .addr
        .iter()
        .all(|r| r.source.as_deref() == Some(SOURCE)));
    assert_eq!(
        h.state().scout_find_for_test(),
        (false, false),
        "the card is gone"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
