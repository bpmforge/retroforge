//! RetroAchievements client (D-031): RA's official, MIT-licensed `rcheevos`
//! C library, vendored and built by `build.rs`, behind a safe wrapper with
//! no networking and no file I/O.
//!
//! # Threading and ownership
//!
//! [`Achievements`] is `!Send` and `!Sync` (it owns a raw C pointer) and is
//! meant to live on the **core thread**, next to the emulator it observes.
//! Every call, and every callback rcheevos makes back into Rust, happens
//! synchronously inside one of its methods on that thread. Nothing here
//! spawns a thread or touches a socket.
//!
//! # Network protocol (the app does the HTTP)
//!
//! rcheevos wants HTTP. This crate turns each request into a plain
//! [`PendingRequest`]; the app drains them with
//! [`Achievements::take_requests`], performs the HTTP on **its own thread**
//! (sending [`Achievements::user_agent`] as `User-Agent`; POST with the given
//! `content_type` when `post_data` is `Some`, else GET), and hands the result
//! back **on the core thread** via [`Achievements::complete`] (any HTTP
//! response, 2xx or not) or [`Achievements::fail`] (no response at all).
//! rcheevos owns retry policy: a retryable failure is re-issued as a new
//! request on a later [`Achievements::idle`]/[`Achievements::do_frame`].
//!
//! # Memory
//!
//! rcheevos reads emulated memory through RA's own flat address space, not
//! CPU addresses; [`translate`] converts (see `memmap`). The app supplies a
//! `&dyn Fn(u32) -> u8` over `EmulatorCore::peek` on every
//! [`Achievements::do_frame`] / [`idle`](Achievements::idle) /
//! [`reset`](Achievements::reset); outside those calls memory reads fail
//! (return 0 bytes), and the client is configured so rcheevos never reads
//! outside them. A game therefore becomes active, and
//! [`AchievementEvent::GameLoaded`] is emitted, from the first `do_frame`
//! after its data arrives.
//!
//! # Hardcore
//!
//! [`Achievements::set_hardcore`] / [`hardcore`](Achievements::hardcore) are
//! plain switches. **rcheevos defaults hardcore ON; this wrapper turns it
//! OFF at construction** (D-030: first release is softcore). Enforcement
//! (refusing cheats, rewind, state loads...) lives in the app (W41-05).
//! Enabling hardcore with a game loaded makes rcheevos raise
//! [`AchievementEvent::ResetRequested`] and pause processing until the app
//! resets the machine and calls [`Achievements::reset`].
//!
//! # Panics
//!
//! No panic ever crosses the FFI boundary: every `extern "C"` callback is
//! wrapped in `catch_unwind`. A panic inside the app's peek closure is
//! swallowed there and the read reports failure.

// This crate IS the FFI boundary. The workspace lint is `warn`; every unsafe
// block below carries a `// SAFETY:` comment (clippy enforces it).
#![allow(unsafe_code)]

mod ffi;
mod memmap;

pub use memmap::{translate, translate_with, CartMap, Console};

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;

use ffi::*;

/// Identifies an outstanding HTTP request.
pub type RequestId = u64;

/// One HTTP request for the app to perform.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingRequest {
    pub id: RequestId,
    pub url: String,
    /// `Some` means POST this body; `None` means GET.
    pub post_data: Option<String>,
    pub content_type: Option<String>,
}

/// A credential that must not appear in logs.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Secret(value.into())
    }

    /// The raw value (to store in the keychain).
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// What the app needs to know, drained with [`Achievements::drain_events`].
#[derive(Clone, Debug, PartialEq)]
pub enum AchievementEvent {
    /// Sign-in finished. Store `token` (not the password).
    LoginSucceeded {
        username: String,
        display_name: String,
        token: Secret,
        score: u32,
        score_softcore: u32,
    },
    LoginFailed {
        code: i32,
        message: String,
    },
    /// Game data loaded and the achievement set is live.
    GameLoaded {
        game_id: u32,
        title: String,
        console_id: u32,
        achievements: u32,
        unlocked: u32,
        points: u32,
    },
    GameLoadFailed {
        code: i32,
        message: String,
    },
    Unlocked {
        id: u32,
        title: String,
        description: String,
        points: u32,
    },
    /// A measured achievement's progress indicator changed.
    Progress {
        id: u32,
        title: String,
        progress: String,
        percent: f32,
    },
    GameCompleted,
    /// The server refused or garbled a request and rcheevos will not retry.
    ServerError {
        api: String,
        message: String,
        code: i32,
        related_id: u32,
    },
    /// An unlock could not be delivered and is queued for retry.
    Disconnected,
    /// All queued unlocks were delivered.
    Reconnected,
    /// Hardcore was enabled mid-game: reset the machine, then call
    /// [`Achievements::reset`].
    ResetRequested,
}

