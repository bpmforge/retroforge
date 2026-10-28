//! **W41-06: the RetroAchievements UI** — the Achievements section of the Quick Menu and the
//! unlock toast, driven by a fake HTTP side (nothing reaches the real server). One test per
//! binary that sets the config folder: this one signs in with a stored token.

use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;
use retroforge::app::RetroForgeApp;
use retroforge::cheats::{mod_id, CheatSet, RamCheat};
use retroforge::core_thread::CoreCommand;
use retroforge::quick_menu::Section;
use retroforge::ra::{OutboundRequest, StoredLogin, TokenStore};
use rf_core_api::RamWrite;

const FIXTURE: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";
const RAM_ADDR: u32 = 0x0600;

fn param<'a>(r: &'a OutboundRequest, key: &str) -> Option<&'a str> {
    r.post_data
        .as_deref()?
        .split('&')
        .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
}

fn answer(which: &str) -> Vec<u8> {
    match which {
        "login2" => br#"{"Success":true,"User":"Bob","Token":"tok123","Score":100,"SoftcoreScore":5,"Messages":0}"#.to_vec(),
        "achievementsets" => br#"{"Success":true,"GameId":1234,"Title":"Test Game","ConsoleId":7,
"ImageIconUrl":"https://media.retroachievements.org/Images/000001.png","RichPresencePatch":"",
"Sets":[{"AchievementSetId":9,"GameId":1234,"Title":"Test Game","Type":"core",
"ImageIconUrl":"https://media.retroachievements.org/Images/000001.png",
"Achievements":[{"ID":101,"Title":"Five Alive","Description":"Make the counter reach 5","Flags":3,"Points":5,
"MemAddr":"0xH0600=5","Author":"t","BadgeName":"00001","Created":1700000000,"Modified":1700000000,"Type":""}],
"Leaderboards":[]}]}"#.to_vec(),
        "startsession" => br#"{"Success":true,"Unlocks":[],"HardcoreUnlocks":[],"ServerNow":1700000000}"#.to_vec(),
        "awardachievement" => br#"{"Success":true,"Score":105,"SoftcoreScore":5,"AchievementID":101,"AchievementsRemaining":0}"#.to_vec(),
        _ => br#"{"Success":true}"#.to_vec(),
    }
}

fn step_until(
    h: &mut Harness<'_, RetroForgeApp>,
    wire: &mpsc::Receiver<OutboundRequest>,
    mut done: impl FnMut(&RetroForgeApp) -> bool,
) {
    let start = Instant::now();
    while !done(h.state()) {
        assert!(start.elapsed() < Duration::from_secs(30), "timed out");
        while let Ok(r) = wire.try_recv() {
            h.state()
                .ra_respond_for_test(r.id, answer(param(&r, "r").unwrap_or("")));
        }
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
}

fn five() -> CheatSet {
    let id = mod_id(&format!("{RAM_ADDR:04X}:05"));
    CheatSet {
        ram: vec![RamCheat {
            id_index: 0,
            write: RamWrite {
                addr: RAM_ADDR,
                value: 5,
                compare: None,
            },
        }],
        ram_ids: vec![id.clone()],
        ids: vec![id],
        ..CheatSet::default()
    }
}

#[test]
fn the_panel_lists_a_locked_achievement_then_toasts_and_shows_it_unlocked() {
    let dir = std::env::temp_dir().join(format!("retroforge_w41_06_ui_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    TokenStore::save(
        &dir,
        &StoredLogin {
            username: "Bob".into(),
            token: "tok123".into(),
        },
    )
    .expect("token");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let rom = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    let (tx, wire) = mpsc::channel();
    let mut h = Harness::builder()
        .with_size(egui::vec2(1000.0, 900.0))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    h.run();
    h.state_mut().use_ra_transport_for_test(tx);
    h.state_mut().launch_rom(&rom);
    h.state_mut().set_ra_for_test(true, false);
    step_until(&mut h, &wire, |a| a.ra_list_for_test().len() == 1);

    // The panel: signed-in user, game, mode, progress, and the locked row.
    h.state_mut()
        .open_quick_menu_for_test(Section::Achievements);
    h.run_steps(3);
    assert!(h.query_by_label_contains("Test Game").is_some());
    assert!(h.query_by_label_contains("Softcore").is_some());
    assert!(h
        .query_by_label_contains("0 of 1 unlocked, 0 of 5 points")
        .is_some());
    assert!(h.query_by_label("Five Alive").is_some());
    assert!(h.query_by_label("Make the counter reach 5").is_some());
    assert!(h.query_by_label("Locked").is_some());
    assert!(
        h.query_by_label("Five Alive, 5 points, locked. Make the counter reach 5")
            .is_some(),
        "row accessible name"
    );

    // Close the menu (it resumes the game), then make the counter reach 5.
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    h.state()
        .core_command_for_test(CoreCommand::SetCheats(Box::new(five())));
    step_until(&mut h, &wire, |a| {
        a.ra_state_for_test().2 >= 1 && a.ra_list_for_test().iter().all(|i| i.unlocked_softcore)
    });
    assert!(
        h.state().toast_texts_for_test().iter().any(|t| t
            .starts_with("Achievement unlocked: Five Alive (5 pts)")
            && t.contains("Make the counter reach 5")),
        "{:?}",
        h.state().toast_texts_for_test()
    );

    h.state_mut()
        .open_quick_menu_for_test(Section::Achievements);
    h.run_steps(3);
    assert!(h.query_by_label("Unlocked").is_some());
    assert!(h
        .query_by_label_contains("1 of 1 unlocked, 5 of 5 points")
        .is_some());
    let _ = std::fs::remove_dir_all(&dir);
}
