# RetroForge — project rules (read first, every session)

Entry point for coding agents: `MASTER_PROMPT.md` → `plan.json` → `PLAYBOOK.md`.

## Laws

1. **One ticket at a time** from `plan.json`; respect `write_scope` and
   `depends_on`. Claim = set `in_progress` + commit.
2. **Verify every external API** against docs.rs/source before use. Traps:
   bincode 3 (`Encode`/`Decode`, not 1.x serde fns), wgpu 30 + egui 0.35 are
   newer than most training data — check signatures, upgrade only in
   lockstep, versions live in `docs/TECH_STACK.md`.
3. **Full gate before closing any ticket**:
   `cargo fmt --check && cargo clippy --workspace -- -D warnings && cargo test --workspace && scripts/validate-arch.sh`
4. **Determinism invariant** and **layer boundaries** (ARCHITECTURE.md §3,
   §6) are inviolable. Cores never import upper layers; nothing mutates core
   state off the core thread; cores emit indexed pixels + metadata.
5. **No ROM bytes in git**; no copyrighted titles hardcoded in engine code;
   hardware claims cite nesdev/fullsnes in doc comments.
6. **Accuracy Mode is the reference**: enhancements are opt-in overlays over
   an unmodified simulation; a fresh install boots in Accuracy Mode.
7. Report test counts in commit/PR bodies (e.g. "workspace: 214 passing").
8. Push `main` to both remotes after merged work: `git push origin main &&
   git push github main` (origin/Gitea may be unreachable off-LAN — GitHub
   always; note unsynced state in docs/STATUS.md when it happens).

## Map

- Architecture: `docs/ARCHITECTURE.md` (start here) · deep-dives in `docs/design/`
- Requirements: `docs/SRS.md` · stories `docs/USER_STORIES.md`
- Tests & gates: `docs/TESTING.md` · roadmap/exit criteria `docs/ROADMAP.md`
- Research (verified 2026-07-06, with URLs): `docs/research/`
- Profiles spec: `docs/design/GAME_PROFILES.md` · example profiles `/profiles`
- Progress ledger: `docs/STATUS.md`

## Build

`cargo test --workspace` — no external deps needed until W0-03 adds the
test-ROM fetcher (network, gitignored `roms/`). GPU tests are headless via
wgpu; on CI they fall back to llvmpipe/lavapipe (see .github/workflows/ci.yml).
