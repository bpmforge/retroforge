//! **W41-05: the RetroAchievements client on the core thread** (`docs/design/CHEATS_ACHIEVEMENTS.md`
//! section 4). No network: a fake HTTP side answers the core's requests with canned JSON (the same
//! shapes `rf-achievements/tests/fake_server.rs` uses). Runs against our own CC0 RF-Scroller fixture.
//!
//! * nothing leaves the core while RetroAchievements is off;
//! * token sign-in, game lookup by the RA hash, and an unlock reach the UI message path;
//! * the password never shows in any `Debug` output or file;
//! * hardcore refuses what the policy says, and refuses to start while cheats are on.

use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use retroforge::cheats::{mod_id, CheatSet, RamCheat};
use retroforge::core_thread::{spawn, CoreCommand, CoreEvent, CoreHandle};
use retroforge::ra::{cart_of, OutboundRequest, Password, StoredLogin, TokenStore};
use rf_achievements::AchievementEvent;
use rf_core_api::RamWrite;

const NES: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";
// cc65's heap RAM: the fixture never touches it (same byte W41-03's tests use).
const RAM_ADDR: u32 = 0x0600;

fn rom() -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(NES);
    std::fs::read(&path).unwrap_or_else(|e| panic!("fixture {}: {e}", path.display()))
}

fn game_json() -> String {
    format!(
        r#"{{"Success":true,"GameId":1234,"Title":"Test Game","ConsoleId":7,
"ImageIconUrl":"https://media.retroachievements.org/Images/000001.png",
"RichPresencePatch":"","Sets":[{{"AchievementSetId":9,"GameId":1234,"Title":"Test Game",
"Type":"core","ImageIconUrl":"https://media.retroachievements.org/Images/000001.png",
"Achievements":[{{"ID":101,"Title":"Five Alive","Description":"Make the counter reach 5",
"Flags":3,"Points":5,"MemAddr":"0xH{RAM_ADDR:04X}=5","Author":"tester","BadgeName":"00001",
"Created":1700000000,"Modified":1700000000,"Type":""}}],"Leaderboards":[]}}]}}"#
    )
}

fn param<'a>(r: &'a OutboundRequest, key: &str) -> Option<&'a str> {
    r.post_data
        .as_deref()?
        .split('&')
        .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
}

/// The test side of the wire: answers each request like the RA server would.
struct Rig {
    core: CoreHandle,
    wire: Receiver<OutboundRequest>,
    log: Vec<OutboundRequest>,
    events: Vec<String>,
    achievement_events: Vec<AchievementEvent>,
    refusals: Vec<String>,
    hardcore: Option<bool>,
    paused: bool,
    frames: u64,
}

impl Rig {
    fn new() -> Self {
        let core = spawn(rom()).expect("spawns");
        let (tx, wire) = mpsc::channel();
        core.cmd_tx.send(CoreCommand::RaSetTransport(tx)).unwrap();
        Rig {
            core,
            wire,
            log: Vec::new(),
            events: Vec::new(),
            achievement_events: Vec::new(),
            refusals: Vec::new(),
            hardcore: None,
            paused: false,
            frames: 0,
        }
    }

    fn send(&self, cmd: CoreCommand) {
        self.core.cmd_tx.send(cmd).unwrap();
    }

    fn answer(&mut self, r: OutboundRequest) {
        let which = param(&r, "r").unwrap_or("").to_string();
        let body: Vec<u8> = match which.as_str() {
            "login2" => br#"{"Success":true,"User":"Bob","Token":"tok123","Score":100,"SoftcoreScore":5,"Messages":0}"#.to_vec(),
            "achievementsets" => game_json().into_bytes(),
            "startsession" => br#"{"Success":true,"Unlocks":[],"HardcoreUnlocks":[],"ServerNow":1700000000}"#.to_vec(),
            "awardachievement" => br#"{"Success":true,"Score":105,"SoftcoreScore":5,"AchievementID":101,"AchievementsRemaining":0}"#.to_vec(),
            _ => br#"{"Success":true}"#.to_vec(),
        };
        self.send(CoreCommand::RaResponse {
            id: r.id,
            status: 200,
            body,
        });
        self.log.push(r);
    }

