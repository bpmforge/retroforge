# RF-Scroller level format (ticket W2-10)

This document describes the on-ROM level data `src/level_data.c` contains
and the runtime mechanics `src/main.c` builds on top of it. It is written
so a future decoder (the `metatile_screens` family, `docs/design/
GAME_PROFILES.md` §`[decode]`, ticket **W5-02a**, which does not exist
yet) can be pointed at this fixture and decode it, **without** this
ticket having implemented that decoder itself. `src/main.c`'s own reader
(`decode_column()` + `stream_chunk()`) is a from-scratch, RF-Scroller-only
reader, not an instance of `metatile_screens` — the two are designed to
agree on shape, not to share code.

## Source of truth

- Tables: `src/level_data.c` (`metatile_table`, `collision_table`,
  `area_palette`, `sprite_palette`, `level_column_offset`,
  `level_rle_data`).
- Constants: `src/leveldata.h` (`METATILE_COUNT=4`, `SCREEN_ROWS=14`,
  `SCREEN_COLS=48`, `LEVEL_RLE_BYTES=216`).
- CHR layout (which tile IDs mean what pixels): `src/chr.s`'s module doc.

## Metatile table

`metatile_table[METATILE_COUNT][4]`, `METATILE_COUNT = 4`. Each row is
one metatile's four background tile IDs in `{TL, TR, BL, BR}` order,
referencing `chr.s`'s shared pattern table (tiles `$00`-`$0E`; see that
file for why BG and sprites share one physical CHR table on this
toolchain). In `metatile_screens` terms this is `decode.metatile = {
size = 4, ... }` — a flat 2x2-tile-ID array per metatile, no run-length
or indirection inside a metatile.

```
0: sky            {0x00, 0x00, 0x00, 0x00}   all-transparent
1: ground         {0x01, 0x01, 0x02, 0x02}   flat top row / flat fill row
2: landmark A     {0x03, 0x04, 0x05, 0x06}   diamond outline quadrants
3: landmark B     {0x07, 0x08, 0x09, 0x0A}   checkerboard quadrants
```

## Collision table

`collision_table[METATILE_COUNT]`, one byte per metatile: `bit0=solid,
bit1=platform, bit2=hazard` (matches `GAME_PROFILES.md`'s
`collision.bits = "solid,platform,hazard"`). Values: sky `0x00`, ground/
landmark A/landmark B all `0x01` (solid).

**This ticket's own runtime does not consult this table.** Player-vs-
world blocking in `main.c` is a single hardcoded world-X bound
(`MAX_PLAYER_X`), not a per-metatile lookup — see "What this ticket's
runtime does NOT do" below. The table is populated and correct so a real
decoder has something to read; RF-Scroller just doesn't need it yet
because there is no jump/fall mechanic in this ticket's scope.

## Palette table

`area_palette[4]`: `[backdrop, index1, index2, index3]` — one
background-palette entry, `{0x0F, 0x00, 0x10, 0x30}`. This level has
exactly one area, so this is the degenerate (area-count = 1) case of
`GAME_PROFILES.md`'s `palettes = { ..., per_area = true }` shape, not a
deviation from it — a real multi-area game would have one 4-byte row per
area here.

`sprite_palette[4]` (`{0x0F, 0x16, 0x21, 0x30}`) is **not** part of the
level format proper — `GAME_PROFILES.md`'s `[decode].palettes` documents
only the background/area table above. Sprite palette is fixed hardware
state this fixture just hardcodes alongside it.

## Column-RLE encoding

`screens = { width = 48, height = 14, order = "column_rle" }` in
`GAME_PROFILES.md` terms. The level is `SCREEN_COLS=48` metatile columns
wide by `SCREEN_ROWS=14` metatile rows tall. Each column is stored as a
run-length-encoded sequence of `(run, metatile_id)` byte pairs, packed
tightly (not fixed-stride) into one flat byte array:

- `level_column_offset[SCREEN_COLS]`: `level_column_offset[c]` is the
  byte offset into `level_rle_data` where metatile-column `c`'s RLE
  pairs begin. Column `c`'s data ends where the next column's begins
  (`level_column_offset[c+1]`, or `LEVEL_RLE_BYTES` for the last
  column).
- `level_rle_data[LEVEL_RLE_BYTES]` (216 bytes): the packed pairs
  themselves. Each pair is `(run: u8, metatile_id: u8)`. A column is
  fully decoded once the sum of its runs reaches exactly
  `SCREEN_ROWS=14`; rows are emitted top-to-bottom.

