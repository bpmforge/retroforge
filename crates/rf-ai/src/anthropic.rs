//! The Anthropic Messages API as the naming provider's wire format (ticket
//! W36-09; D-026).
//!
//! Rust has no official Anthropic SDK, so this is the raw HTTP the API
//! documents, kept in three small pieces that need no network to test:
//!
//! - [`build_body`] and [`headers`]: the request,
//! - [`parse_response`]: the answer (checks `stop_reason` before the content,
//!   parses JSON with serde, never string-matches),
//! - [`call_with_retries`]: 429 and 5xx back off and retry, other 4xx do not,
//!   all over a [`Transport`] the app shell implements with `ureq`.
//!
//! Request shape (per the brief for this ticket): `POST` to [`API_URL`] with
//! `x-api-key`, `anthropic-version`, `content-type: application/json` and the
//! `anthropic-beta` header for server-side fallback; body field
//! `"fallbacks": "default"`; no `thinking` field (the model's default,
//! adaptive, applies; `disabled` and `budget_tokens` are rejected);
//! `output_config` asks for low effort and a JSON-schema-shaped answer.
//!
//! The API key is an [`ApiKey`]: it prints as `ApiKey(***)` and is only read
//! through [`ApiKey::expose`], at the one place it goes into a header. It is
//! never logged, never put in a recipe or a savestate.

use std::time::Duration;

use serde_json::{json, Value};

use crate::naming::{NamingError, NamingRequest, NamingResponse, PAYLOAD_VERSION};

pub const API_URL: &str = "https://api.anthropic.com/v1/messages";
pub const API_VERSION: &str = "2026-10-21";
pub const BETA: &str = "server-side-fallback-2026-07-01";
pub const DEFAULT_MODEL: &str = "claude-opus-5-5";
pub const MAX_TOKENS: u32 = 16_000;

/// An API key that cannot be printed by accident.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
    /// `None` for an empty or whitespace-only key.
    #[must_use]
    pub fn new(key: &str) -> Option<Self> {
        let k = key.trim();
        (!k.is_empty()).then(|| Self(k.to_string()))
    }

    /// The key text, for the one header that carries it.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiKey(***)")
    }
}

pub(crate) const SYSTEM: &str =
    "You name functions of a retro game's machine code from how they behave. \
You are given only measurements taken while the game ran: how often each function is called, \
which hardware registers it touches, which RAM it reads and writes, who calls it. You are not \
given any code. For each function id propose a short snake_case name (3 to 48 characters, \
lower case, letters digits underscores, starting with a letter), a one-sentence purpose, and a \
confidence from 0 to 100 that the name is right. Prefer names a game developer would use. Use \
neighbour names and RAM names as context. If you cannot tell what a function does, say so in \
the purpose and give a low confidence. Answer only with the JSON object.";

/// The JSON schema of the answer.
#[must_use]
pub fn response_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "names": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "id": {"type": "integer"},
                        "name": {"type": "string"},
                        "purpose": {"type": "string"},
                        "confidence": {"type": "integer"}
                    },
                    "required": ["id", "name", "purpose", "confidence"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["names"],
        "additionalProperties": false
    })
}

/// The `POST` body for one naming request.
#[must_use]
pub fn build_body(model: &str, request: &NamingRequest) -> Value {
    let facts = serde_json::to_string(request).unwrap_or_default();
    json!({
        "model": model,
        "max_tokens": MAX_TOKENS,
        "fallbacks": "default",
        "system": SYSTEM,
        "messages": [{
            "role": "user",
            "content": format!(
                "Behaviour facts (payload version {PAYLOAD_VERSION}):\n{facts}"
            )
        }],
        "output_config": {
            "effort": "low",
            "format": {"type": "json_schema", "schema": response_schema()}
        }
    })
}

/// The request headers, name and value.
#[must_use]
pub fn headers(key: &ApiKey) -> Vec<(&'static str, String)> {
    vec![
        ("x-api-key", key.expose().to_string()),
        ("anthropic-version", API_VERSION.to_string()),
        ("anthropic-beta", BETA.to_string()),
        ("content-type", "application/json".to_string()),
    ]
}

