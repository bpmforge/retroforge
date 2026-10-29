//! The local, on-device naming backend (ticket W40-02; D-026, D-024).
//!
//! A second [`NamingProvider`] next to the cloud one in [`crate::anthropic`].
//! It talks to an OpenAI-compatible chat server running on this machine (LM
//! Studio at `http://localhost:1234/v1`, llama.cpp's `llama-server`, Ollama's
//! `/v1`), so names are produced with no network at all: the request never
//! leaves the loopback interface ([`crate::localhttp::LoopbackTransport`]
//! refuses anything else, and [`LocalProvider::new`] refuses a non-loopback
//! URL up front).
//!
//! ## Same path as the cloud backend
//!
//! The request is the same [`NamingRequest`] (the behaviour-facts payload and
//! its [`crate::naming::audit_request`] guard), the system prompt is the same
//! [`crate::anthropic`] one, the answer is the same JSON shape
//! ([`crate::anthropic::response_schema`]) and goes through the same
//! untrusted-output filter ([`crate::naming::proposals_from_response`]:
//! asked-about ids only, [`crate::naming::sanitize_name`], unique names,
//! clamped confidence). Only the wire format differs (chat completions, not
//! the Messages API) and the answer is parsed the same defensive way: the
//! `finish_reason` first, then serde, never string-matching.
//!
//! ## Names are labels
//!
//! Like every proposal, a name from this backend changes no execution. It
//! becomes a recipe row with `source = "ai:<label>"` (grade `candidate`) whose
//! hooks are the same inert entry/exit hooks every named function gets; the
//! atlas and the profile are only borrowed. `tests/local_naming.rs` shows the
//! atlas bytes and the recipe match result are identical with naming off,
//! with the cloud mock, and with this backend.
//!
//! ## Settings
//!
//! [`NamingBackend`] is the choice (`cloud`, the default, or `local`) and
//! [`LocalConfig`] the local side (URL, model, optional key, timeouts); both
//! are plain serde types for the shell's `[ai_naming]` table. Nothing here
//! runs unless the user turned naming on and chose `local`.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::anthropic::{
    is_retryable, response_schema, ApiKey, HttpReply, RetryPolicy, Transport, SYSTEM,
};
use crate::localhttp::{is_loopback_url, LoopbackTransport};
use crate::naming::{
    BatchOptions, NamingError, NamingProvider, NamingRequest, NamingResponse, PAYLOAD_VERSION,
};

pub const DEFAULT_BASE_URL: &str = "http://localhost:1234/v1";
pub const DEFAULT_MODEL: &str = "qwen/qwen3-8b";
/// Room for a batch of answers plus a model that thinks first.
pub const MAX_TOKENS: u32 = 8_000;

/// Which backend the naming pass asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NamingBackend {
    /// The Anthropic Messages API ([`crate::anthropic`]).
    #[default]
    Cloud,
    /// A server on this machine ([`LocalProvider`]).
    Local,
}

impl NamingBackend {
    /// `"cloud"` or `"local"`, case-insensitive; `None` for anything else.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "cloud" => Some(Self::Cloud),
            "local" => Some(Self::Local),
            _ => None,
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Cloud => "cloud",
            Self::Local => "local",
        }
    }
}

/// The local backend's settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalConfig {
    /// Base URL of the OpenAI-compatible server, ending in `/v1`.
    pub base_url: String,
    pub model: String,
    /// Some servers want a bearer token even locally; most do not. Never
    /// printed ([`ApiKey`]).
    #[serde(skip)]
    pub api_key: Option<ApiKey>,
    /// Append `/no_think` to the request (Qwen3-style models then skip their
    /// reasoning pass, which is much faster for a labelling job).
    pub no_think: bool,
    /// Per-read timeout in seconds.
    pub timeout_secs: u64,
}

impl Default for LocalConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            model: DEFAULT_MODEL.to_string(),
            api_key: None,
            no_think: false,
            timeout_secs: 600,
        }
    }
}

