# HANDOFF — resume point for the next session (written 2026-07-15)

For a fresh coding session (any model). The design-review arc is DONE and
merged to main. **Your job is implementation: execute tickets from
plan.json, one at a time.** Do not redesign anything.

## Read in this order (skim, don't study)

1. `CLAUDE.md` — the laws (short, mandatory)
2. `MASTER_PROMPT.md` — session protocol incl. the **always-writable set**
3. `plan.json` — the board (64 tickets; schema note at top is authoritative)
4. `PLAYBOOK.md` — per-ticket loop, gate command, block-note discipline

## Current state (verified 2026-07-15, commit 92cc3ce on main)

- Workspace: 16 skeleton crates, no implementation yet. Only W0-01 is done.
- Full gate GREEN: `cargo fmt --check && cargo clippy --workspace -- -D warnings && cargo test --workspace && scripts/validate-arch.sh && node scripts/validate-plan.mjs && node scripts/validate-traceability.mjs`
- Toolchain pinned: Rust **1.94** (rust-toolchain.toml — never change to "stable").
- Board: 64 tickets / 366 pts · validators green · zero dangling refs.

## START HERE

**Claimable now: `W0-02` (rf-cart header parsing/hashing) and `W0-04`
(rf-core-api traits).** Claim = set status `in_progress` in plan.json +
commit that change first. Recommended order: W0-02, then W0-04, then
W0-03, W0-05.

## Rules a cheap model must not improvise around

1. ONE ticket per session. Stay inside `write_scope` + the always-writable
   set (plan.json own status/notes · docs/STATUS.md append · Cargo.lock ·
   docs/TECH_STACK.md §2 row for any new dependency).
2. Verify EVERY external crate API on docs.rs for the pinned version before
   use. Known traps: **bincode 3** (`Encode`/`Decode` derives — the 1.x
   `serialize` free functions are a build error by policy), wgpu 30,
   egui 0.35 (both newer than most training data).
3. Full gate (command above) must pass before marking a ticket done. Then
   append one line to docs/STATUS.md and commit as `feat(W0-02): <summary>`.
4. Blocked? Write a `notes` entry (array of strings) with: root cause ·
   exact fix · why workarounds fail · "do not retry without X". Append an
   `attempt:` line each session. Two blocked attempts = stop, leave for
   human. Blocked-with-evidence is success, not failure.
5. Determinism invariant: no wall-clock/RNG/floats in core crates
   (validate-arch.sh greps for it). Cores emit indexed pixels, never RGB.
6. No ROM bytes in git. Test ROMs come via tests/rom-manifest.toml (W0-03).
7. Push after merged work: `git push origin main && git push github main`
   (origin/Gitea may be offline off-LAN — GitHub always; note it in
   STATUS.md if origin unreachable).
8. Tickets W5-04 and W5-05 have `hold: true` — humans only, never claim.

## Context you don't need to re-derive (it's all threaded)

- Decisions D-000..D-007: `docs/DECISIONS.md` (fixture doctrine, trust
  ladder, containment, etc. — already reflected in ticket acceptance).
- Why any choice was made: `docs/ADRS.md` (16 rows with Rejected:).
- Failure handling per subsystem: `docs/design/FAILURE_MODES.md`.
- Human-only prerequisites: `docs/PREREQUISITES.md` (HP-4..7 open; none
  block W0-W2 work).
- Full review history: docs/work/ (DESIGN_REVIEW, IMPROVEMENT_
  RECOMMENDATIONS, READINESS) — background only, not needed to execute.

## If something goes wrong

Gate red after your change → fix or revert before anything else. Validator
red after a plan.json edit → your edit broke the board contract; read the
violation line, it names the check. New lesson learned → append a row to
`docs/LESSONS.md` (format in the file header).
