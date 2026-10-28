//! RetroAchievements in the app (ticket W41-05; `docs/design/CHEATS_ACHIEVEMENTS.md`
//! section 4; decision D-031).
//!
//! `rf-achievements` is the network-free rcheevos wrapper. This module is
//! everything the app adds around it:
//!
//! * [`RaSession`] lives on the **core thread** (the wrapper is `!Send`),
//!   next to the machine whose memory it reads after every frame, and never
//!   mutates that machine (CLAUDE.md law 4).
//! * HTTP is performed by [`spawn_worker`], a thread of its own with
//!   `ureq`, so neither the UI thread nor the core thread ever blocks on a
//!   socket. Results come back as `CoreCommand::RaResponse` / `RaFailed`.
//!   Nothing here runs, and nothing is spawned, until the player turns
//!   RetroAchievements on (NON_GOALS 6).
//! * [`hardcore_refuses`] is the one place the hardcore policy is written
//!   down; the core thread's command loop and the UI both call it.
//! * [`TokenStore`] keeps the one thing that persists: `{username, token}`.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::time::Duration;

use rf_achievements::{AchievementEvent, Achievements, CartMap, Console, PendingRequest};

use crate::core_thread::{CoreCommand, CoreEvent};

// ---------------------------------------------------------------------------
// The hardcore policy
// ---------------------------------------------------------------------------

/// Something the player can do that hardcore may forbid.
///
/// What hardcore forbids is RetroAchievements' rule, enforced by the client
/// (rc_client.h: "Enabling hardcore with a game loaded will raise an
/// RC_CLIENT_EVENT_RESET"; the library leaves the rest to the emulator).
/// Their published list: no cheats, no save-state loading, no rewind, no
/// slow motion, no memory writes by the player. Saving a state, pausing
/// (rate-limited by `rc_client_can_pause`) and fast-forward are allowed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HardcoreAction {
    /// Any cheat in force (RAM or ROM code).
    Cheats,
    /// Holding rewind, or keeping a rewind history.
    Rewind,
    /// Loading a save state (saving is allowed: [`Self::SaveState`]).
    LoadState,
    /// Run-ahead steps a hidden copy and shows its result.
    RunAhead,
    /// Single-stepping a frame or a scanline.
    FrameAdvance,
    /// Running below 100% speed. The app has no entry point for it today;
    /// the policy exists so adding one cannot forget the rule.
    SlowMotion,
    /// Recipe mods and soft patches (the patched ROM is not the game).
    RecipeMods,
    /// A hook script holding the `write_memory` capability.
    MemoryHook,
    /// An out-of-band write into the machine (the memory editor).
    MemoryWrite,
    // The actions below are allowed; they are listed so the table is the
    // whole picture and each has a test.
    SaveState,
    /// Faster than 100%: RetroAchievements allows it.
    FastForward,
    Pause,
}

/// `Some(message)` when hardcore forbids `action`, naming what was refused;
/// `None` when it is allowed. The caller checks that hardcore is on.
#[must_use]
pub fn hardcore_refuses(action: HardcoreAction) -> Option<&'static str> {
    match action {
        HardcoreAction::Cheats => Some("Cheats are off in hardcore"),
        HardcoreAction::Rewind => Some("Rewind is off in hardcore"),
        HardcoreAction::LoadState => Some("Loading a save state is off in hardcore"),
        HardcoreAction::RunAhead => Some("Run-ahead is off in hardcore"),
        HardcoreAction::FrameAdvance => Some("Frame advance is off in hardcore"),
        HardcoreAction::SlowMotion => Some("Slow motion is off in hardcore"),
        HardcoreAction::RecipeMods => Some("Recipe mods and patches are off in hardcore"),
        HardcoreAction::MemoryHook => Some("Hook scripts that write memory are off in hardcore"),
        HardcoreAction::MemoryWrite => Some("Editing memory is off in hardcore"),
        HardcoreAction::SaveState | HardcoreAction::FastForward | HardcoreAction::Pause => None,
    }
}

