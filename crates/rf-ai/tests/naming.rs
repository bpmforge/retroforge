//! Ticket W36-09: the opt-in naming pass without a network. The payload
//! guard (D-024: behaviour facts only, never ROM bytes, disassembly, lifted
//! code or hashes), the mock provider, the Messages API request builder, the
//! response parser over hand-written captures, and the retry policy over a
//! scripted transport.

use std::cell::RefCell;
use std::time::Duration;

use rf_ai::anthropic::{
    build_body, call_with_retries, headers, is_retryable, parse_response, ApiKey, HttpReply,
    RetryPolicy, Transport, API_URL, BETA, DEFAULT_MODEL,
};
use rf_ai::naming::{
    audit_request, audit_text, build_requests, closed_vocabulary, proposals_from_response,
    run_naming, sanitize_name, AuditInput, BatchOptions, MockProvider, NamingError, NamingProvider,
    NamingRequest, NamingResponse, ProposedName,
};
use rf_rebuild::hw::flag;
use rf_rebuild::naming::propose_names;
use rf_rebuild::profile::{FunctionProfile, RegUse, Understanding, Vectors};
use rf_rebuild::{entry, Atlas, Function, HwClass, Platform, RamMeaning, Stats};
use serde_json::Value;

// ---- stand-in game data ------------------------------------------------------

/// A deterministic stand-in "ROM": the payload test checks that no window of
/// it appears in anything serialised.
fn fake_rom() -> Vec<u8> {
    let mut x = 0x2545_F491u32;
    (0..0x8000)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x >> 8) as u8
        })
        .collect()
}

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn atlas_over(rom: &[u8], n: u32, base_cpu: u32) -> Atlas {
    let functions = (0..n)
        .map(|i| {
            let at = (i as usize) * 64;
            Function {
                entry_rom: at as u32,
                entry_cpu: base_cpu + at as u32,
                entry_mode: 0,
                entry_kinds: if i < 2 { entry::INTERRUPT } else { entry::CALL },
                blocks: vec![],
                insn_count: 20,
                byte_len: 64,
                byte_hash: fnv(&rom[at..at + 64]),
                shape_hash: fnv(&rom[at + 1..at + 64]),
            }
        })
        .collect();
    Atlas {
        prg_len: rom.len() as u32,
        mode_count: 1,
        blocks: vec![],
        functions,
        indirect_sites: vec![],
        volatile: vec![],
        stats: Stats::default(),
    }
}

fn touch(p: &mut FunctionProfile, class: HwClass, reg: u16, reads: u64, writes: u64) {
    p.regs.insert(reg, (class, RegUse { reads, writes }));
}

fn profile(insns: u64) -> FunctionProfile {
    FunctionProfile {
        insns,
        entries: 120,
        calls: 120,
        frames_active: 118,
        main_insns: insns,
        ..FunctionProfile::default()
    }
}

/// A profile set shaped like a NES game: NMI, a pad reader, a PPU uploader
/// that touches named RAM, a mapper switcher, and unnamed game logic.
fn nes_shaped() -> (Understanding, Atlas, Vec<RamMeaning>) {
    let rom = fake_rom();
    let atlas = atlas_over(&rom, 8, 0x8000);
    let mut fs: Vec<FunctionProfile> = (0..8).map(|i| profile(1000 + i)).collect();
    fs[0].calls = 0;
    fs[0].entries = 1;
    fs[1].nmi_entries = 120;
    fs[1].nmi_insns = fs[1].insns;
    fs[1].main_insns = 0;
    touch(&mut fs[2], HwClass::Controller, 0x4016, 960, 240);
    touch(&mut fs[3], HwClass::Ppu, 0x2006, 0, 40);
    touch(&mut fs[3], HwClass::Ppu, 0x2007, 0, 640);
    fs[3].flags = flag::PPU_ADDR_NAMETABLE;
    fs[3].ram_read.insert(0x0300);
    fs[3].ram_read.insert(0x0301);
    fs[3].ram_write.insert(0x075A);
    touch(&mut fs[4], HwClass::Mapper, 0x8000, 0, 12);
    for c in [5u32, 6, 7] {
        fs[4].callees.insert(c, 120);
        fs[c as usize].callers.insert(4, 120);
    }
    fs[5].ram_read.insert(0x0086);
    fs[5].ram_write.insert(0x0086);
    fs[5].ram_write.insert(0x006D);
    let u = Understanding {
        platform: Platform::Nes,
        frames: 600,
        functions: fs,
        vectors: Vectors {
            reset: vec![0x8000],
            nmi: vec![0x8040],
            irq: vec![0x8080],
        },
        unattributed_insns: 0,
    };
    let ram = vec![
        RamMeaning {
            grade: rf_rebuild::Grade::Observed,
            addr: 0x075A,
            width: 1,
            name: "lives".into(),
            source: "profile".into(),
        },
        RamMeaning {
            grade: rf_rebuild::Grade::Observed,
            addr: 0x0086,
            width: 1,
            name: "player_x".into(),
            source: "profile".into(),
        },
    ];
    (u, atlas, ram)
}

