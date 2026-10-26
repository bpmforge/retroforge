//! Ticket W36-09: the app-side half of Name functions, with no network and no
//! ROM. The opt-in gate and where the key comes from, the key never printing,
//! the real provider over a scripted transport (headers, body, retries), the
//! understand pass on a private copy of a synthetic machine, and the recipe
//! export (marked block, round trip, human rows win).

use std::sync::{Arc, Mutex};

use retroforge::ai_naming::{
    ai_gate, default_picks, export_recipe_text, picked, resolve_key, run_job, AiGate,
    AnthropicProvider, NameJob, Pick,
};
use retroforge::settings::{AiNamingSettings, AppSettings};
use retroforge::stepper::EmuStepper;
use rf_ai::anthropic::{ApiKey, HttpReply, Transport};
use rf_ai::naming::{MockProvider, NamingProvider};
use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel, ResetKind};
use rf_nes::core::NesCore;

// ---- gate and key -----------------------------------------------------------

fn settings(enabled: bool, key: Option<&str>) -> AiNamingSettings {
    AiNamingSettings {
        enabled,
        model: "claude-opus-5-5".into(),
        api_key: key.map(str::to_string),
    }
}

#[test]
fn ai_naming_is_off_by_default_and_nothing_is_ready_until_opted_in_with_a_key() {
    let d = AppSettings::default();
    assert!(!d.ai_naming.enabled);
    assert_eq!(d.ai_naming.model, "claude-opus-5-5");
    assert_eq!(d.ai_naming.api_key, None);
    // Off: even with a key in the environment, nothing is ready.
    assert_eq!(ai_gate(&d.ai_naming, Some("sk-env")), AiGate::Off);
    assert_eq!(ai_gate(&settings(false, Some("sk-set")), None), AiGate::Off);
    // On with no key anywhere.
    assert_eq!(ai_gate(&settings(true, None), None), AiGate::NoKey);
    assert_eq!(
        ai_gate(&settings(true, Some("  ")), Some("")),
        AiGate::NoKey
    );
    // The key from settings wins over the environment; the environment fills in.
    let AiGate::Ready(k) = ai_gate(&settings(true, Some("sk-set")), Some("sk-env")) else {
        panic!("ready");
    };
    assert_eq!(k.expose(), "sk-set");
    let AiGate::Ready(k) = ai_gate(&settings(true, None), Some("sk-env")) else {
        panic!("ready");
    };
    assert_eq!(k.expose(), "sk-env");
    assert!(resolve_key(&settings(false, None), None).is_none());
}

#[test]
fn the_settings_round_trip_and_never_print_the_key() {
    let mut s = AppSettings::default();
    s.ai_naming = settings(true, Some("sk-ant-very-secret"));
    let text = s.to_toml().expect("serialises");
    assert!(text.contains("[ai_naming]"));
    let back = AppSettings::from_toml(&text).expect("parses");
    assert_eq!(back.ai_naming, s.ai_naming);
    for dbg in [format!("{:?}", s.ai_naming), format!("{s:?}")] {
        assert!(!dbg.contains("very-secret"), "{dbg}");
    }
    // A settings file from before the section existed leaves it off.
    let old = AppSettings::from_toml("[video]\n").expect("parses");
    assert!(!old.ai_naming.enabled);
    // A fresh install's file never mentions a key.
    assert!(!AppSettings::default().to_toml().unwrap().contains("sk-"));
}

// ---- the real provider over a scripted transport -----------------------------

