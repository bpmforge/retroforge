# W33-09: scene identity (Super Mario Bros. 3, Mega Man 2, Super Mario World)

Clean-room: every address below was found by running the game in this repo and
diffing work RAM across states the scene key has to tell apart; no
disassembly-derived map and no emulator source was used. The probes are
`crates/retroforge/tests/w33_09_scenes.rs` (NES) and
`w33_09_scenes_smw.rs` (SNES), ROM-gated (`--ignored`), headless (`EmuStepper`
plus the same `CanvasModel` calls the core thread makes), run in release.
NES pad bits and scripts as in `w32_09_scout_nes.rs`; `RF_WATCH=hex,hex` logs
changes of RAM bytes.

## 1. The measurement: one id for a whole session

Before the change, `CanvasModel::observe_frame` returned **`6570426d2c2e2fa3`
for every frame** of a Super Mario Bros. 3 session (title, map, World 1-1, 1276
frames) and of a Mega Man 2 session (title, menus, stage select, Air Man,
2172 frames): `1 distinct scene id`. That constant is the FNV-1a hash of 112
"flat" edge buckets followed by the length 112 (reproduced: `0x6570426d2c2e2fa3`).
Three separate faults, in the order the evidence points to them:

1. **The hash is constant on NES content.** The pure per-frame
   `compute_scene_id` (no continuity, no tracker) is also one value over both
   sessions (`raw_smb3`, `raw_mm2`: "raw per-frame hash distinct 1"). It
   thresholds adjacent-block differences of `palette_index` averages at 48; NES
   indices never differ that much (the W4-03d module note already said so for
   RF-Scroller), so every edge is "flat". Even the jumps the tracker did see
   recomputed the same constant (SMB3 f303: world jump 208, f482: 240; MM2 f1167
   and f1215: 240, all the same id after).
2. **Most cuts are not seen.** The old rule was "world jump over 64 px or a
   mapper bank change", and `rf-nes` gives no bank state (the feed is `&[]`).
   Title to map to level all land at scroll (0,0). SMB3 map to World 1-1 (the A
   press at about f906) and every MM2 menu step logged **no jump at all**. A fade
   is a stretch of band-less frames (`writes []`, `uniform true` at SMB3 f482),
   which `SceneTracker` deliberately read as "no evidence".
3. **A Y wrap is a cut.** MM2's climb writes Y 0..239 and starts over at 240.
   The tracker's Y axis wraps at 480, so `236 -> 0` was read as 236 px
   backwards: world y went 428 -> 192 (f2245 -> f2246 of the stage walk). Every
   240 px of climb was a "cut" under the old 64 px rule, and the placement
   smeared. `WorldAxis::advance_y` now reads a small step across the 240 wrap as
   the step it is (`advance_y_reads_a_y_register_wrapping_at_240...`). Its one
   blind spot, a jump of exactly 240, is a no-step on the scroll and is left to
   the picture.

## 2. What the scene key is now

* The profile's `[[scene.level_id]]` bytes when the game says it is in a level;
  `hash(bytes)`, nothing about the picture.
* The picture otherwise: the set of dominant palette indices (at least 2 % of
  the background pixels), `palette_scene_id`. Title, map and level each draw
  with their own palette.
* Sections: a switch of the scroll axis after 256 px of travel on the other one.
* A cut is a key change, a bank change, a colour-histogram move over 30 %, a
  scroll jump over 64 px whose picture is NOT the previous one moved by the
  measured scroll, or an axis switch. A cut settles for 12 frames of a held
  picture or key before it is named (at most 120); the frames in between are no
  scene's and are not stitched. A 64+ px fall in a vertical section is the old
  picture moved by exactly the scroll, so it is not a cut (synthetic test
  `a_fall_of_80_px_a_frame_in_a_vertical_section_is_not_a_cut`; a door that
  lands 80 px away on another arrangement of the SAME colours is).

Cost: `SceneTracker` alone, 116 us a frame on SMB3 (release, M-series), 176 us
for the whole `observe_frame` including the stitch; a 60 Hz frame is 16 700 us.

## 3. Super Mario Bros. 3

Reached headlessly: `w250 S6 w120 S6 w400` (map), `R8 w60 U8 w60 A8 w300`
(1-1). A second level needed the first cleared: walk the scout's way (Mario's
Y at `$A2` held at 0x28, X words `$90`/`$75` poked +4 a frame, neutral pad) to
the end, then lower Mario under the end card (`$A2` 0x7A at X 0xA86) and jump
into it; after the card and the map, `R8 w60 R8 w60 A8 w300` enters 1-2.

| Byte | title | map | 1-1 (whole level) | 1-2 |
|---|---|---|---|---|
| `$0061/$0062` word | 0 | 0 | `$BC92` (after `$BB82 $BB8E $BBBC $BC0F $BC74` over f970-f975) | `$B0DC` (settles over 5 frames at f3194) |
| `$0065/$0066` word | 0 | 0 | `$C527` | `$C6AA` |

