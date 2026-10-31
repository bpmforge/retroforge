//! **W42-06: Resume and celebration.** The Resume page is a summary card with a Resume button;
//! an achievement unlock is its own card (not a toast); the HD-2D card in Original mode offers
//! "Switch to Enhanced and turn on". One test function: `RETROFORGE_CONFIG_DIR` is process-global.

use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;
use retroforge::app::RetroForgeApp;
use retroforge::cheats::{mod_id, CheatSet, RamCheat};
use retroforge::core_thread::CoreCommand;
use retroforge::game_settings::Mode;
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
fn resume_card_unlock_card_and_hd2d_mode_switch() {
    let dir = std::env::temp_dir().join(format!("retroforge_w42_06_{}", std::process::id()));
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

    // Resume page: the summary card names the mode and has a Resume button that closes the menu.
    h.state_mut().open_quick_menu_for_test(Section::Resume);
    h.run_steps(4);
    assert!(h.state().quick_menu_open_for_test());
    assert!(h.query_by_label("Original").is_some(), "mode badge");
    assert!(
        h.query_by_label("Not saved yet").is_some(),
        "no save on a fresh config"
    );
    assert!(h.query_by_label("rf-scroller").is_some(), "game title");
    let buttons = h.query_all_by_label("Resume").count();
    assert!(buttons >= 1, "a Resume control exists");
    // The card's button is the last "Resume" in the tree (the rail item comes first).
    h.query_all_by_label("Resume")
        .last()
        .expect("button")
        .click();
    h.run_steps(3);
    assert!(
        !h.state().quick_menu_open_for_test(),
        "Resume closes the menu"
    );

    // An unlock is its own card, not a toast.
    h.state()
        .core_command_for_test(CoreCommand::SetCheats(Box::new(five())));
    step_until(&mut h, &wire, |a| {
        a.ra_state_for_test().2 >= 1 && a.ra_list_for_test().iter().all(|i| i.unlocked_softcore)
    });
    h.run_steps(2);
    assert!(
        h.state()
            .achievement_cards_for_test()
            .iter()
            .any(|t| t == "Five Alive (5 pts)"),
        "{:?}",
        h.state().achievement_cards_for_test()
    );
    assert!(
        !h.state()
            .toast_texts_for_test()
            .iter()
            .any(|t| t.contains("Five Alive")),
        "not a file-style toast: {:?}",
        h.state().toast_texts_for_test()
    );

    // HD-2D in Original mode: the card offers the switch, and the click does both.
    h.state_mut().set_mode_for_test(Mode::Accuracy);
    h.state_mut()
        .open_quick_menu_for_test(Section::Enhancements);
    h.run_steps(4);
    h.get_by_label("Switch to Enhanced and turn on").click();
    h.run_steps(3);
    let settings = h.state().game_settings_for_test();
    assert_eq!(settings.mode, Mode::Enhanced);
    assert!(
        retroforge::hd2d_preset::is_applied(settings),
        "preset is on"
    );
    assert!(h.query_by_label("Switch to Enhanced and turn on").is_none());
    let _ = std::fs::remove_dir_all(&dir);
}