type Seen = Vec<(String, Vec<(&'static str, String)>, String)>;

/// Records what was posted and answers from a script.
struct Scripted {
    seen: Arc<Mutex<Seen>>,
    replies: Mutex<Vec<Result<HttpReply, String>>>,
}

impl Transport for Scripted {
    fn post(
        &self,
        url: &str,
        headers: &[(&'static str, String)],
        body: &str,
    ) -> Result<HttpReply, String> {
        self.seen
            .lock()
            .unwrap()
            .push((url.to_string(), headers.to_vec(), body.to_string()));
        self.replies.lock().unwrap().pop().expect("scripted")
    }
}

fn request_for(ids: &[u32]) -> rf_ai::naming::NamingRequest {
    let f = |id| rf_ai::naming::FunctionFacts {
        id,
        calls_per_100_frames: 100,
        frames_active_pct: 100,
        insns_per_call: 40,
        main_pct: 100,
        nmi_pct: 0,
        irq_pct: 0,
        entered_by_interrupt: false,
        callers: vec![],
        callees: vec![],
        registers: vec![],
        facts: vec![],
        ram_read: vec![],
        ram_written: vec![],
        neighbour_names: vec![],
        rule_hint: None,
    };
    rf_ai::naming::NamingRequest {
        payload_version: rf_ai::naming::PAYLOAD_VERSION,
        console: "nes".into(),
        frames: 300,
        functions: ids.iter().map(|&i| f(i)).collect(),
    }
}

#[test]
fn the_anthropic_provider_posts_the_documented_request_and_reads_the_answer() {
    let answer =
        r#"{"names":[{"id":4,"name":"spawn_enemies","purpose":"Places enemies","confidence":72}]}"#;
    let message = format!(
        r#"{{"stop_reason":"end_turn","content":[{{"type":"text","text":{}}}]}}"#,
        json_string(answer)
    );
    let seen = Arc::new(Mutex::new(Vec::new()));
    let t = Scripted {
        seen: Arc::clone(&seen),
        replies: Mutex::new(vec![Ok(HttpReply {
            status: 200,
            body: message,
            retry_after: None,
        })]),
    };
    let p = AnthropicProvider::with_transport(
        ApiKey::new("sk-ant-test").unwrap(),
        "claude-opus-5-5",
        Box::new(t),
    );
    assert_eq!(p.model(), "claude-opus-5-5");
    let r = p.propose(&request_for(&[4, 5])).expect("answers");
    assert_eq!(r.names[0].name, "spawn_enemies");
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    let (url, headers, body) = &seen[0];
    assert_eq!(url, "https://api.anthropic.com/v1/messages");
    let h = |n: &str| {
        headers
            .iter()
            .find(|(k, _)| *k == n)
            .map(|(_, v)| v.as_str())
    };
    assert_eq!(h("x-api-key"), Some("sk-ant-test"));
    assert_eq!(h("anthropic-version"), Some("2026-10-21"));
    assert_eq!(h("anthropic-beta"), Some("server-side-fallback-2026-07-01"));
    assert!(body.contains("\"fallbacks\":\"default\""));
    assert!(!body.contains("thinking"));
    assert!(
        !body.contains("sk-ant-test"),
        "the key is a header, not content"
    );
}

fn json_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ---- the understand pass on a private copy ------------------------------------

/// Reset: NMI on, then `JSR pads; JSR oam; JSR mover; JMP loop`. `mover`
/// (`INC $20; LDA $20; STA $21; RTS`) touches no hardware, so only a model
/// could name it.
fn rom() -> Vec<u8> {
    let mut d = vec![0u8; 16];
    d[0..4].copy_from_slice(b"NES\x1a");
    d[4] = 1;
    d[5] = 1;
    let mut prg = vec![0xEAu8; 0x4000];
    let mut put = |at: usize, code: &[u8]| prg[at..at + code.len()].copy_from_slice(code);
    put(
        0x0000,
        &[
            0x78, 0xA9, 0x80, 0x8D, 0x00, 0x20, // SEI; LDA #$80; STA $2000
            0x20, 0x00, 0x81, 0x20, 0x20, 0x81, 0x20, 0x60, 0x81, // JSRs
            0x4C, 0x06, 0x80, // JMP loop
        ],
    );
    put(
        0x0100,
        &[
            0xA9, 0x01, 0x8D, 0x16, 0x40, 0xA9, 0x00, 0x8D, 0x16, 0x40, 0xA2, 0x08, 0xAD, 0x16,
            0x40, 0xCA, 0xD0, 0xFA, 0x60,
        ],
    );
    put(0x0120, &[0xA9, 0x02, 0x8D, 0x14, 0x40, 0x60]);
    put(0x0140, &[0xE6, 0x10, 0x40]);
    put(0x0160, &[0xE6, 0x20, 0xA5, 0x20, 0x85, 0x21, 0x60]);
    prg[0x3FFA..0x3FFC].copy_from_slice(&0x8140u16.to_le_bytes());
    prg[0x3FFC..0x3FFE].copy_from_slice(&0x8000u16.to_le_bytes());
    d.extend(prg);
    d.extend(vec![0u8; 0x2000]);
    d
}

struct Quiet;
impl CoreSink for Quiet {
    fn video_scanline(&mut self, _y: u16, _p: &[PpuPixel]) {}
    fn audio(&mut self, _s: &[i16]) {}
    fn event(&mut self, _e: CoreEvent) {}
}

fn atlas_of(raw: &[u8]) -> rf_rebuild::Atlas {
    let mut rec = NesCore::from_ines_bytes(raw).expect("loads");
    rec.set_coverage_recording(true);
    rec.reset(ResetKind::Hard);
    for _ in 0..4 {
        rec.run_frame(&InputFrame::empty(), &mut Quiet);
    }
    let log = rec.take_coverage().expect("armed");
    let prg = rf_nes::NesRom::from_ines_bytes(raw).expect("rom");
    rf_nes::rebuild::build_atlas(prg.prg_rom(), &log)
}

#[test]
fn name_functions_runs_on_a_copy_and_the_live_machine_is_untouched() {
    let raw = rom();
    let atlas = atlas_of(&raw);
    let mut live = EmuStepper::open(&raw).expect("opens");
    for _ in 0..3 {
        live.latch_and_advance_frame(InputFrame::empty(), &mut Quiet);
    }
    let snap = live.snapshot().expect("snapshot");
    let before = snap.state_hash();
    let job = NameJob {
        frames: 60,
        ram: vec![rf_rebuild::RamMeaning {
            addr: 0x20,
            width: 1,
            name: "counter".into(),
            source: "test".into(),
        }],
        ai: None,
    };
    let out = run_job(
        &raw,
        &snap,
        Arc::new(atlas.clone()),
        InputFrame::empty(),
        &job,
        None,
    )
    .expect("runs");
    // The live machine is where it was.
    assert_eq!(live.snapshot().unwrap().state_hash(), before);
    let names: Vec<&str> = out.rule.iter().map(|p| p.name.as_str()).collect();
    for want in ["reset", "nmi", "read_controllers", "oam_dma"] {
        assert!(names.contains(&want), "{want} in {names:?}");
    }
    assert!(out.ai.is_empty() && out.ai_note.contains("off"));
    assert_eq!(out.frames, 60);
    assert!(out.observed >= 5);

    // With a provider the unnamed function gets a model name, in its own list.
    let job = NameJob { ai: None, ..job };
    let out = run_job(
        &raw,
        &snap,
        Arc::new(atlas.clone()),
        InputFrame::empty(),
        &job,
        Some(&MockProvider),
    )
    .expect("runs");
    assert_eq!(out.ai.len(), 1, "{:?}", out.ai);
    assert_eq!(out.ai[0].source, "ai:mock");
    assert!(out.ai_note.contains("mock"));
    assert!(out.rule.iter().all(|p| p.function != out.ai[0].function));

    // Default picks: confident rule names only, never the model's.
    let picks = default_picks(&out);
    assert!(picks.values().all(|p| *p == Pick::Rule));
    assert!(!picks.contains_key(&out.ai[0].function));
    let accepted = picked(&out, &picks);
    assert!(accepted.iter().all(|p| p.source.starts_with("rule:")));
    let mut with_ai = picks.clone();
    with_ai.insert(out.ai[0].function, Pick::Ai);
    assert_eq!(picked(&out, &with_ai).len(), accepted.len() + 1);

    // A failing provider costs the model names, not the rule names.
    struct Down;
    impl NamingProvider for Down {
        fn model(&self) -> &str {
            "down"
        }
        fn propose(
            &self,
            _: &rf_ai::naming::NamingRequest,
        ) -> Result<rf_ai::naming::NamingResponse, rf_ai::naming::NamingError> {
            Err(rf_ai::naming::NamingError::Network("offline".into()))
        }
    }
    let out = run_job(
        &raw,
        &snap,
        Arc::new(atlas),
        InputFrame::empty(),
        &job,
        Some(&Down),
    )
    .expect("runs");
    assert!(out.ai.is_empty());
    assert!(out.ai_note.contains("offline"), "{}", out.ai_note);
    assert!(out.rule.len() >= 4);
}

// ---- export ----------------------------------------------------------------------

#[test]
fn export_writes_a_marked_block_and_keeps_human_rows_and_comments() {
    let raw = rom();
    let atlas = atlas_of(&raw);
    let live = EmuStepper::open(&raw).expect("opens");
    let snap = live.snapshot().unwrap();
    let job = NameJob {
        frames: 60,
        ram: vec![],
        ai: None,
    };
    let out = run_job(
        &raw,
        &snap,
        Arc::new(atlas.clone()),
        InputFrame::empty(),
        &job,
        None,
    )
    .unwrap();
    let accepted = picked(&out, &default_picks(&out));
    assert!(accepted.len() >= 4);

    // A blank recipe.
    let (text, n) = export_recipe_text(None, "demo", &atlas, &accepted).expect("exports");
    assert_eq!(n, accepted.len());
    let recipe = rf_profiles::load_recipe_str(&text).expect("parses");
    assert_eq!(recipe.functions.len(), n);
    assert!(recipe
        .functions
        .iter()
        .all(|f| (f.source.starts_with("rule:"))
            && f.confidence.is_some()
            && f.exact_hash.is_some()));
    // The names resolve against the atlas, each to its own function.
    let report = rf_rebuild::match_recipe(&atlas, &recipe);
    assert!(report.unmatched.is_empty());
    assert_eq!(report.matched.len(), n);

    // A recipe a person wrote: they named the NMI themselves.
    let nmi = out.rule.iter().find(|p| p.name == "nmi").unwrap();
    let f = &atlas.functions[nmi.function as usize];
    let human = format!(
        "# written by hand\n[recipe]\nformat = 1\nid = \"demo\"\nengine = \"x\"\n\n[[function]]\n\
         name = \"vblank_handler\"\nshape_hash = \"{:#018x}\"\nexact_hash = \"{:#018x}\"\n\
         source = \"observed: by hand\"\n",
        f.shape_hash, f.byte_hash
    );
    let (merged, n2) =
        export_recipe_text(Some(&human), "demo", &atlas, &accepted).expect("exports");
    assert_eq!(n2, n - 1, "the person's name for the NMI wins");
    assert!(merged.starts_with("# written by hand"));
    let r2 = rf_profiles::load_recipe_str(&merged).unwrap();
    assert!(r2.functions.iter().any(|f| f.name == "vblank_handler"));
    assert!(!r2.functions.iter().any(|f| f.name == "nmi"));

    // Exporting again replaces the block instead of stacking another.
    let (again, n3) = export_recipe_text(Some(&merged), "demo", &atlas, &accepted).unwrap();
    assert_eq!(n3, n2);
    assert_eq!(
        again.matches("understand pass").count(),
        merged.matches("understand pass").count()
    );
    assert_eq!(
        rf_profiles::load_recipe_str(&again)
            .unwrap()
            .functions
            .len(),
        n2 + 1
    );

    // Nothing in the recipe is a key or a payload.
    assert!(!again.contains("sk-"));

    // Garbage in an existing file is refused, not overwritten.
    assert!(export_recipe_text(Some("not [ toml"), "demo", &atlas, &accepted).is_err());
}
