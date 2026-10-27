//! Hand-written `extern "C"` declarations for the slice of rcheevos'
//! `rc_client` API this crate uses (D-031; no bindgen).
//!
//! Every struct mirrors `vendor/rcheevos/include/rc_client.h` /
//! `rc_api_request.h` at v12.5.0 field for field. `shim/layout_check.c`
//! exports `sizeof`/`offsetof` probes compiled from the real headers and the
//! `ffi_layout_matches_c` test in `lib.rs` compares them with this file, so a
//! re-vendor that moves a field fails a test instead of corrupting memory.
//! `RC_CCONV` is empty on every non-Windows target, so plain `extern "C"` is
//! the right ABI for the macOS/Linux builds RetroForge ships.

use std::ffi::{c_char, c_int, c_void};
use std::marker::{PhantomData, PhantomPinned};

/// `rc_client_t`: opaque.
#[repr(C)]
pub struct RcClient {
    _opaque: [u8; 0],
    _pin: PhantomData<(*mut u8, PhantomPinned)>,
}

/// `rc_client_async_handle_t`: opaque, returned by the `begin_*` calls and
/// only meaningful to `rc_client_abort_async`, which this crate does not use.
#[repr(C)]
pub struct RcAsyncHandle {
    _opaque: [u8; 0],
    _pin: PhantomData<(*mut u8, PhantomPinned)>,
}

/// Prefix of `rc_api_request_t`. The real struct ends with an `rc_buffer_t`
/// that owns the strings; we only ever read through a `*const` handed to us
/// by rcheevos, so the prefix is all that is declared.
#[repr(C)]
pub struct RcApiRequest {
    pub url: *const c_char,
    pub post_data: *const c_char,
    pub content_type: *const c_char,
}

/// `rc_api_server_response_t`.
#[repr(C)]
pub struct RcApiServerResponse {
    pub body: *const c_char,
    pub body_length: usize,
    pub http_status_code: c_int,
}

/// `rc_client_achievement_t` (v12.5.0 layout, including the 11.1 and 12.0
/// extensions). `time_t` is `i64` on every 64-bit target; checked against C.
#[repr(C)]
pub struct RcClientAchievement {
    pub title: *const c_char,
    pub description: *const c_char,
    pub badge_name: [c_char; 8],
    pub measured_progress: [c_char; 24],
    pub measured_percent: f32,
    pub id: u32,
    pub points: u32,
    pub unlock_time: i64,
    pub state: u8,
    pub category: u8,
    pub bucket: u8,
    pub unlocked: u8,
    pub rarity: f32,
    pub rarity_hardcore: f32,
    pub ty: u8,
    pub badge_url: *const c_char,
    pub badge_locked_url: *const c_char,
}

/// `rc_client_server_error_t`.
#[repr(C)]
pub struct RcClientServerError {
    pub error_message: *const c_char,
    pub api: *const c_char,
    pub result: c_int,
    pub related_id: u32,
}

/// `rc_client_event_t`. Pointer members whose pointee types this crate never
/// reads (leaderboards, subsets) are declared `*mut c_void`; only offsets
/// matter and every one is pointer-sized.
#[repr(C)]
pub struct RcClientEvent {
    pub ty: u32,
    pub achievement: *mut RcClientAchievement,
    pub leaderboard: *mut c_void,
    pub leaderboard_tracker: *mut c_void,
    pub leaderboard_scoreboard: *mut c_void,
    pub server_error: *mut RcClientServerError,
    pub subset: *mut c_void,
}

/// `rc_client_game_t` (prefix through `hash`).
#[repr(C)]
pub struct RcClientGame {
    pub id: u32,
    pub console_id: u32,
    pub title: *const c_char,
    pub hash: *const c_char,
}

/// `rc_client_user_t` (prefix through `score_softcore`).
#[repr(C)]
pub struct RcClientUser {
    pub display_name: *const c_char,
    pub username: *const c_char,
    pub token: *const c_char,
    pub score: u32,
    pub score_softcore: u32,
}

/// `rc_client_user_game_summary_t`.
#[repr(C)]
pub struct RcClientUserGameSummary {
    pub num_core_achievements: u32,
    pub num_unofficial_achievements: u32,
    pub num_unlocked_achievements: u32,
    pub num_unsupported_achievements: u32,
    pub points_core: u32,
    pub points_unlocked: u32,
    pub beaten_time: i64,
    pub completed_time: i64,
}