Invariant a decoder MUST be able to rely on: **every column's runs sum to
exactly `SCREEN_ROWS`.** This was verified offline (by the small
generation script that produced the table, not checked into the ROM) and
is trusted, not re-verified, at runtime — `decode_column()` in `main.c`
has no bounds/sum check and would silently mis-decode a malformed table.
A shipped `metatile_screens` decoder should decide for itself whether to
add that defense; this fixture's data satisfies the invariant by
construction.

Level content (for reference, not load-bearing beyond what the tables
say): sky above, a solid ground row at the bottom (`SCREEN_ROWS-1`, i.e.
metatile row 13), and a landmark (alternating A/B every 4th column) one
row above the ground. This is deliberately simple, geometric content —
see `chr.s`'s module doc for why ("shapes, not art").

## Raw tile columns vs. metatile columns

The PPU nametable is tile-addressed, not metatile-addressed. A **raw
tile column** is one nametable column (8px wide); a **metatile column**
is two raw tile columns wide (16px). `raw_col = mc*2 + side` where `mc`
is the metatile column and `side` (0=left, 1=right) selects which half
of each metatile's 2x2 tile block to emit: `side=0` emits `{TL, BL}`
top-to-bottom, `side=1` emits `{TR, BR}`. The level is 48 metatile
columns wide, i.e. **96 raw tile columns** (raw column indices 0-95).

## Streaming writes and physical VRAM wraparound

The playfield uses **vertical PPU mirroring** (two physically distinct
nametables side by side horizontally, NOT the NES's other mirroring
mode — do not "fix" this to horizontal mirroring; it is required for a
2-nametable-wide horizontally-scrolling playfield). Nametable 0 covers
raw tile columns 0-31, nametable 1 covers columns 32-63 (64 raw columns
= 512px = the game's `MAX_CAMERA_X`, i.e. two full nametables' worth of
initial content, preloaded once at startup by `init_video()`).

Once the camera advances far enough that a **third** screen's worth of
content is needed (raw column 64+), that content must live in one of the
same two physical nametables — there is no third one. `main.c` computes
`physical_col = raw_col & 0x3F` (mod 64) and writes there, **overwriting
whatever raw content used to occupy that physical column** (which, by
construction, is far enough behind the camera to already be off-screen).
`columns_streamed` (a documented RAM address, see below) tracks the
highest raw column known-correct in VRAM right now; it can only exceed
63 once a physical nametable has genuinely been reused for new content —
this is this fixture's wraparound acceptance criterion, and a replay
that lets `columns_streamed` reach 95 (the level's last raw column) is
direct evidence wraparound occurred, not merely that scrolling happened.

VRAM writes for one raw tile column touch 28 bytes: the nametable's rows
2-29 (rows 0-1 are the fixed HUD, never touched after `init_video()`),
one byte per tile row, written via `$2007` relying on PPUCTRL's +32
auto-increment (so 28 back-to-back writes walk straight down the column
without re-issuing `$2006` per byte).

### Per-frame budget: why streaming is chunked, not done in one frame

Decoding and blitting an entire 14-metatile-row column in a single
frame's vblank-safe window (the design this ticket started with) was
measured to overrun that window by 3-8x under cc65's own codegen (see
"Known defects" below for the full story) — no piece of that work
(RLE decode, metatile-table expansion, or the 28-write blit) was cheap
enough alone to fit, even under `cl65 -O`. The shipped design
(`stream_chunk()` in `main.c`) instead processes `STREAM_CHUNK_ROWS=2`
metatile rows per frame, resuming a persistent RLE-run cursor across
calls, so one column takes 7 frames to fully stream. Player movement is
paced (8px every 8 frames) specifically so that a new raw column is
never demanded faster than this 2-rows/frame supply can deliver
(demand: 1.75 rows/frame; supply: 2 rows/frame) — **this movement speed
is a budget-derived constant, not a pacing choice**; see `main.c`'s
`main_loop()` comment for the arithmetic.

A decoder outside this specific 6502/cc65 budget (i.e. any host-side
decoder, or `metatile_screens` running on a faster core) has no reason to
chunk at all — chunking is an artifact of this fixture's own runtime
being 6502 assembly-adjacent C, not a property of the level format
itself.

## HUD/playfield split