/// One achievement of the loaded game, for a list UI.
#[derive(Clone, Debug, PartialEq)]
pub struct AchievementInfo {
    pub id: u32,
    pub title: String,
    pub description: String,
    pub points: u32,
    pub unlocked_softcore: bool,
    pub unlocked_hardcore: bool,
    /// `RC_CLIENT_ACHIEVEMENT_STATE_*`: 0 inactive, 1 active, 2 unlocked,
    /// 3 disabled (unsupported or unreadable addresses).
    pub state: u8,
    pub measured_progress: String,
    pub badge_name: String,
}

type MemFn = dyn Fn(u32) -> u8;

/// State shared with the C callbacks. Lives in a `Box` so its address (the
/// rcheevos userdata) is stable; interior mutability because callbacks
/// re-enter while a public method is on the stack (no borrow is ever held
/// across a call into C).
struct Shared {
    events: RefCell<Vec<AchievementEvent>>,
    outbox: RefCell<Vec<PendingRequest>>,
    in_flight: RefCell<HashMap<RequestId, (ServerCallbackFn, *mut c_void)>>,
    next_id: Cell<RequestId>,
    memory: Cell<Option<*const MemFn>>,
    console: Cell<Console>,
    cart: Cell<CartMap>,
    epoch: Instant,
}

/// Clears the per-call memory source even if the C side unwinds.
struct MemoryScope<'a>(&'a Shared);

impl<'a> MemoryScope<'a> {
    fn new(shared: &'a Shared, peek: &dyn Fn(u32) -> u8) -> Self {
        // SAFETY: only the lifetime is erased. The pointer is stored for the
        // duration of this scope and cleared in Drop, and the scope never
        // outlives the `peek` borrow it was created from.
        let erased = unsafe {
            std::mem::transmute::<*const (dyn Fn(u32) -> u8 + '_), *const MemFn>(peek as *const _)
        };
        shared.memory.set(Some(erased));
        MemoryScope(shared)
    }
}

impl Drop for MemoryScope<'_> {
    fn drop(&mut self) {
        self.0.memory.set(None);
    }
}

/// The RetroAchievements session. See the crate docs for the thread and
/// network rules.
pub struct Achievements {
    client: *mut RcClient,
    shared: Box<Shared>,
}

/// # Safety
/// `client` must be a live client created by [`Achievements::new`].
unsafe fn shared_of<'a>(client: *const RcClient) -> &'a Shared {
    // SAFETY: caller contract; the userdata is the `Box<Shared>` address set
    // in `new`, which outlives the client (Drop destroys the client first).
    unsafe { &*(rc_client_get_userdata(client) as *const Shared) }
}

fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        // SAFETY: rcheevos hands out NUL-terminated strings that stay valid
        // for the duration of the callback/call that produced them.
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

fn opt_cstr(p: *const c_char) -> Option<String> {
    (!p.is_null()).then(|| cstr(p))
}

unsafe extern "C" fn read_memory_cb(
    address: u32,
    buffer: *mut u8,
    num_bytes: u32,
    client: *mut RcClient,
) -> u32 {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: rcheevos passes the client we created.
        let shared = unsafe { shared_of(client) };
        let Some(mem) = shared.memory.get() else {
            return 0;
        };
        if buffer.is_null() || num_bytes == 0 || num_bytes > 8 {
            return 0;
        }
        let console = shared.console.get();
        // SAFETY: rcheevos guarantees `buffer` holds `num_bytes` bytes.
        let out = unsafe { std::slice::from_raw_parts_mut(buffer, num_bytes as usize) };
        for (i, slot) in (0u32..).zip(out.iter_mut()) {
            let Some(ra) = address.checked_add(i) else {
                return 0;
            };
            let Some(cpu) = translate_with(console, ra, shared.cart.get()) else {
                return 0;
            };
            // SAFETY: `mem` is valid while a MemoryScope is alive, and the
            // scope clears it before the borrowed closure goes away.
            *slot = unsafe { (*mem)(cpu) };
        }
        num_bytes
    }))
    .unwrap_or(0)
}