/// What a command asks for, if hardcore has a rule about it. Commands that
/// turn a feature OFF (empty cheats, no run-ahead, no mods) map to `None`.
#[must_use]
pub fn action_of(cmd: &CoreCommand) -> Option<HardcoreAction> {
    match cmd {
        CoreCommand::StepFrame | CoreCommand::StepScanline => Some(HardcoreAction::FrameAdvance),
        CoreCommand::RewindStep => Some(HardcoreAction::Rewind),
        CoreCommand::SetRewind(Some(_)) => Some(HardcoreAction::Rewind),
        CoreCommand::ApplyState(_) | CoreCommand::ApplySnapshot(_) => {
            Some(HardcoreAction::LoadState)
        }
        CoreCommand::SetRunAhead(n) if *n > 0 => Some(HardcoreAction::RunAhead),
        CoreCommand::SetCheats(set) if !set.is_empty() => Some(HardcoreAction::Cheats),
        CoreCommand::SetModSession(Some(s)) if !s.mods.is_empty() => {
            Some(HardcoreAction::RecipeMods)
        }
        CoreCommand::SetHooks(Some(h)) if h.manifest.capabilities.write_memory => {
            Some(HardcoreAction::MemoryHook)
        }
        CoreCommand::PokeBus { .. } => Some(HardcoreAction::MemoryWrite),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Credentials
// ---------------------------------------------------------------------------

/// A password typed in Settings. It exists only to travel from the text box
/// to rcheevos: no `Debug` output, no `Clone`, no serialization, and it is
/// consumed (dropped) by the login call. The bytes are not zeroed on drop:
/// that would need `unsafe`, which this crate does not use.
pub struct Password(String);

impl Password {
    #[must_use]
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// Hand the text to the one consumer (the login call).
    fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Password {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Password(<redacted>)")
    }
}

/// The stored sign-in: the user name and the token RetroAchievements issued,
/// never the password.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StoredLogin {
    pub username: String,
    pub token: String,
}

impl std::fmt::Debug for StoredLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredLogin")
            .field("username", &self.username)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// Where the token lives, and how.
///
/// A file in the app's config directory with permissions `0600` (owner read
/// and write only), written through a temp file and renamed so a crash never
/// leaves a half-written token. The macOS keychain would be better, but
/// `security-framework` is only a transitive dependency here (through the
/// TLS stack), not one this crate may call, and adding it is a dependency
/// decision for its own ticket. Kept apart from `settings.toml` so sharing
/// or syncing that file never shares the token.
pub struct TokenStore;

/// File name under the app's config directory.
pub const TOKEN_FILE: &str = "ra_token.toml";

impl TokenStore {
    #[must_use]
    pub fn path(config_root: &Path) -> PathBuf {
        config_root
            .join(crate::bindings_store::APP_DIR)
            .join(TOKEN_FILE)
    }

    /// The stored login, if there is a readable one.
    #[must_use]
    pub fn load(config_root: &Path) -> Option<StoredLogin> {
        let text = std::fs::read_to_string(Self::path(config_root)).ok()?;
        let login: StoredLogin = toml::from_str(&text).ok()?;
        (!login.username.is_empty() && !login.token.is_empty()).then_some(login)
    }

    /// Write `{username, token}` with mode `0600`.
    ///
    /// # Errors
    /// The I/O or encoding error text.
    pub fn save(config_root: &Path, login: &StoredLogin) -> Result<(), String> {
        let path = Self::path(config_root);
        let dir = path.parent().ok_or("no config directory")?;
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let text = toml::to_string(login).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("toml.tmp");
        write_private(&tmp, text.as_bytes())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
    }

    /// Forget the login (sign out). Absent is fine.
    pub fn delete(config_root: &Path) {
        let _ = std::fs::remove_file(Self::path(config_root));
    }
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| e.to_string())?;
    // `mode` only applies to a file this call created; a leftover temp file
    // keeps its old mode, so set it either way.
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|e| e.to_string())?;
    file.write_all(bytes).map_err(|e| e.to_string())
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    // Windows: the file lives under the user's own profile directory, whose
    // ACL already limits it to the user.
    std::fs::write(path, bytes).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

