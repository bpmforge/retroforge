//! End-to-end tests with an in-process fake RetroAchievements server: no
//! sockets, no files. The canned JSON follows the response parsers in
//! `vendor/rcheevos/src/rapi/` (field names read from there).

use std::cell::Cell;

use rf_achievements::{AchievementEvent, Achievements, Console, PendingRequest};

const HASH: &str = "0123456789abcdef0123456789abcdef";

fn login_ok() -> String {
    r#"{"Success":true,"User":"Bob","Token":"tok123","Score":100,"SoftcoreScore":5,"Messages":0}"#
        .to_string()
}

fn game_json(console_id: u32, mem_addr: &str) -> String {
    format!(
        r#"{{"Success":true,"GameId":1234,"Title":"Test Game","ConsoleId":{console_id},
"ImageIconUrl":"https://media.retroachievements.org/Images/000001.png",
"RichPresencePatch":"","Sets":[{{"AchievementSetId":9,"GameId":1234,"Title":"Test Game",
"Type":"core","ImageIconUrl":"https://media.retroachievements.org/Images/000001.png",
"Achievements":[{{"ID":101,"Title":"Five Alive","Description":"Make the counter reach 5",
"Flags":3,"Points":5,"MemAddr":"{mem_addr}","Author":"tester","BadgeName":"00001",
"Created":1700000000,"Modified":1700000000,"Type":""}}],"Leaderboards":[]}}]}}"#
    )
}

fn param<'a>(r: &'a PendingRequest, key: &str) -> Option<&'a str> {
    r.post_data
        .as_deref()?
        .split('&')
        .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
}

/// Answers each request like the real server would; records every request.
struct Server {
    game: String,
    award_status: Option<i32>, // None = normal answer, Some(-2) = network failure
    log: Vec<PendingRequest>,
}

impl Server {
    fn new(game: String) -> Self {
        Server {
            game,
            award_status: None,
            log: Vec::new(),
        }
    }

    fn pump(&mut self, ach: &mut Achievements) {
        // Bounded: login -> sets -> session -> pings is at most a few rounds.
        for _ in 0..10 {
            let reqs = ach.take_requests();
            if reqs.is_empty() {
                return;
            }
            for r in reqs {
                let which = param(&r, "r").unwrap_or("").to_string();
                self.log.push(r.clone());
                match which.as_str() {
                    "login2" => assert!(ach.complete(r.id, 200, login_ok().as_bytes())),
                    "achievementsets" => {
                        assert!(ach.complete(r.id, 200, self.game.as_bytes()))
                    }
                    "startsession" => assert!(ach.complete(
                        r.id,
                        200,
                        br#"{"Success":true,"Unlocks":[],"HardcoreUnlocks":[],"ServerNow":1700000000}"#
                    )),
                    "awardachievement" => match self.award_status {
                        Some(_) => assert!(ach.fail(r.id, true, "offline")),
                        None => assert!(ach.complete(
                            r.id,
                            200,
                            br#"{"Success":true,"Score":105,"SoftcoreScore":5,"AchievementID":101,"AchievementsRemaining":0}"#
                        )),
                    },
                    _ => assert!(ach.complete(r.id, 200, br#"{"Success":true}"#)),
                }
            }
        }
        panic!("request loop did not settle");
    }
}

/// Sign in and load the game, then run until it is live.
fn start(
    console: Console,
    game: String,
    hardcore: bool,
    peek: &dyn Fn(u32) -> u8,
) -> (Achievements, Server) {
    let mut ach = Achievements::new();
    let mut srv = Server::new(game);
    ach.set_hardcore(hardcore);
    ach.login_with_token("Bob", "tok123");
    srv.pump(&mut ach);
    ach.load_game(console, HASH);
    srv.pump(&mut ach);
    // The load completes inside the first do_frame (no background reads).
    ach.do_frame(peek);
    srv.pump(&mut ach);
    (ach, srv)
}

fn unlocks(events: &[AchievementEvent]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, AchievementEvent::Unlocked { .. }))
        .count()
}