unsafe extern "C" fn server_call_cb(
    request: *const RcApiRequest,
    callback: ServerCallbackFn,
    callback_data: *mut c_void,
    client: *mut RcClient,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: rcheevos passes our client and a valid request for the
        // duration of this call; strings are copied out before returning.
        let (shared, req) = unsafe { (shared_of(client), &*request) };
        let id = shared.next_id.get();
        shared.next_id.set(id + 1);
        shared.outbox.borrow_mut().push(PendingRequest {
            id,
            url: cstr(req.url),
            post_data: opt_cstr(req.post_data),
            content_type: opt_cstr(req.content_type),
        });
        shared
            .in_flight
            .borrow_mut()
            .insert(id, (callback, callback_data));
    }));
}

unsafe extern "C" fn event_cb(event: *const RcClientEvent, client: *mut RcClient) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: valid for the duration of the callback.
        let (shared, ev) = unsafe { (shared_of(client), &*event) };
        // SAFETY: when non-null, the pointee is valid for this callback.
        let ach = unsafe { ev.achievement.as_ref() };
        let mapped = match (ev.ty, ach) {
            (RC_CLIENT_EVENT_ACHIEVEMENT_TRIGGERED, Some(a)) => AchievementEvent::Unlocked {
                id: a.id,
                title: cstr(a.title),
                description: cstr(a.description),
                points: a.points,
            },
            (
                RC_CLIENT_EVENT_ACHIEVEMENT_PROGRESS_INDICATOR_SHOW
                | RC_CLIENT_EVENT_ACHIEVEMENT_PROGRESS_INDICATOR_UPDATE,
                Some(a),
            ) => AchievementEvent::Progress {
                id: a.id,
                title: cstr(a.title),
                progress: cstr(a.measured_progress.as_ptr()),
                percent: a.measured_percent,
            },
            (RC_CLIENT_EVENT_GAME_COMPLETED, _) => AchievementEvent::GameCompleted,
            (RC_CLIENT_EVENT_SERVER_ERROR, _) => {
                // SAFETY: when non-null, valid for this callback.
                let Some(e) = (unsafe { ev.server_error.as_ref() }) else {
                    return;
                };
                AchievementEvent::ServerError {
                    api: cstr(e.api),
                    message: cstr(e.error_message),
                    code: e.result,
                    related_id: e.related_id,
                }
            }
            (RC_CLIENT_EVENT_DISCONNECTED, _) => AchievementEvent::Disconnected,
            (RC_CLIENT_EVENT_RECONNECTED, _) => AchievementEvent::Reconnected,
            (RC_CLIENT_EVENT_RESET, _) => AchievementEvent::ResetRequested,
            _ => return,
        };
        shared.events.borrow_mut().push(mapped);
    }));
}

unsafe extern "C" fn time_cb(client: *const RcClient) -> u64 {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: rcheevos passes our client.
        let shared = unsafe { shared_of(client) };
        shared.epoch.elapsed().as_millis() as u64
    }))
    .unwrap_or(0)
}

/// Userdata tags distinguishing the two async completions (never dereferenced).
const TAG_LOGIN: usize = 1;
const TAG_LOAD: usize = 2;