/// A SNES-shaped set: DMA to VRAM, sound ports, 24-bit RAM addresses.
fn snes_shaped() -> (Understanding, Atlas, Vec<RamMeaning>) {
    let rom = fake_rom();
    let atlas = atlas_over(&rom, 6, 0x00_8000);
    let mut fs: Vec<FunctionProfile> = (0..6).map(|i| profile(5000 + i)).collect();
    fs[1].nmi_entries = 100;
    touch(&mut fs[2], HwClass::Dma, 0x420B, 0, 30);
    touch(&mut fs[2], HwClass::Dma, 0x4301, 0, 30);
    fs[2].flags = flag::DMA_TO_VRAM;
    touch(&mut fs[3], HwClass::Apu, 0x2140, 20, 40);
    fs[4].ram_read.insert(0x7E_0100 + 0x0010);
    fs[4].ram_write.insert(0x7E_0094);
    fs[5].ram_read.insert(0x7F_8000);
    let u = Understanding {
        platform: Platform::Snes,
        frames: 900,
        functions: fs,
        vectors: Vectors {
            reset: vec![0x8000],
            nmi: vec![0x8040],
            irq: vec![0x8080],
        },
        unattributed_insns: 0,
    };
    let ram = vec![RamMeaning {
        grade: rf_rebuild::Grade::Observed,
        addr: 0x7E_0094,
        width: 2,
        name: "player_x".into(),
        source: "profile".into(),
    }];
    (u, atlas, ram)
}

// ---- the payload guard -------------------------------------------------------

fn audit(
    requests: &[NamingRequest],
    rom: &[u8],
    atlas: &Atlas,
    extra_strings: &[&str],
) -> Result<(), String> {
    assert!(!requests.is_empty());
    let mut allowed = closed_vocabulary();
    allowed.extend(extra_strings.iter().map(|s| (*s).to_string()));
    let hashes: Vec<u64> = atlas
        .functions
        .iter()
        .flat_map(|f| [f.byte_hash, f.shape_hash])
        .collect();
    for r in requests {
        audit_request(
            r,
            &AuditInput {
                rom,
                hashes: &hashes,
                allowed_strings: &allowed,
            },
        )?;
    }
    Ok(())
}

fn assert_payload_is_behaviour_only(
    requests: &[NamingRequest],
    rom: &[u8],
    atlas: &Atlas,
    extra_strings: &[&str],
) {
    audit(requests, rom, atlas, extra_strings).expect("payload is behaviour facts only");
}

