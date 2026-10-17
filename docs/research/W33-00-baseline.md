# W33-00: backtrack offset re-check and live fps baseline

Measured 2026-10-17 on `main` (184ce48 plus the W33-00 fps harness
`shot_fps` in `crates/retroforge/tests/w30_screens.rs`), release build, Apple
M2 Max shared with other jobs (load average 13-26 during the runs; each figure
is printed with its `uptime`).

## 1. The ~125 px backtrack offset (NEXT_PLANS section 2): closed

Repro: `RF_SCREENS_ONLY=widescreen RF_SMW_RUN_FRAMES=240 RF_SMW_BACK_FRAMES=120`
(run right 240 frames, back left 120, then the 32:9 shot).

Result: the canvas margin no longer shows a copy of the first slope or the
coin at the wrong place. Evidence, `widescreen-32x9-on.png` (stitched canvas)
against `target/screens-probe/widescreen-32x9-vram.png` (VRAM margins of the
same moment), compared pixel by pixel, 4:3 centre 256 px excluded:

* centre 256 columns: 48 540 of 48 640 pixels identical (99.8 %);
* the two 128 px margins the VRAM probe can show (columns 144-272 and 528-656),
  rows 0-189: 44 816 of 48 640 identical (92 %). Every differing pixel is in
  rows 130-169 (the light sky stripe band), none in the ground, bushes, hills
  or clouds. That band is the row-backdrop vote (F3) and is W33-02's job, not
  a placement error. The second bush at x 560-625 sits on the same columns in
  both pictures.

Why it closed: the widened-line bundle fix (55508af, the bundle took the
first 256 dots of a widened line instead of the middle) and the shared
`ScrollLag` rule (b4fd601, W32-08), both on `main` before this run.
The plan's "if open, order W33-02 first" branch does not apply.

Still visible and expected in that shot: black under the ground in the left
margin (nothing known there, W33-02) and no BG2 hills in either margin
(W33-01).

## 2. Live fps (median of 600 frames)

Method: the app harness, pacing OFF, 20 chunks of 30 frames, median chunk fps
and overall fps, in one session per game, in the order below. Unpaced fps is
the whole live loop (core, sinks, stitching, UI frame in the test harness), so
only the differences between rows mean anything. The game cannot be held in
one scene (a standing player dies about 220 frames in), so the 4:3 control is
repeated at the end; the spread between the two controls is the noise floor.
Harness: `RF_SCREENS_ONLY=fps RF_FPS_GAME=smw|smb3|mm2`.

| Game | 32:9 scout ON | 32:9 scout off | 4:3 off | 32:9 scout off (again) | 4:3 off (again) |
|---|---|---|---|---|---|
| SMW (SNES) | 53.6 | 50.1 | 88.5 | 44.1 | 99.7 |
| SMB3 (NES) | 442.9 | 427.3 | 413.9 | 430.0 | 443.2 |
| MM2 (NES) | 362.9 | 353.0 | 381.9 | 421.3 | 420.1 |

Load averages (1 min) per row: SMW 15.0 / 13.0 / 13.2 / 13.8 / 13.4; SMB3 and
MM2 14-16. The scout was running for the whole "scout ON" row (started 1;
finished 1 on SMW, 0 on the NES games inside the window).

Reading it:

* SNES 32:9 costs about half the live fps (roughly 88-100 fps at 4:3 to 44-50
  at 32:9). That is the widened PPU render (800 dots per line, plus the
  widened sub-screen line), not stitching, and it is the number W33-01's
  "< 0.3 ms per frame" is measured against (0.3 ms is 1.5 % of a 20 ms frame
  here).
* The NES shows no effect of 32:9 or of the scout beyond the 10-15 % spread
  between the two 4:3 controls: the widened NES frame is cheap and the scout
  runs on its own thread.
* Scout ON being as fast as or faster than scout off on SMW is scene drift
  (the first row is measured right after entering the level, the later rows
  after Mario died), not a scout speed-up. Do not quote it as a gain.

For the 0.3 ms criterion this harness is too noisy (spread of 10-15 %
between identical rows). W33-01 measures its cost with an in-process A/B
instead (`crates/retroforge/tests/w33_01_bundle_cost.rs`).