    /// Run the wire and the UI event path until `done` says so (or 30 s).
    fn pump_until(&mut self, mut done: impl FnMut(&Rig) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !done(self) {
            assert!(
                Instant::now() < deadline,
                "timed out; events: {:?}",
                self.events
            );
            while let Ok(r) = self.wire.try_recv() {
                self.answer(r);
            }
            match self.core.evt_rx.recv_timeout(Duration::from_millis(10)) {
                Ok(CoreEvent::Frame(_)) => {
                    self.frames += 1;
                    self.core.pending_frames.fetch_sub(1, Ordering::AcqRel);
                }
                Ok(CoreEvent::Achievement(ev)) => {
                    self.events.push(format!("{ev:?}"));
                    self.achievement_events.push(ev);
                }
                Ok(CoreEvent::RaRefused(m)) => self.refusals.push(m),
                Ok(CoreEvent::RaHardcore(on)) => self.hardcore = Some(on),
                Ok(CoreEvent::RaPaused) => self.paused = true,
                Ok(CoreEvent::Crashed(r)) => panic!("core crashed: {}", r.message),
                Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(e) => panic!("core gone: {e}"),
            }
        }
    }

    fn saw(&self, f: impl Fn(&AchievementEvent) -> bool) -> bool {
        self.achievement_events.iter().any(f)
    }

    /// Give the core `ms` of real time and report what arrived (no assertions inside).
    fn settle(&mut self, ms: u64) {
        let until = Instant::now() + Duration::from_millis(ms);
        self.pump_until(|_| Instant::now() >= until);
    }

    fn sign_in_and_load(&mut self, hardcore: bool) {
        self.send(CoreCommand::RaEnable { hardcore });
        self.send(CoreCommand::RaLoginToken {
            user: "Bob".into(),
            token: "tok123".into(),
        });
        self.send(CoreCommand::Resume);
        self.pump_until(|r| r.saw(|e| matches!(e, AchievementEvent::GameLoaded { .. })));
    }

    fn finish(self) {
        self.core.cmd_tx.send(CoreCommand::Shutdown).unwrap();
        self.core.join_handle.join().unwrap();
    }
}

fn ram_set(value: u8) -> CheatSet {
    let id = mod_id(&format!("{RAM_ADDR:04X}:{value:02X}"));
    CheatSet {
        ram: vec![RamCheat {
            id_index: 0,
            write: RamWrite {
                addr: RAM_ADDR,
                value,
                compare: None,
            },
        }],
        ram_ids: vec![id.clone()],
        ids: vec![id],
        ..CheatSet::default()
    }
}

#[test]
fn nothing_leaves_the_core_while_retroachievements_is_off() {
    let mut rig = Rig::new();
    rig.send(CoreCommand::Resume);
    rig.settle(600);
    assert!(rig.log.is_empty(), "no request: {:?}", rig.log);
    assert!(rig.achievement_events.is_empty());
    // Signing in without a session is ignored too: there is no client to ask.
    rig.send(CoreCommand::RaLoginToken {
        user: "Bob".into(),
        token: "tok123".into(),
    });
    rig.settle(300);
    assert!(rig.log.is_empty() && rig.achievement_events.is_empty());
    rig.finish();
}

