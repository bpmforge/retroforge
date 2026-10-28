# W37-00: input-movie coverage import (2026-10-26)

Community input movies used as **input scripts only**: parsed, replayed on our
cores with the coverage recorder armed, merged into the W36 script coverage and
baked into an atlas per ROM hash. Nothing from a movie or a ROM is stored or
quoted here; the table below is counts and frame numbers.

Code: `crates/rf-harness/src/movie/` (parsers), `movie_replay.rs` (laying a
movie out against a boot and driving a core), `tests/w37_00_movies.rs`
(ROM- and movie-gated, `--ignored`). Parsers live in rf-harness, not rf-input,
because W37-00's write scope is rf-harness/rf-rebuild/retroforge-src and because
two formats are zip archives (the `zip` pin moved from rf-harness's
dev-dependencies to its dependencies; same version as `retroforge`).

## Formats

| Format | Console | What the parser reads |
|---|---|---|
| FCEUX `.fm2` | NES | header keys, `\|cmd\|port0\|port1\|` lines, buttons `RLDUTSBA`, soft/power reset bits |
| BizHawk `.bk2` | NES, SNES | `Header.txt` platform, `Input Log.txt` with its `LogKey`; columns matched by name |
| lsnes `.lsmv` | SNES | `input` member, one line per poll; first poll of a frame used, extra polls counted |
| Snes9x 1.43 `.smv` | SNES | binary v1 header; pad data found through the offset field, never by assumed header length |

Pad words come out in the core's own bit layout (all four sources list buttons
as a layout written backwards, and a unit test pins that against
`rf_input::NesButton`/`SnesButton`). Findings while decoding the real files:
the `.smv` header stores the re-record count before the frame count and holds
one record more than its frame count says (tolerated); the `.lsmv` here has
no sub-frame polls and no resets.

Hostile input: every parser returns an error rather than panicking, walks its
input with iterators (`lines()`, `chunks_exact`), bounds memory by bytes that
exist and by `MAX_FRAMES`, and reads archive members through `take`
(`MAX_MEMBER_BYTES`). Tests cut every sample at every byte, feed garbage and
lying headers (frame count `u32::MAX`, data offset past the end) and a
compressed member one byte over the cap.

## Per-movie results

"Symptom frame" is the first movie frame at which the per-game heuristic (below)
saw the run leave the movie. It is an upper bound on where the run actually
left it. Coverage is kept up to that frame.

| Movie | Frames in file | Replayed | Symptom (confidence) | Levels entered | PRG coverage (W36 script, then plus this movie) | Atlas functions |
|---|---|---|---|---|---|---|
| SMB warpless (fm2) | 67,117 | 1,017 | lives dropped at 1,016 (high) | 1-1 only; furthest x in it 898 | 25.3% to 25.3% (movie alone 16.5%) | 389 to 389 |
| SMB with warps (fm2) | 17,868 | 4,956 | lives dropped at 4,955 (high) | 1-1, 1-2, 1-3 (transient), 4-1 | 25.3% to 27.3% (movie alone 21.7%) | 389 to 412 |
| SMW with warps (lsmv, bsnes v085) | 35,720 | 11,982 | no progress for 6,000 frames, starting at 5,980 (medium); first life lost at 4,618 | 3 translevels | 4.9% to 5.4% (movie alone 4.9%) | 770 to 791 |
| SMW 96 exits (smv, Snes9x 1.43) | 269,100 | 17,979 | file-select/title range reached at 17,978 (medium); first life lost at 5,209 | 3 translevels | 4.9% to 6.0% (movie alone 5.6%) | 770 to 813 |

Baked (per ROM hash, `code-atlas-v3`, into `RF_W37_CACHE_DIR`, default
`$TMPDIR/retroforge-w37-atlas-cache`, a directory of its own because the app's
cache index races under two writers): SMB 412 functions, 27.3% of PRG
(baseline plus both movies); SMW 813 functions, 6.1% (baseline plus both).
"Before" is the W36 script coverage recorded fresh in the test (SMB: both
drive scripts over all 32 levels; SMW: the W36-08 menus-and-level script), not
whatever the cache holds. PRG percent is executed bytes over PRG size.

No movie replayed to its end. By the ticket's own words that is a reported
compatibility finding, not a test failure; the acceptance line "replay to the
end" is therefore **not met** and stays open.

## Desync heuristics

There is no reference trace to diff against, so each game gets RAM facts that a
faithful replay would keep true.

