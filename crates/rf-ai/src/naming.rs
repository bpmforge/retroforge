//! Opt-in AI naming for the Rebuild lab (ticket W36-09; D-026, with D-024).
//!
//! The rules namer in `rf_rebuild::naming` names what hardware facts name.
//! This pass asks a model for the rest: given what each function was
//! *observed doing*, propose a short name and a one-line purpose.
//!
//! ## What may leave the machine (D-024)
//!
//! Behaviour facts only. [`FunctionFacts`] is the whole payload and it is a
//! closed type: counts, ids, hardware register classes and addresses, RAM
//! ranges with the recipe's own RAM names, already-accepted names of
//! neighbouring functions. It has no field that can hold ROM bytes, a
//! disassembly, lifted code, a byte hash or a shape hash, **and no static
//! property of the code at all** (no function size, no entry address, no
//! block count): everything in it was measured while the game ran. The
//! `payload_*` tests serialise requests for NES- and SNES-shaped input and
//! fail on any key outside the allow-list, any string outside the closed
//! vocabulary, any hash and any window of a stand-in ROM image.
//!
//! ## Off by default
//!
//! Nothing here runs unless the user turned the pass on and gave a key (the
//! shell enforces that); a [`NamingProvider`] is only ever constructed then.
//! Model output is untrusted: names are re-validated by [`sanitize_name`],
//! ids must be ones that were asked about, and a proposal changes no
//! execution (the lockstep tests apply every name as an inert hook).
//!
//! ## Where the trait lives
//!
//! Here, in `rf-ai`, not in `rf-rebuild`: `rf-rebuild` is a core-layer crate
//! with no serde and no network concept (validate-arch rule 2b), and
//! FR-AI-001 keeps AI off the frame path. `rf-ai` may depend on `rf-rebuild`
//! for the profile it derives facts from. The real provider
//! ([`crate::anthropic`]) is pure request building, response parsing and
//! retry logic over a [`crate::anthropic::Transport`]; the app shell supplies
//! the transport over its existing `ureq`.

use std::collections::BTreeMap;

use rf_rebuild::hw::flag;
use rf_rebuild::naming::Proposal;
use rf_rebuild::profile::{FunctionProfile, RangeSet, Understanding};
use rf_rebuild::{Atlas, Platform, RamMeaning};
use serde::{Deserialize, Serialize};

/// Bumped when the payload shape changes.
pub const PAYLOAD_VERSION: u32 = 1;
/// Functions per request unless the caller says otherwise.
pub const DEFAULT_PER_REQUEST: usize = 40;
/// Requests per run unless the caller says otherwise (cost cap).
pub const DEFAULT_MAX_REQUESTS: usize = 6;
const MAX_EDGES: usize = 6;
const MAX_RAM_RANGES: usize = 16;

/// Value-derived facts, a closed vocabulary (see `rf_rebuild::hw::flag`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fact {
    PaletteAddressed,
    NametableAddressed,
    VblankFlagSeen,
    Sprite0FlagSeen,
    DmaToVram,
    DmaToOam,
    DmaToCgram,
}

impl Fact {
    fn from_bits(bits: u32) -> Vec<Self> {
        [
            (flag::PPU_ADDR_PALETTE, Self::PaletteAddressed),
            (flag::PPU_ADDR_NAMETABLE, Self::NametableAddressed),
            (flag::STATUS_VBLANK_SEEN, Self::VblankFlagSeen),
            (flag::STATUS_SPRITE0_SEEN, Self::Sprite0FlagSeen),
            (flag::DMA_TO_VRAM, Self::DmaToVram),
            (flag::DMA_TO_OAM, Self::DmaToOam),
            (flag::DMA_TO_CGRAM, Self::DmaToCgram),
        ]
        .into_iter()
        .filter(|(b, _)| bits & b != 0)
        .map(|(_, f)| f)
        .collect()
    }
}

/// `{ "id": 7, "count": 120 }`: a call-graph edge to another function.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub id: u32,
    pub count: u64,
}

/// A hardware register the function touched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegFact {
    /// `HwClass::label`: ppu, apu, controller, oam_dma, dma, mapper, ...
    pub class: String,
    pub addr: u16,
    pub reads: u64,
    pub writes: u64,
}

/// A run of RAM addresses, with the recipe's names for what lies in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RamFact {
    pub start: u32,
    pub end: u32,
    pub names: Vec<String>,
}

/// An already-accepted name of a caller or callee.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NeighbourName {
    pub id: u32,
    pub name: String,
}

