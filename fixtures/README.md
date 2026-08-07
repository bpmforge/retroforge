# Fixtures — self-contained game content (D-001)

In-repo, built-from-source game fixtures the project demos, gates, and
regresses on. Everything here is ours: code and assets CC0 or MIT (per-dir
LICENSE), built deterministically in CI (cc65/neslib for NES, libSFX for
SNES), ROM hashes checked in.

- `nes/rf-scroller/` — the NES fixture platformer (ticket W2-10): multi-
  screen horizontal scroll + physical-nametable wraparound, static HUD
  band split from the scrolling playfield (sprite-0 sentinel; NROM has no
  IRQ source), documented decodable level format (`FORMAT.md`). The
  sprite-overflow scene and intentional-blink enemy are **not** part of
  this ticket — see `nes/rf-scroller/FORMAT.md`'s "What this ticket's
  runtime does NOT do" (moved to W2-10a).
- `snes/rf-scroller-s/` — SNES sibling, defined at phase-6 entry (W6-00).

Hardware-accuracy oracles (blargg, SingleStepTests, …) are NOT fixtures —
they stay external, fetched by `tests/rom-manifest.toml` (TESTING.md §3).