#[test]
fn payload_nes_shaped_input_is_behaviour_facts_only() {
    let (u, atlas, ram) = nes_shaped();
    let rom = fake_rom();
    let props = propose_names(&u, &atlas);
    assert!(props.iter().any(|p| p.name == "nmi"), "rules named the NMI");
    let requests = build_requests(
        &u,
        &atlas,
        &props,
        &ram,
        BatchOptions {
            include_named: true,
            ..BatchOptions::default()
        },
    );
    let mut words: Vec<&str> = vec!["lives", "player_x"];
    let rule_names: Vec<String> = props
        .iter()
        .flat_map(|p| [p.name.clone(), p.base.clone()])
        .collect();
    words.extend(rule_names.iter().map(String::as_str));
    assert_payload_is_behaviour_only(&requests, &rom, &atlas, &words);

    // The facts that make the pass worth running ARE there.
    let all: Vec<_> = requests.iter().flat_map(|r| &r.functions).collect();
    let uploader = all.iter().find(|f| f.id == 3).expect("uploader");
    assert!(uploader
        .registers
        .iter()
        .any(|r| r.addr == 0x2007 && r.writes == 640));
    assert_eq!(uploader.ram_written[0].names, ["lives"]);
    assert!(uploader.ram_read[0].start == 0x300 && uploader.ram_read[0].end == 0x301);
    let player = all.iter().find(|f| f.id == 5).expect("mover");
    assert_eq!(player.ram_written.iter().flat_map(|r| &r.names).count(), 1);
    assert!(player.ram_written.iter().any(|r| r.names == ["player_x"]));
    let switcher = all.iter().find(|f| f.id == 4).expect("switcher");
    assert_eq!(switcher.callees.len(), 3);
    assert_eq!(all.iter().find(|f| f.id == 5).unwrap().callers[0].id, 4);
    let nmi = all.iter().find(|f| f.id == 1).expect("nmi");
    assert!(nmi.entered_by_interrupt && nmi.nmi_pct == 100 && nmi.main_pct == 0);
    assert_eq!(nmi.rule_hint.as_ref().map(|h| h.name.as_str()), Some("nmi"));
}

#[test]
fn payload_snes_shaped_input_is_behaviour_facts_only() {
    let (u, atlas, ram) = snes_shaped();
    let rom = fake_rom();
    let props = propose_names(&u, &atlas);
    let requests = build_requests(&u, &atlas, &props, &ram, BatchOptions::default());
    let mut words: Vec<&str> = vec!["player_x"];
    let rule_names: Vec<String> = props
        .iter()
        .flat_map(|p| [p.name.clone(), p.base.clone()])
        .collect();
    words.extend(rule_names.iter().map(String::as_str));
    assert_payload_is_behaviour_only(&requests, &rom, &atlas, &words);
    let all: Vec<_> = requests.iter().flat_map(|r| &r.functions).collect();
    assert!(requests.iter().all(|r| r.console == "snes"));
    // 24-bit RAM addresses survive, folded to bank $7E.
    assert!(all
        .iter()
        .any(|f| f.ram_read.iter().any(|r| r.start == 0x7E_0110)));
    assert!(all
        .iter()
        .any(|f| f.ram_written.iter().any(|r| r.names == ["player_x"])));
}

#[test]
fn requests_are_batched_ordered_and_capped() {
    let (u, atlas, ram) = nes_shaped();
    let opts = BatchOptions {
        per_request: 2,
        max_requests: 2,
        include_named: true,
    };
    let reqs = build_requests(&u, &atlas, &[], &ram, opts);
    assert_eq!(reqs.len(), 2, "capped at two requests");
    assert!(reqs.iter().all(|r| r.functions.len() == 2));
    // Busiest first (insns grow with the id in this fixture).
    assert_eq!(reqs[0].functions[0].id, 7);
    // Rule-named functions are not asked about by default.
    let props = propose_names(&u, &atlas);
    let default = build_requests(&u, &atlas, &props, &ram, BatchOptions::default());
    let asked: Vec<u32> = default
        .iter()
        .flat_map(|r| r.functions.iter().map(|f| f.id))
        .collect();
    for p in &props {
        assert!(!asked.contains(&p.function), "{} already named", p.name);
    }
}

// ---- mock provider and answer handling ----------------------------------------