/// What the rules namer thought, if it had a view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleHint {
    pub name: String,
    pub confidence: u8,
}

/// Everything sent about one function.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FunctionFacts {
    /// Opaque atlas index; the reply refers to functions by it.
    pub id: u32,
    pub calls_per_100_frames: u32,
    pub frames_active_pct: u8,
    pub insns_per_call: u32,
    pub main_pct: u8,
    pub nmi_pct: u8,
    pub irq_pct: u8,
    pub entered_by_interrupt: bool,
    pub callers: Vec<Edge>,
    pub callees: Vec<Edge>,
    pub registers: Vec<RegFact>,
    pub facts: Vec<Fact>,
    pub ram_read: Vec<RamFact>,
    pub ram_written: Vec<RamFact>,
    pub neighbour_names: Vec<NeighbourName>,
    pub rule_hint: Option<RuleHint>,
}

/// One request: a console label and a batch of functions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamingRequest {
    pub payload_version: u32,
    /// `"nes"` or `"snes"`.
    pub console: String,
    /// Frames the game was observed for.
    pub frames: u32,
    pub functions: Vec<FunctionFacts>,
}

/// A model's proposal for one function.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposedName {
    pub id: u32,
    pub name: String,
    pub purpose: String,
    /// 0-100.
    pub confidence: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct NamingResponse {
    pub names: Vec<ProposedName>,
}

/// Why a naming request produced nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NamingError {
    /// No API key is configured.
    NoKey,
    /// The transport failed (DNS, TLS, timeout), after any retries.
    Network(String),
    /// A non-2xx answer that is not retryable, or retries ran out.
    Http { status: u16, message: String },
    /// The model declined (`stop_reason: refusal`).
    Refused(String),
    /// The answer was cut off (`stop_reason: max_tokens`).
    Truncated,
    /// The answer was not the JSON we asked for.
    BadResponse(String),
    /// More requests than the run's cap.
    OverBudget,
}

impl std::fmt::Display for NamingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoKey => write!(f, "no API key configured"),
            Self::Network(e) => write!(f, "network error: {e}"),
            Self::Http { status, message } => write!(f, "HTTP {status}: {message}"),
            Self::Refused(why) => write!(f, "the model declined the request: {why}"),
            Self::Truncated => write!(f, "the answer was cut off (max_tokens)"),
            Self::BadResponse(e) => write!(f, "unusable answer: {e}"),
            Self::OverBudget => write!(f, "request cap reached"),
        }
    }
}

impl std::error::Error for NamingError {}

/// Something that turns behaviour facts into proposed names.
///
/// `Send` so the shell can run it on a worker thread.
pub trait NamingProvider: Send {
    /// Model label for `ai:<model>` sources.
    fn model(&self) -> &str;
    /// Answer one request.
    ///
    /// # Errors
    /// [`NamingError`].
    fn propose(&self, request: &NamingRequest) -> Result<NamingResponse, NamingError>;
}

/// A deterministic provider for tests and the UI's dry run: names each
/// function from its facts alone, no network.
#[derive(Debug, Clone, Default)]
pub struct MockProvider;

impl NamingProvider for MockProvider {
    fn model(&self) -> &str {
        "mock"
    }

    fn propose(&self, request: &NamingRequest) -> Result<NamingResponse, NamingError> {
        let names = request
            .functions
            .iter()
            .map(|f| {
                let top = f
                    .registers
                    .iter()
                    .max_by_key(|r| (r.reads + r.writes, std::cmp::Reverse(r.addr)))
                    .map(|r| r.class.as_str());
                let (stem, purpose) = match top {
                    Some(class) => (
                        format!("{class}_helper"),
                        format!("Touches {class} registers"),
                    ),
                    None if !f.ram_written.is_empty() => (
                        "state_update".to_string(),
                        "Updates game state in RAM".to_string(),
                    ),
                    None => ("routine".to_string(), "No hardware access".to_string()),
                };
                ProposedName {
                    id: f.id,
                    name: format!("{stem}_{}", f.id),
                    purpose,
                    confidence: 50,
                }
            })
            .collect();
        Ok(NamingResponse { names })
    }
}