impl LocalConfig {
    /// Smaller batches than the cloud default: an 8B model labels 10 to 15
    /// functions per request more reliably than 40.
    #[must_use]
    pub fn batch_options(&self) -> BatchOptions {
        BatchOptions {
            per_request: 12,
            max_requests: 8,
            include_named: false,
        }
    }

    /// The chat-completions endpoint.
    #[must_use]
    pub fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }
}

/// Why a local backend could not be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalConfigError {
    /// The URL is not `http://` on this machine.
    NotLoopback(String),
    EmptyModel,
}

impl std::fmt::Display for LocalConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotLoopback(u) => write!(
                f,
                "`{u}` is not an http:// URL on this machine; the local backend never leaves it"
            ),
            Self::EmptyModel => write!(f, "no local model name configured"),
        }
    }
}

impl std::error::Error for LocalConfigError {}

/// The `POST` body for one request.
#[must_use]
pub fn build_body(cfg: &LocalConfig, request: &NamingRequest) -> Value {
    let facts = serde_json::to_string(request).unwrap_or_default();
    let mut user = format!("Behaviour facts (payload version {PAYLOAD_VERSION}):\n{facts}");
    if cfg.no_think {
        user.push_str("\n/no_think");
    }
    json!({
        "model": cfg.model,
        "max_tokens": MAX_TOKENS,
        "temperature": 0,
        "stream": false,
        "messages": [
            {"role": "system", "content": SYSTEM},
            {"role": "user", "content": user}
        ],
        "response_format": {
            "type": "json_schema",
            "json_schema": {"name": "function_names", "strict": true, "schema": response_schema()}
        }
    })
}

/// Request headers (a bearer token only when one is configured).
#[must_use]
pub fn headers(cfg: &LocalConfig) -> Vec<(&'static str, String)> {
    let mut h = vec![("content-type", "application/json".to_string())];
    if let Some(k) = &cfg.api_key {
        h.push(("authorization", format!("Bearer {}", k.expose())));
    }
    h
}

fn error_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .and_then(Value::as_str)
                .or_else(|| v.get("error").and_then(Value::as_str))
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.chars().take(200).collect())
}

/// The JSON object in a model's text: `<think>` blocks and a code fence are
/// removed, then the outermost `{ ... }` is taken.
fn json_in(text: &str) -> &str {
    let mut t = text;
    if let Some(i) = t.rfind("</think>") {
        t = &t[i + "</think>".len()..];
    }
    match (t.find('{'), t.rfind('}')) {
        (Some(a), Some(b)) if a < b => &t[a..=b],
        _ => t.trim(),
    }
}

/// Read a chat-completions answer.
///
/// `finish_reason` is checked before the content: `stop` is an answer;
/// `length` is [`NamingError::Truncated`]; `content_filter` is
/// [`NamingError::Refused`]. A `message.refusal` is a refusal too.
///
/// # Errors
/// [`NamingError`]; a non-2xx `status` is [`NamingError::Http`].
pub fn parse_response(status: u16, body: &str) -> Result<NamingResponse, NamingError> {
    if !(200..300).contains(&status) {
        return Err(NamingError::Http {
            status,
            message: error_message(body),
        });
    }
    let v: Value =
        serde_json::from_str(body).map_err(|e| NamingError::BadResponse(e.to_string()))?;
    let choice = v
        .pointer("/choices/0")
        .ok_or_else(|| NamingError::BadResponse("no choices".to_string()))?;
    match choice.get("finish_reason").and_then(Value::as_str) {
        Some("stop") | None => {}
        Some("length") => return Err(NamingError::Truncated),
        Some("content_filter") => return Err(NamingError::Refused("content filter".to_string())),
        Some(other) => {
            return Err(NamingError::BadResponse(format!(
                "unexpected finish_reason `{other}`"
            )))
        }
    }
    if let Some(r) = choice
        .pointer("/message/refusal")
        .and_then(Value::as_str)
        .filter(|r| !r.is_empty())
    {
        return Err(NamingError::Refused(r.to_string()));
    }
    let text = choice
        .pointer("/message/content")
        .and_then(Value::as_str)
        .ok_or_else(|| NamingError::BadResponse("no message content".to_string()))?;
    serde_json::from_str::<NamingResponse>(json_in(text))
        .map_err(|e| NamingError::BadResponse(format!("answer is not the requested JSON: {e}")))
}