#[test]
fn the_mock_provider_is_deterministic_and_its_names_pass_the_filter() {
    let (u, atlas, ram) = nes_shaped();
    let reqs = build_requests(&u, &atlas, &[], &ram, BatchOptions::default());
    let a = run_naming(&MockProvider, &reqs, &mut Vec::new()).expect("mock answers");
    let b = run_naming(&MockProvider, &reqs, &mut Vec::new()).expect("mock answers");
    assert_eq!(a, b);
    assert_eq!(a.len(), 8);
    assert!(a
        .iter()
        .all(|p| p.source == "ai:mock" && p.confidence == 50));
    let mut names: Vec<_> = a.iter().map(|p| p.name.clone()).collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), 8, "unique");
    assert_eq!(MockProvider.model(), "mock");
}

#[test]
fn model_output_is_untrusted() {
    assert_eq!(
        sanitize_name("  Read Controllers! "),
        Some("read_controllers".into())
    );
    assert_eq!(sanitize_name("9lives"), None, "must start with a letter");
    assert_eq!(sanitize_name("ab"), None, "too short");
    assert_eq!(sanitize_name(&"x".repeat(60)), None, "too long");
    assert_eq!(sanitize_name("../../etc"), Some("etc".into()));

    let (u, atlas, ram) = nes_shaped();
    let req = build_requests(&u, &atlas, &[], &ram, BatchOptions::default()).remove(0);
    let pn = |id, name: &str, conf| ProposedName {
        id,
        name: name.into(),
        purpose: "does a thing\u{7}".into(),
        confidence: conf,
    };
    let resp = NamingResponse {
        names: vec![
            pn(3, "Upload Tiles", 250),
            pn(3, "second answer for 3", 10),
            pn(99, "unasked_function", 90),
            pn(4, "!!", 90),
            pn(5, "upload_tiles", 40),
        ],
    };
    let mut taken = vec!["nmi".to_string()];
    let out = proposals_from_response("claude-opus-5-5", &req, &resp, &mut taken);
    let got: Vec<_> = out
        .iter()
        .map(|p| (p.function, p.name.as_str(), p.confidence))
        .collect();
    assert_eq!(
        got,
        [(3, "upload_tiles", 100), (5, "upload_tiles_5", 40)],
        "unasked ids, repeats and unusable names dropped; clashes suffixed; confidence clamped"
    );
    assert_eq!(out[0].source, "ai:claude-opus-5-5");
    assert!(!out[0].evidence[0].contains('\u{7}'));
}

struct Refusing;
impl NamingProvider for Refusing {
    fn model(&self) -> &str {
        "x"
    }
    fn propose(&self, _: &NamingRequest) -> Result<NamingResponse, NamingError> {
        Err(NamingError::Refused("no".into()))
    }
}

#[test]
fn a_failed_request_fails_the_run_without_partial_results() {
    let (u, atlas, ram) = nes_shaped();
    let reqs = build_requests(&u, &atlas, &[], &ram, BatchOptions::default());
    assert_eq!(
        run_naming(&Refusing, &reqs, &mut Vec::new()),
        Err(NamingError::Refused("no".into()))
    );
}

// ---- the Messages API ----------------------------------------------------------

fn sample_request() -> NamingRequest {
    let (u, atlas, ram) = nes_shaped();
    build_requests(&u, &atlas, &[], &ram, BatchOptions::default()).remove(0)
}