/// `rc_client_achievement_bucket_t`.
#[repr(C)]
pub struct RcClientAchievementBucket {
    pub achievements: *const *const RcClientAchievement,
    pub num_achievements: u32,
    pub label: *const c_char,
    pub subset_id: u32,
    pub bucket_type: u8,
}

/// `rc_client_achievement_list_t`.
#[repr(C)]
pub struct RcClientAchievementList {
    pub buckets: *const RcClientAchievementBucket,
    pub num_buckets: u32,
}

pub type ReadMemoryFn = unsafe extern "C" fn(
    address: u32,
    buffer: *mut u8,
    num_bytes: u32,
    client: *mut RcClient,
) -> u32;
pub type ServerCallbackFn =
    unsafe extern "C" fn(response: *const RcApiServerResponse, callback_data: *mut c_void);
pub type ServerCallFn = unsafe extern "C" fn(
    request: *const RcApiRequest,
    callback: ServerCallbackFn,
    callback_data: *mut c_void,
    client: *mut RcClient,
);
pub type ClientCallbackFn = unsafe extern "C" fn(
    result: c_int,
    error_message: *const c_char,
    client: *mut RcClient,
    userdata: *mut c_void,
);
pub type EventHandlerFn = unsafe extern "C" fn(event: *const RcClientEvent, client: *mut RcClient);
pub type TimeMillisFn = unsafe extern "C" fn(client: *const RcClient) -> u64;

// rc_error.h
pub const RC_OK: c_int = 0;
// rc_api_request.h
pub const RC_API_SERVER_RESPONSE_CLIENT_ERROR: c_int = -1;
pub const RC_API_SERVER_RESPONSE_RETRYABLE_CLIENT_ERROR: c_int = -2;
// rc_consoles.h
pub const RC_CONSOLE_SUPER_NINTENDO: u32 = 3;
pub const RC_CONSOLE_NINTENDO: u32 = 7;
// rc_client.h: achievement list category / grouping
pub const RC_CLIENT_ACHIEVEMENT_CATEGORY_CORE: c_int = 1;
pub const RC_CLIENT_ACHIEVEMENT_LIST_GROUPING_LOCK_STATE: c_int = 0;
// rc_client.h: event types (the ones this crate maps)
pub const RC_CLIENT_EVENT_ACHIEVEMENT_TRIGGERED: u32 = 1;
pub const RC_CLIENT_EVENT_ACHIEVEMENT_PROGRESS_INDICATOR_SHOW: u32 = 7;
pub const RC_CLIENT_EVENT_ACHIEVEMENT_PROGRESS_INDICATOR_UPDATE: u32 = 9;
pub const RC_CLIENT_EVENT_RESET: u32 = 14;
pub const RC_CLIENT_EVENT_GAME_COMPLETED: u32 = 15;
pub const RC_CLIENT_EVENT_SERVER_ERROR: u32 = 16;
pub const RC_CLIENT_EVENT_DISCONNECTED: u32 = 17;
pub const RC_CLIENT_EVENT_RECONNECTED: u32 = 18;