/// One request for the HTTP worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboundRequest {
    pub id: rf_achievements::RequestId,
    pub url: String,
    /// `Some` means POST this body; `None` means GET.
    pub post_data: Option<String>,
    pub content_type: Option<String>,
    /// `Achievements::user_agent()`.
    pub user_agent: String,
}

/// Why a request got no response at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransportError {
    pub retryable: bool,
    pub message: String,
}

/// Performs one request. The real one is [`UreqTransport`]; tests answer
/// from canned JSON.
pub trait RaTransport: Send + Sync {
    /// The HTTP status and body of any response (2xx or not).
    ///
    /// # Errors
    /// No response was obtained (offline, DNS, TLS, timeout).
    fn perform(&self, req: &OutboundRequest) -> Result<(i32, Vec<u8>), TransportError>;
}

/// Per-request time limit (connect, send and read together).
const HTTP_TIMEOUT: Duration = Duration::from_secs(20);

/// `ureq` with the OS trust store, like the box-art and AI clients
/// (`crate::art::UreqArtClient`). HTTP error statuses are read, not raised:
/// rcheevos wants a 4xx/5xx body and status, not an exception.
#[derive(Debug, Default, Clone, Copy)]
pub struct UreqTransport;

impl RaTransport for UreqTransport {
    fn perform(&self, req: &OutboundRequest) -> Result<(i32, Vec<u8>), TransportError> {
        let tls = ureq::tls::TlsConfig::builder()
            .root_certs(ureq::tls::RootCerts::PlatformVerifier)
            .build();
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .tls_config(tls)
            .http_status_as_error(false)
            .timeout_global(Some(HTTP_TIMEOUT))
            .build()
            .into();
        let err = |e: ureq::Error| TransportError {
            retryable: true,
            message: e.to_string(),
        };
        let mut response = match &req.post_data {
            Some(body) => {
                let mut r = agent.post(&req.url).header("User-Agent", &req.user_agent);
                if let Some(ct) = &req.content_type {
                    r = r.header("Content-Type", ct);
                }
                r.send(body.as_str()).map_err(err)?
            }
            None => agent
                .get(&req.url)
                .header("User-Agent", &req.user_agent)
                .call()
                .map_err(err)?,
        };
        let status = i32::from(response.status().as_u16());
        let body = response.body_mut().read_to_vec().map_err(err)?;
        Ok((status, body))
    }
}

/// Start the HTTP worker: requests in on the returned sender, results back
/// to the core thread as commands. The thread ends when the sender is
/// dropped (the session closing). `waker` nudges the UI event loop; the core
/// thread itself polls its command channel every pass.
pub fn spawn_worker(
    transport: Arc<dyn RaTransport>,
    cmd_tx: Sender<CoreCommand>,
) -> Sender<OutboundRequest> {
    let (tx, rx) = mpsc::channel::<OutboundRequest>();
    std::thread::Builder::new()
        .name("ra-http".to_string())
        .spawn(move || {
            // Ends when `tx` (held by the session) is dropped.
            for req in rx {
                let cmd = match transport.perform(&req) {
                    Ok((status, body)) => CoreCommand::RaResponse {
                        id: req.id,
                        status,
                        body,
                    },
                    Err(e) => CoreCommand::RaFailed {
                        id: req.id,
                        retryable: e.retryable,
                        message: e.message,
                    },
                };
                if cmd_tx.send(cmd).is_err() {
                    break; // the core thread is gone
                }
            }
        })
        .expect("spawn ra-http thread");
    tx
}

// ---------------------------------------------------------------------------
// The session (core thread)
// ---------------------------------------------------------------------------

/// What identifies the open game to RetroAchievements.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaCart {
    pub console: Console,
    pub map: CartMap,
    /// RA-compatible MD5 of the normalised image (`rf_cart::hash`).
    pub md5: String,
}