#[test]
fn the_request_body_and_headers_follow_the_brief() {
    let req = sample_request();
    let body = build_body(DEFAULT_MODEL, &req);
    assert_eq!(body["model"], "claude-opus-5-5");
    assert_eq!(body["max_tokens"], 16_000);
    assert_eq!(body["fallbacks"], "default");
    assert!(body.get("thinking").is_none(), "adaptive is the default");
    assert_eq!(body["output_config"]["effort"], "low");
    assert_eq!(body["output_config"]["format"]["type"], "json_schema");
    let schema = &body["output_config"]["format"]["schema"];
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(schema["required"][0], "names");
    assert_eq!(body["messages"][0]["role"], "user");
    let content = body["messages"][0]["content"].as_str().expect("text");
    assert!(content.contains("\"calls_per_100_frames\""));
    // The facts in the message are the request, nothing more.
    let json_start = content.find('{').expect("json");
    let echoed: NamingRequest = serde_json::from_str(&content[json_start..]).expect("round trip");
    assert_eq!(echoed, req);

    let key = ApiKey::new("  sk-ant-test-key-1234 ").expect("key");
    let h = headers(&key);
    let get = |n: &str| h.iter().find(|(k, _)| *k == n).map(|(_, v)| v.as_str());
    assert_eq!(get("x-api-key"), Some("sk-ant-test-key-1234"));
    assert_eq!(get("anthropic-version"), Some("2026-10-21"));
    assert_eq!(get("anthropic-beta"), Some(BETA));
    assert_eq!(get("content-type"), Some("application/json"));
    assert_eq!(API_URL, "https://api.anthropic.com/v1/messages");
    assert!(
        !body.to_string().contains("sk-ant"),
        "the key is a header only"
    );
}

#[test]
fn the_api_key_never_prints() {
    let key = ApiKey::new("sk-ant-secret").expect("key");
    assert_eq!(format!("{key:?}"), "ApiKey(***)");
    assert!(!format!("{key:#?}").contains("secret"));
    assert!(ApiKey::new("   ").is_none());
}

fn message(stop: &str, extra: &str, text: &str) -> String {
    let text = serde_json::to_string(text).unwrap();
    format!(
        r#"{{"id":"msg_01","type":"message","role":"assistant","model":"claude-opus-5-5",
        "content":[{{"type":"text","text":{text}}}],"stop_reason":"{stop}"{extra},
        "usage":{{"input_tokens":10,"output_tokens":20}}}}"#
    )
}

#[test]
fn a_good_answer_parses_by_serde() {
    let answer = r#"{"names":[{"id":3,"name":"upload_tiles","purpose":"Copies tiles to the PPU","confidence":80}]}"#;
    let r = parse_response(200, &message("end_turn", "", answer)).expect("parses");
    assert_eq!(r.names.len(), 1);
    assert_eq!(
        (
            r.names[0].id,
            r.names[0].name.as_str(),
            r.names[0].confidence
        ),
        (3, "upload_tiles", 80)
    );
}