unsafe extern "C" fn async_done_cb(
    result: c_int,
    error_message: *const c_char,
    client: *mut RcClient,
    userdata: *mut c_void,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: rcheevos passes our client.
        let shared = unsafe { shared_of(client) };
        let tag = userdata as usize;
        let ev = if result == RC_OK {
            if tag == TAG_LOGIN {
                // SAFETY: valid (or null) for the duration of this callback.
                let Some(u) = (unsafe { rc_client_get_user_info(client).as_ref() }) else {
                    return;
                };
                AchievementEvent::LoginSucceeded {
                    username: cstr(u.username),
                    display_name: cstr(u.display_name),
                    token: Secret(cstr(u.token)),
                    score: u.score,
                    score_softcore: u.score_softcore,
                }
            } else {
                // SAFETY: valid (or null) for the duration of this callback.
                let Some(g) = (unsafe { rc_client_get_game_info(client).as_ref() }) else {
                    return;
                };
                let mut s = RcClientUserGameSummary {
                    num_core_achievements: 0,
                    num_unofficial_achievements: 0,
                    num_unlocked_achievements: 0,
                    num_unsupported_achievements: 0,
                    points_core: 0,
                    points_unlocked: 0,
                    beaten_time: 0,
                    completed_time: 0,
                };
                // SAFETY: `s` matches rc_client_user_game_summary_t
                // (layout-tested) and the client is live.
                unsafe { rc_client_get_user_game_summary(client, &mut s) };
                AchievementEvent::GameLoaded {
                    game_id: g.id,
                    title: cstr(g.title),
                    console_id: g.console_id,
                    achievements: s.num_core_achievements,
                    unlocked: s.num_unlocked_achievements,
                    points: s.points_core,
                }
            }
        } else {
            let message = cstr(error_message);
            if tag == TAG_LOGIN {
                AchievementEvent::LoginFailed {
                    code: result,
                    message,
                }
            } else {
                AchievementEvent::GameLoadFailed {
                    code: result,
                    message,
                }
            }
        };
        shared.events.borrow_mut().push(ev);
    }));
}

impl Default for Achievements {
    fn default() -> Self {
        Self::new()
    }
}

impl Achievements {
    /// A signed-out, softcore session talking to retroachievements.org.
    pub fn new() -> Self {
        let shared = Box::new(Shared {
            events: RefCell::new(Vec::new()),
            outbox: RefCell::new(Vec::new()),
            in_flight: RefCell::new(HashMap::new()),
            next_id: Cell::new(1),
            memory: Cell::new(None),
            console: Cell::new(Console::Nes),
            cart: Cell::new(CartMap::None),
            epoch: Instant::now(),
        });
        // SAFETY: both callbacks have the exact C signatures declared in
        // rc_client.h; the returned client is checked for null; `shared` is
        // boxed so the userdata pointer stays valid until Drop.
        let client = unsafe {
            let c = rc_client_create(read_memory_cb, server_call_cb);
            assert!(
                !c.is_null(),
                "rc_client_create returned NULL (out of memory)"
            );
            rc_client_set_userdata(c, &*shared as *const Shared as *mut c_void);
            rc_client_set_event_handler(c, event_cb);
            rc_client_set_get_time_millisecs_function(c, time_cb);
            // Reads only inside do_frame/idle/reset, where the peek source
            // exists; load completion is deferred to the next do_frame.
            rc_client_set_allow_background_memory_reads(c, 0);
            // rcheevos defaults hardcore ON; the first release is softcore.
            rc_client_set_hardcore_enabled(c, 0);
            rc_client_set_encore_mode_enabled(c, 0);
            rc_client_set_unofficial_enabled(c, 0);
            rc_client_set_spectator_mode_enabled(c, 0);
            c
        };
        Achievements { client, shared }
    }

    /// `User-Agent` the app must send with every request:
    /// `RetroForge/<crate version> rcheevos/<rcheevos version>`.
    pub fn user_agent(&self) -> String {
        // SAFETY: returns a static NUL-terminated string.
        let rc = cstr(unsafe { rc_version_string() });
        format!("RetroForge/{} rcheevos/{}", env!("CARGO_PKG_VERSION"), rc)
    }

    /// The vendored rcheevos version string, e.g. `"12.5"`.
    pub fn rcheevos_version() -> String {
        // SAFETY: returns a static NUL-terminated string.
        cstr(unsafe { rc_version_string() })
    }

    /// Point at a different RA-compatible server (self-hosted or a test
    /// double). Takes a bare host such as `"localhost:8080"`; the empty
    /// string restores the default.
    pub fn set_host(&mut self, host: &str) {
        let Ok(h) = CString::new(host) else { return };
        // SAFETY: live client; rcheevos copies the string.
        unsafe { rc_client_set_host(self.client, h.as_ptr()) };
    }

