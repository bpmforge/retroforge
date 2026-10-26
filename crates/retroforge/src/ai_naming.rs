//! The Rebuild lab's "Name functions" action (ticket W36-09; D-026):
//! observe the running game, name what the hardware rules can name, and --
//! only if the player opted in -- ask a model about the rest.
//!
//! ## What runs where
//!
//! - The core thread takes a snapshot of the live machine and hands it, with
//!   the installed atlas, to a worker ([`spawn_job`]). The worker opens a
//!   **private copy** of the machine from the same image, restores the
//!   snapshot, holds the pad as it was and runs a bounded number of frames
//!   under the observer ([`crate::stepper::EmuStepper::understand`]). The
//!   live game is never touched and never paused.
//! - The rules namer (`rf_rebuild::naming`) always runs. The AI pass runs
//!   only when [`ai_gate`] says [`AiGate::Ready`]: the setting is on **and** a
//!   key exists. It sends behaviour facts only (`rf_ai::naming`'s payload
//!   guard), over [`AnthropicProvider`], which is `rf_ai::anthropic`'s request
//!   builder, parser and retry policy on this app's existing `ureq`.
//! - A failed AI call never costs the rule names: the outcome carries the
//!   reason ([`NamingOutcome::ai_note`]) next to whatever the rules found.
//!
//! ## Export
//!
//! [`export_recipe_text`] writes the names the player accepted into the game's
//! recipe as a marked block of `[[function]]` rows (source `rule:<rule>` or
//! `ai:<model>`, with the confidence), replacing a block an earlier run wrote
//! and leaving every other row and comment alone. Rows people wrote win: a
//! function a human row already describes is skipped. The API key is never
//! written anywhere but the user's settings file.

use std::collections::BTreeSet;
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::Duration;

use rf_ai::anthropic::{call_with_retries, ApiKey, HttpReply, RetryPolicy, Transport};
use rf_ai::naming::{
    build_requests, run_naming, BatchOptions, NamingError, NamingProvider, NamingRequest,
    NamingResponse,
};
use rf_core_api::InputFrame;
use rf_rebuild::naming::{human_only, name_rows, propose_names, Proposal};
use rf_rebuild::{Atlas, Platform, RamMeaning};

use crate::settings::AiNamingSettings;
use crate::stepper::{EmuStepper, MachineSnapshot};

/// Frames the understand run observes (five seconds of game time).
pub const DEFAULT_FRAMES: u32 = 300;
/// How long one HTTPS request may take, whole.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(180);

/// Whether the AI pass may run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AiGate {
    /// The player did not opt in.
    Off,
    /// Opted in, but no key in settings or `ANTHROPIC_API_KEY`.
    NoKey,
    Ready(ApiKey),
}

/// The key to use: the settings' own if present, else `env_key` (the value of
/// `ANTHROPIC_API_KEY`, passed in so tests need no environment).
#[must_use]
pub fn resolve_key(settings: &AiNamingSettings, env_key: Option<&str>) -> Option<ApiKey> {
    settings
        .api_key
        .as_deref()
        .and_then(ApiKey::new)
        .or_else(|| env_key.and_then(ApiKey::new))
}

/// The opt-in gate: nothing leaves the machine unless this is `Ready`.
#[must_use]
pub fn ai_gate(settings: &AiNamingSettings, env_key: Option<&str>) -> AiGate {
    if !settings.enabled {
        return AiGate::Off;
    }
    match resolve_key(settings, env_key) {
        Some(k) => AiGate::Ready(k),
        None => AiGate::NoKey,
    }
}

/// `ureq` as the Messages API transport. One agent per call (a handful of
/// requests per run); trust anchors from the OS store like the box-art
/// client (`crate::art`), and HTTP error statuses are read, not raised, so a
/// 429's body and `Retry-After` reach the retry policy.
#[derive(Debug, Default, Clone, Copy)]
pub struct UreqTransport;

