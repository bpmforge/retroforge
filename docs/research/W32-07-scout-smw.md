# W32-07: the level scout on Super Mario World (USA), Yoshi's Island 1

Clean-room: every address below was found or confirmed by running the game
in this repo and watching RAM against the PPU's scroll registers; no
emulator source and no disassembly-derived map was used. The probes are
`crates/retroforge/tests/w32_07_scout_smw.rs` (ROM-gated, `--ignored`):
`observe_camera`, `observe_game_mode`, `scout_strategies`,
`restore_fidelity`. Machine load was 30-50 on 12 cores while these ran, so
absolute speeds are pessimistic; ratios are the useful part.

## How the level was reached

The `w30_screens::shot_widescreen` script, headless: boot, ten Start/A
taps, the "Welcome" message, left on the map to Yoshi's Island 1, A to
enter, 150 frames of fade-in (`reach_yi1`). Input is `latch_and_advance_frame`
with SNES button bits.

## 1. The camera and the player (observe_camera)

600 frames walking right with hops (Right held, B for the first 18 of every
36 frames), BG1HOFS read from the PPU (`bg_layer_views()[0].hofs`) after
every frame, the first 0x2000 bytes of work RAM sampled after every frame.
BG1HOFS went 0 -> 587 over 446 moving frames.

| Word | Evidence |
|---|---|
| `$7E001A` (16-bit) | BG1HOFS read after frame f equals this word read after frame f-1 on **592 of 592** frames (lag table: f-3 145, f-2 146, **f-1 592**, f+0 146, f+1 145 ... of 592). It is the camera X. |
| `$7E1462` (16-bit) | Identical to `$1A` on every sampled frame (same lag table). A mirror. |
| `$7E0094` (16-bit) | Within `[camera, camera+255]` on 599 of 599 frames and moves by the camera's step on 441 of 446 moving frames: the player's X in the level (Mario's screen column = `$94 - $1A`, 16..105 during the walk). |
| `$7E00D1` (16-bit) | Equal to `$94` on every frame of the traced scout runs; 16 at the start, 693 after the walk. The profile already listed it as `player_x`. |

The top BG1HOFS matches outside `$1A`/`$1462` are 4-valued bytes at `$046C..$0471`
(112 hits, 4 distinct values: noise, not a camera).

**Scroll timing (matters for stitching).** The register value read after a
frame is the one that draws the NEXT frame (the NMI writes it in vblank,
after the visible lines). A frame stitched at the value read after it lands
one frame of scroll too far: with the scout moving 3, 4 and 6 px per frame
the left edge of the raised block (true world x = 944 = 59 x 16) appeared at
947, 948 and 950. Stitching a frame at the value read after the PREVIOUS
frame puts it at 944 for every step, and the scout canvas then agrees with a
real walk on **39 110 of 39 110** shared BG1 cells (before the fix: 74 % to
86 %). `Scout::step` and the live core-thread stitcher both use the previous
frame's read now.

## 2. Strategy (scout_strategies)

One snapshot of the live machine at the start of the level, a second
`EmuStepper` restored from it (`restore_fidelity`: 40 frames, 0 differing
pixels against the live machine), neutral input, a poke before each frame:

| Recipe | Result |
|---|---|
| (a) write `$1A` and `$1462` (+8 px/frame) | **Fails.** The game recomputes the camera from the player each frame: BG1 canvas 264 px wide after 123 frames (both passes stalled). |
| (b) write `$D1` only | **Fails.** `$D1` is recopied from `$94` every frame: 256 px. |
| (b) write `$94` (+8 px/frame) | **Works**, the camera follows and the game streams the columns. But the first enemy kills Mario (`$71` goes 0 -> 9, `$9D` 0 -> 0x30); the camera still follows a dead Mario, but the game restarts the level about 200 frames after the hit and the pass ends with the camera jumping to 0: 2 191 px, only the part reached first. |
| (b) write `$94` and hold `$96`/`$97` (Mario's Y word) at 150, above the top of the screen | **Works and is robust.** `$71` stays 0 for the whole pass; the camera follows to its end at 4 864; BG1 canvas 5 120 px wide (the level is 20 screens), both passes ended by "no new level for 60 frames". 738 frames. |

`$96` as Mario's Y word: 352 standing on the ground, 284..312 in a jump, 821
falling out of the level (observed). The camera Y (`$1C`) stayed at 192
throughout, so Mario above the screen does not move the picture.

Step size: 3, 4, 6 and 8 px per frame all give the same level (the columns
stream at these speeds; 8 gives 100 % agreement with the walk). 1 px/frame
only starts scrolling after ~110 frames (Mario first walks to mid-screen),
longer than a 60-frame stall rule, so the profile uses 8.

## 3. When it is safe to start (observe_game_mode)

Run-lengths of `$0100` across the whole script (frame numbers of that run):
0x00 title, 0x01..0x0D menus and the map loads, 0x0E the map (frames
1770..2224), 0x0F..0x13 the level load and fade-in, **0x14 for the whole of a
level played** (frames 2384..2795), then 0x0B/0x0C/0x0D back to the map once
Mario was hit (`$71 = 9`, `$9D = 0x30` from frame 2604, 191 frames before the
mode changed). A scout started while Mario was dying (or on the map, where
`$1A` also mirrors BG1HOFS) either stalled or ran into the restart. The
profile therefore starts a scout only while `$0100 == 0x14` and `$71 == 0`,
for 30 live frames running.

## Numbers

* A scout frame costs about one live frame: 10.3 ms against 10.1 ms for a
  live frame including stitching, measured in the same process, same load
  (`scout_strategies`).
* Yoshi's Island 1 (5 120 px, both passes): 738 scout frames, 7.6 s here at
  97 fps (load ~35). Pass 1 is 4 864 / 8 = 608 frames plus the 60-frame
  stall; pass 2 (left of the start) finds nothing at the start of a level and
  ends on its stall rule.
* Merged into the live canvases: 268 853 cells in the GUI run; a merge only
  scans the box the scout wrote since the last one.