    /// Sign in with a password (first sign-in). The password is passed to C
    /// for the request body only; store the token from
    /// [`AchievementEvent::LoginSucceeded`], never the password.
    pub fn login_with_password(&mut self, username: &str, password: &str) {
        let (Ok(u), Ok(p)) = (CString::new(username), CString::new(password)) else {
            return self.invalid_arg(TAG_LOGIN);
        };
        // SAFETY: live client; strings are copied by rcheevos before return.
        unsafe {
            rc_client_begin_login_with_password(
                self.client,
                u.as_ptr(),
                p.as_ptr(),
                async_done_cb,
                TAG_LOGIN as *mut c_void,
            );
        }
    }

    /// Sign in with a stored token.
    pub fn login_with_token(&mut self, username: &str, token: &str) {
        let (Ok(u), Ok(t)) = (CString::new(username), CString::new(token)) else {
            return self.invalid_arg(TAG_LOGIN);
        };
        // SAFETY: as in `login_with_password`.
        unsafe {
            rc_client_begin_login_with_token(
                self.client,
                u.as_ptr(),
                t.as_ptr(),
                async_done_cb,
                TAG_LOGIN as *mut c_void,
            );
        }
    }

    /// Work around an rcheevos 12.5.0 defect: `rc_client_unload_game` raises
    /// a still-pending "game completed" event after it has set
    /// `client->game = NULL`, and `rc_client_raise_mastery_event`
    /// dereferences `client->game->subsets` (SIGSEGV; reproduced by the
    /// `award_then_drop_without_a_frame` test). `rc_client_do_frame` raises
    /// pending events while `client->game` is still set, so run one frame
    /// before anything that unloads. No memory source is installed, so every
    /// read fails and no achievement can trigger during the flush.
    fn flush_pending_events(&mut self) {
        if self.is_game_loaded() {
            // SAFETY: live client; outside a MemoryScope reads report failure.
            unsafe { rc_client_do_frame(self.client) };
        }
    }

    pub fn logout(&mut self) {
        self.flush_pending_events();
        // SAFETY: live client.
        unsafe { rc_client_logout(self.client) };
    }

    /// Start loading a game by its RA hash (rf-cart computes it). The
    /// result arrives as [`AchievementEvent::GameLoaded`] /
    /// [`GameLoadFailed`](AchievementEvent::GameLoadFailed); the load waits
    /// for sign-in if one is in progress.
    pub fn load_game(&mut self, console: Console, ra_hash: &str) {
        self.load_game_with(console, CartMap::None, ra_hash);
    }

    /// [`load_game`](Self::load_game) plus where the cartridge's RAM sits on
    /// the CPU bus, so RA's cartridge-RAM addresses can be served (see
    /// [`CartMap`]).
    pub fn load_game_with(&mut self, console: Console, cart: CartMap, ra_hash: &str) {
        self.shared.cart.set(cart);
        self.flush_pending_events();
        self.shared.console.set(console);
        let Ok(h) = CString::new(ra_hash) else {
            return self.invalid_arg(TAG_LOAD);
        };
        // SAFETY: live client; hash copied by rcheevos before return.
        unsafe {
            rc_client_begin_load_game(
                self.client,
                h.as_ptr(),
                async_done_cb,
                TAG_LOAD as *mut c_void,
            );
        }
    }

    pub fn unload_game(&mut self) {
        self.flush_pending_events();
        // SAFETY: live client.
        unsafe { rc_client_unload_game(self.client) };
    }

    pub fn is_game_loaded(&self) -> bool {
        // SAFETY: live client.
        unsafe { rc_client_is_game_loaded(self.client) != 0 }
    }

    /// See the crate docs: the app enforces what hardcore forbids.
    pub fn set_hardcore(&mut self, on: bool) {
        // SAFETY: live client.
        unsafe { rc_client_set_hardcore_enabled(self.client, c_int::from(on)) };
    }

    pub fn hardcore(&self) -> bool {
        // SAFETY: live client.
        unsafe { rc_client_get_hardcore_enabled(self.client) != 0 }
    }

