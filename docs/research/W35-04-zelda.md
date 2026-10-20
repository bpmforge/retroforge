# W35-04: A Link to the Past (SNES) and The Legend of Zelda (NES), scout settings found by observation (evidence)

Clean-room: everything below was watched in the emulator (the finder, and the probes in
`crates/retroforge/tests/w35_04_zelda.rs`, ROM-gated, `#[ignore]`). D-021 respected: no disassembly, no
zelda3 source, no decomp-derived rows; every row of the table carries `source = "observed: auto
(W33-07)"`. The shipped `profiles/snes/the-legend-of-zelda-a-link-to-the-past/profile.toml` was not
touched (the screens test compares its bytes before and after; it has no `[scout]`, none was added by
hand). ROMs: `Legend of Zelda, The - A Link to the Past (USA).zip`, `Legend of Zelda, The (USA).zip`.
Release builds, one process at a time under an RSS watchdog, cold canvas cache (fresh `TMPDIR`) for first
visits.

## 1. Where free play starts (A Link to the Past)

Script `alttp_to_free_play()`, frames at 60 Hz: boot and title (~900), Start, Start, Start to the file
screen, `A` on a letter, Start, then the register screen's `A` (~2,200); a telepathy speech ("Help me...",
mode `$10 = 0x0e`) that pages only on `A` and types itself out slowly (16 presses, `A3 w150` each, to
frame ~5,500); Link is up out of bed in the house (`$10 = 7`, `$1B = 1`). Then `R32 D60 D60 D40 L14 D40`
to the door and out (`$10` 0x0f, 0x10 while it loads, then 9): Link stands outside in the rain at frame
~6,800 (`$10 = 9`, `$1B = 0`, x `0x08b8`, y `0x0b37`).

What the area is, observed:

* Outside the door Link is in a nook: bushes left and right at his row, the door above (`U` goes back in),
  a ledge below; `D` drops him onto a dirt road that runs west (up into a forest, trees stop him at x
  `~0x0700`) and east (two soldiers at x `~0x09c0` stop him with a speech and push him back: never walk
  there). The road is `0x0600`..`0x0900` of camera, a vertical corridor in the trees gives `0x0a7e..0x0b1e`
  of camera Y.
* Link walks 1.5 px a frame (steps 1, 2, 1, 2). The map layer (BG2 on the main screen) moves with him
  exactly (same steps, same frames) inside the scroll zone. Walking off an area's edge scrolls the camera
  8 px a frame (16 every other frame) while Link stands (a screen-edge transition), then drops him on the
  far side; the scene tracker sees a continuous scroll (the same scene before and after), which is what
  the stitcher wants.
* BG1 is a rain overlay on the SUB screen (BG1 is not on the main screen; its scroll registers still run:
  about 2/3 of the camera's speed in x, steps of half a tilemap in y every few frames, and per-line
  registers that wobble). It is why most of the generic work below was needed.
* Script traps: `X` opens the map and nothing closes it; the telepathy speech comes ~1,500 frames after
  the house wherever Link is (once per game) and holds him until `A` has paged through it: the session
  carries five `A3 w150` presses after every long leg (`alttp_dismiss()`); a soldier's speech does the
  same; `A` is "lift/talk", harmless with nothing in front; the house door is straight up from the spot
  outside (a second visit to the house is a scene cut, below).

## 2. The table the session found

One scripted session (`alttp_play()`: back through the house door and out, west along the road to the
trees, the vertical corridor there, east and west across the `0x0800` and `0x0900` page boundaries, back
to the road middle; ~14,000 frames in all with the dismissals), fed to `retroforge::scout_find::ScoutFind`
exactly as the core thread feeds it. 78 s of wall time from the first frame of the session to the table.

