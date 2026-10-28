# Vendored: rcheevos

* Upstream: https://github.com/RetroAchievements/rcheevos
* Tag: v12.5.0
* Commit: 1433173220a7eaede6a9ed7a18e94117be1821e0 (commit date 2026-09-13)
* Vendored: 2026-10-27 (ticket W41-04, decision D-033)
* Licence: MIT (`LICENSE`), except `src/rhash/md5.c` / `md5.h`, which are
  L. Peter Deutsch's Aladdin Enterprises zlib-style permissive MD5 (notice in
  the file headers). Both are in THIRD-PARTY-NOTICES at the repo root.

## Contents (50 files)

`include/` (all public headers except `rc_client_raintegration.h`), `src/`
(`rc_client*.c/h`, `rc_compat.*`, `rc_util.c`, `rc_version.*`), `src/rcheevos/`
(`*.c`, `rc_internal.h`, `rc_validate.h`), `src/rapi/`, and from `src/rhash/`
only `md5.c` and `md5.h`.

## Omitted, and why

* `src/rhash/` except md5: file/disc hashing, zip, CHD/CD readers. Needs
  file I/O; RetroForge hashes ROMs itself (rf-cart) and calls
  `rc_client_begin_load_game(hash)`, so `RC_CLIENT_SUPPORTS_HASH` is NOT
  defined. `md5.c` stays: `rc_api_runtime.c` signs award requests with it.
* `src/rc_libretro.*`, `src/rc_client_raintegration*`, `include/
  rc_client_raintegration.h`, `include/module.modulemap`: libretro/RAIntegration
  glue, not used (all guarded by undefined macros or not referenced).
* `test/`, `validator/`, `Package.swift`, `CHANGELOG.md`, `README.md`,
  `*.natvis`: not needed to build; tests are not vendored.

## Local patches

None. Vendored files are byte-identical to the tag.

## Known upstream defect, worked around in Rust (not patched here)

`rc_client_unload_game` (also reached from `rc_client_destroy`,
`rc_client_logout`, `rc_client_begin_load_game`) sets `client->game = NULL` and
then raises pending events; if a game-completed (mastery) event is pending,
`rc_client_raise_mastery_event` dereferences `client->game->subsets` and
crashes. `Achievements::flush_pending_events` runs one `rc_client_do_frame`
with all memory reads failing before any unload. Test:
`award_then_drop_without_a_frame`. Re-check on every re-vendor.

## Build

`crates/rf-achievements/build.rs` compiles an explicit source list with the
`cc` crate. No defines are required (no Lua flag exists in v12.5.0).
