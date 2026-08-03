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

**Wire encoding, normative as of W0-05 (2026-08-02).** The sketch above left
two fields unspecified; the shipped implementation had to choose, and under
NFR-008 those choices are public commitments from first release. All
multi-byte fields are **little-endian**. The header is **uncompressed**; a
single zstd stream (level 3) covers the chunk body only.

| Field | Encoding |
|---|---|
| `magic` | `[u8;4]` = `"RFST"` |
| `container_version` | `u16` — only `1` exists; any other value is refused **in both directions** (older and newer), deliberately |
| `console` | `u8` |
| `flags` | `u16` — must be `0` in v1 |
| `rom_sha256` | `[u8;32]` |
| `emu_version` | `u16` byte-length prefix, then that many UTF-8 bytes; **length capped at 128** |
| `timestamp` | `u64` — Unix seconds |
| chunk | `tag [u8;4]` \| `version u16` \| `len u32` \| `payload[len]` |

**Determinism rule (FR-STATE-002, gated by NFR-001):** `timestamp` and
`emu_version` are metadata ONLY and are **excluded from any state hash**. Two
containers holding identical chunk payloads but different timestamps MUST hash
equal while their encoded bytes differ. The container is byte-deterministic for
a fixed input, which is what makes the golden fixtures meaningful — so the
timestamp is a caller-supplied constructor argument, never read from the clock
inside the writer.

**Robustness (untrusted input):** a declared chunk `len` is bounds-checked
against remaining input *before* allocating, and decompression is capped
(64 MiB in v1) because the wire format carries no decompressed-size field.

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
- Payload encoding: bincode **2** (`Encode`/`Decode` derives — NOT the 1.x
  serde API; see docs/research/rust-stack.md) wrapped by the hand-rolled
  envelope above, so the *container* is stable even if the codec changes.
  Corrected 2026-08-02 (was "bincode 3"): `bincode 3.0.0` is a placeholder
  crate whose entire source is `compile_error!("https://xkcd.com/2347/")` —
  it cannot build. 2.0.1 provides the exact `Encode`/`Decode` derive API this
  design assumes; the design intent is unchanged.

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