#[test]
fn token_sign_in_game_lookup_and_a_softcore_unlock_reach_the_ui_path() {
    let mut rig = Rig::new();
    rig.sign_in_and_load(false);

    // Identity: the game was looked up by the RA-compatible MD5 rf-cart computes.
    let md5 = cart_of(&rom()).expect("cart").md5;
    assert!(
        rig.log.iter().any(|r| param(r, "m") == Some(md5.as_str())),
        "a request carried the RA hash {md5}: {:?}",
        rig.log
    );
    let login = rig
        .log
        .iter()
        .find(|r| param(r, "r") == Some("login2"))
        .expect("login request");
    assert_eq!(param(login, "t"), Some("tok123"));
    assert!(
        login.user_agent.starts_with("RetroForge/"),
        "{}",
        login.user_agent
    );
    assert!(rig.saw(
        |e| matches!(e, AchievementEvent::LoginSucceeded { username, .. } if username == "Bob")
    ));

    // Softcore: a RAM cheat is allowed. It writes 5 into the byte the achievement watches.
    // rcheevos holds a trigger that is already true when it goes live (it waits for the
    // value to fall first), and goes live a few frames after the load: wait those out
    // before the cheat makes the value true.
    let live_at = rig.frames + 30;
    rig.pump_until(|r| r.frames >= live_at);
    rig.send(CoreCommand::SetCheats(Box::new(ram_set(5))));
    rig.pump_until(|r| r.saw(|e| matches!(e, AchievementEvent::Unlocked { id: 101, .. })));
    // ... and the award went out through the worker seam.
    rig.pump_until(|r| {
        r.log
            .iter()
            .any(|q| param(q, "r") == Some("awardachievement"))
    });
    assert!(
        rig.refusals.is_empty(),
        "softcore refuses nothing: {:?}",
        rig.refusals
    );
    rig.finish();
}

#[test]
fn the_password_is_never_in_debug_output_or_on_disk() {
    const PASSWORD: &str = "hunter2-very-secret";
    let mut rig = Rig::new();
    rig.send(CoreCommand::RaEnable { hardcore: false });
    rig.send(CoreCommand::RaLoginPassword {
        user: "Bob".into(),
        password: Password::new(PASSWORD.into()),
    });
    rig.send(CoreCommand::Resume);
    rig.pump_until(|r| r.saw(|e| matches!(e, AchievementEvent::LoginSucceeded { .. })));

    // The server (the fake) did get it: that is the one place it has to go.
    let login = rig
        .log
        .iter()
        .find(|r| param(r, "r") == Some("login2"))
        .unwrap();
    assert_eq!(param(login, "p"), Some(PASSWORD));

    // Everything the UI side sees, and the stored login, is free of it.
    rig.settle(300);
    assert!(
        rig.events.iter().all(|e| !e.contains(PASSWORD)),
        "{:?}",
        rig.events
    );
    let token = rig
        .achievement_events
        .iter()
        .find_map(|e| match e {
            AchievementEvent::LoginSucceeded {
                username, token, ..
            } => Some((username.clone(), token.expose().to_string())),
            _ => None,
        })
        .expect("login");
    assert_eq!(token.1, "tok123");
    let stored = StoredLogin {
        username: token.0,
        token: token.1,
    };
    let root = std::env::temp_dir().join(format!("rf-w41-05-pw-{}", std::process::id()));
    TokenStore::save(&root, &stored).expect("save");
    let bytes = std::fs::read(TokenStore::path(&root)).expect("read");
    let text = String::from_utf8_lossy(&bytes);
    assert!(!text.contains(PASSWORD), "{text}");
    assert!(text.contains("tok123") && text.contains("Bob"));
    assert!(!format!("{stored:?}").contains("tok123"));
    // Nothing else was written under the config root.
    let files: Vec<_> = walk(&root);
    assert_eq!(files.len(), 1, "{files:?}");
    for f in files {
        assert!(!std::fs::read_to_string(&f)
            .unwrap_or_default()
            .contains(PASSWORD));
    }
    let _ = std::fs::remove_dir_all(&root);
    rig.finish();
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out
}