/// Send `request` and read the answer, retrying 429/5xx and transport
/// failures with backoff. A server that is simply not running fails fast:
/// the caller's policy decides how many tries a refused connection gets.
///
/// # Errors
/// [`NamingError`]: 4xx other than 429 at once, everything else after the
/// attempts run out.
pub fn call_with_retries(
    transport: &dyn Transport,
    cfg: &LocalConfig,
    request: &NamingRequest,
    policy: &RetryPolicy,
    sleep: &mut dyn FnMut(Duration),
) -> Result<NamingResponse, NamingError> {
    let url = cfg.endpoint();
    let body = build_body(cfg, request).to_string();
    let hdrs = headers(cfg);
    let mut last = NamingError::Network("no attempt made".to_string());
    for attempt in 0..policy.max_attempts.max(1) {
        let wait = match transport.post(&url, &hdrs, &body) {
            Err(e) => {
                last = NamingError::Network(e);
                None
            }
            Ok(HttpReply { status, body, .. }) if (200..300).contains(&status) => {
                return parse_response(status, &body)
            }
            Ok(HttpReply {
                status,
                body,
                retry_after,
            }) if is_retryable(status) => {
                last = parse_response(status, &body).err().unwrap_or(last);
                retry_after
            }
            Ok(r) => return parse_response(r.status, &r.body),
        };
        if attempt + 1 < policy.max_attempts {
            sleep(policy.delay(attempt, wait));
        }
    }
    Err(last)
}

/// The local backend as a [`NamingProvider`], over any [`Transport`].
pub struct LocalProvider<T: Transport + Send> {
    cfg: LocalConfig,
    label: String,
    transport: T,
    policy: RetryPolicy,
}

impl<T: Transport + Send> LocalProvider<T> {
    /// # Errors
    /// [`LocalConfigError`] for a URL that is not on this machine or an empty
    /// model name.
    pub fn with_transport(cfg: LocalConfig, transport: T) -> Result<Self, LocalConfigError> {
        if !is_loopback_url(&cfg.base_url) {
            return Err(LocalConfigError::NotLoopback(cfg.base_url));
        }
        if cfg.model.trim().is_empty() {
            return Err(LocalConfigError::EmptyModel);
        }
        // `ai:<label>` becomes a recipe `source`: keep it to plain characters.
        let label = format!(
            "local:{}",
            cfg.model
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                    c
                } else {
                    '_'
                })
                .collect::<String>()
        );
        Ok(Self {
            cfg,
            label,
            transport,
            // A local server either answers or is not there: two tries.
            policy: RetryPolicy {
                max_attempts: 2,
                base: Duration::from_millis(500),
                cap: Duration::from_secs(5),
            },
        })
    }

    #[must_use]
    pub fn with_policy(mut self, policy: RetryPolicy) -> Self {
        self.policy = policy;
        self
    }
}

impl LocalProvider<LoopbackTransport> {
    /// The real thing: loopback HTTP, per-read timeout from the config.
    ///
    /// # Errors
    /// See [`LocalProvider::with_transport`].
    pub fn new(cfg: LocalConfig) -> Result<Self, LocalConfigError> {
        let t = LoopbackTransport {
            read_timeout: Duration::from_secs(cfg.timeout_secs.max(1)),
            ..LoopbackTransport::default()
        };
        Self::with_transport(cfg, t)
    }
}

impl<T: Transport + Send> NamingProvider for LocalProvider<T> {
    fn model(&self) -> &str {
        &self.label
    }

    fn propose(&self, request: &NamingRequest) -> Result<NamingResponse, NamingError> {
        call_with_retries(
            &self.transport,
            &self.cfg,
            request,
            &self.policy,
            &mut std::thread::sleep,
        )
    }
}