impl Transport for UreqTransport {
    fn post(
        &self,
        url: &str,
        headers: &[(&'static str, String)],
        body: &str,
    ) -> Result<HttpReply, String> {
        let tls = ureq::tls::TlsConfig::builder()
            .root_certs(ureq::tls::RootCerts::PlatformVerifier)
            .build();
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .tls_config(tls)
            .http_status_as_error(false)
            .timeout_global(Some(REQUEST_TIMEOUT))
            .build()
            .into();
        let mut req = agent.post(url);
        for (k, v) in headers {
            req = req.header(*k, v.as_str());
        }
        let mut response = req.send(body).map_err(|e| e.to_string())?;
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok());
        let body = response
            .body_mut()
            .read_to_string()
            .map_err(|e| e.to_string())?;
        Ok(HttpReply {
            status,
            body,
            retry_after,
        })
    }
}

/// The real provider: Messages API over a [`Transport`].
pub struct AnthropicProvider {
    key: ApiKey,
    model: String,
    transport: Box<dyn Transport + Send>,
    policy: RetryPolicy,
}

impl AnthropicProvider {
    #[must_use]
    pub fn new(key: ApiKey, model: &str) -> Self {
        Self::with_transport(key, model, Box::new(UreqTransport))
    }

    #[must_use]
    pub fn with_transport(key: ApiKey, model: &str, transport: Box<dyn Transport + Send>) -> Self {
        Self {
            key,
            model: model.to_string(),
            transport,
            policy: RetryPolicy::default(),
        }
    }
}

impl NamingProvider for AnthropicProvider {
    fn model(&self) -> &str {
        &self.model
    }

    fn propose(&self, request: &NamingRequest) -> Result<NamingResponse, NamingError> {
        call_with_retries(
            self.transport.as_ref(),
            &self.key,
            &self.model,
            request,
            &self.policy,
            &mut std::thread::sleep,
        )
    }
}

/// What the player asked the core thread for.
#[derive(Clone)]
pub struct NameJob {
    pub frames: u32,
    /// The recipe's RAM names, for the AI facts.
    pub ram: Vec<RamMeaning>,
    /// Present only when [`ai_gate`] was `Ready`.
    pub ai: Option<(ApiKey, String)>,
}

impl std::fmt::Debug for NameJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NameJob")
            .field("frames", &self.frames)
            .field("ram", &self.ram.len())
            .field("ai", &self.ai.as_ref().map(|(_, m)| m))
            .finish()
    }
}

/// What a finished run found.
#[derive(Debug)]
pub struct NamingOutcome {
    pub platform: Platform,
    pub frames: u32,
    pub atlas: Arc<Atlas>,
    /// Atlas functions that executed during the run.
    pub observed: usize,
    /// Names from the rules, sorted by function.
    pub rule: Vec<Proposal>,
    /// Names from the model for functions the rules did not name.
    pub ai: Vec<Proposal>,
    /// One line on the AI pass: skipped, how many requests, or why it failed.
    pub ai_note: String,
}

/// Where the Name functions action stands, as the panel shows it.
#[derive(Debug, Clone, Default)]
pub enum NamingStatus {
    #[default]
    Idle,
    Running,
    Done(Arc<NamingOutcome>),
    Failed(String),
}

/// Run the understand pass on a private copy of the machine and name what it
/// saw. Blocking and slow (it steps every instruction): run it on a worker.
///
/// # Errors
/// The image or snapshot does not load, or the atlas does not fit.
pub fn run_job(
    rom: &[u8],
    snapshot: &MachineSnapshot,
    atlas: Arc<Atlas>,
    input: InputFrame,
    job: &NameJob,
    provider: Option<&dyn NamingProvider>,
) -> Result<NamingOutcome, String> {
    let mut machine = EmuStepper::open(rom).map_err(|e| format!("{e:?}"))?;
    machine
        .restore(snapshot)
        .map_err(|e| format!("could not copy the running machine: {e:?}"))?;
    let u = machine.understand(&atlas, job.frames, input)?;
    let rule = propose_names(&u, &atlas);
    let (ai, ai_note) = match provider {
        None => (Vec::new(), "AI naming is off".to_string()),
        Some(p) => {
            let requests = build_requests(&u, &atlas, &rule, &job.ram, BatchOptions::default());
            if requests.is_empty() {
                (Vec::new(), "nothing left for the model to name".to_string())
            } else {
                let mut taken: Vec<String> = rule.iter().map(|r| r.name.clone()).collect();
                match run_naming(p, &requests, &mut taken) {
                    Ok(names) => {
                        let note = format!(
                            "{} name(s) from {} request(s) to {}",
                            names.len(),
                            requests.len(),
                            p.model()
                        );
                        (names, note)
                    }
                    Err(e) => (Vec::new(), format!("AI naming failed: {e}")),
                }
            }
        }
    };
    Ok(NamingOutcome {
        platform: u.platform,
        frames: u.frames,
        observed: u.observed_functions(),
        atlas,
        rule,
        ai,
        ai_note,
    })
}