    /// Evaluate achievements for the frame just emulated. `peek` takes a
    /// CPU-bus address (already translated from RA's map).
    pub fn do_frame(&mut self, peek: &dyn Fn(u32) -> u8) {
        let _scope = MemoryScope::new(&self.shared, peek);
        // SAFETY: live client; callbacks re-enter only through `shared`.
        unsafe { rc_client_do_frame(self.client) };
    }

    /// Service timers and retries while emulation is paused.
    pub fn idle(&mut self, peek: &dyn Fn(u32) -> u8) {
        let _scope = MemoryScope::new(&self.shared, peek);
        // SAFETY: live client.
        unsafe { rc_client_idle(self.client) };
    }

    /// Whether the player may pause now. rcheevos rate-limits pausing in
    /// hardcore (`rc_client_can_pause`, rc_client.h: call it only when the
    /// user is trying to pause). `Err(frames)` is how many more frames must
    /// run first. Always `Ok` in softcore.
    pub fn can_pause(&mut self) -> Result<(), u32> {
        let mut remaining: u32 = 0;
        // SAFETY: live client; `remaining` outlives the call.
        let ok = unsafe { rc_client_can_pause(self.client, &mut remaining) };
        if ok != 0 {
            Ok(())
        } else {
            Err(remaining)
        }
    }

    /// Tell rcheevos the machine was reset.
    pub fn reset(&mut self, peek: &dyn Fn(u32) -> u8) {
        let _scope = MemoryScope::new(&self.shared, peek);
        // SAFETY: live client.
        unsafe { rc_client_reset(self.client) };
    }

    /// Requests waiting for the app to perform.
    pub fn take_requests(&mut self) -> Vec<PendingRequest> {
        std::mem::take(&mut *self.shared.outbox.borrow_mut())
    }

    /// Feed back an HTTP response (any status). Returns `false` if `id` is
    /// unknown or already completed. Core thread only.
    pub fn complete(&mut self, id: RequestId, http_status: i32, body: &[u8]) -> bool {
        self.finish(id, http_status, body)
    }

    /// Report that no HTTP response was obtained (offline, DNS, TLS, ...).
    /// `retryable` lets rcheevos queue unlocks and try again. Returns `false`
    /// if `id` is unknown.
    pub fn fail(&mut self, id: RequestId, retryable: bool, message: &str) -> bool {
        let status = if retryable {
            RC_API_SERVER_RESPONSE_RETRYABLE_CLIENT_ERROR
        } else {
            RC_API_SERVER_RESPONSE_CLIENT_ERROR
        };
        self.finish(id, status, message.as_bytes())
    }

    fn finish(&mut self, id: RequestId, status: i32, body: &[u8]) -> bool {
        // Borrow released before calling into C, which re-enters.
        let entry = self.shared.in_flight.borrow_mut().remove(&id);
        let Some((callback, data)) = entry else {
            return false;
        };
        // rcheevos treats `body` as a C string: copy and NUL-terminate.
        let mut buf = Vec::with_capacity(body.len() + 1);
        buf.extend_from_slice(body);
        buf.push(0);
        let response = RcApiServerResponse {
            body: buf.as_ptr().cast(),
            body_length: body.len(),
            http_status_code: status,
        };
        // SAFETY: `callback`/`data` are exactly what rcheevos gave
        // server_call_cb for this request; `response` and `buf` outlive the
        // call; the client is live.
        unsafe { callback(&response, data) };
        true
    }

    /// Take every event raised since the last call.
    pub fn drain_events(&mut self) -> Vec<AchievementEvent> {
        std::mem::take(&mut *self.shared.events.borrow_mut())
    }