/// Console, cartridge-RAM layout and RA hash of a ROM image. `None` for an
/// image the cartridge reader refuses.
#[must_use]
pub fn cart_of(rom: &[u8]) -> Option<RaCart> {
    use rf_cart::{Cartridge, Coprocessor, SnesMapMode};
    match Cartridge::load(rom).ok()? {
        Cartridge::Nes { identity, .. } => Some(RaCart {
            console: Console::Nes,
            map: CartMap::None,
            md5: identity.normalized.md5,
        }),
        Cartridge::Snes { header, identity } => {
            let ram = u32::try_from(header.ram_size).unwrap_or(0);
            let map = match (header.map_mode, header.coprocessor) {
                (SnesMapMode::LoRom, Coprocessor::None | Coprocessor::Dsp1)
                | (SnesMapMode::Sdd1, _) => CartMap::LoRom { ram },
                (SnesMapMode::HiRom, Coprocessor::None | Coprocessor::Dsp1) => {
                    CartMap::HiRom { ram }
                }
                (SnesMapMode::Sa1, _) => CartMap::Sa1 { bwram: ram },
                // Super FX, CX4, OBC1 and the rest keep their RAM elsewhere.
                _ => CartMap::None,
            };
            Some(RaCart {
                console: Console::Snes,
                map,
                md5: identity.normalized.md5,
            })
        }
    }
}

/// What a pass through the session produced.
#[derive(Debug, Default)]
pub struct Drained {
    pub events: Vec<AchievementEvent>,
    /// Hardcore was switched on mid-game: reset the machine, then call
    /// [`RaSession::after_reset`].
    pub reset: bool,
}

/// The RetroAchievements session of one open game. Core thread only.
pub struct RaSession {
    ach: Achievements,
    cart: RaCart,
    cmd_tx: Sender<CoreCommand>,
    out: Option<Sender<OutboundRequest>>,
    user_agent: String,
    hardcore: bool,
    load_started: bool,
}

impl RaSession {
    #[must_use]
    pub fn new(cart: RaCart, cmd_tx: Sender<CoreCommand>) -> Self {
        let ach = Achievements::new();
        let user_agent = ach.user_agent();
        Self {
            ach,
            cart,
            cmd_tx,
            out: None,
            user_agent,
            hardcore: false,
            load_started: false,
        }
    }

    #[must_use]
    pub fn hardcore(&self) -> bool {
        self.hardcore
    }

    /// Send requests somewhere other than the built-in `ureq` worker (tests).
    pub fn set_outbound(&mut self, out: Sender<OutboundRequest>) {
        self.out = Some(out);
    }

    /// Set hardcore; returns events if it asked for a reset.
    pub fn set_hardcore(&mut self, on: bool) -> Drained {
        self.hardcore = on;
        self.ach.set_hardcore(on);
        self.settle()
    }

    pub fn login_token(&mut self, user: &str, token: &str) -> Drained {
        self.ach.login_with_token(user, token);
        self.settle()
    }

    /// The password goes to rcheevos and is dropped (wiped) here.
    pub fn login_password(&mut self, user: &str, password: Password) -> Drained {
        self.ach.login_with_password(user, password.expose());
        drop(password);
        self.settle()
    }

    pub fn logout(&mut self) -> Drained {
        self.ach.logout();
        self.load_started = false;
        self.settle()
    }

    pub fn response(
        &mut self,
        id: rf_achievements::RequestId,
        status: i32,
        body: &[u8],
    ) -> Drained {
        self.ach.complete(id, status, body);
        self.settle()
    }

    pub fn failed(
        &mut self,
        id: rf_achievements::RequestId,
        retryable: bool,
        message: &str,
    ) -> Drained {
        self.ach.fail(id, retryable, message);
        self.settle()
    }

    /// After a frame the machine just ran: evaluate achievements.
    pub fn frame(&mut self, peek: &dyn Fn(u32) -> u8) -> Drained {
        self.ach.do_frame(peek);
        self.settle()
    }

    /// While paused: timers and retries only.
    pub fn idle(&mut self, peek: &dyn Fn(u32) -> u8) -> Drained {
        self.ach.idle(peek);
        self.settle()
    }