| Row | Address | Evidence |
|---|---|---|
| `camera_x` (u16) | `$7E011E` | held BG2's H-scroll low byte on 95% of 454 scrolling frames; the carry check found the high byte. `$E2` (the register's other copy) is the same value; the finder keeps the higher-scoring byte |
| `camera_y` (u16) | `$7E0122` | held BG2's V-scroll low byte on 100% of 158 frames the screen scrolled vertically |
| `player_x` | `$7E0022` (high `$0023`) | stepped with the camera on 100% of 452 scrolling frames, up on Right, down on Left, kept moving on 438 frames the camera stood still (the dead zone, the area edges) |
| `player_y`, `player_y_hi` | `$7E0020`, `$7E0021` | stepped with the camera's Y on 115 of 117 frames the screen scrolled vertically while Up or Down was held; the Down pass from the road took the canvases to 3.3 screens |
| `start_when` | `$7E0130 = 3`, `$7E0133 = 3` | weak, see section 5: bytes that read 3 on 97% of the session and something else off it, not a mode |
| `hold` | none | a top-down game: a hold floats the player above the ground, which means nothing here; none is tried or written |

Not in the table, observed: `$1B` reads 1 in the house and 0 outside (the finder's top gate candidate in the
headless sessions, rank 0, with `$A2 = 4`); `$10` is the mode (7 interior, 9 overworld, 0x0e a speech, 0x0f
and 0x10 the door's load); `$11` a submode that counts the area-edge scroll. The finder names the camera
words `$11E`/`$122` (higher scoring copies) before `$E2`; the scout reads either. Bytes of the speech's text
box answer `A` like a jump does (they passed the jump test with the dismissals in the session); they are not
Y (section 4.2).

## 3. Acceptance numbers

| Check | Result |
|---|---|
| Table found, no `[scout]` | yes (`alttp_acceptance` through `ScoutFind`; `discover_alttp` through the trials: width 1,024, agreement 99.8%) |
| Saved to the user profile path through "Save scout settings" | 7 rows (`camera_x`, `camera_y`, `player_x`, `player_y`, `player_y_hi`, two `start_when`) plus the scout block, written to `RETROFORGE_CONFIG_DIR/retroforge/profiles/snes/the-legend-of-zelda-a-link-to-the-past/profile.toml`; shipped profile bytes unchanged (asserted) |
| Road scouted, in the app, from the saved profile | started 1, finished 1, aborted 0; 929 scout frames from the middle of the road, 148,974 cells merged; passes Right, Left; rung `player_move@4px`; agreement **99.9% over 331,288 shared cells** (headless driver, `alttp_acceptance`: 608 wide, 99.9%; headless discovery from the start of the road: 1,024 wide, 99.8%) |
| Scene cuts | the house and the overworld are different scenes: indoors flag 1 `{0x7e4f8ffcf4928992}` (the house), outdoors flag 0 `{0x816401acddefffd0, 0xf1b2a331d67496c3}` (the road, and the door's exit fade, mode 8), disjoint (asserted). The road stays ONE scene across every area edge of the session (the `0x0800` and `0x0900` crossings, both ways, five times: scene `...5360` throughout): edges are continuous scrolls, not cuts, so nothing splits there and the stitcher joins both sides |
| Second visit | new app, same cache and profile folder: first sample at live frame 14 already has the 229,376 remembered cells, margins 114,688 of 121,856 known (94%), scouts started 0, skipped 1 |
| Regression, discovery tests | SMW 5120/5120 px (agreement 100.0%), SMB3 2816/2816 (99.7%), MM2 2560/2560 (98.7%), MMX 2820 px (100.0%), Super Metroid corridor 512 (99.8%, was 99.7%) and shaft 706 (99.7%), Castlevania courtyard 768 (100.0%): identical to before |

Shots (uncommitted): `docs/screens/w35/alttp-32x9-{off,on}.png`, 800 x 224. OFF: the 256 px game picture in
the middle (x 272 to 528) with the HUD at its top, Link on a dirt path in the rain, the cliff edge crossing it;
both margins a flat green (the backdrop colour; nothing has been stitched). ON: the left margin shows three
big round-crowned trees with dark bush clumps under them and the dirt path and cliff edge continuing to the
join; the right margin shows the dirt path, a long cliff ledge and rows of boulders continuing the cliff edge
across the join; the cobbled lines carry over, no gap. Not good: the middle picture is darker than the
margins (the rain overlay tints the live picture through colour math and the stitched canvas holds only the
map layer, so there is a visible brightness step at x 272 and 528); the margins have no rain and no HUD;
the picture is one instant (Link and the animated water in the margins are not animated); the margins reach
no further than the scout went (the road, not the forest beyond it). The 8-way picture of "ahead and behind":
the road scout fills both sides because it ran Right and Left.

## 4. What the generic code needed (each with a test)

1. **The layer the finder watches.** The finder takes the first background whose scroll moved. BG1 (switched
   off, or only on the sub screen as a colour-math overlay) moves by itself at its own pace, so the camera
   finder named BG1's word and Link's X matched it on 87 of 1,113 frames (no player was named). The feed
   (`scout_find::finder_snes_scroll`, also the test rig) now gives a layer no registers unless it is on the
   main screen, or on the sub screen when no layer is on the main one (Super Mario World's hills still
   count when BG1 is absent). `camera_matches` (the driver's check that the plan's camera word agrees with
   a live layer) now accepts any layer: the first stitched layer here is the rain.
2. **A player Y for a game with no jump.** The finder named Y only by what a byte does at a jump press; here
   no byte moves at A/B/X/Y, so `ready()` was never true and no trial ever ran. `Frame` gains a vertical
   pad (`Vert`) and the finder a walk test, the twin of the X one: a byte that steps with the camera's Y,
   rises on Down, falls on Up, and keeps moving the way the pad is held while the camera's Y stands still
   (`a_top_down_game_names_the_player_by_walking_in_both_axes`: a synthetic top-down game, a half-speed
   layer switched off, no jump). `Observed::top_down` says the Y was named that way. A byte that answers a
   face button (a text box's cursor, paged with A) looks like a jump's Y: when a walk-found Y is not the
   byte a jump named, the walk's bytes are the only candidates (the same test, a junk byte). A camera that
   eases back the other way while the player pushes a wall is not counted (`down`/`up` direction).
3. **Scripted scrolls.** A screen-edge transition scrolls the camera 8 px a frame while the player stands:
   half the camera-moving frames of the session, so Link's X matched 44% (bar 85%). The player (and the Y)
   is also judged over the camera's walking-pace frames alone (`|step| <= 4`)
   (`a_scripted_scroll_does_not_hide_the_player`).
4. **The camera's own word is not the player.** (NES Zelda, below.) A scroll mirror that holds still for a few
   frames while a direction is held passes the wall test; it is now excluded by offset
   (`the_cameras_own_word_is_not_the_player`, which fails without it: wall 6).
5. **Memory.** The first scout trial took 9 GB in 90 seconds. The rain layer's per-line registers placed one
   row of BG1 18,000 rows below the picture, and a canvas is one dense rectangle (`Canvas::ensure_bounds`
   allocates the gap): one row, 14 million cells. Two guards: `Canvas::may_cover` refuses a row that would
   take a canvas past 65,536 a side or 8 million cells (Super Mario World's widest level is 2.2 million;
   `a_row_far_from_the_canvas_is_refused`), and `LayerAxes::is_wild` keeps a layer that moves more than
   24 px a frame, four times in a few frames, out of the canvases (`a_layer_that_jumps_every_frame_is_wild...`).
   The app with widescreen on would have taken the same memory on this game: the test rig now keeps the sub
   screen and the per-line registers as the app does (`set_capture_wide`), which is how it was seen.
6. **Trials.** The extent a trial judges is the biggest single layer's (`Scout::main_extent`), not the sum: a
   rain layer's junk passed the 1.4 screen floor with the camera standing still. A recipe that grew nothing
   to the right is tried to the left (the player stood at the end of a camera). No hold is tried for a
   top-down game; its vertical pass runs from the room the trials start in (`confirm_y` with the main
   snapshot). The skeleton's Y rows say how the Y was named.
7. `explore_*` probes in `w35_04_zelda.rs`, `Scout::layer_dims`, `ALTTP`, `ZELDA1` and the session builders
   in `w33_07_common`.

## 5. What failed, honestly

* The gate is still not a mode byte. `$0130 = 3` / `$0133 = 3` hold on 97% of the session and differ off it;
  `$1B = 0` and `$A2 = 4` rank first in the headless sessions. None is "outdoors, in control, not in a
  speech": a speech is mode `0x0e` and the finder's candidates do not separate it. The survival checks keep
  them honest (the scout started in the app) but a scout can start during a speech.
* No safe hold, by design: the soldiers and the telepathy are not things a hold avoids; the scout (a second
  machine) pokes Link through trees and the self-check rejects what disagrees.
* The scout runs one axis per scene (`scene_axis`): the road scout ran Right and Left (what a 32:9 margin
  needs); the Down/Up pass was only tried by the discovery trials (`vertical 0x20`: 3.3 screens), not in the
  app. 8-way in one run is not built.
* The session needs the dismissals; the telepathy and the soldiers' speech hold the player and a
  snapshot taken during one (`ScoutFind` starts its trials after 20 idle frames) scouts nothing.
* The area-edge transition and its far side were not scouted separately; nothing here shows the next area
  after the edge (the poke stops at the camera clamp, `0x0600` and `0x0900`).

## 6. NES: The Legend of Zelda

Profile (`profiles/nes/legend-of-zelda/profile.toml`): `[decode] kind = "room_grid"` with no `[decode.room_grid]`
(sizes not sourced), no `[decode.collision]`, `full_level = false`, no `[scout]`. Hence the `DecodedLevel`
margin source has nothing to decode and fills no neighbour screens.

Free play: boot, Start to the select screen, Start on "REGISTER YOUR NAME", `A` types a letter (the heart is
the slot, Select moves it, three Selects reach REGISTER), Start returns to the select screen, Select twice
more and Start begin slot 1 (`Z1_TO_PLAY`: Link on the first overworld screen, map `$EB = 0x77`, X `$70`,
Y `$84`, mode `$12 = 5`). Right off the screen the map byte steps to `0x78`; the scene tracker (no `[scene]`
key) calls it all one scene.

Shot `docs/screens/w35/zelda1-32x9-on.png` (856 x 240, uncommitted): the 256 px game picture in the middle
(x 300 to 556), the HUD above, both margins pure black. The snapshot was loaded into a fresh app after a walk
over the first screen's right edge and back, so the live canvas knows nothing of the neighbour screen:
the margins show what the live stitcher has seen since the load, which is nothing.

Does the scout apply to a screen-flip overworld? Observed with the generic finder on a session of four
crossings of the first screen's edge (`observe_z1`, `discover_z1`):

* The camera moves only in the flips (512 moving frames; the scroll register steps 4 px a frame for 64
  frames, Link stands). Link's X `$70` never steps with it (0 of 512) and moves 1,028 frames with the camera
  still: the player finder rightly names nobody.
* It named `$FD` (the scroll mirror) as the player: it holds still for 8 frames with Right held (the mirror
  updates a frame before the register). The scout poked the scroll mirror, the game scrolled the same
  screen's nametable round, and the stitcher joined it to itself: a canvas 8,187 wide at 100.0% agreement
  from one screen. That is a false positive and is fixed (section 4.4, with a test: a camera word is not the
  player); the shipped result is no table, as it should be.
* A walk (the NES `walk` recipe, Right held) with the trials' patience (90 frames without a new cell) never
  reaches the edge (120 frames of walking show nothing new). With 300 frames of patience it reaches two
  screens (512 wide, extent 122,880) but the scout's deltas disagree with the live canvas where the second
  screen scrolls in: **82.9% agreement**, two deltas rejected at 52% and 57% by the self-check, nothing
  merged from them. So the scout fills no margins here. The cause was not found in the time (the scroll
  transition's rows are placed by the band tracker, which sees the HUD band fixed and the playfield band
  moving 4 px a frame; the frames of the transition agree with themselves and not with the live screen
  before it). I did not keep the patient-walk change: it would write a table that the app's own check
  mostly rejects. It is the next step for a screen-flip game; the generic finder says "no player" now.

## 7. Run

```
cargo test --release -p retroforge --test w35_04_zelda -- --ignored --nocapture alttp_acceptance    # ~3 min
RF_SCREENS_ONLY=alttp-scout  (w30_screens, scripts: shot.sh)                                        # ~2.5 min
RF_SCREENS_ONLY=z1-margins   (w30_screens)                                                          # ~10 s
cargo test --release -p retroforge --test w35_04_zelda -- --ignored --nocapture observe_alttp       # RANK lines, the probes
cargo test --release -p retroforge --test w35_04_zelda -- --ignored --nocapture discover_alttp      # ~1.7 min
cargo test --release -p retroforge --test w33_07_discovery -- --ignored --nocapture discover_smw    # etc.
```
