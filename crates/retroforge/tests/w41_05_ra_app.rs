//! **W41-05: the app side of RetroAchievements** — off by default and silent, the stored token is
//! picked up, the badge names the mode, and turning it off stops everything. A fake HTTP side
//! answers; nothing reaches the real server.

use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::RetroForgeApp;
use retroforge::ra::{OutboundRequest, StoredLogin, TokenStore};

const FIXTURE: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

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
"Achievements":[{"ID":101,"Title":"Five Alive","Description":"d","Flags":3,"Points":5,
"MemAddr":"0xH0600=5","Author":"t","BadgeName":"00001","Created":1700000000,"Modified":1700000000,"Type":""}],
"Leaderboards":[]}]}"#.to_vec(),
        "startsession" => br#"{"Success":true,"Unlocks":[],"HardcoreUnlocks":[],"ServerNow":1700000000}"#.to_vec(),
        _ => br#"{"Success":true}"#.to_vec(),
    }
}

fn step_until(
    h: &mut Harness<'_, RetroForgeApp>,
    wire: &mpsc::Receiver<OutboundRequest>,
    seen: &mut Vec<String>,
    mut done: impl FnMut(&RetroForgeApp) -> bool,
) {
    let start = Instant::now();
    while !done(h.state()) {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "timed out; wire saw {seen:?}"
        );
        while let Ok(r) = wire.try_recv() {
            seen.push(param(&r, "r").unwrap_or("?").to_string());
            h.state()
                .ra_respond_for_test(r.id, answer(param(&r, "r").unwrap_or("")));
        }
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
}

#[test]
fn off_by_default_silent_then_softcore_hardcore_and_off_again() {
    let dir = std::env::temp_dir().join(format!("retroforge_w41_05_app_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // A sign-in from an earlier session is on disk; RetroAchievements itself is off.
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
        .with_size(egui::vec2(900.0, 900.0))
        .build_eframe(|cc| RetroForgeApp::new(cc));
    h.run();
    h.state_mut().use_ra_transport_for_test(tx);
    assert_eq!(
        h.state().ra_state_for_test().3.as_deref(),
        Some("Bob"),
        "stored user is read"
    );

    let mut seen = Vec::new();
    h.state_mut().launch_rom(&rom);
    let from = h.state().frame_count_for_test();
    step_until(&mut h, &wire, &mut seen, |a| {
        a.frame_count_for_test() >= from + 60
    });
    assert!(seen.is_empty(), "RA off: nothing on the wire, saw {seen:?}");
    assert!(!h.state().status_badge().contains("RA:"));

    // Turn it on: sign in with the stored token, load the game, badge says softcore.
    h.state_mut().set_ra_for_test(true, false);
    step_until(&mut h, &wire, &mut seen, |a| a.ra_state_for_test().0);
    assert!(
        h.state().status_badge().ends_with(" \u{b7} RA: softcore"),
        "{}",
        h.state().status_badge()
    );
    assert!(
        seen.iter().any(|r| r == "login2") && seen.iter().any(|r| r == "achievementsets"),
        "{seen:?}"
    );

    // Hardcore: the core says yes, the badge follows.
    h.state_mut().set_ra_for_test(true, true);
    step_until(&mut h, &wire, &mut seen, |a| a.ra_state_for_test().1);
    assert!(
        h.state().status_badge().ends_with(" \u{b7} RA: hardcore"),
        "{}",
        h.state().status_badge()
    );

    // Off again: the badge clears and the wire goes quiet.
    h.state_mut().set_ra_for_test(false, false);
    let n = seen.len();
    let from = h.state().frame_count_for_test();
    step_until(&mut h, &wire, &mut seen, |a| {
        a.frame_count_for_test() >= from + 60
    });
    assert!(
        !h.state().status_badge().contains("RA:"),
        "{}",
        h.state().status_badge()
    );
    assert_eq!(seen.len(), n, "no further requests: {seen:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