    /// The machine was reset (after a [`Drained::reset`]).
    pub fn after_reset(&mut self, peek: &dyn Fn(u32) -> u8) {
        self.ach.reset(peek);
    }

    /// May the player pause now? `Err(message)` names the wait.
    pub fn can_pause(&mut self) -> Result<(), String> {
        if !self.hardcore {
            return Ok(());
        }
        self.ach.can_pause().map_err(|frames| {
            let secs = (f64::from(frames) / 60.0).ceil();
            format!("Hardcore: pause again in {secs:.0} s")
        })
    }

    /// The loaded game's achievements as they stand now (empty before the
    /// game data arrives).
    #[must_use]
    pub fn list(&self) -> Vec<rf_achievements::AchievementInfo> {
        self.ach.achievements()
    }

    /// Unload with the flush rcheevos needs, before the session is dropped.
    pub fn close(&mut self) {
        self.ach.unload_game();
    }

    /// Hand queued requests to the worker, turn events into [`Drained`],
    /// and start the game load once signed in.
    fn settle(&mut self) -> Drained {
        let mut drained = Drained::default();
        // The load adds requests and a login can precede it: two rounds.
        for _ in 0..2 {
            self.flush_requests();
            for ev in self.ach.drain_events() {
                match ev {
                    AchievementEvent::ResetRequested => drained.reset = true,
                    ev => {
                        if matches!(ev, AchievementEvent::LoginSucceeded { .. })
                            && !self.load_started
                        {
                            self.load_started = true;
                            self.ach.load_game_with(
                                self.cart.console,
                                self.cart.map,
                                &self.cart.md5,
                            );
                        }
                        drained.events.push(ev);
                    }
                }
            }
        }
        self.flush_requests();
        drained
    }

    fn flush_requests(&mut self) {
        let requests: Vec<PendingRequest> = self.ach.take_requests();
        for r in requests {
            let out = self
                .out
                .get_or_insert_with(|| spawn_worker(Arc::new(UreqTransport), self.cmd_tx.clone()));
            let sent = out.send(OutboundRequest {
                id: r.id,
                url: r.url,
                post_data: r.post_data,
                content_type: r.content_type,
                user_agent: self.user_agent.clone(),
            });
            if let Err(e) = sent {
                // The worker is gone: tell rcheevos so it can retry later.
                self.ach.fail(e.0.id, true, "http worker stopped");
                self.out = None;
            }
        }
    }
}

/// Send a pass's events to the UI. `false` when the UI has hung up.
pub fn deliver(d: Drained, evt_tx: &Sender<CoreEvent>) -> bool {
    d.events
        .into_iter()
        .all(|ev| evt_tx.send(CoreEvent::Achievement(ev)).is_ok())
}

/// Badge suffix for a loaded game (`status_badge`).
#[must_use]
pub fn badge_suffix(game_loaded: bool, hardcore: bool) -> Option<&'static str> {
    game_loaded.then_some(if hardcore {
        " \u{b7} RA: hardcore"
    } else {
        " \u{b7} RA: softcore"
    })
}

/// Ticket W41-06: the quiet status line while unlocks cannot be delivered.
pub const OFFLINE_NOTICE: &str = "RetroAchievements offline, unlocks queued";
/// Ticket W41-06: the status line once the queue was delivered.
pub const ONLINE_NOTICE: &str = "RetroAchievements back online";

/// Whether these events change what the achievement list shows, so the core
/// should send a fresh [`CoreEvent::RaList`].
#[must_use]
pub fn wants_list_refresh(events: &[AchievementEvent]) -> bool {
    events.iter().any(|e| {
        matches!(
            e,
            AchievementEvent::GameLoaded { .. }
                | AchievementEvent::Unlocked { .. }
                | AchievementEvent::Progress { .. }
                | AchievementEvent::GameCompleted
        )
    })
}

/// Whether `a` counts as unlocked in the mode being played: hardcore counts
/// only hardcore unlocks, softcore counts either.
#[must_use]
pub fn is_unlocked(a: &rf_achievements::AchievementInfo, hardcore: bool) -> bool {
    if hardcore {
        a.unlocked_hardcore
    } else {
        a.unlocked_softcore || a.unlocked_hardcore
    }
}