- SMB (high confidence): lives drop; game over; `$0770` back to 0 after play
  began; world/level going backwards. Medium: no progress for 3,000 frames.
  Success is `$0770 == 2` in 8-4. The lives drop lags the real drift by a
  death animation, so the real divergence is some tens of frames earlier.
- SMW (medium confidence): game mode back into the title/file-select range
  after play began; no progress for 6,000 frames. A lost life is recorded but
  does not stop the run, since a SMW run may lose a life on purpose; for the
  Snes9x movie it came about 12,800 frames before the title fallback (the run
  ended in game over), so treat the life loss as the better estimate of where
  the drift began.

## SMB on the multicart: alignment

The movies start at a console power-on; the only SMB image here is the SMB +
Duck Hunt multicart (mapper 66), which boots to a game select first.

1. The menu ignores Start before about frame 120. A movie laid out too early
   presses that Start itself (it has Start at its frame 41) and then never
   gets past SMB's title: the first sweep scored every offset as "alive and
   unarmed" until a run that never starts the game was ranked below any run
   that did. Pre-roll is now 160 idle frames plus 5 of Start.
2. SMB's own boot is visible in RAM (the RNG byte becomes non-zero three
   frames after the menu Start). Movie frame 0 belongs about there. The sweep
   slides the movie over -22..=20 frames around that point (negative means
   dropping idle movie frames) and ranks by level reached, furthest x in it
   and frames kept.
3. The result repeats every 21 frames (offsets -1 and 20, -3 and 18, and so
   on give the same scores) and shifting by multiples of 21 up to 440 frames
   changes nothing: what matters is the phase against SMB's 21-frame interval
   timer, not the RNG. Winner for both movies: offset -1 mod 21 (the table
   run printed 20 for the warpless one and -1 for the warped one; the two
   score the same to within one frame).
4. At that phase the warped movie runs through 1-1 and 1-2, takes the warp to
   world 4 and reaches 4-1 before a life is lost near frame 4,955. The
   warpless movie shares the same opening but is off the movie in 1-1 by
   about frame 600 (Mario's x stops advancing near 860 to 900) and loses a
   life at 1,016. The other 20 phases do worse (furthest x 180 to 435).

## SMW alignment

Both SMW movies start at power-on, so the only slack is a few frames. The
sweep tried 0 to 4 idle frames before movie frame 0; the winners were 3 (lsmv)
and 4 (smv). The scores differ by one level entered at the 4,000-frame mark,
so this is weak evidence, not a measured alignment.

## What the desyncs most likely are

- The warpless SMB movie fails mid 1-1 at the best phase, while the warps
  movie (same FCEUX build number in its header) gets four levels further. The
  movie's checksum is an MD5 over the standalone game's PRG and CHR, and it
  matches none of the multicart's four PRG-bank/CHR-bank combinations, so the
  cart's SMB is not byte-identical to the movie's ROM. Candidates, none
  verified: a changed byte in the cart's SMB; a cycle-level difference in the
  NES core that only a frame-perfect run exposes (sprite-0, NMI entry, DMA);
  FCEUX behaviours around the opening frames. The way to separate them is a
  standalone SMB dump (not available locally) or a second emulator's trace,
  neither of which this ticket had.
- SMW: the lsnes movie names its ROM by SHA-256 and the hash equals our
  image's, so the ROM is the right one; it was made on bsnes v085's
  compatibility core and our core drifts after about 6,000 frames (100 s).
  The Snes9x 1.43 movie was made on an older emulator and drifts somewhere
  between about 5,200 and 18,000 frames.
  Both are SNES timing-accuracy findings (HDMA/IRQ/DMA timing in the long
  run), not importer bugs: the pads of both movies were checked against the
  console's layout (bits 0-3 of every `.smv` word are clear; the dominant
  bits are the run and right buttons).

## W37-00b: the desyncs, diffed against a reference (2026-10-28)

W37-00 had no trace to diff against. W37-00b used a second emulator as a
**black-box oracle**: Mesen 2 (already installed on the workstation; GPL,
so only ever run, never read) in its headless `--testrunner` mode, fed the
same pad words frame by frame from a generated Lua script and printing the
same per-frame RAM facts as our core. Tool: `crates/rf-harness/tests/
w37_00b_oracle.rs` (ROM- and movie-gated, `--ignored`); it writes our
trace and the reference script, and the two traces are diffed outside the
repo. Nothing from the ROM, the movie or the reference is stored.

Three things had to be right before a diff meant anything, each found the
hard way:

- **Sample at the same raster point.** The runner's frame END is the
  vblank (SNES line 225, NES line 240) but ours is the end of the last
  line, so its trace is sampled at its frame START (SNES line 0, NES
  pre-render), and its pads switch on its own input event (vblank).
- **A controller.** With no settings the runner has no SNES pad: pass
  `--snes.port1.type=SnesController` (the NES port has one by default).
- **Clean SRAM.** The runner keeps a save per ROM file name; SMW found its
  own earlier save and skipped the intro. Each run uses a fresh copy of the
  ROM under a new name.

Mesen itself does not replay either SMW movie to its end (they were made
on bsnes v085 and Snes9x 1.43), so for SMW it is a timing reference, not
proof the movie syncs. It does replay the SMB movies far further than we
did, on the same multicart with the same plan.

### Found and fixed

| # | Where | What was wrong | Evidence | Effect |
|---|---|---|---|---|
| 1 | `rf-snes` `apu/boot.rs` (HLE IPL) | The handshake echoed each uploaded byte at the END of its 25-cycle loop and only looked at the ports again a cycle later (26). The boot ROM echoes 9 cycles after the compare (fullsnes "Boot ROM Disassembly": `jnz` 2, `mov A,$F5` 3, `mov $F4,Y` 4) and loops in 25. | Our SMW upload cost 30.6 SPC cycles a byte against the reference's ~25 (measured master clock between `$2140` writes); its 11,974-byte upload ended at frame 21 instead of 17. | SMW boots on the reference's frame. |
| 2 | `rf-snes` `system.rs`/`timing.rs` | No WRAM refresh: the S-CPU (and DMA) lose 40 master cycles per scanline at H=538 (fullsnes "SNES Timings"), so the CPU ran about 2.9% fast. | The decompressor's pointer gained on the reference every frame; with the refresh its per-frame advance matches to a few bytes. | SMW state matches the reference to frame 1,325 instead of 190. |
| 3 | `rf-harness` `movie_replay.rs` (NES) | Movie buttons switched at our frame boundary (end of pre-render). FCEUX's movie frame, and the reference, switch at vblank. SMB reads the pad before vblank when its NMI work runs over, and got the NEXT frame's buttons. | First divergence at plan frame 503: the pad byte `$06FC` held frame 503's buttons on ours, 502's on the reference; Mario jumped a frame early. | Warps movie plays to the 8-4 win; warpless gets from 1-1 into 4-2 (see below). |

### Remaining divergences (pinned, not fixed)

- **SMW, frame 1,326** (lsnes warps, plan delay 0): the overworld load
  (game mode `$0C`) ends one frame early on ours. Mid-load the
  decompressor pointer is up to ~0xA4 bytes ahead of the reference (its
  first load, frames 67-84, is ~0x40 behind), so a sub-percent timing
  difference is left in loads. Candidates, unverified: the SPC700 clock
  (we use 21 master cycles per SPC cycle, the nominal 1.024 MHz; the
  reference's own state reports ~1.0253 MHz, the commonly measured
  32,040 Hz DSP rate), DMA/HDMA cost, or the refresh position. From there
  the run takes a different route (translevel `$28` vs `$2A`), so the
  heuristic's "first life lost at 4,616" is downstream of this frame.
- **SMB warpless, plan frame 28,322** (in 4-2): ours has a lag frame the
  reference does not (frame counter `$09` stalls one frame). An NES CPU,
  OAM/DMC DMA or NMI-timing difference in a busy frame; not traced yet.

### The multicart question (acceptance line 2)

Answered by the reference, not by a standalone dump: with the identical
plan (160 idle + 5 Start lead, offset -1) on the same SMB + Duck Hunt
image, the reference plays the warpless movie to the 5-4 axe without losing a
life (where its runner's time limit cut the run, plan frame 39,876) and
we now match it to 28,322. So the multicart's SMB is not what broke the
replay; the input-latch convention (fix 3) was. The FM2's ROM checksum
still matches none of the cart's bank combinations, so a byte difference
in the cart's SMB stays possible, but it is not needed to explain
anything seen here.

## Not done / follow-ups

- Replay to the end for any of the four movies (needs the cause above).
- Level maps: the ticket title says "atlas and level maps"; the brief was the
  atlas only, so no scout run was added.
- No `.bk2` exists locally; that parser is tested on hand-written logs only.
- A cart-agnostic way to find the boot frame (here: the RNG byte, a game fact
  in the test) would let the same sweep run on other multicarts.
