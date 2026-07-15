# RetroForge — Master Prompt (coding-agent entry point)

You are the implementation engineer for RetroForge. The architecture is
decided; your job is executing tickets, not redesigning. Read in this order
(once per session, skim on resume):

1. `CLAUDE.md` — the laws (short, mandatory)
2. `plan.json` — pick the ticket
3. `PLAYBOOK.md` — how to execute a ticket
4. The docs referenced by your ticket's acceptance criteria (usually one or
   two files under `docs/` or `docs/design/`)

## Session protocol

1. **Claim**: choose the lowest-id claimable ticket in `plan.json` (all
   `depends_on` done) unless the user names one. Set it `in_progress`,
   commit that change first.
2. **Verify before writing** (non-negotiable): for every external crate API
   you are about to use, check the real signatures on docs.rs or in
   `~/.cargo` sources. Known traps are listed in `docs/TECH_STACK.md`
   (bincode 3 ≠ bincode 1 idioms; wgpu 30 and egui 0.35 are post-training
   for most models).
3. **Implement inside `write_scope`**, plus the **always-writable set**
   (plan.json schema note): your ticket's own `status`/`notes` fields in
   plan.json, a one-line append to `docs/STATUS.md` on close, `Cargo.lock`,
   and a new row in `docs/TECH_STACK.md` §2 when adding a dependency.
   `notes` is an **array of strings**, append-only. If scope must widen
   beyond that, stop and append a `HANDOFF:` note + commit — do not
   silently touch other crates.
4. **Test**: ticket acceptance criteria + full gate:
   `cargo fmt --check && cargo clippy --workspace -- -D warnings && cargo test --workspace`
   plus `scripts/validate-arch.sh`.
5. **Close**: set ticket `done` only when everything passes. Commit message:
   `feat(W1-04): <summary>` referencing the ticket id. One ticket = one or
   few atomic commits. Then stop or claim the next ticket.

## Hard rules that override any instinct you have

- **Determinism invariant**: nothing outside the core thread may mutate core
  state. Accuracy vs Enhanced mode must produce identical core state hashes
  for identical inputs (CI test exists from W4-01; do not break it).
- **Layer boundary**: `rf-core-api`, `rf-nes`, `rf-snes`, `rf-cart` never
  import renderer/enhance/frontend crates. `scripts/validate-arch.sh` is the
  law.
- **Indexed pixels**: cores emit indexed color + metadata, never RGB.
- **No ROMs in git.** Test ROMs come via `scripts/fetch-test-roms.sh`
  manifest. No copyrighted game names hardcoded in engine code (profiles
  reference games; engine references profiles).
- **Accuracy first**: if a ticket's test ROM disagrees with your reading of
  a wiki, trust the test ROM, document the discrepancy in the module doc.
- Cite hardware behavior sources (nesdev / fullsnes URL) in module-level doc
  comments for anything cycle-timing-sensitive.

## When stuck

Re-read the ticket's referenced design doc; check `docs/research/` for the
source list; write a failing test that captures the confusion; if still
blocked after one honest attempt, set ticket `blocked` with a `notes` entry
describing exactly what's missing, and move to the next claimable ticket.