/// "N of M unlocked, P of Q points".
#[must_use]
pub fn progress_line(items: &[rf_achievements::AchievementInfo], hardcore: bool) -> String {
    let (mut n, mut p) = (0u32, 0u32);
    let mut q = 0u32;
    for a in items {
        q = q.saturating_add(a.points);
        if is_unlocked(a, hardcore) {
            n += 1;
            p = p.saturating_add(a.points);
        }
    }
    format!("{n} of {} unlocked, {p} of {q} points", items.len())
}

/// The toast for an unlock: title and points, then the description.
#[must_use]
pub fn unlock_toast(title: &str, points: u32, description: &str) -> String {
    let head = format!("Achievement unlocked: {title} ({points} pts)");
    if description.is_empty() {
        head
    } else {
        format!("{head}\n{description}")
    }
}

/// The toast when every achievement of the game is unlocked.
#[must_use]
pub fn mastered_toast(game: Option<&str>) -> String {
    match game {
        Some(g) if !g.is_empty() => format!("Game mastered: {g}"),
        _ => "Game mastered".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [HardcoreAction; 12] = [
        HardcoreAction::Cheats,
        HardcoreAction::Rewind,
        HardcoreAction::LoadState,
        HardcoreAction::RunAhead,
        HardcoreAction::FrameAdvance,
        HardcoreAction::SlowMotion,
        HardcoreAction::RecipeMods,
        HardcoreAction::MemoryHook,
        HardcoreAction::MemoryWrite,
        HardcoreAction::SaveState,
        HardcoreAction::FastForward,
        HardcoreAction::Pause,
    ];

    #[test]
    fn policy_refuses_each_forbidden_action_and_names_it() {
        for (action, word) in [
            (HardcoreAction::Cheats, "Cheats"),
            (HardcoreAction::Rewind, "Rewind"),
            (HardcoreAction::LoadState, "save state"),
            (HardcoreAction::RunAhead, "Run-ahead"),
            (HardcoreAction::FrameAdvance, "Frame advance"),
            (HardcoreAction::SlowMotion, "Slow motion"),
            (HardcoreAction::RecipeMods, "Recipe mods"),
            (HardcoreAction::MemoryHook, "Hook scripts"),
            (HardcoreAction::MemoryWrite, "memory"),
        ] {
            let msg = hardcore_refuses(action).unwrap_or_else(|| panic!("{action:?} allowed"));
            assert!(msg.contains(word), "{action:?}: {msg}");
        }
    }

    #[test]
    fn policy_allows_saving_fast_forward_and_pause() {
        for action in [
            HardcoreAction::SaveState,
            HardcoreAction::FastForward,
            HardcoreAction::Pause,
        ] {
            assert_eq!(hardcore_refuses(action), None, "{action:?}");
        }
        // The table is complete: nine refused, three allowed.
        assert_eq!(
            ALL.iter()
                .filter(|a| hardcore_refuses(**a).is_some())
                .count(),
            9
        );
    }

    #[test]
    fn commands_map_to_their_action() {
        let nonempty = crate::cheats::CheatSet::build(
            rf_cheats::Console::Nes,
            &[crate::game_settings::CheatEntry {
                code: "0300:05".into(),
                label: String::new(),
                enabled: true,
            }],
        );
        assert!(!nonempty.is_empty());
        assert_eq!(
            action_of(&CoreCommand::SetCheats(Box::new(nonempty))),
            Some(HardcoreAction::Cheats)
        );
        assert_eq!(
            action_of(&CoreCommand::SetCheats(Box::default())),
            None,
            "switching cheats off is always fine"
        );
        assert_eq!(
            action_of(&CoreCommand::StepFrame),
            Some(HardcoreAction::FrameAdvance)
        );
        assert_eq!(
            action_of(&CoreCommand::StepScanline),
            Some(HardcoreAction::FrameAdvance)
        );
        assert_eq!(
            action_of(&CoreCommand::RewindStep),
            Some(HardcoreAction::Rewind)
        );
        assert_eq!(
            action_of(&CoreCommand::SetRunAhead(2)),
            Some(HardcoreAction::RunAhead)
        );
        assert_eq!(action_of(&CoreCommand::SetRunAhead(0)), None);
        assert_eq!(
            action_of(&CoreCommand::PokeBus { addr: 0, value: 0 }),
            Some(HardcoreAction::MemoryWrite)
        );
        // Loading a state, by either door.
        let rom = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/nes/rf-scroller/build/rf-scroller.nes"
        ))
        .expect("fixture");
        let stepper = crate::stepper::EmuStepper::open(&rom).expect("opens");
        let state = stepper.save_state(0).expect("state");
        assert_eq!(
            action_of(&CoreCommand::ApplyState(Box::new(state))),
            Some(HardcoreAction::LoadState)
        );
        let snap = stepper.snapshot().expect("snapshot");
        assert_eq!(
            action_of(&CoreCommand::ApplySnapshot(Box::new(snap))),
            Some(HardcoreAction::LoadState)
        );
        assert_eq!(
            action_of(&CoreCommand::SetRewind(None)),
            None,
            "switching rewind off is fine"
        );
        assert_eq!(action_of(&CoreCommand::SetModSession(None)), None);
        assert_eq!(action_of(&CoreCommand::SetHooks(None)), None);
        assert_eq!(action_of(&CoreCommand::Pause), None);
        assert_eq!(action_of(&CoreCommand::SetPacingEnabled(false)), None);
    }

    #[test]
    fn password_and_token_never_show_in_debug_output() {
        let p = Password::new("hunter2-secret".into());
        assert!(!format!("{p:?}").contains("hunter2"));
        let login = StoredLogin {
            username: "Bob".into(),
            token: "tok-secret-123".into(),
        };
        let shown = format!("{login:?}");
        assert!(shown.contains("Bob") && !shown.contains("tok-secret"));
        let cmd = CoreCommand::RaLoginPassword {
            user: "Bob".into(),
            password: Password::new("hunter2-secret".into()),
        };
        // CoreCommand is not Debug; the field's own Debug is what any
        // derive would use.
        if let CoreCommand::RaLoginPassword { password, .. } = &cmd {
            assert!(!format!("{password:?}").contains("hunter2"));
        }
    }

    fn temp_root(label: &str) -> PathBuf {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("rf-w41-05-{label}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn token_store_round_trips_with_private_permissions_and_deletes() {
        let root = temp_root("tok");
        assert_eq!(TokenStore::load(&root), None);
        let login = StoredLogin {
            username: "Bob".into(),
            token: "tok123".into(),
        };
        TokenStore::save(&root, &login).expect("save");
        assert_eq!(TokenStore::load(&root), Some(login));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(TokenStore::path(&root))
                .expect("meta")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "owner read/write only");
        }
        // Overwriting an existing, looser file tightens it again.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let tmp = TokenStore::path(&root).with_extension("toml.tmp");
            std::fs::write(&tmp, "x").expect("tmp");
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644)).expect("chmod");
            TokenStore::save(
                &root,
                &StoredLogin {
                    username: "Bob".into(),
                    token: "t2".into(),
                },
            )
            .expect("save");
            let mode = std::fs::metadata(TokenStore::path(&root))
                .expect("meta")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        TokenStore::delete(&root);
        assert_eq!(TokenStore::load(&root), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn worker_turns_transport_results_into_commands() {
        struct Fake;
        impl RaTransport for Fake {
            fn perform(&self, req: &OutboundRequest) -> Result<(i32, Vec<u8>), TransportError> {
                if req.url.contains("down") {
                    Err(TransportError {
                        retryable: true,
                        message: "offline".into(),
                    })
                } else {
                    assert_eq!(req.user_agent, "ua");
                    Ok((200, b"{}".to_vec()))
                }
            }
        }
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let tx = spawn_worker(Arc::new(Fake), cmd_tx);
        for (id, url) in [(1, "https://x/up"), (2, "https://x/down")] {
            tx.send(OutboundRequest {
                id,
                url: url.into(),
                post_data: None,
                content_type: None,
                user_agent: "ua".into(),
            })
            .expect("send");
        }
        let a = cmd_rx.recv_timeout(Duration::from_secs(5)).expect("first");
        let b = cmd_rx.recv_timeout(Duration::from_secs(5)).expect("second");
        assert!(matches!(
            a,
            CoreCommand::RaResponse {
                id: 1,
                status: 200,
                ..
            }
        ));
        assert!(matches!(
            b,
            CoreCommand::RaFailed {
                id: 2,
                retryable: true,
                ..
            }
        ));
    }

    #[test]
    fn cart_of_the_nes_fixture_has_an_md5_and_no_cart_map() {
        let rom = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/nes/rf-scroller/build/rf-scroller.nes"
        ))
        .expect("fixture");
        let cart = cart_of(&rom).expect("cart");
        assert_eq!(cart.console, Console::Nes);
        assert_eq!(cart.md5.len(), 32);
        assert_eq!(cart.map, CartMap::None);
        assert_eq!(cart_of(&[0u8; 10]), None);
    }

    /// Hardcore turned on mid-game restarts the game: the CPU is back at the
    /// reset vector, not wherever the game had got to.
    #[test]
    fn reset_returns_the_cpu_to_the_reset_vector() {
        let rom = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/nes/rf-scroller/build/rf-scroller.nes"
        ))
        .expect("fixture");
        let mut stepper = crate::stepper::EmuStepper::open(&rom).expect("opens");
        let start_pc = stepper.pc();
        let mut sink = rf_renderer::FrameBuffer::new();
        for _ in 0..30 {
            stepper.latch_and_advance_frame(rf_core_api::InputFrame::default(), &mut sink);
        }
        assert_ne!(stepper.pc(), start_pc, "the game is running somewhere else");
        stepper.reset();
        assert_eq!(stepper.pc(), start_pc);
    }

    #[test]
    fn badge_names_the_mode_only_with_a_game_loaded() {
        assert_eq!(badge_suffix(false, true), None);
        assert_eq!(badge_suffix(true, false), Some(" \u{b7} RA: softcore"));
        assert_eq!(badge_suffix(true, true), Some(" \u{b7} RA: hardcore"));
    }
    fn info(points: u32, soft: bool, hard: bool) -> rf_achievements::AchievementInfo {
        rf_achievements::AchievementInfo {
            id: 1,
            title: "t".into(),
            description: String::new(),
            points,
            unlocked_softcore: soft,
            unlocked_hardcore: hard,
            state: 0,
            measured_progress: String::new(),
            badge_name: String::new(),
        }
    }

    #[test]
    fn progress_line_counts_unlocks_and_points() {
        let items = [
            info(5, false, false),
            info(10, true, false),
            info(25, true, true),
        ];
        assert_eq!(
            progress_line(&items, false),
            "2 of 3 unlocked, 35 of 40 points"
        );
        // Hardcore counts only hardcore unlocks.
        assert_eq!(
            progress_line(&items, true),
            "1 of 3 unlocked, 25 of 40 points"
        );
        assert_eq!(progress_line(&[], false), "0 of 0 unlocked, 0 of 0 points");
    }

    #[test]
    fn toasts_name_the_title_points_and_description() {
        assert_eq!(
            unlock_toast("Five Alive", 5, "Reach 5"),
            "Achievement unlocked: Five Alive (5 pts)\nReach 5"
        );
        assert_eq!(
            unlock_toast("Five Alive", 5, ""),
            "Achievement unlocked: Five Alive (5 pts)"
        );
        assert_eq!(
            mastered_toast(Some("Test Game")),
            "Game mastered: Test Game"
        );
        assert_eq!(mastered_toast(None), "Game mastered");
    }

    #[test]
    fn only_list_changing_events_ask_for_a_refresh() {
        assert!(wants_list_refresh(&[AchievementEvent::GameCompleted]));
        assert!(!wants_list_refresh(&[
            AchievementEvent::Disconnected,
            AchievementEvent::Reconnected
        ]));
    }
}