/// A model-proposed name as a recipe-safe identifier: lower-case snake case,
/// 3-48 characters, starting with a letter. `None` when it cannot be made one.
#[must_use]
pub fn sanitize_name(raw: &str) -> Option<String> {
    let mut out = String::new();
    let mut last_underscore = true;
    for c in raw.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_underscore = false;
        } else if !last_underscore {
            out.push('_');
            last_underscore = true;
        }
    }
    let out = out.trim_matches('_').to_string();
    let ok = (3..=48).contains(&out.len()) && out.starts_with(|c: char| c.is_ascii_lowercase());
    ok.then_some(out)
}

/// What to ask about and how to batch it.
#[derive(Debug, Clone, Copy)]
pub struct BatchOptions {
    pub per_request: usize,
    pub max_requests: usize,
    /// Also ask about functions the rules already named.
    pub include_named: bool,
}

impl Default for BatchOptions {
    fn default() -> Self {
        Self {
            per_request: DEFAULT_PER_REQUEST,
            max_requests: DEFAULT_MAX_REQUESTS,
            include_named: false,
        }
    }
}

fn pct(part: u64, whole: u64) -> u8 {
    if whole == 0 {
        0
    } else {
        (part.saturating_mul(100) / whole).min(100) as u8
    }
}

fn ram_facts(set: &RangeSet, ram: &[RamMeaning]) -> Vec<RamFact> {
    set.ranges()
        .iter()
        .take(MAX_RAM_RANGES)
        .map(|&(start, end)| {
            let mut names: Vec<String> = ram
                .iter()
                .filter(|m| {
                    let last = m.addr.saturating_add(u32::from(m.width.max(1))) - 1;
                    m.addr <= end && last >= start
                })
                .map(|m| m.name.clone())
                .collect();
            names.sort();
            names.dedup();
            RamFact { start, end, names }
        })
        .collect()
}

fn top_edges(edges: &BTreeMap<u32, u64>) -> Vec<Edge> {
    let mut v: Vec<Edge> = edges
        .iter()
        .map(|(&id, &count)| Edge { id, count })
        .collect();
    v.sort_by(|a, b| b.count.cmp(&a.count).then(a.id.cmp(&b.id)));
    v.truncate(MAX_EDGES);
    v
}

fn facts_of(
    id: u32,
    p: &FunctionProfile,
    frames: u32,
    names: &BTreeMap<u32, &Proposal>,
) -> FunctionFacts {
    let calls = p.entries.max(p.calls);
    let callers = top_edges(&p.callers);
    let callees = top_edges(&p.callees);
    let neighbour_names = callers
        .iter()
        .chain(&callees)
        .filter_map(|e| names.get(&e.id).map(|pr| (e.id, pr.name.clone())))
        .map(|(id, name)| NeighbourName { id, name })
        .collect();
    FunctionFacts {
        id,
        calls_per_100_frames: u32::try_from(calls.saturating_mul(100) / u64::from(frames.max(1)))
            .unwrap_or(u32::MAX),
        frames_active_pct: pct(u64::from(p.frames_active), u64::from(frames)),
        insns_per_call: u32::try_from(p.insns / calls.max(1)).unwrap_or(u32::MAX),
        main_pct: pct(p.main_insns, p.insns),
        nmi_pct: pct(p.nmi_insns, p.insns),
        irq_pct: pct(p.irq_insns, p.insns),
        entered_by_interrupt: p.nmi_entries + p.irq_entries > 0,
        callers,
        callees,
        registers: p
            .regs
            .iter()
            .map(|(&addr, (class, u))| RegFact {
                class: class.label().to_string(),
                addr,
                reads: u.reads,
                writes: u.writes,
            })
            .collect(),
        facts: Fact::from_bits(p.flags),
        ram_read: Vec::new(),
        ram_written: Vec::new(),
        neighbour_names,
        rule_hint: names.get(&id).map(|pr| RuleHint {
            name: pr.base.clone(),
            confidence: pr.confidence,
        }),
    }
}