#[test]
fn nes_unlock_flow() {
    let counter = Cell::new(0u8);
    // RA NES $0010 is CPU $0010 (work RAM).
    let peek = |a: u32| if a == 0x10 { counter.get() } else { 0 };
    let (mut ach, mut srv) = start(Console::Nes, game_json(7, "0xH0010=5"), false, &peek);

    let ev = ach.drain_events();
    assert!(
        ev.iter().any(
            |e| matches!(e, AchievementEvent::LoginSucceeded { username, token, .. }
            if username == "Bob" && token.expose() == "tok123")
        ),
        "{ev:?}"
    );
    assert!(
        ev.iter().any(|e| matches!(
            e,
            AchievementEvent::GameLoaded {
                game_id: 1234,
                achievements: 1,
                ..
            }
        )),
        "{ev:?}"
    );
    assert!(ach.is_game_loaded());
    let list = ach.achievements();
    assert_eq!(list.len(), 1);
    assert_eq!((list[0].id, list[0].points), (101, 5));
    assert_eq!(list[0].state, 1, "active, address valid");
    assert!(!list[0].unlocked_softcore);

    // The startsession request carried the hash and softcore (h=0).
    let start_req = srv
        .log
        .iter()
        .find(|r| param(r, "r") == Some("startsession"))
        .unwrap();
    assert_eq!(param(start_req, "m"), Some(HASH));
    assert_eq!(param(start_req, "h"), Some("0"));

    // Not 5 yet: many frames, no unlock, no award request.
    for _ in 0..30 {
        counter.set(3);
        ach.do_frame(&peek);
        srv.pump(&mut ach);
    }
    assert_eq!(unlocks(&ach.drain_events()), 0);
    assert!(srv
        .log
        .iter()
        .all(|r| param(r, "r") != Some("awardachievement")));

    // Reach 5: unlock + award request.
    counter.set(5);
    ach.do_frame(&peek);
    let ev = ach.drain_events();
    assert!(
        ev.iter().any(
            |e| matches!(e, AchievementEvent::Unlocked { id: 101, title, points: 5, .. }
            if title == "Five Alive")
        ),
        "{ev:?}"
    );
    let reqs = ach.take_requests();
    let award = reqs
        .iter()
        .find(|r| param(r, "r") == Some("awardachievement"))
        .expect("award request");
    assert_eq!(award.url, "https://retroachievements.org/dorequest.php");
    assert_eq!(
        award.content_type.as_deref(),
        Some("application/x-www-form-urlencoded")
    );
    assert_eq!(param(award, "u"), Some("Bob"));
    assert_eq!(param(award, "t"), Some("tok123"));
    assert_eq!(param(award, "a"), Some("101"));
    assert_eq!(param(award, "h"), Some("0"));
    assert_eq!(param(award, "m"), Some(HASH));
    assert!(
        param(award, "v").is_some_and(|v| v.len() == 32),
        "md5 signature"
    );
    for r in reqs {
        ach.complete(r.id, 200, br#"{"Success":true,"Score":105,"SoftcoreScore":5,"AchievementID":101,"AchievementsRemaining":0}"#);
    }
    assert!(ach.achievements()[0].unlocked_softcore);

    // No second unlock on later frames.
    ach.do_frame(&peek);
    assert_eq!(unlocks(&ach.drain_events()), 0);
}

#[test]
fn snes_ra_address_maps_to_wram() {
    // RA $000100 = CPU $7E0100.
    let counter = Cell::new(0u8);
    let peek = |a: u32| if a == 0x7E_0100 { counter.get() } else { 0 };
    let (mut ach, mut srv) = start(Console::Snes, game_json(3, "0xH000100=7"), false, &peek);
    assert_eq!(ach.achievements()[0].state, 1);
    // rcheevos only arms a trigger after seeing it false once.
    ach.do_frame(&peek);
    counter.set(7);
    ach.do_frame(&peek);
    srv.pump(&mut ach);
    let ev = ach.drain_events();
    assert_eq!(unlocks(&ev), 1);
}

#[test]
fn unmapped_address_disables_achievement() {
    // SNES cartridge RAM ($020000) cannot be served: rcheevos must disable
    // the achievement rather than read garbage.
    let peek = |_a: u32| 5u8;
    let (mut ach, _srv) = start(Console::Snes, game_json(3, "0xH020000=5"), false, &peek);
    assert_eq!(ach.achievements()[0].state, 3, "disabled");
    ach.do_frame(&peek);
    assert_eq!(unlocks(&ach.drain_events()), 0);
}

#[test]
fn hardcore_flag_reaches_the_server() {
    let counter = Cell::new(0u8);
    let peek = |a: u32| if a == 0x10 { counter.get() } else { 0 };
    let (mut ach, mut srv) = start(Console::Nes, game_json(7, "0xH0010=5"), true, &peek);
    assert!(ach.hardcore());
    let start_req = srv
        .log
        .iter()
        .find(|r| param(r, "r") == Some("startsession"))
        .unwrap();
    assert_eq!(param(start_req, "h"), Some("1"));
    ach.do_frame(&peek);
    counter.set(5);
    ach.do_frame(&peek);
    let reqs = ach.take_requests();
    let award = reqs
        .iter()
        .find(|r| param(r, "r") == Some("awardachievement"))
        .expect("award");
    assert_eq!(param(award, "h"), Some("1"));
    srv.log.extend(reqs);
}

#[test]
fn hardcore_enable_mid_game_requests_reset() {
    let peek = |_a: u32| 0u8;
    let (mut ach, _srv) = start(Console::Nes, game_json(7, "0xH0010=5"), false, &peek);
    ach.drain_events();
    ach.set_hardcore(true);
    assert!(ach.hardcore());
    assert!(ach
        .drain_events()
        .contains(&AchievementEvent::ResetRequested));
    ach.reset(&peek);
}

#[test]
fn network_failure_on_award_raises_disconnected() {
    let counter = Cell::new(0u8);
    let peek = |a: u32| if a == 0x10 { counter.get() } else { 0 };
    let (mut ach, mut srv) = start(Console::Nes, game_json(7, "0xH0010=5"), false, &peek);
    ach.do_frame(&peek);
    srv.award_status = Some(-2);
    counter.set(5);
    ach.do_frame(&peek);
    // First retry is immediate, the second failure schedules a backoff and
    // queues the Disconnected event, raised on the next processing pass.
    srv.pump(&mut ach);
    ach.idle(&peek);
    let ev = ach.drain_events();
    assert_eq!(unlocks(&ev), 1, "{ev:?}");
    assert!(ev.contains(&AchievementEvent::Disconnected), "{ev:?}");
}

#[test]
fn garbage_server_json_is_an_error_event_not_a_panic() {
    let peek = |_a: u32| 0u8;

    // Garbage login answer.
    let mut ach = Achievements::new();
    ach.login_with_token("Bob", "tok123");
    for r in ach.take_requests() {
        assert!(ach.complete(r.id, 200, b"<html>502 bad gateway</html>{{{"));
    }
    let ev = ach.drain_events();
    assert!(
        ev.iter()
            .any(|e| matches!(e, AchievementEvent::LoginFailed { .. })),
        "{ev:?}"
    );

    // Valid login, garbage game data.
    let mut ach = Achievements::new();
    let mut srv = Server::new("not json at all \u{0}\u{1}{".to_string());
    ach.login_with_token("Bob", "tok123");
    srv.pump(&mut ach);
    ach.load_game(Console::Nes, HASH);
    srv.pump(&mut ach);
    ach.do_frame(&peek);
    let ev = ach.drain_events();
    assert!(
        ev.iter()
            .any(|e| matches!(e, AchievementEvent::GameLoadFailed { .. })),
        "{ev:?}"
    );
    assert!(!ach.is_game_loaded());

    // Empty body and a client-side failure must not panic either.
    let mut ach = Achievements::new();
    ach.login_with_token("Bob", "tok123");
    let id = ach.take_requests()[0].id;
    assert!(ach.fail(id, false, "connection refused"));
    assert!(ach
        .drain_events()
        .iter()
        .any(|e| matches!(e, AchievementEvent::LoginFailed { .. })));
}

#[test]
fn unknown_or_repeated_request_ids_are_rejected() {
    let mut ach = Achievements::new();
    ach.login_with_token("Bob", "tok123");
    let id = ach.take_requests()[0].id;
    assert!(!ach.complete(id + 99, 200, b"{}"));
    assert!(ach.complete(id, 200, login_ok().as_bytes()));
    assert!(
        !ach.complete(id, 200, login_ok().as_bytes()),
        "double completion"
    );
}

#[test]
fn dropping_with_requests_in_flight_is_clean() {
    let mut ach = Achievements::new();
    ach.login_with_token("Bob", "tok123");
    drop(ach);
}

#[test]
fn nul_in_arguments_is_an_error_event() {
    let mut ach = Achievements::new();
    ach.login_with_password("Bo\0b", "x");
    assert!(matches!(
        ach.drain_events()[0],
        AchievementEvent::LoginFailed { .. }
    ));
}

/// Regression: rcheevos 12.5.0 crashed unloading a game whose "completed"
/// event was still pending (see `Achievements::flush_pending_events`).
#[test]
fn award_then_drop_without_a_frame() {
    let counter = Cell::new(0u8);
    let peek = |a: u32| if a == 0x10 { counter.get() } else { 0 };
    let (mut ach, mut srv) = start(Console::Nes, game_json(7, "0xH0010=5"), false, &peek);
    ach.do_frame(&peek);
    counter.set(5);
    ach.do_frame(&peek);
    srv.pump(&mut ach);
    drop(ach);
}

#[test]
fn unload_and_reload_after_completion() {
    let counter = Cell::new(0u8);
    let peek = |a: u32| if a == 0x10 { counter.get() } else { 0 };
    let (mut ach, mut srv) = start(Console::Nes, game_json(7, "0xH0010=5"), false, &peek);
    ach.do_frame(&peek);
    counter.set(5);
    ach.do_frame(&peek);
    srv.pump(&mut ach);
    ach.unload_game();
    assert!(!ach.is_game_loaded());
    assert!(ach
        .drain_events()
        .contains(&AchievementEvent::GameCompleted));
}
