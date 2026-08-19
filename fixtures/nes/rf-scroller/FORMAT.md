# RF-Scroller level format (ticket W2-10, extended by W2-10a)

**W2-10a** ("RF-Scroller red-fixture scenes") added three scenes exercised
only in the level's tail (world X 704-768, `columns_streamed >= 95`): a
12-sprites-on-one-scanline scene intended to force a genuine PPU
sprite-overflow, an intentional-blink enemy, and a vertical sub-area. See
"W2-10a red-fixture scenes" below for what each one is and how it was
verified — **the sprite-overflow scene's placement is verified but its
`$2002`-bit evidence is NOT proven genuine** (mutation testing showed the
measurement doesn't discriminate; see that section and "Known defects").
"Known defects" also has a re-measured split-timing histogram covering
this ticket's own changes, a corrected account of a 2026-08-07 correction
that was itself wrong, and an honestly-documented gap (the vertical
sub-area's `camera_y` has no observable rendered effect).

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
  `SCREEN_COLS=48`, `LEVEL_RLE_BYTES=246` — grew from `216` at W2-10a,
  metatile columns 44-47 replaced with a ladder pattern for the vertical
  sub-area, see "W2-10a red-fixture scenes" below).
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
| `camera_y` | `0x602E` | `unsigned char` (W2-10a) | Vertical sub-area scroll offset, pixels, `0..VERTICAL_MAX` (48). Real, bounded, Up/Down-driven RAM value — but see "W2-10a red-fixture scenes" / "Known defects" for why it has no rendered effect with the current split technique |
| `blink_visible` | `0x602F` | `unsigned char` (W2-10a) | `1` when the intentional-blink enemy is currently drawn, `0` when hidden. Exact formula: `(frame_counter & 0x08) != 0` |
| `frame_counter` | `0x6030` | `unsigned char` (static) | Increments once per `main_loop()` iteration, unconditionally |

### Address re-pin (W2-10a)

`frame_counter` moved from `0x602E` (its W2-10 address) to `0x6030`
because `camera_y` and `blink_visible` — both non-`static` globals, like
`player_x`/`camera_x`/`columns_streamed` above them — were declared ahead
of it in source order. cc65's linker buckets every non-`static` global
together, ahead of file-scope `static`s, **regardless of source
interleaving** — so inserting two new non-`static` globals pushed
`frame_counter` (a `static`) two bytes later even though it was not
itself edited. Confirmed empirically by rebuilding with `cl65 -g -Wl
"--dbgfile,..."` and reading the resulting symbol table, not assumed from
the source diff — **the checked-in `build/rf-scroller.dbg` (if present
locally) is stale and must never be trusted; `build.sh` never regenerates
it.** Any test or tool that hardcodes `0x602E` for `frame_counter` against
a post-W2-10a ROM will silently read `camera_y` instead — both are
plausible-looking small counters, which is exactly what makes this trap
dangerous rather than loud.

## W2-10a red-fixture scenes

All three scenes below are call-site-gated in `main_loop()` behind a
single `columns_streamed >= TAIL_GATE_COL` (95) check — see "Known
defects"' "W2-10a's own re-measurement, and the fix it required" for why
that gate is ONE check, not three, and why that mattered for split
timing. Two of the
three also require `player_x >= VERTICAL_AREA_START_X` (704). The tail
begins once the camera has reached `MAX_CAMERA_X` (512) and streaming has
fully drained (`FORMAT.md`'s own wraparound witness) — reached at some
point within the acceptance tests' scripted 900-frame held-Right session
(confirmed: 239-243 of the last ~250 DMA-anchored iterations land in the
tail, depending on exact build).

### Sprite-overflow scene (criterion 1) — PROVEN via `Ppu::oam()`, not `$2002`

`GEM_COUNT=12` collectible-gem sprites (`update_gems()` in `main.c`), all
sharing one scanline (`GEM_Y=99`, OAM slots `OAM_GEM_BASE_SLOT..+11` =
2-13), tile `0x0E` (reuses the sprite-0 sentinel's tile — no CHR budget
spent). 12 > 8 forces the PPU's real per-scanline sprite limit. `gem_order[]`
(a 12-entry permutation, rotated by one position every `GEM_ROTATE_MASK+1`
= 8 frames) reassigns which LOGICAL gem occupies which PHYSICAL OAM slot;
all 12 logical gems are written into OAM every tail frame regardless of
rotation phase, so ">8 in range" holds unconditionally, not just on lucky
frames — rotation only changes which 8-of-12 the hardware's first-8-by-
slot-order pick actually draws, producing genuine hardware-forced flicker
(not a software visibility toggle) as a side effect.

**Proof** (`crates/rf-harness/tests/rf_scroller_red_fixture_scenes.rs::
more_than_eight_sprites_share_the_gem_scanline_counted_from_ppu_oam`):
counts sprites in range of `GEM_Y`'s scanline directly out of `Ppu::oam()`
— the PPU's own 256-byte array, not the CPU-side shadow, not `$2002` —
sampled during vblank after `OAM_DMA` has certainly completed, across a
full `GEM_ROTATE_MASK` rotation period so it cannot pass on one lucky
frame. Measured: **worst = 12** sprites in range of scanline 100 (the
hardware limit is 8), holding on every one of the 8 sampled frames, not
just the worst one.

**Why `$2002` bit 5 was abandoned as evidence, and why that is correct
emulation, not an rf-nes defect**
(`sprite_overflow_is_genuine_ppu_status_evidence`, kept as a reported,
not-asserted measurement): samples `$2002` bit 5 itself
(`bus.peek(0x2002)`, side-effect-free), edge-triggered on
`CoreEvent::Scanline(GEM_Y + 1)`. Against the real ROM it shows overflow on
~72% of tail samples but ALSO on samples before the tail gate opens, where
gems provably do not exist in OAM yet. Mutation testing (`GEM_COUNT` 12 →
6, below the hardware's 8-sprite limit, where genuine overflow is
impossible) left the measurement almost unchanged — decisive evidence this
isn't a sampling artifact around a real signal. Root cause:
`crates/rf-nes/src/ppu/sprites.rs` implements the AUTHENTIC BUGGY hardware
overflow scan (`m = (m + 1) & 3`, deliberately misreading tile/attribute/X
bytes as Y once eight sprites are already in range) — this is exactly why
the flag fires on garbage on real hardware and why real games never rely
on it for this purpose. `$2002` bit 5 therefore cannot cleanly witness
">8 sprites on one scanline" for anyone, at any threshold; do not open an
rf-nes investigation into this. `PpuPixel::dropped_by_limit` would be the
natural alternative witness, but `sprites.rs` sets it unconditionally
`false` today (its own module doc says so) — populating it is W3-05's
sprite-limit-bypass work.

**The real bug this criterion surfaced, and the fix**: the gems' journey
from `OAM_SHADOW` (correct on every sample, all 12 gems at the right byte
offsets) into `Ppu::oam()` was itself broken — measured at one point
during this ticket's history as only 2 of 12 gems arriving intact, and
after a redesign of `update_gems()`, 0 of 12. `$2003` (OAMADDR) not being
reset to 0 before `$4014` was a real, necessary fix (added — see
`main_loop()`) but not sufficient alone. The actual mechanism: the
gem/blink/vertical-area update block used to run BEFORE `OAM_ADDR=0;
OAM_DMA=0x02;`, and its own cost (a 12-sprite shadow write plus the
periodic `gem_order[]` rotation) pushed the DMA's start late enough on
tail frames that its own duration (`crate::system::NesBus::run_oam_dma`
ticks the PPU in lockstep for the full transfer — ~514 stolen cycles,
~1536 PPU dots, ~4.5 scanlines) straddled a visible or pre-render
scanline's dots 257-320 window, where `sprites.rs` correctly implements
the real hardware's `OAMADDR`-reset-to-0-mid-transfer behavior
(nesdev.org/wiki/PPU_registers: "`OAMADDR` is set to 0 during each of
ticks 257-320 ... of the pre-render and visible scanlines"). A reset
mid-copy restarts the remaining `OAMDATA` writes at OAM offset 0,
scrambling everything written after that point — this is a genuine,
hardware-accurate corruption mode, not an rf-nes bug. **Fix**: moved the
gem/blink/vertical-area update block to run AFTER the split write, at the
very end of `main_loop()`, so nothing new runs before either scroll-write
pair on ANY frame — the DMA's pre-cost is now byte-for-byte identical to
what W2-10 shipped, decoupling the tail scenes' cost from DMA timing
entirely. The one-frame latency this introduces (gem rotation and blink
phase take effect on the FOLLOWING frame's DMA rather than the same one)
is imperceptible for an 8-frame rotation period and a 16-frame blink
period, and `camera_y` never touched OAM/DMA at all.

**Test-methodology traps found and fixed while building the (abandoned)
`$2002` proof**, kept here because the pattern recurred repeatedly this
ticket and is worth a future reader's attention: peeking `$2002` right
after a coarse `run_frame()` call (i.e. right after `bus.frame_count()`
ticks) is USELESS for this measurement — `frame_count` only ticks at the
pre-render scanline's OWN wrap to scanline 0, which happens dots AFTER
that SAME pre-render's dot-1 status clear
(`crates/rf-nes/src/ppu/mod.rs::process_dot`), so by the time
`frame_count()` advances, the very frame that just set overflow has
ALREADY had it cleared again by its own pre-render. Fixed by subscribing
to `EventMask::SCANLINE` and peeking `$2002` the instant
`CoreEvent::Scanline(GEM_Y + 1)` fires. A second bug, found only after
that fix: counting "the most recently observed event value" from OUTSIDE
the `CoreSink`, polled after every `cpu.step()`, treats every CPU step
between two genuine events as a fresh occurrence — massive overcounting.
Fixed by counting strictly INSIDE `CoreSink::event()`, once per genuinely
new event delivered.

### Intentional-blink scene (criterion 2)

A single enemy sprite (OAM slot `OAM_BLINK_SLOT=14`, world position
`(680, 149)`, tile `0x0D` — reuses the player's tile, no CHR budget
spent) that the game itself blinks on a fixed, game-driven period —
invincibility-frame style, which de-flicker (W3-05, FR-ENH-013/D-004)
must learn NOT to erase. Exact formula (`update_blink_enemy()`):
`blink_visible = (frame_counter & BLINK_PERIOD_BIT) != 0` where
`BLINK_PERIOD_BIT = 0x08` — period 16 frames, 8 visible / 8 hidden.
`blink_visible` (RAM `0x602F`) is the documented witness a de-flicker
consumer should assert against directly, not eyeball a rendered frame.

**Proof this is genuine, not merely "disappears sometimes"**
(`rf_scroller_red_fixture_scenes.rs::
blink_visible_is_driven_by_frame_counter_bit_3_not_incidental`): samples
`(frame_counter, blink_visible)` together, 120 times, at genuine
`main_loop()` iteration boundaries (`settle_to_iteration_boundary`, below)
in the tail, and asserts the EXACT formula above holds on every sample —
strictly stronger than counting run lengths (immune to a sampling
artifact, next paragraph), and by construction (a monotonic counter's bit
3 toggles in a perfect period-16 pattern) this single formula check IS the
periodicity proof, not merely correlated with it. Both phases (visible and
hidden) were observed across the 120 samples.

**Sampling-boundary artifact found and accommodated, not chased into the
game code:** `settle_to_iteration_boundary` guarantees `frame_counter`'s
OWN write (near the top of `main_loop()`) has landed, but
`update_blink_enemy()` (called later in the SAME iteration, after
`stream_chunk()` and the other tail-gated calls) can occasionally not
have run yet, if `bus.frame_count()`'s PPU-level tick — which
`run_frame()` waits on — happens to fall in the narrow CPU-cycle window
between the two. This is a sampling-instant artifact, not
non-determinism: `blink_visible` at that instant still deterministically
reflects the PREVIOUS iteration's own (`frame_counter - 1`) computation.
The test accepts a match against EITHER `frame_counter` or
`frame_counter - 1` (15 of 120 samples needed the lagged match) — still
strictly ruling out "incidental" (an uncorrelated value would fail both
checks about as often as it passed either).

### Vertical sub-area (criterion 3) — RAM witness real, rendered effect BLOCKED

Once `player_x >= VERTICAL_AREA_START_X` (704, world X — the level's
last four metatile columns, 44-47, replaced with a ladder pattern:
4 sky / landmark-A row 4 / 4 sky / landmark-B row 9 / 3 sky / ground row
13, each column's runs still summing to `SCREEN_ROWS=14`) AND the tail
gate is open, holding Up/Down adjusts `camera_y` (RAM `0x602E`) within
`[0, VERTICAL_MAX]` (48 px, `update_vertical_area()`). `camera_y` is a
real, bounded, correctly-gated, bidirectional RAM value — proven by
`rf_scroller_red_fixture_scenes.rs::vertical_sub_area_camera_y_ram_witness`
(gate-inactive check: holding Down before the gate opens leaves `camera_y`
at 0; then clamps at `VERTICAL_MAX` under sustained Down, and back to 0
under sustained Up).

**It has NO observable effect on the rendered picture, and this ticket
does not fix that — see "Known defects" for the full account (root
cause, the rendering-verification method that found it, and why fixing
it is out of scope).** Asserting a rendered-pixel effect in the test
above would reintroduce the acceptance criterion's own vacuity trap in
the opposite direction: claiming a working effect that isn't there,
instead of a defect that isn't there. NROM's fixed vertical mirroring
(inherited from W2-10 — two nametables side by side HORIZONTALLY; there
is no four-screen option, and changing mirroring mode is a mapper-level
decision out of scope for a fixture whose whole point is NROM) means this
sub-area could never exceed one nametable's worth (240px) of genuinely
unique vertical content even if the rendering gap were fixed —
`VERTICAL_MAX=48` was sized to stay inside that budget regardless.

## What this ticket's runtime does NOT do

Explicitly out of scope, either by design or as a documented, honest gap:

- No collision-table-driven blocking (world bound is a hardcoded
  world-X constant, not a per-metatile lookup — see "Collision table").
- No jump. Vertical *input handling* exists (the vertical sub-area, W2-10a
  above) but produces no rendered vertical scroll — see "Known defects".
- No Left (D-pad Right only; Up/Down are consulted, W2-10a, but only for
  `camera_y`, never for world-X movement).
- Intentional-blink scene now exists and is proven genuine (W2-10a,
  above) — no longer out of scope. The sprite-overflow scene exists and
  is correctly placed, but its `$2002`-bit evidence is NOT proven genuine
  (see "Sprite-overflow scene" above and "Known defects" below) — treat
  criterion 1 as still open, not closed.
- No enemies, hazards, or any entity beyond the player, the sprite-0
  sentinel, the gems, and the blink enemy.

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

**CORRECTED AGAIN, 2026-08-08, by W2-10a — the 2026-08-07 correction above
was itself measuring the wrong write.** `main_loop()` performs TWO
`$2005`-pair scroll commits per iteration: the top-of-frame write
(`PPU_SCROLL=0; PPU_SCROLL=camera_y;`, vblank-safe) and, after `OAM_DMA`
and `SPLIT_DELAY`, the split write (`PPU_SCROLL=camera_x&0xFF;
PPU_SCROLL=0;`, mid-picture). Each `$2005` write fires its own
`CoreEvent::ScrollWrite`, so a naive "was the write in-window" filter over
the whole `ScrollWrite` stream — value-filtered (`x != 0`) or
undifferentiated ordinal counting — silently mixes the two pairs. Filtering
by VALUE is additionally unreliable: `stream_chunk()`'s own `$2006`/`$2007`
writes go through the SAME `t`/`v` loopy-register machinery `$2005` does,
so a `ScrollWrite` sampled shortly after a `stream_chunk()` call can report
a `y` value that's really `t` mid-stomp from an UNRELATED nametable write,
not a genuine scroll value — flagged again below as its own trap for W3-05
and W4-03d, both of which consume `ScrollWrite`.

The reliable disambiguator is ORDINAL POSITION relative to `DmaStart`, not
value: per iteration, `DmaStart` then exactly 4 `ScrollWrite`s — 1st/2nd
are the top-of-frame pair, 3rd/4th are the split pair
(`crates/rf-harness/tests/rf_scroller_split_timing.rs` implements this).
Re-measured this way, against a rebuild of the pre-W2-10a source (git rev
`a486c6d`), 900 frames of held Right, split into the streaming window
(iterations before `columns_streamed` reaches 95) and the tail:

```
STREAMING WINDOW top-of-frame pair: 1182 writes over 591 iterations
  14->320  15->48  16->11  17->5  18->48  19->16  239->734
STREAMING WINDOW split pair: 1182 writes over 591 iterations
  16->103  17->631  32->320  33->41  34->16  35->7  36->37  37->27
TAIL top-of-frame pair: 474 writes over 238 iterations
  0->41  239->433
TAIL split pair: 474 writes over 238 iterations
  17->428  18->46
```

The 2026-08-07 correction's `1682`/`916@16`/`766@17`/`zero-late` figures
match neither pair's totals above exactly (900 vs 591/238-iteration
denominators differ, methodology differences noted above), but their
SHAPE is unmistakably the TOP-OF-FRAME pair's: scanline 239 (`734` of
`1182`, i.e. genuinely still in vblank — the GOOD case, numerically `>18`
but not "late") is exactly what the correction's own filter would have
silently excluded, leaving a 14-19-only picture that reads as "100% in
14-18". **The split pair itself NEVER lands in vblank on the streaming
window, on this ROM or the pre-W2-10a one: it is bimodal, roughly 60% at
16-17 and the remaining ~40% at 32-37** — the original (pre-2026-08-07)
report was right, and the "correction" was the phantom. The TAIL split
pair on the pre-W2-10a ROM, by contrast, sat cleanly at 17-18 for all 474
samples — the tail had no gated scenes running yet, so this was simply a
lighter-cost region than the streaming window's own 32-37 cluster.

**What IS true, and matters for anything editing this fixture further:**
the split's landing scanline is coupled to how much CPU work happens
before it (`SPLIT_DELAY` is a small fixed trim on top of whatever that
work already cost, not the thing doing the work) — real for BOTH pairs,
and the split pair's own 32-37 cluster during streaming shows this
coupling was ALREADY present, undocumented as such, before W2-10a touched
anything. Any ticket that changes per-iteration cost (this one included)
must re-measure this distribution, not assume it holds.

### W2-10a's own re-measurement, and the fix it required

Re-measuring the same way against the FIRST draft of this ticket's ROM
(before any fix) found the streaming window WAS perturbed, contrary to
the intent recorded in `main.c`'s own (then-inaccurate) comment claiming
the new scenes' call-site gate cost "one cheap byte-compare": both pairs
shifted ~19-20 scanlines later across the board, and — the actually
blocking finding — **62 of the TAIL's 243 split-pair writes (26%) landed
at scanline 239, meaning the split was missed entirely on those frames**
(the HUD's horizontal scroll bled into the whole playfield for the rest
of the visible picture on each one).

Root-caused via four isolation builds (each reverting exactly one piece
of the diff, all against the same tracker): `read_buttons()`'s
shift-accumulate 8-read loop (~11-12 of the ~19-20 scanlines — cc65 `-O0`,
`build.sh` passes no optimization flag, reloads a local accumulator from
the C stack every iteration) and the three tail-gated `update_*()` calls'
redundant per-call `JSR`/`RTS` + gate-check overhead even when each
immediately early-returns (~5-8 scanlines); `camera_y`'s own read/write
was negligible.

**Fix** (both required; neither alone closed the 62/243 miss rate):
`read_buttons()` rewritten as an unrolled 8-read sequence with no loop
counter (still hardware-correct — 8 reads to drain the shift register —
but only 3 conditional ORs instead of an accumulate-every-iteration
pattern); the three `update_*()` calls gated behind ONE shared
`columns_streamed >= TAIL_GATE_COL` check in `main_loop()` instead of
three redundant per-function ones. NOT a reorder of `main_loop()`'s
structure ("move the split before the streaming work" was considered and
rejected: `stream_chunk()`'s `$2006`/`$2007` writes are only PPU-safe
during vblank/forced-blank and would corrupt the picture on real hardware
if deferred past the split) — a narrower, measured fix at the two actual
cost centers.

Re-measured against the fixed ROM (`sha256=642e4d7a7439a4fe73aa08f890c99
aa84e79fc57f1dcf95d0545e507e90bda20`, this ticket's shipped hash), same
900-frame session:

```
STREAMING WINDOW top-of-frame pair: 1180 writes over 590 iterations
  11->272  12->82  13->28  15->64  28->2  239->732
STREAMING WINDOW split pair: 1180 writes over 590 iterations
  13->266  14->466  29->272  30->82  31->28  33->59  34->5  46->2
TAIL top-of-frame pair: 478 writes over 239 iterations
  2->100  3->110  11->3  12->186  13->19  209->12  210->18  219->19  220->11
TAIL split pair: 478 writes over 239 iterations
  20->84  21->126  29->2  30->165  31->41  227->12  228->18  237->18  238->12
```

Streaming-window shape is close to the pre-ticket baseline (bimodal,
~13-15 and ~29-34, vs. the ancestor's ~16-17/32-37 — a few scanlines
earlier, not later, since the rewritten `read_buttons()` is CHEAPER than
even W2-10's original `read_right()`, whose own loop tracked a counter
variable this version doesn't need). **The tail split pair's maximum
observed scanline is 238 — zero missed splits across all 239 tail
iterations**, down from 62/243 before the fix.

**This measurement and its hash are now superseded** — see the next
section for the incident that invalidated them and the fresh numbers that
replaced them; kept here unmodified as the historical record of the FIRST
split-timing fix, which remains correct and is not undone by the second.

### The `git checkout` incident and W2-10a's SECOND fix (the tail-scene reorder)

A conductor-side `git checkout` on `main.c`/`rom.sha256` during verification
(recorded on the ticket in `plan.json`'s W2-10a notes) destroyed the
in-flight `main.c` and forced a rebuild from the surviving backup files
plus a fresh reconstruction of `main.c` itself. That reconstruction is what
surfaced a REAL bug the first pass had shipped without catching: the
gem-scene update block ran BEFORE `OAM_DMA`, and its cost (12-sprite
shadow write plus periodic rotation) pushed the DMA late enough on tail
frames to straddle a visible scanline's dots 257-320 window, where
`sprites.rs` resets `OAMADDR` mid-transfer per real hardware behavior —
see "Sprite-overflow scene (criterion 1)" above for the full mechanism.
The fix moved that whole update block to run AFTER the split write, which
changes per-iteration cost ordering enough to require re-measuring split
timing a second time, against the current hash
(`sha256=<see rom.sha256, updated below>`), same 900-frame session:

```
STREAMING WINDOW top-of-frame pair: 1182 writes over 591 iterations
  11->272  12->84  13->27  14->1  15->58  16->6  239->734
STREAMING WINDOW split pair: 1182 writes over 591 iterations
  13->150  14->584  29->272  30->82  31->27  32->3  33->52  34->12
TAIL top-of-frame pair: 482 writes over 241 iterations
  239->482
TAIL split pair: 480 writes over 241 iterations
  14->443  15->37
```

**The tail split pair is now 100% at scanlines 14-15, zero later than 18,
zero missed** (compare to the first fix's 238-max/zero-missed and the
original pre-W2-10a baseline's own tail region, which sat at 17-18) —
strictly tighter than every prior measurement of this fixture, because the
reorder means the tail scenes contribute literally zero cost before either
scroll-write pair. The streaming window is essentially unchanged from the
first fix's own numbers (bimodal ~13-15/~29-34, same shape, iteration
count 591 vs 590 — a 1-iteration difference attributable to normal
startup-pacing variance, not a regression). These are the numbers now
frozen in `crates/rf-harness/tests/rf_scroller_split_timing.rs`'s
`frozen_streaming_top`/`frozen_streaming_split`/`FROZEN_STREAMING_ITERATIONS`,
superseding the first fix's frozen values.

### Sprite-overflow criterion (1): RESOLVED — proven via `Ppu::oam()`, DMA/OAMADDR bug fixed

Full account is in "Sprite-overflow scene (criterion 1)" above; summarized
here because this section is where a future reader checking "what's
actually broken" looks first, and this WAS broken for most of this
ticket's history before being closed out. Two separate things were wrong,
and both are now fixed/resolved:

1. **`$2002` bit 5 cannot witness this criterion, for anyone, ever** —
   confirmed root cause: `crates/rf-nes/src/ppu/sprites.rs` implements the
   authentic buggy hardware overflow scan, which fires on garbage once 8
   sprites are in range. This is correct emulation of a real chip quirk,
   not an rf-nes defect. The fix was to stop trying to use `$2002` as
   evidence at all, not to sample it differently — criterion 1's actual
   proof is `more_than_eight_sprites_share_the_gem_scanline_counted_from_
   ppu_oam`, which counts directly out of `Ppu::oam()` and never reads
   `$2002`.
2. **The gems genuinely did not reach OAM** (a real fixture bug, not a
   proof-methodology problem) — root cause: `main_loop()`'s tail-scene
   update block ran BEFORE `OAM_DMA`, and its own cost pushed the DMA's
   ~514-cycle transfer late enough on tail frames to straddle a visible
   scanline's dots 257-320 window, where `sprites.rs` correctly resets
   `OAMADDR` to 0 mid-transfer per real hardware behavior, scrambling the
   copy. Fixed by moving that block to run AFTER the split write, so
   nothing new runs before `OAM_DMA` on any frame — the DMA's timing is
   now decoupled from the tail scenes' cost by construction. Measured
   result: `worst = 12` sprites in range of `GEM_Y`'s scanline (hardware
   limit 8), holding on every sampled frame across a full rotation
   period.

The re-ordering also required re-measuring split timing (below) since it
changes when the tail-scene work actually runs — see "W2-10a's own
re-measurement, and the fix it required" for the fresh numbers, which land
at least as good as W2-10's own original baseline.

### The `$2006`-stomps-`ScrollWrite`-values trap (for W3-05 / W4-03d)

Recorded here explicitly, not just inside the correction above:
`stream_chunk()`'s `$2006`/`$2007` writes (PPU_ADDR/PPU_DATA, streaming
level columns into nametable RAM) go through the same loopy `t`/`v`
register machinery `$2005` (PPU_SCROLL) does on real hardware and in
`crates/rf-nes/src/ppu/scroll.rs`. A `CoreEvent::ScrollWrite` sampled
shortly after a `stream_chunk()` call can report an `x`/`y` pair that is
really `t` mid-stomp from an unrelated nametable write, not a value the
game deliberately wrote as a scroll offset (this is why an early
diagnostic dump showed a top-of-frame write reporting `y:114` — real
scroll values here are only ever `0` or `camera_y`, both `<=48`). Any
consumer of `ScrollWrite` values (not just this fixture's own tests) must
disambiguate by ORDINAL POSITION relative to a known anchor event (this
file's method: position relative to `DmaStart`), never by the value
alone.

### Vertical sub-area: `camera_y` reaches the PPU but not the rendered picture

W2-10a's vertical sub-area (`camera_y`, RAM `0x602E`) is a real, bounded,
correctly-gated RAM value (see "W2-10a red-fixture scenes" above), but
**produces no visible vertical scroll**, verified by rendering rather than
assumed:

**Method:** captured a full 16-frame cycle of rendered frames at
`camera_y==0` and again at a STEADY `camera_y==48` (Down released once
`VERTICAL_MAX` is reached, so only Right is held — identical button state
to the baseline), matched pairs by `frame_counter % 16` (controlling for
gem-rotation, period 8, and blink, period 16, phase — both change OAM
independent of `camera_y` and would otherwise swamp any real signal). A
naive unmatched diff (different, arbitrary frames at each `camera_y`)
showed thousands of differing pixels including full-width rows — that
turned out to be entirely gem/blink/split-timing-phase noise: a
SAME-`camera_y` control diff (two frames a few frames apart, both at
`camera_y==0`) showed the identical pattern and magnitude. Phase-matched
pairs at `camera_y` 0 vs. 48: **byte-identical** at three consecutive
matched samples (`frame_counter % 16` = 10, 11, 12); a fourth showed 84
differing pixels confined to rows 33-40 (the split-boundary band, not the
playfield) — consistent with residual split-timing jitter, not a
vertical-scroll effect.

**Suspected root cause** (not chased further — see below for why):
`main_loop()`'s top-of-frame write sets `t`'s Y bits to `camera_y`, but
the split write LATER IN THE SAME ITERATION sets `PPU_SCROLL=0` for Y —
and that write, not the top write, is the LAST vertical write before that
SAME frame's own pre-render `vert(v)=vert(t)` copy (dots 280-304, once
per frame, after the split and before the next iteration's own top
write). So `v` gets `0`, not `camera_y`, on every single frame — `main.c`
originally had this backwards in both writes' own comments (each claimed
the OTHER write was the one that "never survives"); both are now
corrected in-source to describe the actual mechanism. This diagnosis is
inferred from the write ordering, the emulator's own dot-range gate for
`vert(v)=vert(t)`
(`crates/rf-nes/src/ppu/background.rs`), and the rendering evidence
above; it was not independently re-verified beyond that (see "not chased
further" below).

**Why this is not fixed in this ticket:** the fix is a `$2006`
v-register-reconstruction technique at the split point — exactly what
`main.c`'s own split-write comment already documents REJECTING for the
horizontal case, with measurements (mid-scanline coarse/fine Y
reconstruction complexity). Attempting it would invalidate the frozen
streaming/tail histograms above, require re-deriving `SPLIT_DELAY`, and
re-open a technique question this file's own history already closed once
— a separate ticket's worth of work, not a fix folded into this one.
Criterion 3's own wording ("a vertical sub-area exercising vertical
scroll within NROM's fixed mirroring") asks the fixture to EXERCISE
vertical scroll, which it does at the input/RAM level; it does not
require the HUD or playfield to visibly respond while doing so. This is
the same shape of decision as the split-timing residual defect above —
documented, characterized, and shipped rather than chased — and
`crates/rf-harness/tests/rf_scroller_red_fixture_scenes.rs::
vertical_sub_area_camera_y_ram_witness` asserts only the RAM-witness half
accordingly, with a comment pointing back here.

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
`frame_counter` (at the time this was measured, address `0x602E` — see
"Address re-pin" above for why it is `0x6030` as of W2-10a; this
observation pre-dates that move and was not re-verified against the
re-pinned address or the new ROM) failing
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
source regressed. **Re-pinned at W2-10a, twice** — once for the original
three-scenes pass, and again after the `git checkout` incident forced a
`main.c` reconstruction that also fixed the gem-transfer/OAMADDR bug (see
"The `git checkout` incident and W2-10a's SECOND fix" above): `rom.sha256`
now reads
`c79f6f61ae82a87beaf57edb6757d75c1cfb2a1316196b1c8295f22a1699e087`, built
with `cl65 V2.18` (cc65 package version 2.19 — cc65's own version string
lags its package version; not a mismatch), same machine. Every known
consumer of the hash was re-pinned in the same change as this second
re-pin: this file, `crates/rf-harness/tests/rf_scroller_replay.rs`'s
`GOLDEN_HASHES` (regenerated from a re-verified record+independent-replay
run against the new ROM, both agreeing byte-exact — see that constant's
own doc for the capture-methodology fix this required), and
`crates/rf-harness/tests/rf_scroller_split_timing.rs`'s frozen streaming
histograms (re-measured against the new ROM a second time — see "the tail-
scene reorder" above — not copied from either prior measurement). W5-01's
identity block does not yet exist (ticket not started) so had nothing to
re-pin.

## Streaming order and vblank budget (ticket W5-02c)

`stream_chunk()` is the **first** thing `main_loop()` does after the
vblank wait, before `read_buttons()` and the camera arithmetic. That
ordering is load-bearing, not stylistic.

### The bug it fixes

Until W5-02c the order was: wait for vblank → read buttons → update
camera → `stream_chunk()`. `read_buttons()` is eight `$4016` reads under
cc65's codegen and the camera update is 16-bit arithmetic, so roughly
fifteen scanlines of vblank were gone before the chunk started. Vblank
ends at scanline 260. The chunk's last `$2007` writes were landing on
**scanlines 0-1** — during rendering, where the PPU owns `v` and a write
goes to whatever address rendering left there
(nesdev.org/wiki/PPU_registers: writes to `$2007` during rendering
"will corrupt the address").

The result was isolated wrong tiles in the tail columns, and it was
invisible on screen: a handful of tiles in a level of 1344.

### How it was found, and one wrong turn worth recording

Ticket W5-02b measured it first, from the outside: the offline
`metatile_screens` decode disagreed with the nametable bytes the ROM
streamed, on 15 of 96 raw tile columns, in whole `STREAM_CHUNK_ROWS`
(2)-sized units, only in the tail.

**Attempt 1 guessed and was wrong.** The theory was that tail-frame work
overran vblank so the top-of-loop `PPU_STATUS` poll found the flag
already set and started mid-vblank; the fix was to detect that and skip
the chunk. Implemented and measured, 15 disagreeing columns became
**17**. Reverted.

**Attempt 2 instrumented instead of inferring.** Stepping the machine
instruction by instruction and recording the PPU scanline at every
playfield VRAM write showed writes clustering at scanlines
`{0, 1, 256, 257, 258}` — the 256-258 group being correct late-vblank
streaming and the 0-1 group being the overrun, hitting exactly the tile
rows W5-02b had measured as wrong. That is the difference between a
plausible story and a cause.

### What this constrains

Anything added between the vblank wait and `stream_chunk()` eats the
chunk's budget directly. The gem/blink/vertical-area block already lives
at the very end of `main_loop()` for the sibling reason W2-10a records
(its cost was landing on the OAM DMA's timing); this is the same rule
applied to the streamer.

`streamer_demand` carries the demand target from the previous frame so
the chunk can run before the camera update that would otherwise produce
it. That costs one frame of lookahead and nothing else — the streamer
already runs `STREAM_MARGIN_TILES` ahead of the camera precisely so a
frame of slack is free.

It is declared **after every other static in `main.c`**, deliberately:
cc65 assigns BSS in declaration order, so a static declared earlier would
shift `gem_order[]` and everything above it, and this document's
"Documented RAM addresses" table — plus the shipped profile keyed to it —
would silently go stale. Declared last, nothing documented moves, and the
symbol dump confirms it: `streamer_demand` sits at `$606F`, above
`gem_order` at `$6063`.