#[test]
fn stop_reasons_are_checked_before_content() {
    // A refusal carries text that must not be read as an answer.
    let refusal = message(
        "refusal",
        r#","stop_details":{"category":"policy","explanation":"declined"}"#,
        r#"{"names":[{"id":1,"name":"looks_valid","purpose":"x","confidence":99}]}"#,
    );
    match parse_response(200, &refusal) {
        Err(NamingError::Refused(why)) => assert!(why.contains("declined"), "{why}"),
        other => panic!("{other:?}"),
    }
    let bare = message("refusal", "", "");
    assert!(
        matches!(parse_response(200, &bare), Err(NamingError::Refused(w)) if w == "no reason given")
    );
    assert_eq!(
        parse_response(200, &message("max_tokens", "", r#"{"names":["#)),
        Err(NamingError::Truncated)
    );
    assert!(matches!(
        parse_response(200, &message("tool_use", "", "{}")),
        Err(NamingError::BadResponse(_))
    ));
}

#[test]
fn http_errors_and_junk_become_errors_not_panics() {
    let rate = r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#;
    assert_eq!(
        parse_response(429, rate),
        Err(NamingError::Http {
            status: 429,
            message: "slow down".into()
        })
    );
    assert!(matches!(
        parse_response(400, "<html>bad</html>"),
        Err(NamingError::Http { status: 400, .. })
    ));
    assert!(matches!(
        parse_response(200, "not json"),
        Err(NamingError::BadResponse(_))
    ));
    assert!(matches!(
        parse_response(200, r#"{"stop_reason":"end_turn","content":[]}"#),
        Err(NamingError::BadResponse(_))
    ));
    assert!(matches!(
        parse_response(200, &message("end_turn", "", "the names are: foo")),
        Err(NamingError::BadResponse(_))
    ));
    // Right JSON, wrong shape.
    assert!(matches!(
        parse_response(
            200,
            &message("end_turn", "", r#"{"names":[{"id":"three"}]}"#)
        ),
        Err(NamingError::BadResponse(_))
    ));
}

struct Script {
    replies: RefCell<Vec<Result<HttpReply, String>>>,
    posts: RefCell<u32>,
    saw_headers: RefCell<Vec<Vec<(&'static str, String)>>>,
}

impl Script {
    fn new(mut replies: Vec<Result<HttpReply, String>>) -> Self {
        replies.reverse();
        Self {
            replies: RefCell::new(replies),
            posts: RefCell::new(0),
            saw_headers: RefCell::default(),
        }
    }
}

impl Transport for Script {
    fn post(
        &self,
        url: &str,
        h: &[(&'static str, String)],
        body: &str,
    ) -> Result<HttpReply, String> {
        assert_eq!(url, API_URL);
        assert!(serde_json::from_str::<Value>(body).is_ok());
        *self.posts.borrow_mut() += 1;
        self.saw_headers.borrow_mut().push(h.to_vec());
        self.replies.borrow_mut().pop().expect("script ran out")
    }
}

fn reply(status: u16, body: &str, retry_after: Option<u64>) -> Result<HttpReply, String> {
    Ok(HttpReply {
        status,
        body: body.into(),
        retry_after,
    })
}

const OK_BODY_TEXT: &str =
    r#"{"names":[{"id":3,"name":"upload_tiles","purpose":"p","confidence":70}]}"#;

#[test]
fn rate_limits_and_server_errors_back_off_and_retry() {
    let key = ApiKey::new("k").unwrap();
    let ok = message("end_turn", "", OK_BODY_TEXT);
    let t = Script::new(vec![
        reply(429, r#"{"error":{"message":"slow"}}"#, Some(7)),
        reply(503, "overloaded", None),
        Err("connection reset".into()),
        reply(200, &ok, None),
    ]);
    let mut slept = Vec::new();
    let r = call_with_retries(
        &t,
        &key,
        DEFAULT_MODEL,
        &sample_request(),
        &RetryPolicy {
            max_attempts: 5,
            ..RetryPolicy::default()
        },
        &mut |d| slept.push(d),
    )
    .expect("succeeds on the fourth try");
    assert_eq!(r.names[0].name, "upload_tiles");
    assert_eq!(*t.posts.borrow(), 4);
    assert_eq!(
        slept,
        [
            Duration::from_secs(7),
            Duration::from_secs(2),
            Duration::from_secs(4)
        ],
        "Retry-After first, then exponential from the base"
    );
    assert!(t
        .saw_headers
        .borrow()
        .iter()
        .all(|h| h.iter().any(|(k, _)| *k == "anthropic-beta")));
}

#[test]
fn client_errors_are_not_retried_and_retries_are_bounded() {
    let key = ApiKey::new("k").unwrap();
    let policy = RetryPolicy::default();
    let t = Script::new(vec![reply(
        400,
        r#"{"error":{"message":"bad schema"}}"#,
        None,
    )]);
    let mut slept = Vec::new();
    let e = call_with_retries(
        &t,
        &key,
        DEFAULT_MODEL,
        &sample_request(),
        &policy,
        &mut |d| slept.push(d),
    )
    .unwrap_err();
    assert_eq!(
        e,
        NamingError::Http {
            status: 400,
            message: "bad schema".into()
        }
    );
    assert_eq!((*t.posts.borrow(), slept.len()), (1, 0));

    let t = Script::new((0..4).map(|_| reply(529, "overloaded", None)).collect());
    let mut slept = Vec::new();
    let e = call_with_retries(
        &t,
        &key,
        DEFAULT_MODEL,
        &sample_request(),
        &policy,
        &mut |d| slept.push(d),
    )
    .unwrap_err();
    assert!(matches!(e, NamingError::Http { status: 529, .. }));
    assert_eq!(*t.posts.borrow(), policy.max_attempts);
    assert_eq!(
        slept.len() as u32,
        policy.max_attempts - 1,
        "no sleep after the last try"
    );
    assert!(slept.iter().all(|d| *d <= policy.cap));

    // A refusal is an answer, not a retry.
    let refusal = message("refusal", "", "");
    let t = Script::new(vec![reply(200, &refusal, None)]);
    let e = call_with_retries(
        &t,
        &key,
        DEFAULT_MODEL,
        &sample_request(),
        &policy,
        &mut |_| {},
    )
    .unwrap_err();
    assert!(matches!(e, NamingError::Refused(_)));
    assert!(is_retryable(429) && is_retryable(500) && is_retryable(529));
    assert!(!is_retryable(400) && !is_retryable(401) && !is_retryable(404));
}

// ---- the guard has teeth ------------------------------------------------------

/// A request with one extra neighbour name smuggled in, and the audit result.
fn smuggled(into: &str, allow_it: bool) -> Result<(), String> {
    let (u, atlas, ram) = nes_shaped();
    let rom = fake_rom();
    let mut reqs = build_requests(&u, &atlas, &[], &ram, BatchOptions::default());
    reqs[0].functions[0]
        .neighbour_names
        .push(rf_ai::naming::NeighbourName {
            id: 1,
            name: into.to_string(),
        });
    let extra: Vec<&str> = if allow_it {
        vec!["lives", "player_x", into]
    } else {
        vec!["lives", "player_x"]
    };
    audit(&reqs[..1], &rom, &atlas, &extra)
}

#[test]
fn guard_rejects_an_unknown_string() {
    let e = smuggled("lda_abs_2002", false).unwrap_err();
    assert!(e.contains("outside the closed vocabulary"), "{e}");
    // The same string is fine once someone vouches for it as a name...
    assert!(smuggled("lda_abs_2002", true).is_ok());
}

#[test]
fn guard_rejects_a_function_hash_even_if_the_vocabulary_allowed_it() {
    let (_, atlas, _) = nes_shaped();
    let h = atlas.functions[3].byte_hash;
    for text in [format!("{h:x}"), h.to_string()] {
        let e = smuggled(&text, true).unwrap_err();
        assert!(e.contains("code hash"), "{e}");
    }
}

#[test]
fn guard_rejects_a_rom_window_even_if_the_vocabulary_allowed_it() {
    let rom = fake_rom();
    let w = rom
        .windows(4)
        .find(|w| {
            format!("{:02x}{:02x}{:02x}{:02x}", w[0], w[1], w[2], w[3])
                .chars()
                .any(|c| c.is_ascii_alphabetic())
        })
        .expect("a window with a letter in its hex");
    let hex: String = w.iter().map(|b| format!("{b:02x}")).collect();
    let e = smuggled(&hex, true).unwrap_err();
    assert!(e.contains("window of the ROM"), "{e}");
    let list = format!("{},{},{},{}", w[0], w[1], w[2], w[3]);
    let e = smuggled(&list, true).unwrap_err();
    assert!(e.contains("window of the ROM"), "{e}");
}

#[test]
fn guard_rejects_a_new_payload_key() {
    // JSON no NamingRequest could produce: the extra field is a key nobody
    // vetted, so the audit stops it before it can be sent.
    let (u, atlas, ram) = nes_shaped();
    let reqs = build_requests(&u, &atlas, &[], &ram, BatchOptions::default());
    let mut v: Value = serde_json::to_value(&reqs[0]).unwrap();
    v["functions"][0]["disassembly"] = Value::String("lda #$00".into());
    let allowed = closed_vocabulary();
    let input = AuditInput {
        rom: &fake_rom(),
        hashes: &[],
        allowed_strings: &allowed,
    };
    let e = audit_text(&v.to_string(), &input).unwrap_err();
    assert!(e.contains("key `disassembly` is not allowed"), "{e}");
    // Without the extra key the request passes the key check (it fails only
    // on its own names, which this audit was not told about).
    let e = audit_text(&serde_json::to_string(&reqs[0]).unwrap(), &input).unwrap_err();
    assert!(!e.contains("key"), "{e}");
}