/// The API's error body: `{"type":"error","error":{"type":..,"message":..}}`.
fn error_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.chars().take(200).collect())
}

/// Read a Messages API answer.
///
/// `stop_reason` is checked before any content is read: `end_turn` and
/// `stop_sequence` are answers; `refusal` is [`NamingError::Refused`] (with
/// `stop_details` when present); `max_tokens` is [`NamingError::Truncated`].
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
    let stop = v.get("stop_reason").and_then(Value::as_str).unwrap_or("");
    match stop {
        "end_turn" | "stop_sequence" => {}
        "refusal" => {
            let why = match v.get("stop_details") {
                Some(Value::String(s)) => s.clone(),
                Some(d) if !d.is_null() => d.to_string(),
                _ => "no reason given".to_string(),
            };
            return Err(NamingError::Refused(why));
        }
        "max_tokens" => return Err(NamingError::Truncated),
        other => {
            return Err(NamingError::BadResponse(format!(
                "unexpected stop_reason `{other}`"
            )))
        }
    }
    let text = v
        .get("content")
        .and_then(Value::as_array)
        .and_then(|blocks| {
            blocks
                .iter()
                .find(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        })
        .and_then(|b| b.get("text").and_then(Value::as_str))
        .ok_or_else(|| NamingError::BadResponse("no text block".to_string()))?;
    serde_json::from_str::<NamingResponse>(text)
        .map_err(|e| NamingError::BadResponse(format!("answer is not the requested JSON: {e}")))
}

/// What a transport hands back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpReply {
    pub status: u16,
    pub body: String,
    /// `Retry-After` in seconds, when the server sent one.
    pub retry_after: Option<u64>,
}

/// One HTTP `POST`. The shell implements it over `ureq` (with
/// `http_status_as_error(false)` so a 429 body can be read).
pub trait Transport {
    /// # Errors
    /// A transport-level failure (DNS, TLS, timeout), as text.
    fn post(
        &self,
        url: &str,
        headers: &[(&'static str, String)],
        body: &str,
    ) -> Result<HttpReply, String>;
}

/// Backoff for retryable failures.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub base: Duration,
    pub cap: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 4,
            base: Duration::from_secs(1),
            cap: Duration::from_secs(30),
        }
    }
}

impl RetryPolicy {
    pub(crate) fn delay(&self, attempt: u32, retry_after: Option<u64>) -> Duration {
        let exp = self.base.saturating_mul(1u32 << attempt.min(16));
        retry_after.map_or(exp, Duration::from_secs).min(self.cap)
    }
}

/// Whether a status is worth another try: rate limits and server errors.
#[must_use]
pub const fn is_retryable(status: u16) -> bool {
    status == 429 || (status >= 500 && status < 600)
}

/// Send `request` and read the answer, retrying 429/5xx and transport
/// failures with backoff (`sleep` is called with each delay).
///
/// # Errors
/// [`NamingError`]: 4xx other than 429 at once, everything else after the
/// attempts run out.
pub fn call_with_retries(
    transport: &dyn Transport,
    key: &ApiKey,
    model: &str,
    request: &NamingRequest,
    policy: &RetryPolicy,
    sleep: &mut dyn FnMut(Duration),
) -> Result<NamingResponse, NamingError> {
    let body = build_body(model, request).to_string();
    let hdrs = headers(key);
    let mut last = NamingError::Network("no attempt made".to_string());
    for attempt in 0..policy.max_attempts.max(1) {
        match transport.post(API_URL, &hdrs, &body) {
            Err(e) => last = NamingError::Network(e),
            Ok(r) if (200..300).contains(&r.status) => return parse_response(r.status, &r.body),
            Ok(r) if is_retryable(r.status) => {
                last = parse_response(r.status, &r.body).err().unwrap_or(last);
                if attempt + 1 < policy.max_attempts {
                    sleep(policy.delay(attempt, r.retry_after));
                }
                continue;
            }
            Ok(r) => return parse_response(r.status, &r.body),
        }
        if attempt + 1 < policy.max_attempts {
            sleep(policy.delay(attempt, None));
        }
    }
    Err(last)
}