    /// Achievements of the loaded game (core set), for a list UI.
    pub fn achievements(&self) -> Vec<AchievementInfo> {
        let mut out = Vec::new();
        // SAFETY: live client; the list is destroyed below and every pointer
        // read is only used before that.
        unsafe {
            let list = rc_client_create_achievement_list(
                self.client,
                RC_CLIENT_ACHIEVEMENT_CATEGORY_CORE,
                RC_CLIENT_ACHIEVEMENT_LIST_GROUPING_LOCK_STATE,
            );
            if list.is_null() {
                return out;
            }
            let l = &*list;
            for b in 0..l.num_buckets as usize {
                let bucket = &*l.buckets.add(b);
                for i in 0..bucket.num_achievements as usize {
                    let a = &**bucket.achievements.add(i);
                    out.push(AchievementInfo {
                        id: a.id,
                        title: cstr(a.title),
                        description: cstr(a.description),
                        points: a.points,
                        unlocked_softcore: a.unlocked & 1 != 0,
                        unlocked_hardcore: a.unlocked & 2 != 0,
                        state: a.state,
                        measured_progress: cstr(a.measured_progress.as_ptr()),
                        badge_name: cstr(a.badge_name.as_ptr()),
                    });
                }
            }
            rc_client_destroy_achievement_list(list);
        }
        out
    }

    fn invalid_arg(&self, tag: usize) {
        let message = "argument contains a NUL byte".to_string();
        let code = -25; // RC_INVALID_STATE
        let ev = if tag == TAG_LOGIN {
            AchievementEvent::LoginFailed { code, message }
        } else {
            AchievementEvent::GameLoadFailed { code, message }
        };
        self.shared.events.borrow_mut().push(ev);
    }
}

impl Drop for Achievements {
    fn drop(&mut self) {
        // Outstanding requests own C-side state that only their callback
        // frees; fail them while the client is still alive. Bounded: a
        // failure callback can issue a fresh request, but not forever.
        for _ in 0..8 {
            let ids: Vec<RequestId> = self.shared.in_flight.borrow().keys().copied().collect();
            if ids.is_empty() {
                break;
            }
            for id in ids {
                self.finish(id, RC_API_SERVER_RESPONSE_CLIENT_ERROR, b"shutting down");
            }
        }
        self.flush_pending_events();
        // SAFETY: the client was created in `new` and is destroyed once;
        // `shared` is dropped after this body, so callbacks during destroy
        // still see valid userdata.
        unsafe { rc_client_destroy(self.client) };
    }
}

#[cfg(test)]
mod tests {
    use super::ffi::probe::*;
    use super::*;
    use std::mem::{offset_of, size_of};