/// Derive the requests for an understand run: behaviour facts of the observed
/// functions (those the rules left unnamed, busiest first, unless
/// `include_named`), batched and capped. `ram` is the recipe's RAM names.
#[must_use]
pub fn build_requests(
    u: &Understanding,
    _atlas: &Atlas,
    proposals: &[Proposal],
    ram: &[RamMeaning],
    opts: BatchOptions,
) -> Vec<NamingRequest> {
    let named: BTreeMap<u32, &Proposal> = proposals.iter().map(|p| (p.function, p)).collect();
    let mut order: Vec<u32> = (0..u.functions.len() as u32)
        .filter(|&i| {
            let p = &u.functions[i as usize];
            !p.is_empty() && p.insns > 0 && (opts.include_named || !named.contains_key(&i))
        })
        .collect();
    order.sort_by(|&a, &b| {
        u.functions[b as usize]
            .insns
            .cmp(&u.functions[a as usize].insns)
            .then(a.cmp(&b))
    });
    let console = match u.platform {
        Platform::Nes => "nes",
        Platform::Snes => "snes",
    };
    order
        .chunks(opts.per_request.max(1))
        .take(opts.max_requests)
        .map(|chunk| NamingRequest {
            payload_version: PAYLOAD_VERSION,
            console: console.to_string(),
            frames: u.frames,
            functions: chunk
                .iter()
                .map(|&i| {
                    let p = &u.functions[i as usize];
                    let mut f = facts_of(i, p, u.frames, &named);
                    f.ram_read = ram_facts(&p.ram_read, ram);
                    f.ram_written = ram_facts(&p.ram_write, ram);
                    f
                })
                .collect(),
        })
        .collect()
}

/// Turn a model's answer into proposals: only ids that were asked about, only
/// names that survive [`sanitize_name`], unique against `taken` and each
/// other (a clash gets the id appended), confidence clamped to 0-100.
#[must_use]
pub fn proposals_from_response(
    model: &str,
    request: &NamingRequest,
    response: &NamingResponse,
    taken: &mut Vec<String>,
) -> Vec<Proposal> {
    let asked: Vec<u32> = request.functions.iter().map(|f| f.id).collect();
    let mut out = Vec::new();
    let mut seen_ids: Vec<u32> = Vec::new();
    for n in &response.names {
        if !asked.contains(&n.id) || seen_ids.contains(&n.id) {
            continue;
        }
        let Some(base) = sanitize_name(&n.name) else {
            continue;
        };
        let mut name = base.clone();
        if taken.contains(&name) {
            name = format!("{base}_{}", n.id);
        }
        if taken.contains(&name) {
            continue;
        }
        taken.push(name.clone());
        seen_ids.push(n.id);
        let purpose: String = n
            .purpose
            .chars()
            .filter(|c| !c.is_control())
            .take(160)
            .collect();
        out.push(Proposal {
            function: n.id,
            name,
            base,
            source: format!("ai:{model}"),
            confidence: n.confidence.min(100) as u8,
            evidence: vec![purpose],
            also: Vec::new(),
        });
    }
    out
}

/// Ask `provider` about every request, stopping at the first failure.
///
/// # Errors
/// The first [`NamingError`]; proposals gathered before it are discarded so a
/// run is all-or-nothing.
pub fn run_naming(
    provider: &dyn NamingProvider,
    requests: &[NamingRequest],
    taken: &mut Vec<String>,
) -> Result<Vec<Proposal>, NamingError> {
    let mut out = Vec::new();
    for r in requests {
        let resp = provider.propose(r)?;
        out.extend(proposals_from_response(provider.model(), r, &resp, taken));
    }
    Ok(out)
}

// ---- the payload audit ---------------------------------------------------------

/// Every JSON object key a serialised [`NamingRequest`] may contain. A new
/// field in the payload types fails [`audit_request`] until someone decides
/// it is behaviour and not code and adds it here.
pub const ALLOWED_KEYS: &[&str] = &[
    "payload_version",
    "console",
    "frames",
    "functions",
    "id",
    "calls_per_100_frames",
    "frames_active_pct",
    "insns_per_call",
    "main_pct",
    "nmi_pct",
    "irq_pct",
    "entered_by_interrupt",
    "callers",
    "callees",
    "count",
    "registers",
    "class",
    "addr",
    "reads",
    "writes",
    "facts",
    "ram_read",
    "ram_written",
    "start",
    "end",
    "names",
    "neighbour_names",
    "name",
    "rule_hint",
    "confidence",
];