A static two-row HUD occupies nametable rows 0-1 of **both** physical
nametables (written once by `init_video()`, never touched again) and is
kept locked to a fixed on-screen position (X=0, current nametable's
row 0) every frame via a mid-frame scroll-register rewrite, while the
playfield below scrolls freely with the camera. This is the classic
Super Mario Bros. status-bar technique: `PPUCTRL`'s nametable-select bit
and `PPUSCROLL`'s X are rewritten mid-frame at the boundary between HUD
and playfield (scanline ~15), relying on the PPU's own automatic
`hori(v)=hori(t)` copy at dot 257 of every scanline to apply the new
horizontal position starting the next scanline, without perturbing
vertical scroll at all.

**Deviation from the acceptance criterion's literal wording, recorded
here as instructed:** the split's *timing* is a calibrated fixed delay
(`SPLIT_DELAY` in `main.c`), not a `PPUSTATUS` bit-6 (sprite-0-hit) poll.
Sprite 0 (OAM slot 0, a fully-opaque sentinel tile at the last HUD
scanline) is still present, still renders, and still genuinely collides
with HUD row 1 every single frame — the hardware fact this ticket's
brief asked for (NROM/mapper 0 has no IRQ source, so this is not an
MMC3-style IRQ split; sprite-0-hit is the only mid-frame synchronization
primitive NROM has) is real and was directly confirmed via `PPUSTATUS`
bit 6 during development. What changed is that the *code* no longer
polls that bit to decide *when* to write the split registers; it waits a
measured number of cycles instead. See "Known defects" for the
measurements that led to this and its consequence.

## Documented RAM addresses

Declared in this fixed order in `main.c` so a rebuild from unchanged
source assigns the same linker addresses again (verified: rebuilding in
place and rebuilding from a fresh directory copy both produce the
byte-identical ROM, hash below).

| Symbol | Address | Type | Meaning |
|---|---|---|---|
| `player_x` | `0x6029` | `unsigned int` (2 bytes) | World X, pixels, `0..MAX_PLAYER_X` (752) |
| `camera_x` | `0x602B` | `unsigned int` (2 bytes) | Screen-left world X, pixels, `0..512` |
| `columns_streamed` | `0x602D` | `unsigned char` | Highest raw tile-column (0-95) known-correct in VRAM right now — the wraparound witness |
| `frame_counter` | `0x602E` | `unsigned char` (static) | Increments once per `main_loop()` iteration, unconditionally |

## What this ticket's runtime does NOT do

