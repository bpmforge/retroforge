/* Layout probes for the hand-written FFI structs in src/ffi.rs. Compiled
 * by build.rs against the vendored headers; the Rust unit test
 * `ffi_layout_matches_c` compares every value. Not part of rcheevos. */
#include <stddef.h>
#include <stdint.h>
#include "rc_client.h"

#define PROBE(name, expr) size_t rf_ra_probe_##name(void) { return (size_t)(expr); }

PROBE(sizeof_achievement, sizeof(rc_client_achievement_t))
PROBE(off_ach_title, offsetof(rc_client_achievement_t, title))
PROBE(off_ach_description, offsetof(rc_client_achievement_t, description))
PROBE(off_ach_badge_name, offsetof(rc_client_achievement_t, badge_name))
PROBE(off_ach_measured_progress, offsetof(rc_client_achievement_t, measured_progress))
PROBE(off_ach_measured_percent, offsetof(rc_client_achievement_t, measured_percent))
PROBE(off_ach_id, offsetof(rc_client_achievement_t, id))
PROBE(off_ach_points, offsetof(rc_client_achievement_t, points))
PROBE(off_ach_unlock_time, offsetof(rc_client_achievement_t, unlock_time))
PROBE(off_ach_state, offsetof(rc_client_achievement_t, state))
PROBE(off_ach_unlocked, offsetof(rc_client_achievement_t, unlocked))
PROBE(off_ach_rarity, offsetof(rc_client_achievement_t, rarity))
PROBE(off_ach_type, offsetof(rc_client_achievement_t, type))
PROBE(off_ach_badge_url, offsetof(rc_client_achievement_t, badge_url))

PROBE(sizeof_event, sizeof(rc_client_event_t))
PROBE(off_ev_type, offsetof(rc_client_event_t, type))
PROBE(off_ev_achievement, offsetof(rc_client_event_t, achievement))
PROBE(off_ev_server_error, offsetof(rc_client_event_t, server_error))

PROBE(sizeof_server_error, sizeof(rc_client_server_error_t))
PROBE(off_se_error_message, offsetof(rc_client_server_error_t, error_message))
PROBE(off_se_api, offsetof(rc_client_server_error_t, api))
PROBE(off_se_result, offsetof(rc_client_server_error_t, result))
PROBE(off_se_related_id, offsetof(rc_client_server_error_t, related_id))

PROBE(sizeof_server_response, sizeof(rc_api_server_response_t))
PROBE(off_sr_body, offsetof(rc_api_server_response_t, body))
PROBE(off_sr_body_length, offsetof(rc_api_server_response_t, body_length))
PROBE(off_sr_status, offsetof(rc_api_server_response_t, http_status_code))

PROBE(off_rq_url, offsetof(rc_api_request_t, url))
PROBE(off_rq_post_data, offsetof(rc_api_request_t, post_data))
PROBE(off_rq_content_type, offsetof(rc_api_request_t, content_type))

PROBE(off_game_id, offsetof(rc_client_game_t, id))
PROBE(off_game_console_id, offsetof(rc_client_game_t, console_id))
PROBE(off_game_title, offsetof(rc_client_game_t, title))
PROBE(off_game_hash, offsetof(rc_client_game_t, hash))

PROBE(off_user_display_name, offsetof(rc_client_user_t, display_name))
PROBE(off_user_username, offsetof(rc_client_user_t, username))
PROBE(off_user_score, offsetof(rc_client_user_t, score))

PROBE(sizeof_summary, sizeof(rc_client_user_game_summary_t))
PROBE(off_sum_core, offsetof(rc_client_user_game_summary_t, num_core_achievements))
PROBE(off_sum_unlocked, offsetof(rc_client_user_game_summary_t, num_unlocked_achievements))
PROBE(off_sum_points_core, offsetof(rc_client_user_game_summary_t, points_core))
PROBE(off_sum_points_unlocked, offsetof(rc_client_user_game_summary_t, points_unlocked))

PROBE(sizeof_time_t, sizeof(time_t))