    #[test]
    fn ffi_layout_matches_c() {
        // SAFETY: the probes are pure functions returning constants.
        unsafe {
            assert_eq!(size_of::<i64>(), rf_ra_probe_sizeof_time_t(), "time_t");
            assert_eq!(
                size_of::<RcClientAchievement>(),
                rf_ra_probe_sizeof_achievement()
            );
            assert_eq!(
                offset_of!(RcClientAchievement, title),
                rf_ra_probe_off_ach_title()
            );
            assert_eq!(
                offset_of!(RcClientAchievement, description),
                rf_ra_probe_off_ach_description()
            );
            assert_eq!(
                offset_of!(RcClientAchievement, badge_name),
                rf_ra_probe_off_ach_badge_name()
            );
            assert_eq!(
                offset_of!(RcClientAchievement, measured_progress),
                rf_ra_probe_off_ach_measured_progress()
            );
            assert_eq!(
                offset_of!(RcClientAchievement, measured_percent),
                rf_ra_probe_off_ach_measured_percent()
            );
            assert_eq!(
                offset_of!(RcClientAchievement, id),
                rf_ra_probe_off_ach_id()
            );
            assert_eq!(
                offset_of!(RcClientAchievement, points),
                rf_ra_probe_off_ach_points()
            );
            assert_eq!(
                offset_of!(RcClientAchievement, unlock_time),
                rf_ra_probe_off_ach_unlock_time()
            );
            assert_eq!(
                offset_of!(RcClientAchievement, state),
                rf_ra_probe_off_ach_state()
            );
            assert_eq!(
                offset_of!(RcClientAchievement, unlocked),
                rf_ra_probe_off_ach_unlocked()
            );
            assert_eq!(
                offset_of!(RcClientAchievement, rarity),
                rf_ra_probe_off_ach_rarity()
            );
            assert_eq!(
                offset_of!(RcClientAchievement, ty),
                rf_ra_probe_off_ach_type()
            );
            assert_eq!(
                offset_of!(RcClientAchievement, badge_url),
                rf_ra_probe_off_ach_badge_url()
            );

            assert_eq!(size_of::<RcClientEvent>(), rf_ra_probe_sizeof_event());
            assert_eq!(offset_of!(RcClientEvent, ty), rf_ra_probe_off_ev_type());
            assert_eq!(
                offset_of!(RcClientEvent, achievement),
                rf_ra_probe_off_ev_achievement()
            );
            assert_eq!(
                offset_of!(RcClientEvent, server_error),
                rf_ra_probe_off_ev_server_error()
            );

            assert_eq!(
                size_of::<RcClientServerError>(),
                rf_ra_probe_sizeof_server_error()
            );
            assert_eq!(
                offset_of!(RcClientServerError, error_message),
                rf_ra_probe_off_se_error_message()
            );
            assert_eq!(
                offset_of!(RcClientServerError, api),
                rf_ra_probe_off_se_api()
            );
            assert_eq!(
                offset_of!(RcClientServerError, result),
                rf_ra_probe_off_se_result()
            );
            assert_eq!(
                offset_of!(RcClientServerError, related_id),
                rf_ra_probe_off_se_related_id()
            );

            assert_eq!(
                size_of::<RcApiServerResponse>(),
                rf_ra_probe_sizeof_server_response()
            );
            assert_eq!(
                offset_of!(RcApiServerResponse, body),
                rf_ra_probe_off_sr_body()
            );
            assert_eq!(
                offset_of!(RcApiServerResponse, body_length),
                rf_ra_probe_off_sr_body_length()
            );
            assert_eq!(
                offset_of!(RcApiServerResponse, http_status_code),
                rf_ra_probe_off_sr_status()
            );

            assert_eq!(offset_of!(RcApiRequest, url), rf_ra_probe_off_rq_url());
            assert_eq!(
                offset_of!(RcApiRequest, post_data),
                rf_ra_probe_off_rq_post_data()
            );
            assert_eq!(
                offset_of!(RcApiRequest, content_type),
                rf_ra_probe_off_rq_content_type()
            );

            assert_eq!(offset_of!(RcClientGame, id), rf_ra_probe_off_game_id());
            assert_eq!(
                offset_of!(RcClientGame, console_id),
                rf_ra_probe_off_game_console_id()
            );
            assert_eq!(
                offset_of!(RcClientGame, title),
                rf_ra_probe_off_game_title()
            );
            assert_eq!(offset_of!(RcClientGame, hash), rf_ra_probe_off_game_hash());

            assert_eq!(
                offset_of!(RcClientUser, display_name),
                rf_ra_probe_off_user_display_name()
            );
            assert_eq!(
                offset_of!(RcClientUser, username),
                rf_ra_probe_off_user_username()
            );
            assert_eq!(
                offset_of!(RcClientUser, score),
                rf_ra_probe_off_user_score()
            );

            assert_eq!(
                size_of::<RcClientUserGameSummary>(),
                rf_ra_probe_sizeof_summary()
            );
            assert_eq!(
                offset_of!(RcClientUserGameSummary, num_core_achievements),
                rf_ra_probe_off_sum_core()
            );
            assert_eq!(
                offset_of!(RcClientUserGameSummary, num_unlocked_achievements),
                rf_ra_probe_off_sum_unlocked()
            );
            assert_eq!(
                offset_of!(RcClientUserGameSummary, points_core),
                rf_ra_probe_off_sum_points_core()
            );
            assert_eq!(
                offset_of!(RcClientUserGameSummary, points_unlocked),
                rf_ra_probe_off_sum_points_unlocked()
            );
        }
    }

    #[test]
    fn user_agent_and_defaults() {
        let a = Achievements::new();
        assert_eq!(
            a.user_agent(),
            format!("RetroForge/{} rcheevos/12.5", env!("CARGO_PKG_VERSION"))
        );
        assert!(!a.hardcore(), "softcore by default (rcheevos defaults ON)");
        assert!(!a.is_game_loaded());
    }

    #[test]
    fn hardcore_toggles() {
        let mut a = Achievements::new();
        a.set_hardcore(true);
        assert!(a.hardcore());
        a.set_hardcore(false);
        assert!(!a.hardcore());
    }
}
