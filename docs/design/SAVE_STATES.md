# Design: Save States and Replay (`rf-state`)

## 1. Requirements

- Deterministic: loading a state and replaying the same input log reproduces
  identical machine state — enforced by the `determinism` and `save_state`
  suites in the local gate, not by CI (`docs/TESTING.md` §0).
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
| `CPU_` `PPU_` `APU_` `WRAM` `VRAM` `OAM_` `CGRM` `MAPR` `CART` | core crates | full machine state (registers, counters mid-frame invariant: states are taken at frame boundaries only, which keeps chunks simple and deterministic). A NES "frame boundary" is after the instruction that crosses the pre-render → scanline 0 wrap, so up to ~21 dots of scanline 0 are already drawn; `PPU_` v4 carries that partial line (W2-22) |
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

**Wire encoding, normative as of W1-07 (2026-08-05).** Unlike `.rfstate`,
`.rfreplay` is a single **UTF-8 text file**, not a binary TLV container —
text keeps a replay diffable and hand-editable for TAS/bug repro, needs no
new dependency, and keeps the BizHawk-header-import commitment above cheap
(`docs/research/accuracy-and-testing.md` §5 calls BK2's `Input Log.txt`
shape, header key=value block + first-line log key + one line per frame,
"excellent model for our input-log format"). Shape:

```
RFREPLAY 1
[Header]
console=nes
rom_sha256=<64 lowercase hex>
emu_version=<semver>
core_config=accuracy
start_type=power-on
hash_kind=full-v2
hash_interval=<u64>
[LogKey]
P1:A,B,Select,Start,Up,Down,Left,Right
P2:A,B,Select,Start,Up,Down,Left,Right
[Input]
|........|........|
|A..S....|........|
[Hashes]
59=<64 lowercase hex>
119=<64 lowercase hex>
```

| Field | Encoding |
|---|---|
| Line endings | LF only — a CRLF file is refused outright, not silently normalized |
| Character set | ASCII only — non-ASCII content is refused |
| `[Header]` | `key=value` lines, one per field, all seven required; an unrecognized key is refused (v1 has no forward-compat skip-unknown for `[Header]`, unlike `.rfstate`'s chunk-level "unknown ⇒ skip with a warning" — a BizHawk-header-import reader inherits a header field this crate doesn't know and must decide explicitly, not silently drop it) |
| `rom_sha256` | 64 lowercase hex chars — same normalized-hash convention `.rfstate` uses (§2); mismatch against the loaded ROM is refused |
| `start_type` | only `power-on` is accepted as of W1-07; any other value (e.g. a future savestate-anchored start) is refused with a "not supported until W2-04" message |
| `hash_kind` | names *what* was hashed. **`full-v2` as of W2-22** (the `PPU_` payload gained the partly drawn scanline; `full-v1`, W2-04, is the same set minus it): every `rf_nes::StateRegion` — CPU + bus counters, the whole PPU (VRAM, palette, OAM, loopy registers, sprite units), the whole APU, WRAM, mapper registers, battery PRG-RAM (`crates/retroforge/src/save_state.rs`'s `HASH_KIND`, `EmuStepper::state_hash`). `reachable-v1` is the **historical** value written before `rf-nes` had state serialization, when PPU/APU internals were structurally unreachable; it still names that narrower hash and must not be reused for the wider one — the upgrade this row anticipated was taken as a NEW value, exactly as required. `crates/rf-harness`'s two replay-corpus tests still compute `reachable-v1` from their own independent reimplementation and are unaffected |
| `[LogKey]` | one `P<n>:<button>,<button>,...` line per controller port, declaring both the port count and the per-port button order used by `[Input]` |
| `[Input]` | one `\|...\|...\|` line per frame, 0-indexed by line position; each port group has one char per `[LogKey]` button: `.` = unpressed, else that button's BizHawk-compatible mnemonic (`A`=A, `B`=B, `s`=Select, `S`=Start, `U`=Up, `D`=Down, `L`=Left, `R`=Right) |
| `[Hashes]` | `frame=hex` lines, emitted every `hash_interval` frames **and always for the final frame** |

Sections appear in exactly this order (`[Header]`, `[LogKey]`, `[Input]`,
`[Hashes]`) and all four are required. Refusals are always a diagnostic,
never a panic: unknown magic/version, an unrecognized `[Header]` key,
`rom_sha256` mismatch, unsupported `start_type`, a malformed `[Input]`
line, a port-count mismatch between `[LogKey]` and an `[Input]` line,
CRLF, and non-ASCII content are all rejected with a specific reason
(`rf_input::ReplayError`). Record and
playback both go through one shared "latch input, then advance one frame"
function (`EmuStepper::latch_and_advance_frame`,
`crates/retroforge/src/stepper.rs`) so the two paths cannot silently
diverge in *how* input is applied.

## 4. Rewind (Phase 8)

Ring of delta-compressed snapshots (XOR against previous + zstd) every k
frames; rewind replays inputs from nearest snapshot. Enabled only when the
user turns it on (memory cost is real, ~tens of MB for NES, more for SNES).