#[test]
fn hardcore_refuses_what_the_policy_says() {
    let mut rig = Rig::new();
    rig.sign_in_and_load(true);
    assert_eq!(rig.hardcore, Some(true), "hardcore took");
    rig.refusals.clear();

    // Pause the core so a stepped frame would be visible, then try every forbidden thing.
    rig.send(CoreCommand::Pause);
    rig.settle(200);
    while rig.core.evt_rx.try_recv().is_ok() {}
    rig.core.pending_frames.store(0, Ordering::Release);

    rig.send(CoreCommand::SetCheats(Box::new(ram_set(5))));
    rig.send(CoreCommand::StepFrame);
    rig.send(CoreCommand::StepScanline);
    rig.send(CoreCommand::RewindStep);
    rig.send(CoreCommand::SetRewind(Some(
        rf_state::RewindConfig::enabled(4, 8),
    )));
    rig.send(CoreCommand::SetRunAhead(2));
    rig.send(CoreCommand::PokeBus {
        addr: RAM_ADDR as u16,
        value: 0x11,
    });
    rig.settle(400);
    let said = rig.refusals.join(" | ");
    for word in [
        "Cheats",
        "Frame advance",
        "Rewind is off",
        "Run-ahead",
        "Editing memory",
    ] {
        assert!(said.contains(word), "{word} in: {said}");
    }
    assert_eq!(rig.refusals.len(), 7, "one refusal per command: {said}");

    // Nothing advanced: no stepped frame was produced.
    let mut frames = 0;
    while let Ok(ev) = rig.core.evt_rx.try_recv() {
        if matches!(ev, CoreEvent::Frame(_)) {
            frames += 1;
        }
    }
    assert_eq!(frames, 0, "a refused step produced a frame");

    // Allowed in hardcore: switching things OFF, fast-forward, saving.
    rig.refusals.clear();
    rig.send(CoreCommand::SetCheats(Box::default()));
    rig.send(CoreCommand::SetRunAhead(0));
    rig.send(CoreCommand::SetPacingEnabled(false));
    rig.send(CoreCommand::SetPacingEnabled(true));
    rig.settle(200);
    assert!(rig.refusals.is_empty(), "{:?}", rig.refusals);
    rig.finish();
}

#[test]
fn entering_hardcore_with_a_cheat_on_is_refused_and_works_once_it_is_off() {
    let mut rig = Rig::new();
    rig.send(CoreCommand::SetCheats(Box::new(ram_set(5))));
    rig.sign_in_and_load(false);
    rig.hardcore = None;
    rig.send(CoreCommand::RaEnable { hardcore: true });
    rig.pump_until(|r| r.hardcore.is_some());
    assert_eq!(rig.hardcore, Some(false));
    assert!(
        rig.refusals
            .iter()
            .any(|m| m == "Turn off cheats to enter hardcore"),
        "{:?}",
        rig.refusals
    );

    rig.hardcore = None;
    rig.send(CoreCommand::SetCheats(Box::default()));
    rig.send(CoreCommand::RaEnable { hardcore: true });
    rig.pump_until(|r| r.hardcore.is_some());
    assert_eq!(rig.hardcore, Some(true));
    // Now the same cheat is refused.
    rig.refusals.clear();
    rig.send(CoreCommand::SetCheats(Box::new(ram_set(5))));
    rig.settle(300);
    assert_eq!(rig.refusals, vec!["Cheats are off in hardcore".to_string()]);
    rig.finish();
}

#[test]
fn pause_goes_through_the_client_in_hardcore_and_is_free_in_softcore() {
    let mut rig = Rig::new();
    rig.sign_in_and_load(false);
    rig.send(CoreCommand::RaTryPause);
    rig.pump_until(|r| r.paused);
    rig.send(CoreCommand::Resume);
    rig.send(CoreCommand::RaEnable { hardcore: true });
    rig.pump_until(|r| r.hardcore == Some(true));
    rig.paused = false;
    rig.refusals.clear();
    rig.send(CoreCommand::RaTryPause);
    // Either rcheevos allows it now, or it names the wait; never silence.
    rig.pump_until(|r| r.paused || !r.refusals.is_empty());
    if !rig.paused {
        assert!(
            rig.refusals[0].contains("pause again"),
            "{:?}",
            rig.refusals
        );
    }
    rig.finish();
}

#[test]
fn turning_retroachievements_off_stops_the_client() {
    let mut rig = Rig::new();
    rig.sign_in_and_load(false);
    rig.send(CoreCommand::RaDisable);
    rig.settle(300);
    let seen = rig.log.len();
    rig.send(CoreCommand::SetCheats(Box::new(ram_set(5))));
    rig.settle(600);
    assert_eq!(rig.log.len(), seen, "no further requests");
    assert!(!rig.saw(|e| matches!(e, AchievementEvent::Unlocked { .. })));
    rig.finish();
}