/// [`run_job`] on a worker thread. The provider (and with it the key) is built
/// there from `job.ai`, so nothing but the opted-in configuration crosses.
#[must_use]
pub fn spawn_job(
    rom: Vec<u8>,
    snapshot: MachineSnapshot,
    atlas: Atlas,
    input: InputFrame,
    job: NameJob,
) -> Receiver<Result<NamingOutcome, String>> {
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("name-functions".to_string())
        .spawn(move || {
            let provider = job
                .ai
                .clone()
                .map(|(key, model)| AnthropicProvider::new(key, &model));
            let out = run_job(
                &rom,
                &snapshot,
                Arc::new(atlas),
                input,
                &job,
                provider.as_ref().map(|p| p as &dyn NamingProvider),
            );
            let _ = tx.send(out);
        });
    if spawned.is_err() {
        // The receiver sees a disconnect and reports it; nothing to clean up.
    }
    rx
}

/// Which proposal the player picked for a function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pick {
    #[default]
    None,
    Rule,
    Ai,
}

/// The default picks: every rule name at 80 or more, no AI name (those are
/// guesses until a person says otherwise).
#[must_use]
pub fn default_picks(outcome: &NamingOutcome) -> std::collections::BTreeMap<u32, Pick> {
    outcome
        .rule
        .iter()
        .filter(|p| p.confidence >= 80)
        .map(|p| (p.function, Pick::Rule))
        .collect()
}

/// The proposals the picks select, in function order.
#[must_use]
pub fn picked(
    outcome: &NamingOutcome,
    picks: &std::collections::BTreeMap<u32, Pick>,
) -> Vec<Proposal> {
    let mut out: Vec<Proposal> = Vec::new();
    for (&f, &pick) in picks {
        let from = match pick {
            Pick::None => continue,
            Pick::Rule => &outcome.rule,
            Pick::Ai => &outcome.ai,
        };
        if let Some(p) = from.iter().find(|p| p.function == f) {
            out.push(p.clone());
        }
    }
    out
}

/// A blank recipe file for a game that has none.
fn blank_recipe(id: &str) -> String {
    format!("[recipe]\nformat = 1\nid = \"{id}\"\nengine = \"\"\n")
}

/// `existing` recipe text (or a blank recipe called `id`) with the accepted
/// names written into its marked name block. Returns the new text and the
/// number of rows in the block.
///
/// # Errors
/// `existing` does not parse as a recipe, or the result does not (a bug).
pub fn export_recipe_text(
    existing: Option<&str>,
    id: &str,
    atlas: &Atlas,
    accepted: &[Proposal],
) -> Result<(String, usize), String> {
    let text = existing.map_or_else(|| blank_recipe(id), str::to_string);
    let recipe = rf_profiles::load_recipe_str(&text).map_err(|e| e.to_string())?;
    let rows = name_rows(atlas, accepted, &human_only(&recipe));
    let block = rf_profiles::recipe::name_rows_block(&rows);
    let out = rf_profiles::recipe::splice_name_block(&text, &block);
    rf_profiles::load_recipe_str(&out).map_err(|e| format!("export would not parse: {e}"))?;
    Ok((out, rows.len()))
}

/// Function ids the accepted set covers, for the panel's counts.
#[must_use]
pub fn picked_ids(picks: &std::collections::BTreeMap<u32, Pick>) -> BTreeSet<u32> {
    picks
        .iter()
        .filter(|(_, p)| **p != Pick::None)
        .map(|(f, _)| *f)
        .collect()
}
