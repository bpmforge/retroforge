# W34-00: Diorama baseline (and the W34-01 capture cost)

Measured 2026-10-18 on branch `feat/W34-01-tile-grids` (from `main` at
d3b31c5), release builds, Apple M4 Max (`Metal`), shared with other jobs
(1-minute load average 7 to 13 during the runs; unpaced fps rows are only
comparable to each other, as in `W33-00-baseline.md`). Only the Diorama part
of HD2D_PLAN's W34-00 was in scope; the shadow-map stub, the collision oracle
and the frames-to-80%-coverage count are not measured here.

Harnesses added (all `#[ignore]`d where they time):
`crates/rf-renderer/tests/w34_00_diorama_cost.rs` (the pass alone),
`crates/rf-renderer/tests/w34_00_framing.rs` (camera numbers, always runs),
`shot_w34_baseline` in `crates/retroforge/tests/w30_screens.rs`
(`RF_SCREENS_ONLY=w34-baseline`, whole app, SMW) and
`crates/retroforge/tests/w34_01_real_games.rs` (capture cost on SMW, SMB3).

## 1. The pass alone, at 1x / 2x / 4x

`DioramaPass::render_camera` end to end: textures and buffers created, the
draw, and the synchronous readback. Scene = the live-screen path's: a 33 x 29
grid of 8 px cells, the lowest nine rows walls (8 eighths, a few gaps), 24
billboards, 200 frames per cell of the table after 5 warm-ups. "No geometry"
draws the ground quad only, which isolates what the geometry does not change.

| Image | Readback | Camera | Walls + 24 billboards, pass p50 / p95 | No geometry, pass p50 / p95 | Mesh build p50 |
|---|---|---|---|---|---|
| 256x224 (1x, what the app renders today) | 224 KB | default | 1.66 / 1.70 ms | 1.63 / 2.02 ms | 7 us |
| 512x448 (2x) | 896 KB | default | 1.75 / 2.34 ms | 1.68 / 2.13 ms | 9 us |
| 1024x896 (4x) | 3584 KB | default | 1.88 / 2.52 ms | 1.83 / 1.93 ms | 7 us |
| 256x224 | 224 KB | HD-2D preset (pitch 40, zoom 1.6, follow) | 1.66 / 1.79 ms | 1.65 / 2.20 ms | 12 us |
| 512x448 | 896 KB | preset | 1.72 / 2.53 ms | 1.68 / 2.38 ms | 13 us |
| 1024x896 | 3584 KB | preset | 1.87 / 2.52 ms | 1.83 / 1.88 ms | 12 us |

Reading it:

* The pass is a **fixed ~1.65 ms floor**, set by creating the per-call
  resources, the submit and waiting on the buffer map. Quadrupling each side
  (16x the pixels, a 3.5 MB readback) adds about 0.2 ms; the 4560-vertex mesh
  adds about 0.03 ms and its CPU build is 7 to 13 us. HD2D_PLAN B5 guessed
  the readback alone would be significant at 4x ("~3.7 MB/frame, estimate");
  it is not on this machine. The reason to move to a render-to-texture path is
  the per-call floor and the NEAREST upscale (B4), not bandwidth.
* HD2D_PLAN D6's target of p95 <= 3 ms at 1024x896 **with shadows** therefore
  has about 0.5 to 1.1 ms of headroom over today's 4x pass (p95 1.9 to 2.5 ms)
  for a shadow pass and lighting. It is the floor, not the geometry, that
  spends the budget.
* Both cameras cost the same; the preset camera's mesh differs by a few
  vertices (4518 against 4560) because of the oriented build.

## 2. The app, on a real game

Super Mario World, Yoshi's Island 1, live-screen Diorama path (`GameAware`,
height table made from what is on screen: the 18 distinct tiles of the lowest
nine BG1 rows other than the sky tile, so the mesh has a realistic wall
count). Pace limiter off; **three states interleaved chunk by chunk** (20
chunks of 30 frames each = 600 frames per state), so scene drift and load hit
all three alike.

| State | Median fps | Diorama build + render + readback (app gate p95) | Opaque Diorama px |
|---|---|---|---|
| Diorama off (control) | 100.6 | - | - |
| Diorama on | 98.4 | 1.96 ms | 11 370 |
| HD-2D preset (Diorama + lens) | 98.0 | 2.00 ms | 26 606 |

Diorama costs about 2 % of the unpaced frame rate here (about 0.2 ms of a
10 ms frame, inside the 10 to 15 % noise W33-00 found between identical rows),
and the preset's lens passes add almost nothing on top. The app's budget gate
sees 1.96 ms p95 for the Diorama alone, matching section 1's 256x224 row. (The
two figures measure different things: the fps is how fast the whole loop turns
over, the 1.96 ms is the UI thread's wall time inside the Diorama call, mostly
waiting for the GPU readback; the core thread keeps running meanwhile, so only
part of that 1.96 ms shows as lost frame rate.)
"1x" is the only size the app produces today: `refresh_live_diorama` renders
at the frame's own size and the UI scales the texture NEAREST (B4). There is
no 4x app figure to measure until D5; section 1 is the 4x pass cost.