Both words are 0 from boot to the moment a level starts (f1..f969) and again
on the map after the level (f2444), and constant for the whole level
(`RF_WATCH=61,62,65,66`, f975..f2443). Rejected: `$070A` (0, 1, 0, 3: a tile
set, shared by levels), `$0608/$0609` (1, 2, 3, 0 inside one level: a counter),
and every byte of the first `smb3_level_id` diff that was not constant for the
whole of a level.

## 4. Mega Man 2

`$2A` is the stage: 1 for Air Man for the whole of the stage (horizontal
section and climb, f1167 on), and, with the other stages picked at the select
screen (`mm2_stage_bytes`): Wood Man 3, Bubble Man 5, Metal Man 2. It reads 0 on
the title, `07` on the menu and 2 at the select (a cursor), so the menus are
keyed too, one empty scene per cursor value.

**No section byte exists in what was measured.** `mm2_section_bytes` diffs the
last 120 frames of the horizontal section against the climb and finds `$37`
(0 then 3), `$FE` (a screen counter, 0x0B..0x14 as the camera moves, not a
section), `$43E/$440` (Mega Man's screen), `$543` (a flag flipping at unrelated
times). So the profile declares `sections = "axis"`: the camera stops going
right and starts climbing (world (2304, 12) to (2304, 40)).

## 5. Super Mario World

Reached by `reach_yi1` (as `w32_07_scout_smw.rs`). Yoshi's Island 1 has no pipe
a player can reach by walking (the stone-block walls at the first pipes stop a
star-powered Mario; `smw_find_pipe`: no colour-histogram jump in 3000 frames).
The pipe was entered the way the game enters one: standing Mario by the tall
pipe near the end and writing `$71` = 6 (the pipe-entry animation); the game
then loads the room its own exit table names, a coin room (orange pipe in the
ceiling, coins). Way out: the room's pipe at its far right behind two block
walls, reached by lifting Mario over them (`$96` held, `$94` poked) and walking
in; the game returns to Yoshi's Island 1.

| Byte | title and map | YI1 (whole level) | coin room |
|---|---|---|---|
| `$57` | 0 | `$70` | `$12` |
| `$68..$6A` | stale (`00 D9 FF`..) | `$0CD900` | `$0CE8FE` |
| `$65..$67` | stale | `$0689F7` (a layer pointer) | `$068A2E` |

`$57` goes from 0 to non-zero as the level loads and decides on its own;
`$68..$6A` is level-specific but holds stale values on the map, so it joins the
key with `decides = false`. **`$65..$67` was rejected:** in one run it changed
to `A8 BD 00` mid-level (f3445, camera x 1295), so it is a streaming pointer,
not an id. `13BF` (the node under Mario) holds `0x29` on the map and in the
level: it names a node, not the room. The pointers settle several frames
before `$57` flips (eight, at the pipe): that run produced a one-frame scene
before the room until the settle length was raised from 6 to 12 frames.

## 6. Results (profile rows on, headless, cold model)

SMB3 (`smb3_scenes_with_the_profile`), scene ids at each screen:

| screen | with the profile | without |
|---|---|---|
| title | `9eaa33c1c3901943` | `9eaa33c1c3901943` |
| map | `afabf125b34785df` | `afabf125b34785df` |
| 1-1 | `e782ed12b749a777` | `e3314b3bae070f0c` |
| map after 1-1 | `afabf125b34785df` (the map again) | same |
| 1-2 | `a1115276d017ca61` | `555c100aa29d0f87` |

The old constant `6570426d2c2e2fa3` appears nowhere. 1-1's canvas grows to
460 112 cells in its own scene and is not touched afterwards.

MM2 (`mm2_scenes_with_the_profile`): stage select `40feb1adc11c8d02`, Air Man
horizontal `40feb0adc11c8b4f`, the climb `195deb00a6087f61`; the horizontal
canvas holds 428 338 cells when the climb starts and 428 338 after, and one
edge joins them, `continuous`. Without the profile the axis rule gives the same
split (`mm2_scenes_without_a_profile`).

SMW (`smw_scenes_pipe_room_and_back`): map `4fb5e4518afd13ff`, YI1
`e884f34accfb9fb4`, coin room `79cfe03a304fdfc1` (23 383 cells); the level's
canvas holds 133 736 cells before the room, 133 736 while the room is on screen,
and after coming out the scene id is the same `e884f34accfb9fb4`, 172 094 cells
(the level goes on), of which 133 716 of the original 133 736 cells still hold
(20 moved: animated tiles). The edge level -> room is not continuous and the
level has no margin neighbours.

## 7. Open

* No byte for the MM2 section (axis rule instead). No second SMW level was
  reached, so the SMW rows are proven for one level and its room, not for a
  second level's id.
* SMB3's pointer words were read in worlds 1-1 and 1-2 only.
* The margins compose from the current scene's canvas alone today, so no margin
  shows another scene at all; `CanvasModel::margin_neighbours` is the one rule a
  neighbour fill must go through (continuous edges only). Persisting
  `SceneEdge` per ROM is W33-05 (the type is plain integers, `to_bytes`).
* `scene_identity::compute_scene_id` is no longer called by the tracker; it
  stays for its own tests and the harness baseline.
