//! Ticket W40-02: the local on-device naming backend without any external
//! network. Unit tests use a scripted transport or a fake server bound to
//! 127.0.0.1 inside the test; one `#[ignore]` test calls the real local LM
//! Studio endpoint.

use std::cell::RefCell;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use rf_ai::anthropic::{ApiKey, HttpReply, RetryPolicy, Transport};
use rf_ai::local::{
    build_body, call_with_retries, headers, parse_response, LocalConfig, LocalConfigError,
    LocalProvider, NamingBackend,
};
use rf_ai::localhttp::{is_loopback_url, parse_http_url, parse_reply, LoopbackTransport};
use rf_ai::naming::{
    audit_request, build_requests, closed_vocabulary, run_naming, AuditInput, BatchOptions,
    MockProvider, NamingError, NamingProvider, NamingRequest,
};
use rf_rebuild::hw::flag;
use rf_rebuild::naming::{merge_name_rows, propose_names};
use rf_rebuild::profile::{FunctionProfile, RegUse, Understanding, Vectors};
use rf_rebuild::recipe::{match_recipe, Recipe};
use rf_rebuild::{entry, Atlas, Function, HwClass, Platform, RamMeaning, Stats};

// ---- stand-in game data ------------------------------------------------------

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// A synthetic atlas over a stand-in image; no real game involved.
fn shaped() -> (Understanding, Atlas, Vec<RamMeaning>, Vec<u8>) {
    let rom: Vec<u8> = (0..0x2000u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    let functions = (0..6u32)
        .map(|i| {
            let at = (i as usize) * 64;
            Function {
                entry_rom: at as u32,
                entry_cpu: 0x8000 + at as u32,
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
    let atlas = Atlas {
        prg_len: rom.len() as u32,
        mode_count: 1,
        blocks: vec![],
        functions,
        indirect_sites: vec![],
        volatile: vec![],
        stats: Stats::default(),
    };
    let mut fs: Vec<FunctionProfile> = (0..6u64)
        .map(|i| FunctionProfile {
            insns: 1000 + i,
            entries: 120,
            calls: 120,
            frames_active: 118,
            main_insns: 1000 + i,
            ..FunctionProfile::default()
        })
        .collect();
    fs[1].nmi_entries = 120;
    fs[2].regs.insert(
        0x4016,
        (
            HwClass::Controller,
            RegUse {
                reads: 960,
                writes: 240,
            },
        ),
    );
    fs[3].regs.insert(
        0x2007,
        (
            HwClass::Ppu,
            RegUse {
                reads: 0,
                writes: 640,
            },
        ),
    );
    fs[3].flags = flag::PPU_ADDR_NAMETABLE;
    fs[4].ram_write.insert(0x075A);
    fs[5].ram_read.insert(0x0086);
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
    let ram = vec![RamMeaning {
        grade: rf_rebuild::Grade::Observed,
        addr: 0x075A,
        width: 1,
        name: "lives".into(),
        source: "profile".into(),
    }];
    (u, atlas, ram, rom)
}

fn requests() -> (Vec<NamingRequest>, Atlas, Vec<RamMeaning>, Understanding) {
    let (u, atlas, ram, _) = shaped();
    let reqs = build_requests(&u, &atlas, &[], &ram, BatchOptions::default());
    assert!(!reqs.is_empty());
    (reqs, atlas, ram, u)
}

/// A chat-completions answer naming every function in `req`.
fn answer_for(req: &NamingRequest, stem: &str) -> String {
    let names: Vec<_> = req
        .functions
        .iter()
        .map(|f| {
            serde_json::json!({"id": f.id, "name": format!("{stem}_{}", f.id),
                "purpose": "stand-in purpose", "confidence": 70})
        })
        .collect();
    chat(&serde_json::json!({ "names": names }).to_string(), "stop")
}

fn chat(content: &str, finish: &str) -> String {
    serde_json::json!({
        "choices": [{"index": 0, "finish_reason": finish,
            "message": {"role": "assistant", "content": content}}]
    })
    .to_string()
}

// ---- request shape -------------------------------------------------------------

#[test]
fn request_body_is_chat_completions_with_the_shared_prompt_and_schema() {
    let (reqs, ..) = requests();
    let cfg = LocalConfig::default();
    let b = build_body(&cfg, &reqs[0]);
    assert_eq!(b["model"], "qwen/qwen3-8b");
    assert_eq!(b["stream"], false);
    assert_eq!(b["temperature"], 0);
    assert_eq!(b["messages"][0]["role"], "system");
    assert_eq!(b["messages"][1]["role"], "user");
    let sys = b["messages"][0]["content"].as_str().unwrap();
    assert!(sys.contains("snake_case") && sys.contains("not given any code"));
    assert_eq!(b["response_format"]["type"], "json_schema");
    assert_eq!(
        b["response_format"]["json_schema"]["schema"],
        rf_ai::anthropic::response_schema()
    );
    // The facts are the very same serialised request the cloud backend sends.
    let user = b["messages"][1]["content"].as_str().unwrap();
    assert!(user.contains(&serde_json::to_string(&reqs[0]).unwrap()));
    assert!(!user.contains("/no_think"));
    let cfg = LocalConfig {
        no_think: true,
        ..cfg
    };
    let b = build_body(&cfg, &reqs[0]);
    assert!(b["messages"][1]["content"]
        .as_str()
        .unwrap()
        .ends_with("/no_think"));
}

#[test]
fn the_payload_sent_locally_passes_the_behaviour_only_audit() {
    let (reqs, atlas, _, _) = requests();
    let (_, _, _, rom) = shaped();
    let mut allowed = closed_vocabulary();
    allowed.insert("lives".into());
    let hashes: Vec<u64> = atlas
        .functions
        .iter()
        .flat_map(|f| [f.byte_hash, f.shape_hash])
        .collect();
    for r in &reqs {
        audit_request(
            r,
            &AuditInput {
                rom: &rom,
                hashes: &hashes,
                allowed_strings: &allowed,
            },
        )
        .unwrap();
    }
}

#[test]
fn the_key_is_a_bearer_only_when_set_and_never_prints() {
    let mut cfg = LocalConfig::default();
    assert!(headers(&cfg).iter().all(|(k, _)| *k != "authorization"));
    cfg.api_key = ApiKey::new("sk-local-secret-123");
    let h = headers(&cfg);
    assert!(h
        .iter()
        .any(|(k, v)| *k == "authorization" && v == "Bearer sk-local-secret-123"));
    assert!(!format!("{cfg:?}").contains("secret"));
    // And it never serialises into settings.
    assert!(!serde_json::to_string(&cfg).unwrap().contains("secret"));
}

// ---- answers ---------------------------------------------------------------------

#[test]
fn a_good_answer_parses_and_reasoning_and_fences_are_tolerated() {
    let json = r#"{"names":[{"id":3,"name":"upload_nametable","purpose":"p","confidence":80}]}"#;
    for content in [
        json.to_string(),
        format!("<think>\nlet me see {{ }}\n</think>\n{json}"),
        format!("```json\n{json}\n```"),
    ] {
        let r = parse_response(200, &chat(&content, "stop")).unwrap();
        assert_eq!(r.names.len(), 1);
        assert_eq!(r.names[0].name, "upload_nametable");
    }
}

#[test]
fn finish_reasons_and_errors_are_checked_before_content() {
    let body = r#"{"names":[]}"#;
    assert_eq!(
        parse_response(200, &chat(body, "length")),
        Err(NamingError::Truncated)
    );
    assert!(matches!(
        parse_response(200, &chat(body, "content_filter")),
        Err(NamingError::Refused(_))
    ));
    assert!(matches!(
        parse_response(200, &chat(body, "tool_calls")),
        Err(NamingError::BadResponse(_))
    ));
    let refusal =
        r#"{"choices":[{"finish_reason":"stop","message":{"content":null,"refusal":"no"}}]}"#;
    assert_eq!(
        parse_response(200, refusal),
        Err(NamingError::Refused("no".into()))
    );
    assert!(matches!(
        parse_response(200, "not json"),
        Err(NamingError::BadResponse(_))
    ));
    assert!(matches!(
        parse_response(200, &chat("sorry, I cannot", "stop")),
        Err(NamingError::BadResponse(_))
    ));
    assert!(matches!(
        parse_response(200, r#"{"choices":[]}"#),
        Err(NamingError::BadResponse(_))
    ));
    assert_eq!(
        parse_response(404, r#"{"error":{"message":"model not loaded"}}"#),
        Err(NamingError::Http {
            status: 404,
            message: "model not loaded".into()
        })
    );
    assert_eq!(
        parse_response(400, r#"{"error":"plain"}"#),
        Err(NamingError::Http {
            status: 400,
            message: "plain".into()
        })
    );
}

struct Scripted(RefCell<Vec<Result<HttpReply, String>>>, RefCell<usize>);

impl Transport for Scripted {
    fn post(&self, _: &str, _: &[(&'static str, String)], _: &str) -> Result<HttpReply, String> {
        *self.1.borrow_mut() += 1;
        self.0.borrow_mut().remove(0)
    }
}

fn rep(status: u16, body: &str) -> Result<HttpReply, String> {
    Ok(HttpReply {
        status,
        body: body.to_string(),
        retry_after: None,
    })
}

#[test]
fn server_errors_retry_once_and_client_errors_do_not() {
    let (reqs, ..) = requests();
    let cfg = LocalConfig::default();
    let pol = RetryPolicy {
        max_attempts: 3,
        base: Duration::from_millis(1),
        cap: Duration::from_millis(5),
    };
    let ok = answer_for(&reqs[0], "fn");
    let t = Scripted(
        RefCell::new(vec![Err("refused".into()), rep(503, "busy"), rep(200, &ok)]),
        RefCell::new(0),
    );
    let mut slept = Vec::new();
    let r = call_with_retries(&t, &cfg, &reqs[0], &pol, &mut |d| slept.push(d)).unwrap();
    assert_eq!(r.names.len(), reqs[0].functions.len());
    assert_eq!(*t.1.borrow(), 3);
    assert_eq!(slept.len(), 2);

    let t = Scripted(
        RefCell::new(vec![rep(400, r#"{"error":{"message":"bad"}}"#)]),
        RefCell::new(0),
    );
    assert!(matches!(
        call_with_retries(&t, &cfg, &reqs[0], &pol, &mut |_| {}),
        Err(NamingError::Http { status: 400, .. })
    ));
    assert_eq!(*t.1.borrow(), 1);

    let t = Scripted(
        RefCell::new(vec![
            Err("down".into()),
            Err("down".into()),
            Err("down".into()),
        ]),
        RefCell::new(0),
    );
    assert_eq!(
        call_with_retries(&t, &cfg, &reqs[0], &pol, &mut |_| {}),
        Err(NamingError::Network("down".into()))
    );
    assert_eq!(*t.1.borrow(), 3);
}

// ---- local only --------------------------------------------------------------------

#[test]
fn only_loopback_urls_are_accepted() {
    for ok in [
        "http://localhost:1234/v1",
        "http://127.0.0.1:8080/v1",
        "http://[::1]:1234/v1",
    ] {
        assert!(is_loopback_url(ok), "{ok}");
    }
    for bad in [
        "https://localhost:1234/v1",
        "http://example.com/v1",
        "http://192.168.1.5:1234/v1",
        "http://localhost@evil.example/v1",
        "localhost:1234",
        "",
    ] {
        assert!(!is_loopback_url(bad), "{bad}");
    }
    let cfg = LocalConfig {
        base_url: "http://api.example.com/v1".into(),
        ..LocalConfig::default()
    };
    assert!(matches!(
        LocalProvider::new(cfg),
        Err(LocalConfigError::NotLoopback(_))
    ));
    let cfg = LocalConfig {
        model: "  ".into(),
        ..LocalConfig::default()
    };
    assert_eq!(
        LocalProvider::new(cfg).err(),
        Some(LocalConfigError::EmptyModel)
    );
    // The transport itself refuses a non-loopback literal before connecting.
    let e = LoopbackTransport::default()
        .post("http://192.0.2.1:9/v1/chat/completions", &[], "{}")
        .unwrap_err();
    assert!(e.contains("not a loopback"), "{e}");
    assert_eq!(
        parse_http_url("http://localhost:1234/v1/x").unwrap().port,
        1234
    );
}

#[test]
fn replies_parse_plain_and_chunked() {
    let plain = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}";
    assert_eq!(parse_reply(plain).unwrap().body, "{}");
    let chunked = b"HTTP/1.1 429 Too Many\r\nTransfer-Encoding: chunked\r\nRetry-After: 3\r\n\r\n3\r\nabc\r\n2;ext\r\nde\r\n0\r\n\r\n";
    let r = parse_reply(chunked).unwrap();
    assert_eq!(
        (r.status, r.body.as_str(), r.retry_after),
        (429, "abcde", Some(3))
    );
    for bad in [
        &b"garbage"[..],
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n9\r\nabc",
        b"HTTP/1.1 xx\r\n\r\n",
    ] {
        assert!(parse_reply(bad).is_err());
    }
}

// ---- a fake server on 127.0.0.1 -------------------------------------------------------

/// Serves `replies` (raw HTTP responses), one per connection; records each
/// request's head and body.
type Seen = Arc<Mutex<Vec<(String, String)>>>;

fn serve(replies: Vec<Vec<u8>>) -> (u16, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    thread::spawn(move || {
        for reply in replies {
            let Ok((mut s, _)) = listener.accept() else {
                return;
            };
            s.set_read_timeout(Some(Duration::from_secs(10))).ok();
            let mut raw = Vec::new();
            let mut buf = [0u8; 4096];
            let (head, body) = loop {
                let n = s.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    break (String::new(), String::new());
                }
                raw.extend_from_slice(&buf[..n]);
                if let Some(p) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&raw[..p]).to_string();
                    let len = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    if raw.len() >= p + 4 + len {
                        break (
                            head,
                            String::from_utf8_lossy(&raw[p + 4..p + 4 + len]).to_string(),
                        );
                    }
                }
            };
            log.lock().unwrap().push((head, body));
            s.write_all(&reply).ok();
        }
    });
    (port, seen)
}

fn http_ok(body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

fn http_chunked(body: &str) -> Vec<u8> {
    let (a, b) = body.split_at(body.len() / 2);
    format!(
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{a}\r\n{:x}\r\n{b}\r\n0\r\n\r\n",
        a.len(),
        b.len()
    )
    .into_bytes()
}

#[test]
fn names_are_produced_offline_over_a_loopback_server() {
    let (reqs, ..) = requests();
    // First request answered plainly, second chunked.
    let two: Vec<NamingRequest> = vec![reqs[0].clone(), reqs[0].clone()];
    let (port, seen) = serve(vec![
        http_ok(&answer_for(&reqs[0], "plain")),
        http_chunked(&answer_for(&reqs[0], "chunk")),
    ]);
    let cfg = LocalConfig {
        base_url: format!("http://127.0.0.1:{port}/v1"),
        model: "qwen/qwen3-8b".into(),
        api_key: ApiKey::new("sk-fake-key-do-not-leak"),
        timeout_secs: 20,
        ..LocalConfig::default()
    };
    let provider = LocalProvider::new(cfg).unwrap();
    assert_eq!(provider.model(), "local:qwen_qwen3-8b");
    let mut taken = Vec::new();
    let props = run_naming(&provider, &two, &mut taken).unwrap();
    // Same ids answered twice: the second batch's clashing names get the id
    // appended, none is lost, all are unique.
    assert_eq!(props.len(), 2 * reqs[0].functions.len());
    let mut names: Vec<_> = props.iter().map(|p| p.name.clone()).collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), props.len());
    assert!(props.iter().all(|p| p.source == "ai:local:qwen_qwen3-8b"));
    assert!(props.iter().any(|p| p.name.starts_with("plain_")));
    assert!(props.iter().any(|p| p.name.starts_with("chunk_")));

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    let (head, body) = &seen[0];
    assert!(
        head.starts_with("POST /v1/chat/completions HTTP/1.1"),
        "{head}"
    );
    assert!(head
        .to_ascii_lowercase()
        .contains("authorization: bearer sk-fake-key-do-not-leak"));
    // The body is the shared request, and the key is not in it.
    assert!(!body.contains("sk-fake-key"));
    let v: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(v["model"], "qwen/qwen3-8b");
}

#[test]
fn an_unreachable_server_is_a_network_error_not_a_hang() {
    // Bind then drop: the port is closed, so the connection is refused.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let cfg = LocalConfig {
        base_url: format!("http://127.0.0.1:{port}/v1"),
        ..LocalConfig::default()
    };
    let (reqs, ..) = requests();
    let provider = LocalProvider::new(cfg).unwrap().with_policy(RetryPolicy {
        max_attempts: 2,
        base: Duration::from_millis(1),
        cap: Duration::from_millis(2),
    });
    assert!(matches!(
        provider.propose(&reqs[0]),
        Err(NamingError::Network(_))
    ));
}

// ---- settings --------------------------------------------------------------------------

#[test]
fn the_backend_is_chosen_by_settings_and_defaults_to_cloud() {
    assert_eq!(NamingBackend::default(), NamingBackend::Cloud);
    assert_eq!(NamingBackend::parse(" Local "), Some(NamingBackend::Local));
    assert_eq!(NamingBackend::parse("cloud"), Some(NamingBackend::Cloud));
    assert_eq!(NamingBackend::parse("gpu"), None);
    let b: NamingBackend = serde_json::from_str("\"local\"").unwrap();
    assert_eq!(b.label(), "local");
    // A settings table with only some keys still loads.
    let cfg: LocalConfig = serde_json::from_str(r#"{"model":"qwen/qwen3.8-27b"}"#).unwrap();
    assert_eq!(cfg.model, "qwen/qwen3.8-27b");
    assert_eq!(cfg.base_url, "http://localhost:1234/v1");
    assert_eq!(cfg.endpoint(), "http://localhost:1234/v1/chat/completions");
    assert!(cfg.batch_options().per_request < BatchOptions::default().per_request);
}

// ---- names are labels: lockstep stays green ----------------------------------------------

/// What an executor reads from a recipe: which function each row resolves to
/// and the hook points on it. Names, sources and confidences are not in it.
fn execution_view(atlas: &Atlas, r: &Recipe) -> Vec<(u32, bool, bool, Option<u32>)> {
    let report = match_recipe(atlas, r);
    let mut v: Vec<_> = report
        .matched
        .iter()
        .map(|m| {
            let row = r.functions.iter().find(|f| f.name == m.name).unwrap();
            (
                m.function,
                row.hooks.entry,
                row.hooks.exit,
                row.address_hint,
            )
        })
        .collect();
    v.sort_unstable();
    v
}

#[test]
fn naming_on_or_off_leaves_execution_identical() {
    let (u, atlas, ram, _) = shaped();
    let before_atlas = atlas.to_bytes();
    let rules = propose_names(&u, &atlas);

    // Off: the rules' names alone.
    let mut off = Recipe::default();
    merge_name_rows(&mut off, &atlas, &rules);

    // Cloud-shaped (the mock): rules plus its names.
    let reqs = build_requests(&u, &atlas, &rules, &ram, BatchOptions::default());
    let mut taken: Vec<String> = rules.iter().map(|p| p.name.clone()).collect();
    let mock = run_naming(&MockProvider, &reqs, &mut taken).unwrap();
    let mut cloud = Recipe::default();
    let all: Vec<_> = rules.iter().cloned().chain(mock).collect();
    merge_name_rows(&mut cloud, &atlas, &all);

    // Local: a fake server answers with entirely different names.
    let answers: Vec<Vec<u8>> = reqs
        .iter()
        .map(|r| http_ok(&answer_for(r, "local_name")))
        .collect();
    let (port, _) = serve(answers);
    let provider = LocalProvider::new(LocalConfig {
        base_url: format!("http://localhost:{port}/v1"),
        ..LocalConfig::default()
    })
    .unwrap();
    let mut taken: Vec<String> = rules.iter().map(|p| p.name.clone()).collect();
    let local = run_naming(&provider, &reqs, &mut taken).unwrap();
    assert!(!local.is_empty());
    let mut loc = Recipe::default();
    let all: Vec<_> = rules.iter().cloned().chain(local).collect();
    merge_name_rows(&mut loc, &atlas, &all);

    // The labels differ...
    assert!(loc
        .functions
        .iter()
        .any(|f| f.source.starts_with("ai:local:")));
    assert!(cloud.functions.iter().any(|f| f.source == "ai:mock"));
    // ...the execution view is identical whichever backend (or none) named,
    // for the functions both runs covered, and every extra row only adds an
    // inert entry/exit hook pair on its own function.
    let e_off = execution_view(&atlas, &off);
    for view in [execution_view(&atlas, &cloud), execution_view(&atlas, &loc)] {
        for row in &e_off {
            assert!(view.contains(row), "rule-named function lost: {row:?}");
        }
        assert!(view.iter().all(|r| r.1 && r.2), "hooks are the inert pair");
    }
    assert_eq!(
        execution_view(&atlas, &cloud).len(),
        execution_view(&atlas, &loc).len()
    );
    // And the atlas itself, what the executor runs, never moved.
    assert_eq!(atlas.to_bytes(), before_atlas);
}

// ---- the real local server (not run by default) ---------------------------------------------

#[test]
#[ignore = "calls the real LM Studio server on localhost:1234 with qwen/qwen3-8b loaded or loadable"]
fn real_lm_studio_names_synthetic_functions() {
    let (u, atlas, ram, _) = shaped();
    let rules = propose_names(&u, &atlas);
    let cfg = LocalConfig {
        no_think: true,
        ..LocalConfig::default()
    };
    let opts = cfg.batch_options();
    let reqs = build_requests(&u, &atlas, &rules, &ram, opts);
    let provider = LocalProvider::new(cfg).unwrap();
    let mut taken: Vec<String> = rules.iter().map(|p| p.name.clone()).collect();
    let props = run_naming(&provider, &reqs, &mut taken).expect("LM Studio answered");
    assert!(!props.is_empty());
    for p in &props {
        println!(
            "fn {} -> {} ({}%) [{}] {:?}",
            p.function, p.name, p.confidence, p.source, p.evidence
        );
    }
}
