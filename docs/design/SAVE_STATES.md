# Design: Save States and Replay (`rf-state`)

## 1. Requirements

- Deterministic: loading a state and replaying the same input log reproduces
  identical machine state (CI-enforced).
- Versioned: states survive emulator upgrades or fail *loudly* with a reason.
- Layered: core state and enhancement state are independent — an
  Accuracy-mode session can load a state saved in Enhanced mode.
- Fast: rewind (later) needs cheap incremental snapshots.

## 2. Container format (`.rfstate`)

Chunked TLV container, zstd-compressed body:

```
header:  magic "RFST" | container_version u16 | console u8 | flags u16
         rom_sha256 [32] (normalized hash — refuse mismatched ROM)
         emu_version (semver string) | timestamp
chunks:  [ tag: [u8;4] | version: u16 | len: u32 | payload ]*
```

Chunk ownership — each crate serializes/deserializes only its own chunks:

| Tag | Owner | Contents |
|---|---|---|
| `CPU_` `PPU_` `APU_` `WRAM` `VRAM` `OAM_` `CGRM` `MAPR` `CART` | core crates | full machine state (registers, counters mid-frame invariant: states are taken at frame boundaries only, which keeps chunks simple and deterministic) |
| `INPT` | rf-input | latch state, connected device kinds |
| `PROF` | rf-profiles | active profile id + revision + enabled mods list |
| `ENHC` | rf-enhance | de-flicker history, scroll tracker, stitched-canvas refs (canvas pixels live in rf-cache, referenced by key — states stay small) |
| `RPLY` | rf-state | input-log cursor for replay-attached states |

Rules:
- Unknown chunk ⇒ skipped with a warning (forward compat for optional
  chunks); missing *required* chunk (core set) ⇒ hard error naming the chunk.
- Chunk `version` bump requires a migration fn or an explicit
  "cannot migrate" error message. Golden `.rfstate` fixtures from each
  release are kept in the test suite; CI loads all of them.
- Payload encoding: bincode 3 (`Encode`/`Decode` derives — NOT the 1.x serde
  API; see docs/research/rust-stack.md) wrapped by the hand-rolled envelope
  above, so the *container* is stable even if the codec changes.

## 3. Replay logs (`.rfreplay`)

TASVideos-style movie: header (rom hash, initial state = power-on | embedded
state, emu version, core config) + per-frame input records + periodic state
hashes every N frames for divergence pinpointing. Used by: determinism CI,
bug repros, TAS-style tooling later. Interop commitment (R-A3, P9): the
format stays BK2-shaped so BizHawk header import (v1) and best-effort export
are cheap — community verification workflows transfer.

## 4. Rewind (Phase 8)

Ring of delta-compressed snapshots (XOR against previous + zstd) every k
frames; rewind replays inputs from nearest snapshot. Enabled only when the
user turns it on (memory cost is real, ~tens of MB for NES, more for SNES).