/// The fixed words a request may contain as string values: console labels,
/// hardware class labels, fact names. Names (RAM names from the recipe, rule
/// names of neighbours) are added by the caller from what it knows.
#[must_use]
pub fn closed_vocabulary() -> std::collections::BTreeSet<String> {
    [
        "nes",
        "snes",
        "ppu",
        "apu",
        "controller",
        "oam_dma",
        "dma",
        "mapper",
        "math",
        "cpu_status",
        "wram_port",
        "coprocessor",
        "palette_addressed",
        "nametable_addressed",
        "vblank_flag_seen",
        "sprite0_flag_seen",
        "dma_to_vram",
        "dma_to_oam",
        "dma_to_cgram",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

/// What an audit checks a request against.
pub struct AuditInput<'a> {
    /// The cartridge image (or any stand-in): no 4-byte window of it may
    /// appear in the payload, as hex or as a decimal list.
    pub rom: &'a [u8],
    /// Hashes of the code (an atlas's `byte_hash` and `shape_hash` values):
    /// none may appear, in decimal or hex.
    pub hashes: &'a [u64],
    /// Every string value the payload may hold: [`closed_vocabulary`] plus
    /// the names the caller supplied (recipe RAM names, accepted rule names).
    pub allowed_strings: &'a std::collections::BTreeSet<String>,
}

fn walk(v: &serde_json::Value, f: &mut dyn FnMut(&serde_json::Value)) {
    f(v);
    match v {
        serde_json::Value::Object(m) => m.values().for_each(|v| walk(v, f)),
        serde_json::Value::Array(a) => a.iter().for_each(|v| walk(v, f)),
        _ => {}
    }
}

/// Check that a request is behaviour facts only (D-024): every object key is
/// in [`ALLOWED_KEYS`], every string is in `allowed_strings`, no code hash and
/// no window of the ROM appears anywhere in the serialised text. The same
/// check the unit tests run on synthetic data and the ROM-gated tests run on
/// real SMB and SMW requests.
///
/// # Errors
/// A sentence naming the first violation.
pub fn audit_request(req: &NamingRequest, input: &AuditInput<'_>) -> Result<(), String> {
    let text = serde_json::to_string(req).map_err(|e| e.to_string())?;
    audit_text(&text, input)
}

/// [`audit_request`] over the serialised JSON itself (what actually goes on
/// the wire), so a test can audit text that no [`NamingRequest`] could
/// produce.
///
/// # Errors
/// A sentence naming the first violation.
pub fn audit_text(text: &str, input: &AuditInput<'_>) -> Result<(), String> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let mut problem: Option<String> = None;
    walk(&value, &mut |node| {
        if problem.is_some() {
            return;
        }
        match node {
            serde_json::Value::Object(m) => {
                if let Some(k) = m.keys().find(|k| !ALLOWED_KEYS.contains(&k.as_str())) {
                    problem = Some(format!("key `{k}` is not allowed"));
                }
            }
            serde_json::Value::String(s) if !input.allowed_strings.contains(s) => {
                problem = Some(format!("string `{s}` is outside the closed vocabulary"));
            }
            _ => {}
        }
    });
    if let Some(p) = problem {
        return Err(p);
    }
    for &h in input.hashes {
        if text.contains(&h.to_string()) || text.contains(&format!("{h:x}")) {
            return Err("a code hash is in the payload".to_string());
        }
    }
    // ROM windows. Indexed once (a cartridge is up to megabytes and the text
    // is up to hundreds of kilobytes: scanning text for every window would be
    // quadratic), then the text is scanned once for each way a window could
    // be written.
    let windows: std::collections::HashSet<u32> = input
        .rom
        .windows(4)
        .map(|w| u32::from_be_bytes([w[0], w[1], w[2], w[3]]))
        .collect();
    let lower = text.to_ascii_lowercase();
    let b = lower.as_bytes();
    // As eight hex digits in a row. A window with no letter in its hex is all
    // digits, which a count can equal by accident; the decimal form and the
    // key/string checks cover what could matter there.
    for i in 0..b.len().saturating_sub(7) {
        let run = &b[i..i + 8];
        if run.iter().all(u8::is_ascii_hexdigit) && run.iter().any(u8::is_ascii_lowercase) {
            if let Ok(v) = u32::from_str_radix(std::str::from_utf8(run).unwrap_or(""), 16) {
                if windows.contains(&v) {
                    return Err("a window of the ROM is in the payload (hex)".to_string());
                }
            }
        }
    }
    // As four byte values in a row separated by commas.
    let mut run: Vec<u8> = Vec::new();
    for token in text.split(|c: char| !(c.is_ascii_digit() || c == ',')) {
        run.clear();
        for n in token.split(',') {
            match n.parse::<u8>() {
                Ok(v) => run.push(v),
                Err(_) => run.clear(),
            }
            if run.len() >= 4 {
                let w = &run[run.len() - 4..];
                if windows.contains(&u32::from_be_bytes([w[0], w[1], w[2], w[3]])) {
                    return Err("a window of the ROM is in the payload (decimal list)".to_string());
                }
            }
        }
    }
    Ok(())
}
