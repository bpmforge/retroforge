# RetroForge

A hybrid **NES + SNES emulator and enhancement platform**: accurate,
deterministic emulation cores under one frontend, plus an opt-in enhancement
engine — GPU rendering, anti-flicker, ultrawide/stitched-map views,
profile-driven full-level reconstruction, Lua scripting, and future
local-first AI asset packs.

**Status** (2026-08-30): every ticket on the board that can be worked
without a ruling is closed — **157 of 161 done, 4 blocked**, none of them
blocked on effort. Phases 0-6 have all their tickets closed (only Phase 1
has a *recorded exit gate*, 2026-08-06); Phase 7-9 work has landed
ticket-by-ticket. All seven VISION §2 enhancement promises are reachable
in the running app. **Every green claim in this repo rests on the local
gate on one darwin machine** — hosted CI has not run since ~2026-08-07 and,
by ruling, will not run again (`CLAUDE.md` → Build). See `docs/STATUS.md`.

## Principles

- Baseline mode = original hardware behavior, always. Enhancements are
  opt-in, reversible, and can never perturb the simulation — enforced by
  the `mode_invariant_5k` / `mode_invariant_corpus` tests in the local
  gate, not by CI (see `docs/TESTING.md` §0).
- Honesty about limits: generic tricks (scaling, de-flicker, wideNES-style
  map stitching) work everywhere; full-level/widescreen reconstruction
  requires per-game profiles. See docs/ARCHITECTURE.md §2.
- Bring your own ROMs. No ROM data lives in or ships with this repo.

## Getting started (contributors / coding agents)

```
cargo test --workspace        # builds + tests all 16 crates
```

Read `MASTER_PROMPT.md` → `plan.json` → `PLAYBOOK.md`. Architecture:
`docs/ARCHITECTURE.md`. Research basis (verified July 2026): `docs/research/`.

## License

MIT OR Apache-2.0 (code). Game profiles under `/profiles` are facts +
citations, same license. No third-party ROM content.
