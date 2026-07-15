# RetroForge Playbook — ticket execution discipline

Companion to `MASTER_PROMPT.md`. That file says *what governs you*; this one
says *how a ticket flows*. Ticket board: `plan.json` (schema note at top).

## Phase order and gates

Phases follow `docs/ROADMAP.md`. **Gate rule**: no ticket from phase N+1
starts until the phase-N exit criteria in ROADMAP.md pass and are recorded in
`docs/STATUS.md`. Exception: W0-* infra tickets are always claimable.

Current claimable set at repo creation: `W0-02`, `W0-04` (then W0-03, W0-05).

## Per-ticket loop (the micro-loop)

```
claim → read design doc(s) → verify external APIs → write failing test(s)
     → implement → acceptance green → full gate green → commit → close
```

Full gate = `cargo fmt --check && cargo clippy --workspace -- -D warnings
&& cargo test --workspace && scripts/validate-arch.sh &&
node scripts/validate-plan.mjs && node scripts/validate-traceability.mjs`
(board validators land with the 2026-07 review arc, D-002).

- **Tests first where the ticket is testable-first** (CPU vectors, parsers,
  containers, decoders). For UI tickets, acceptance is a manual checklist in
  the commit body plus any headless assertions possible.
- **Micro-commits**: commit at each stable point. Never leave the tree red
  overnight.
- **Bench-sensitive tickets** (CPU/PPU inner loops): add/refresh a criterion
  benchmark; a >10% regression on existing benches blocks close.

## Testing tiers (from docs/TESTING.md)

| Tier | Runs | Gate |
|---|---|---|
| Unit + JSON vectors | every commit (CI) | must pass |
| Test-ROM integration (rf-harness) | every commit once W0-03 lands | must pass |
| Golden frames | every commit (headless wgpu) | must pass |
| Determinism/replay | every commit | must pass |
| Mode-invariant (accuracy==enhanced state hashes) | every commit from W4-01 | must pass |
| 5-min soak / perf benches | nightly or pre-phase-gate | regression blocks gate |

## Git

- Branch per ticket: `feat/<ticket-id>-<slug>` off `main`; merge back with
  `--no-ff` after gates pass. Small doc-only fixes may go straight to main.
- Push after every merged ticket: `git push origin main && git push github main`
  (origin = Gitea, may be offline off-LAN — push github always; sync origin
  when reachable).
- Commit trailer: `Co-Authored-By:` line naming your model.

## Status reporting

After each merged ticket append one line to `docs/STATUS.md`:
`2026-07-06 W0-02 done — <one-line result, test counts>`. At phase gates,
write a short gate section (criteria → evidence). This file is how humans
resume the project cold.

## Wave-gate coverage loop (Ralph) + challenger (D-002)

At each phase gate, before the STATUS gate section lands:

1. **INVENTORY** — enumerate the phase's tickets + every FR/NFR/story they
   cite; `node scripts/validate-traceability.mjs` prints the map.
2. **VERIFY (objective, never vibes)** — run ALL validators
   (`validate-plan.mjs`, `validate-traceability.mjs`, `validate-arch.sh`,
   full cargo gate) plus the phase's ROADMAP exit criteria plus TESTING.md's
   suite tables for the phase.
3. **GAP** — each uncovered row gets ONE focused `HANDOFF:` note in its
   ticket's `notes`. Fix only flagged rows; never re-run the whole phase.
4. **Repeat, cap 3.** Byte-identical gap set two iterations running =
   no progress → halt and escalate to the user. Never loop past the cap.

**CHALLENGER (before the gate entry lands):** a fresh session/agent — never
one that implemented a ticket in this phase (maker ≠ verifier) — re-derives
each exit criterion from the docs and tries to REFUTE the evidence. Every
accepted criterion records `re-ran independently: <command — counts — exit
code>`. CONTRADICTED evidence reopens the ticket. UNVERIFIABLE criteria are
listed in the gate entry, never waived silently.

## Block-note discipline (D-003)

`blocked` requires a note in the ticket's `notes` array with ALL of: root
cause · exact fix needed · why workarounds fail · "do not retry without X".
Blocked-with-evidence is a SUCCESS state of the discipline. Each session on
a ticket also appends one `attempt: <date> <model> <result>` line; a ticket
blocked twice parks for a human — no third unattended attempt.

## Model & run policy (D-003, applies to unattended builds)

- **Sonnet floor**; small models only for ≤1-pt tickets (the board has
  none). No auto-frontier: a ticket that defeats the floor model parks
  blocked-with-evidence for the morning queue.
- ONE build conductor account-wide, ever. Persist evidence BEFORE mutating
  ticket status. Crash cleanup must never delete `blocked/*` branches.
- Per-ticket close gate includes the board validators (see gate below) —
  a plan edit that cites a nonexistent doc path fails mechanically.

## Refusal conditions (stop and ask the user)

- A ticket requires distributing ROM data or circumventing copy protection.
- Acceptance criteria conflict with the determinism invariant or layer rules.
- You need a dependency not in `docs/TECH_STACK.md` (propose it in notes
  first).
