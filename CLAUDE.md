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
8. **Every hand-rolled walk must prove progress.** In any `while i < n`
   loop, the index has to advance on *every* path through the body — if a
   branch can leave it unchanged, the loop is infinite and an accumulator
   inside it is a memory bomb. `cargo test` runs unsandboxed on the
   developer's workstation: on 2026-08-21 one such loop in
   `interpolation.rs` panicked the machine twice (docs/LESSONS.md RF-L-09).
   Prefer an iterator or a range; if you must index by hand, put the
   unconditional advance first and say in a comment why it terminates. A
   test that hangs is not a failing test — it is a denial of service, so
   never re-run a suite that hung without finding the loop first.
9. Push `main` to both remotes after merged work: `git push origin main &&
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

**GitHub is STORAGE, not a gate (ruling 2026-08-21, made PERMANENT
2026-08-22).** Every run since ~2026-08-07 was rejected with *"The job was
not started because an Actions budget is preventing further use"* — 166
failures to 34 successes, and not one of them a code failure. **Brad's
ruling 2026-08-22: the Actions budget is not being increased.** So this is
policy, not a dip to wait out: hosted CI will not run again, and no
statement anywhere in this repo may cite "green in CI" as evidence. **A red run
on GitHub is not a signal; do not chase it, and do not treat a green local
gate as contradicted by it.** The nine-command gate in law 3 plus
`scripts/local-gate.sh` and `scripts/docs-gate.sh` are what decide whether
work is done.

The workflow files stay in the tree — they are correct, and they would
work on a **self-hosted runner**, which GitHub does not bill for on
private repos (a planned per-minute platform fee was postponed
indefinitely in 2026). Gitea Actions on the `origin` remote is the other
free path. Neither is set up; both are open if 3-OS verification ever
matters again.

`scripts/docs-gate.sh` mirrors `docs.yml` (doc samples, plugin-SDK
examples, profile validation, `mdbook build`) — needed because `mdbook
build` had never executed anywhere until then. What CI covered and now
runs NOWHERE, so treat changes in these areas as unverified: **Linux**
(this is a darwin-only shop), the **software-rasterizer GPU path**
(`LIBGL_ALWAYS_SOFTWARE=1` on llvmpipe, vs Metal locally), and the **cc65
deterministic fixture rebuild** of RF-Scroller/RF-Scroller-S/mirror-maps.
