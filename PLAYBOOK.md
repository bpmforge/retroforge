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

## Refusal conditions (stop and ask the user)

- A ticket requires distributing ROM data or circumventing copy protection.
- Acceptance criteria conflict with the determinism invariant or layer rules.
- You need a dependency not in `docs/TECH_STACK.md` (propose it in notes
  first).