Explicitly out of scope (moved to **W2-10a** per the ticket's own notes):

- No collision-table-driven blocking (world bound is a hardcoded
  world-X constant, not a per-metatile lookup — see "Collision table").
- No jump, no vertical movement, no vertical sub-area.
- No Left (D-pad Right only).
- No >8-sprites-per-scanline scene (anti-flicker/sprite-overflow
  goldens).
- No intentionally-blinking enemy.
- No enemies, hazards, or any entity beyond the player and the sprite-0
  sentinel.

## Known defects (read before building goldens or briefing W2-10a)

Root-caused via per-write PPU cycle timestamps taken directly from
`rf-nes`'s `CoreEvent::ScrollWrite` stream (not inferred): the original
single-frame "decode + expand + blit a whole 14-row column" design
overran the vblank+split cycle budget by roughly 20,000+ cycles on every
streaming frame — multiple single-cause theories (an unbounded
sprite-0-hit poll, a function-call boundary) were tested and each
individually disproven before the real cause (the column-streaming
work's own cost, spread across three sub-parts none of which was cheap
enough alone) was isolated by systematically stubbing each sub-part in
turn. Fixed by `stream_chunk()`'s multi-frame chunking (above).

**CORRECTED 2026-08-07 by conductor measurement — the residual split-timing
defect described here previously DOES NOT EXIST in the shipped build.** An
earlier draft of this section reported that on streaming-active frames the
HUD/playfield split lands at scanline ~32-37 instead of the intended 14-18.
Measured directly against the shipped ROM
(`sha256=aa3b08c2ee7b0d9213203e15ae555ced009215121f28b52d4d6ee460a30b96f0`)
over 900 frames of held-Right — the same input the acceptance test drives —
by tracking `CoreEvent::Scanline` and `CoreEvent::ScrollWrite` through
rf-nes's single ordered event FIFO:

```
1682 mid-frame scroll writes over 900 frames
in target window (14..=18): 1682  (100.0%)
  scanline 16 -> 916
  scanline 17 -> 766
writes later than scanline 18: 0
frames with no mid-frame split: 59  (frames 0-58, pre-render-enable warm-up)
```

Every split lands in-window; none is late. The earlier figures were almost
certainly taken from a build predating `SPLIT_DELAY`'s final calibration to
12 and were not re-measured after it. **A fixture exists to be a trustworthy
regression target, so a documented defect it does not have is as harmful as
an undocumented one it does** — downstream tickets (W4-03d, W3-05, W5-01)
would have designed around a phantom.

**What IS true, and matters for W2-10a:** the split lands correctly because
the per-frame streaming work happens BEFORE it and already costs enough to
carry execution to scanline ~16 on its own; `SPLIT_DELAY=12` is a small
trim on top, not the thing doing the work. That makes the timing **coupled
to how much work precedes it**. W2-10a adds three scenes and therefore
changes that cost, so it MUST re-measure this distribution rather than
assume it holds.

**A second observation from the same investigation, previously recorded as
unexplained and suspected in rf-nes — now EXPLAINED, and it is not a core
bug.** The earlier code polled `PPUSTATUS` for the sprite-0-hit bit and
found that the loop's own read showed bit 6 set (exiting immediately) while
the very next read of the same register showed it clear. That is correct
hardware behaviour: the PPU clears sprite-0 hit at **dot 1 of the
pre-render line**, which occurs AFTER vblank ends — so during the vblank
window this code runs in, bit 6 is still set from the PREVIOUS frame's hit.
A bare "wait until set" exits instantly on that stale flag; a later read,
now past pre-render, reads clear. rf-nes implements exactly this
(`crates/rf-nes/src/ppu/mod.rs`, the `PRERENDER_SCANLINE` arm clears
`STATUS_SPRITE0_HIT` at `dot == 1`), and its `sprite_hit_tests` canary
passes 11/11. SMB1 uses a **two-phase** wait — first for bit 6 to go clear,
then for it to go set — for precisely this reason.

**But two-phase polling is ALSO wrong for this fixture's current structure,
which was verified rather than assumed.** A two-phase poll was implemented
and measured: it tightened the split (1631/1678 writes at scanline 15,
still 100% in-window) but cost so much throughput that 900 frames of Right
moved the player only to `player_x == 528` instead of 752, failing the
anti-vacuity acceptance test. The cause is structural: because streaming
work runs first, execution ARRIVES at the split point AFTER this frame's
sprite-0 hit has already fired, so phase 1's "wait for clear" blocks until
the NEXT pre-render — burning a whole frame. Making the split genuinely
sprite-0-driven therefore requires moving the split BEFORE the streaming
work, which is a restructuring deliberately not attempted here.

**Third observation, smaller and separate from the streaming-correlated
one above:** even on frames doing no streaming work at all, roughly 1 in
10 (measured: 24 of 250 sampled idle-phase frames, after
`columns_streamed` reaches 95 and streaming has fully stopped) land the
TOP-OF-FRAME scroll-reset write (not the split) at scanline 0 rather
than safely inside vblank -- a smaller, `SPLIT_DELAY`-independent
baseline-timing characteristic that was not further investigated once
its magnitude (one scanline, at the very top of the fixed HUD, likely
within TV overscan) was established as clearly smaller than the
already-documented and already-shipped split-timing defect. Separately,
extremely rarely (measured: 7 times across ~900 post-init frames, i.e.
well under 1%), one `main_loop()` iteration takes slightly more than one
real hardware frame's worth of cycles to complete, detected via
`frame_counter` (a debug-only counter added for this investigation, not
part of the shipped RAM layout beyond what's in the table above) failing
to increment exactly once per real PPU frame. Neither of these was
chased further, both are strictly smaller in scanline-magnitude and
frequency than the primary residual defect above, and the golden frames
this fixture ships (`crates/rf-harness/tests/rf_scroller_replay.rs`)
were individually verified clean of all three, not merely assumed clean
because streaming had stopped.

**Toolchain provenance note:** `rom.sha256` was generated by exactly one
`build.sh` invocation on the machine and cc65 version noted in the
ticket report. `build.sh` itself never loosens a hash mismatch to "fix"
it — a mismatch on a different cc65 build means the toolchain, not the
source, is suspect; pin/verify the toolchain version before assuming the
source regressed.