extern "C" {
    pub fn rc_client_create(read_memory: ReadMemoryFn, server_call: ServerCallFn) -> *mut RcClient;
    pub fn rc_client_destroy(client: *mut RcClient);
    pub fn rc_client_set_userdata(client: *mut RcClient, userdata: *mut c_void);
    pub fn rc_client_get_userdata(client: *const RcClient) -> *mut c_void;
    pub fn rc_client_set_event_handler(client: *mut RcClient, handler: EventHandlerFn);
    pub fn rc_client_set_allow_background_memory_reads(client: *mut RcClient, allowed: c_int);
    pub fn rc_client_set_get_time_millisecs_function(client: *mut RcClient, handler: TimeMillisFn);
    pub fn rc_client_set_host(client: *mut RcClient, hostname: *const c_char);

    pub fn rc_client_set_hardcore_enabled(client: *mut RcClient, enabled: c_int);
    pub fn rc_client_get_hardcore_enabled(client: *const RcClient) -> c_int;
    pub fn rc_client_set_encore_mode_enabled(client: *mut RcClient, enabled: c_int);
    pub fn rc_client_set_unofficial_enabled(client: *mut RcClient, enabled: c_int);
    pub fn rc_client_set_spectator_mode_enabled(client: *mut RcClient, enabled: c_int);

    pub fn rc_client_begin_login_with_password(
        client: *mut RcClient,
        username: *const c_char,
        password: *const c_char,
        callback: ClientCallbackFn,
        userdata: *mut c_void,
    ) -> *mut RcAsyncHandle;
    pub fn rc_client_begin_login_with_token(
        client: *mut RcClient,
        username: *const c_char,
        token: *const c_char,
        callback: ClientCallbackFn,
        userdata: *mut c_void,
    ) -> *mut RcAsyncHandle;
    pub fn rc_client_logout(client: *mut RcClient);
    pub fn rc_client_get_user_info(client: *const RcClient) -> *const RcClientUser;

    pub fn rc_client_begin_load_game(
        client: *mut RcClient,
        hash: *const c_char,
        callback: ClientCallbackFn,
        userdata: *mut c_void,
    ) -> *mut RcAsyncHandle;
    pub fn rc_client_is_game_loaded(client: *const RcClient) -> c_int;
    pub fn rc_client_unload_game(client: *mut RcClient);
    pub fn rc_client_get_game_info(client: *const RcClient) -> *const RcClientGame;
    pub fn rc_client_get_user_game_summary(
        client: *const RcClient,
        summary: *mut RcClientUserGameSummary,
    );

    pub fn rc_client_create_achievement_list(
        client: *mut RcClient,
        category: c_int,
        grouping: c_int,
    ) -> *mut RcClientAchievementList;
    pub fn rc_client_destroy_achievement_list(list: *mut RcClientAchievementList);

    pub fn rc_client_do_frame(client: *mut RcClient);
    pub fn rc_client_idle(client: *mut RcClient);
    pub fn rc_client_can_pause(client: *mut RcClient, frames_remaining: *mut u32) -> c_int;
    pub fn rc_client_reset(client: *mut RcClient);

    pub fn rc_version_string() -> *const c_char;
}

/// Layout probes compiled from the real headers (`shim/layout_check.c`).
#[cfg(test)]
pub(crate) mod probe {
    macro_rules! probes {
        ($($name:ident),* $(,)?) => {
            extern "C" { $(pub fn $name() -> usize;)* }
        };
    }
    probes!(
        rf_ra_probe_sizeof_achievement,
        rf_ra_probe_off_ach_title,
        rf_ra_probe_off_ach_description,
        rf_ra_probe_off_ach_badge_name,
        rf_ra_probe_off_ach_measured_progress,
        rf_ra_probe_off_ach_measured_percent,
        rf_ra_probe_off_ach_id,
        rf_ra_probe_off_ach_points,
        rf_ra_probe_off_ach_unlock_time,
        rf_ra_probe_off_ach_state,
        rf_ra_probe_off_ach_unlocked,
        rf_ra_probe_off_ach_rarity,
        rf_ra_probe_off_ach_type,
        rf_ra_probe_off_ach_badge_url,
        rf_ra_probe_sizeof_event,
        rf_ra_probe_off_ev_type,
        rf_ra_probe_off_ev_achievement,
        rf_ra_probe_off_ev_server_error,
        rf_ra_probe_sizeof_server_error,
        rf_ra_probe_off_se_error_message,
        rf_ra_probe_off_se_api,
        rf_ra_probe_off_se_result,
        rf_ra_probe_off_se_related_id,
        rf_ra_probe_sizeof_server_response,
        rf_ra_probe_off_sr_body,
        rf_ra_probe_off_sr_body_length,
        rf_ra_probe_off_sr_status,
        rf_ra_probe_off_rq_url,
        rf_ra_probe_off_rq_post_data,
        rf_ra_probe_off_rq_content_type,
        rf_ra_probe_off_game_id,
        rf_ra_probe_off_game_console_id,
        rf_ra_probe_off_game_title,
        rf_ra_probe_off_game_hash,
        rf_ra_probe_off_user_display_name,
        rf_ra_probe_off_user_username,
        rf_ra_probe_off_user_score,
        rf_ra_probe_sizeof_summary,
        rf_ra_probe_off_sum_core,
        rf_ra_probe_off_sum_unlocked,
        rf_ra_probe_off_sum_points_core,
        rf_ra_probe_off_sum_points_unlocked,
        rf_ra_probe_sizeof_time_t,
    );
}
