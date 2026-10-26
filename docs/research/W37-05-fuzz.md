# W37-05: savestate fuzzing for unplayed branches (2026-10-26)

Scripts and movies reach what a player does on purpose. Deaths, game over,
pause, menus and timeouts need inputs nobody wrote, so this ticket adds a
fuzz driver that starts from savestates and plays seeded-random input. No ROM
bytes or movie content appear here, only counts.

Code: `crates/rf-harness/src/fuzz.rs` (driver, 5 unit tests on a synthetic
ROM), `crates/rf-harness/tests/w37_05_fuzz.rs` (SMB acceptance, `--ignored`).

## Method

- **Seeds.** In-process savestates (`Savestate`: the nine `StateRegion`
  payloads the shipped `.rfstate` carries, no envelope), cut during the W36-04
  drive: SMB title, and a level start (frame 520) and mid-level (frame 1000)
  for each of the 32 levels. Three more are test-side pokes of a seed: last
  life (1-1, 4-1) and a nearly expired timer (1-1). The multicart menu seed is
  off by default: its code is the menu and the other game, not SMB, and one
  62-second probe with it counted 1,668 of its 1,942 new bytes there.
- **Episodes.** 300-1200 frames of input, a pure function of the episode seed
  (`gen_inputs`): bursts of long idle (up to 900 frames, for attract and
  timeouts), held directions, run-and-jump, mashing with a fresh random mask
  every 1-4 frames (10% also on pad 2), Start taps (pause), Select/Start
  combos, arbitrary held masks, left/right flipping. Up+Down and Left+Right
  occur on purpose.
- **Novelty guidance.** Segments of 20 frames. A segment that executed a PRG
  byte nobody had run adds the savestate from its start to the corpus (up to
  256 derived entries); half the episodes then start from a derived entry.
  Every segment's log is merged into the union, novel or not, so indirect
  targets and bank windows reach the atlas too.
- **Determinism.** SplitMix64 local to the module; no host RNG or clock. The
  wall-clock stop is a caller closure, so the module has no `Instant`.
  A find stores `(start entry, episode seed, planned, frames run)`; `replay`
  re-runs it. The test replays three finds and the halts and checks the
  recorded new offsets and halt kind come back.
- **Crash safety.** After every frame: `Step::Frame` not completing within the
  core's cycle budget (no-frame), the CPU's `jammed` flag (the core keeps
  completing frames after a `KIL`), and 600 identical WRAM/VRAM/register
  prints (frozen). Segments run under `catch_unwind` (panic). Each ends the
  episode, is counted, and the next episode restores a clean state.
  Episodes that execute new RAM code are counted separately (informational).

## SMB result (multicart ROM, release, seed `0x5eed00370005`)

Budget: at most 1,500 episodes / 1,500,000 frames / 300 s wall; the wall clock
stopped it: **213 episodes, 155,699 frames in 301 s** (about 520 frames/s with
the recorder armed; the baseline of 64 script runs alone takes 213 s).
Baseline: both W36-04 scripts over 32 levels (25.3% of PRG), plus the two
W37-00 movies replayed at their best alignments to their first desync symptom
(27.3%, 412 atlas functions; same numbers as W37-00).

| | Baseline | After fuzz |
|---|---|---|
| PRG bytes executed | 17,901 (27.31%) | 18,473 (28.19%), +572 |
| Atlas functions | 412 | 418 (+6 entries) |
| Corpus | 68 seeds | +21 derived |
| Finds (episodes with new code) | | 11 |

Halts and crashes: jam 0, no-frame 0, frozen 0, panic 0; no episode ran new
RAM code. An earlier 62-second probe including the menu seed: 45 episodes,
+1,942 bytes, 412 to 460 functions, also 0 halts.

New code by RAM fact, from the segments in which it first ran (bits overlap,
one segment counts for each state it saw): title or demo (mode 0) 369 bytes in
12 segments; play 194 in 9; paused 132 in 5; game over (mode 3) 94 in 3;
player dying 94 in 3; life lost 52 in 3. So the fuzz did reach the death
path, game over, pause and the title/attract code the scripts never touched.
Not reached in this budget: victory, flagpole slide, timer expiry. The
nearly-expired-timer seed found nothing labelled, which may mean the timer
poke is wrong, not that the path is covered.

The gain is small (0.9 point) because SMB's unplayed code is a minority of
PRG and the same few segments are hit repeatedly; the first 100 episodes made
+464 of the +572 bytes. More budget gives a thin tail.

## What is not claimed

Function counts are entries in the rebuilt atlas, not hand-verified game
routines. Labels are the state at the moment, not proof of cause. SMW was not
run. The atlas was baked into the W37 cache directory under the ROM hash and
replaced the W37-00 entry (412 functions) with the 418-function union.

```text
cargo test --release -p rf-harness --test w37_05_fuzz -- --ignored --nocapture
RF_W37_FUZZ_MENU=1 ...   # include the multicart menu seed
```