## 3. Camera framing, today

`w34_00_framing.rs` projects the game's own 256 px window (centred on the
follow target) through the real `view_proj_camera` matrix, pitch 40 degrees,
and reports its share of the image width. The follow target always lands at
ndc (0.000, 0.000), as `follow_centres_the_given_world_point_on_screen` says;
what changes is how small the game's picture is.

| Ground (world px) | Zoom 1.0 | Zoom 1.6 (the preset) | Zoom 4.0 | Camera distance at 1.0 / 1.6 / 4.0 |
|---|---|---|---|---|
| Live screen, 264 x 232 | 44 % | 71 % | 178 % (cropped) | 541 / 338 / 135 px |
| RF-Scroller fixture, 768 x 224 | 19.5 % | 31 % | 78 % | 1230 / 769 / 308 px |
| SMW-sized level, 3328 x 432 | 4.7 % | 7.4 % | 18.6 % | 5162 / 3226 / 1290 px |
| Long level, 5000 x 432 | 3.1 % | 5.0 % | 12.4 % | 7719 / 4824 / 1930 px |

(4:3-ish image, aspect 256/224. At 16:9 every share is 0.64x of these.)

This is HD2D_PLAN B2 in numbers: "zoom 1.6x" on the fixture shows the game
window as the middle 31 % of the image (the plan's "middle third"), and on a
SMW-sized decoded level as 7 %, because the distance is solved from the
whole level's bounding sphere and only divided by zoom. The live-screen path
is not affected (its ground is one screen), which is why the Diorama looks
right on SMW today and wrong on a decoded level.

## 4. What the existing harnesses report

* **W16-01 `bench-passes`** (`cargo run --release -p rf-renderer --bin
  bench-passes`; rows in `docs/evidence/gpu-passes.json`): the Diorama pass on
  a 16 px checkerboard with a billboard on every open tile, 300 frames. Re-run
  today: 256x240 p50 1.653 / p95 1.946 ms, 512x448 p50 1.849 / p95 1.891 ms;
  committed (2026-09-18): 1.739 / 1.816 and 2.120 / 2.192. It stops at 512x448,
  and the checkerboard (half the cells walled) is a heavier scene than the
  live one, which is why section 1 adds the 1024x896 row. (The re-run's output
  was not written to the evidence file.)
* **`diorama_camera_live.rs`**: a pass/fail test, no numbers. With a real GPU
  it checks that moving tilt, turn, zoom and follow changes the Diorama
  picture hash, and that the Accuracy frame the core hands the UI is
  byte-identical (`last_frame_rgba_for_test`) with the camera default or
  moved, and after Diorama is switched off again (0.59 s, passing). It is
  the harness W34's "heights and shadows on/off never change the accuracy
  frame" extension plugs into.
* `diorama_live_heights.rs`, `rf-renderer/tests/diorama_camera.rs` and
  `diorama_golden.rs` also pass unchanged; none prints a cost.

## 5. W34-01 capture cost (tile grids behind `SetTileGridCapture`)

`w34_01_real_games.rs`, in process, capture OFF against ON interleaved frame
by frame (the simulation is identical either way; the hash tests prove it),
600 frames each, SMW Yoshi's Island 1 and SMB3 World 1-1, running right.
"Frame" = advance and take the bundle (ON adds the PPU's tile recording and,
on the SNES, the per-line registers the bundle then keeps); "grids" = the
builder alone on the finished frame.

| Game | Frame OFF (median) | Frame ON (median) | Delta | Grid build median / p95 | Payload median / max | Grids per frame |
|---|---|---|---|---|---|---|
| SMW (SNES) | 9 797 us | 9 817 us | +19 us | 30 / 33 us | 43 904 / 43 904 B | 3 median, 4 max (2 per frame are bands narrower than the picture) |
| SMB3 (NES) | 1 890 us | 1 893 us | +2 us | 52 / 60 us | 4 096 / 4 832 B | 2 to 5 (split-scroll bands) |

Total added per frame is under 0.1 ms on both, against the 0.3 ms target and
the 64 KiB VRAM clone the live path takes today (the grids are 43.9 KB on SMW,
61 KB is the plan's worst case). A
3 800-cell layer set is cheap because the pattern hash is taken once per
distinct character (memo per layer), not per cell.

Findings that shaped the capture:

* **The end-of-frame scroll is the next frame's.** A game moves its scroll in
  vblank, so `SnesDebugFrame::bg_scroll` (and so `live_heights`) is one frame
  ahead of the picture; the first oracle run differed by 1 px for that reason.
  The grids use each line's own registers (`FrameBundle::lines`), so a layer
  whose scroll changes down the picture becomes one grid per band.
* **The NES PPU reports each tile once per scanline** (7 680 records a
  frame), with no tile-row boundary and no nametable address. Two metadata
  fields were added to `rf_nes::ppu::DrawnTile` (`fine_y`, `nt_addr`, derived
  from `v`, written only while capture is on and read by nothing in the
  simulation) so rows can be found and tiles paired on the attribute
  table's 2x2 alignment.
