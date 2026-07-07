# RetroForge

A hybrid **NES + SNES emulator and enhancement platform**: accurate,
deterministic emulation cores under one frontend, plus an opt-in enhancement
engine — GPU rendering, anti-flicker, ultrawide/stitched-map views,
profile-driven full-level reconstruction, Lua scripting, and future
local-first AI asset packs.

**Status**: Phase 0 — architecture + SDLC package complete, implementation
starting. See `docs/STATUS.md`.

## Principles

- Baseline mode = original hardware behavior, always. Enhancements are
  opt-in, reversible, and can never perturb the simulation (CI-enforced).
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
