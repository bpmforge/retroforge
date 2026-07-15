# Fixtures — self-contained game content (D-001)

In-repo, built-from-source game fixtures the project demos, gates, and
regresses on. Everything here is ours: code and assets CC0 or MIT (per-dir
LICENSE), built deterministically in CI (cc65/neslib for NES, libSFX for
SNES), ROM hashes checked in.

- `nes/rf-scroller/` — the NES fixture platformer (ticket W2-10): scroll +
  wraparound, HUD IRQ split, sprite-overflow scene, intentional-blink enemy,
  documented decodable level format (`FORMAT.md`).
- `snes/rf-scroller-s/` — SNES sibling, defined at phase-6 entry (W6-00).

Hardware-accuracy oracles (blargg, SingleStepTests, …) are NOT fixtures —
they stay external, fetched by `tests/rom-manifest.toml` (TESTING.md §3).
