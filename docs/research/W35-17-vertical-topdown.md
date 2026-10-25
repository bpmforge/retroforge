# W35-17: up/down scouting for top-down games (The Legend of Zelda, A Link to the Past)

Observation only (CLAUDE.md law 5, D-021): every figure is a measurement of the running game through
the harness (release, one process at a time, cold canvas cache, RSS watchdog 6 GB). No disassembly,
no decomp. ROM-gated runs are in section 8. "Playfield rows" of Zelda 1's margins are the 176 rows
below the status bar (the bar's 64 rows are black in the margins on purpose, W35-07); "all rows" counts
every picture row.

## 1. Measured first (code of main `25e7842`, then)

| game, protocol | margin cells known |
|---|---|
| Zelda 1, scouted from the first screen (Right, Left), at the start | all rows 106,200 of 144,000 (73.8%); playfield rows 105,600 of 105,600 (100%) |
| the same, after Link walks one screen up | **0 of 144,000 (0.0%)** |
| A Link to the Past, the road, X passes, at the start (level layer, 32:9, 272 px) | 94,125 of 121,856 (77.2%) |
| the same, with Link pushed 224 px up / 448 down / 224 up | 94,125 / 91,157 / 93,695 (77.2 / 74.8 / 76.9%) |

Ranked causes, with how each was checked.

**Zelda 1 (0.0%).**

1. *The live canvas is thrown away at the end of every vertical flip (verified, fixed).* A per-frame
   trace of the live registration shows 46 flip frames on which the playfield band disagrees with the
   canvas (988 of 2,781 samples agree, 177 rows withheld, frame after frame) against `STALE_FRAMES = 45`:
   the candidate canvas is promoted as the flip ends and the canvas becomes the new screen alone
   (256 x 356, `resets` 1). It happens with nothing scouted too (a second rig that only walked up ends
   with the same canvas), so it is not the scout's data that is judged wrong. Why a right canvas
   disagrees with the flip: the tracker's world puts the playfield band 64 rows off for its first frames
   (its first step is -61 where the picture says +3: the status bar's gap between the two screens, which
   one band cannot cross), and the new screen's rows stream in while it scrolls.
2. *Nothing is known beside the screen above (verified).* The scout runs one axis per scene, and a Y
   column would not help anyway: after the flip the margins are the screens to the side OF the screen
   above, which a column does not see.
3. *Twenty rows of junk above the first screen (verified, worked round).* One stray frame at the game's
   start (a `$2005` Y of 242 on the cut from the file screen) leaves twenty rows of status-bar blue at
   world rows -20..-1 of the first screen's column. They are the only live cells under the screen
   above, they disagree with it, and their 11% of the band's samples decide: the check called the
   canvas stale (the scouted rows agree 91.3%, the bar is 90%: by a hair).

**A Link to the Past (77.2%, unchanged by walking).** The camera's vertical window on the first area is
~160 px (`0x0a7e..0x0b1e`, W35-04) and the road is walled: a poke of Link's Y 224 px up is put back
(Y `0x0b78` before and after, camera Y moved 7 px), so there is no screen up or down to walk to there.
The 22.8% unknown is the area's X edges (the camera clamp at `0x0600` and `0x0900`, W35-04), the same at
every height. Nothing in the first area can show a vertical gain (section 5).

## 2. What was built

* **Table.** `dirs` may name both axes (`["right", "left", "down", "up"]`; the profile loader already
  accepts all four). `ScoutPlan::two_axis` / `passes_for`: a scene gets the table's X passes then its Y
  passes, whichever axis the scene scrolls on, when the recipe can move both. A one-axis table is
  exactly what it was (`dirs_for`).
* **Discovery (observation only).** A table gets Down and Up when the game is SEEN to scroll both ways
  under a walking player. SNES: `Observed::top_down` (the finder named the player's Y by walking in
  both axes) and an Up trial that grew as far as the Down one (`confirm_y`). NES `walk`: one way along Y
  as far as 1.4 screens, then, from where that stands (`start_from_here`, as W35-08's Left), the other
  way as far (`walk_vertically`; Zelda 1's first screen is on the bottom row of the map, so only Up is
  open from the start). A platformer's walker cannot climb: it fails the first half.
* **Passes** (`Worker::run_second_axis`), after the X passes, from the restored snapshot, merging
  unknown cells only (the rule is unchanged). Each Y pass walks until the camera has gone half a picture
  twice (two depths: a screen up or down in a game that scrolls freely, two flips of Zelda 1's) and leaves
  a *branch* (a snapshot of the machine and of the tracking: what `start_from_here` seeds) at each
  depth, two per depth, 60 and 100 px past it (where a walker stands decides which gaps in the trees a
  sweep from there meets: measured on Zelda 1, 100 px alone left a 44 px strip, 60 px alone 100%, both
  are kept). From every branch an X sweep (Right, Left) runs until its margin is covered (`reach_px`) and
  the screen that did it has stopped. A sweep from a branch is the diagonal a column cannot give: it is
  what fills the margins at that depth. The zigzag of W35-09 (`sidestep_pad`) serves the sweeps (a second
  try with the zigzag the other way up when the first got no further) and the Y passes (sideways of the
  way walked).
* **Budget.** The first axis is untouched (priority). Total scout frames are capped
  (`MAX_TOTAL_FRAMES` = 24,000, checked before each added pass); each added pass has the pass budget and
  its stall rule as ever; branches are bounded (2 depths x 2). Zelda 1: 12,739 scout frames in the app
  (2,138 before), 13,619 headless; A Link to the Past 4,474 in the app (930 before). All on the worker
  thread, half a minute of wall time at most; nothing on the live thread.
* **What a pass off the first axis does not do.** It is not judged against the live canvas (it lies away
  from what the live game drew, and where they meet the live canvas holds junk, cause 3). It sends its
  cells at its end only (a flip streams the new screen's rows in while it scrolls and a merge writes
  unknown cells only: a delta cut mid-flip left blue lines in the screen, seen). A sweep stitches only the
  rows of its own screen (`clip_rows`) and ends before a frame that scrolls the other way
  (`vertical_scrolled`: the tracker cannot place a flip's seam).
* **Registration (`rf-enhance`), the live canvas, NES bands only** (`guard_rows_scrolling`; SNES and
  every caller of `guard_rows` are exactly as before). (a) A frame the camera crossed vertically at 3 px
  or more (`FLIP_PX`, and the six after it: a flip steps every other frame) that the canvas disagrees
  with is no evidence for or against it: no confidence step, no candidate (cause 1). (b) When every
  judged band disagrees, a second opinion counts all known cells, a scout's too, at 80%, and spares the
  canvas when it agrees and the live cells are a quarter of the evidence at most: a canvas whose few live
  cells are stale and whose scout cells agree is not stale, and the frame counts for it; its rows stay
  withheld, a live cell is never replaced here (cause 3). A stale canvas is a different picture and
  matches by chance only. (A first version took horizontal scrolls and the SNES too; an existing test and
  the A Link to the Past canvas of the second visit showed it was wider than the fault.)

## 3. Results, Zelda 1

Headless (`z1_margins_after_walking_up`, the real driver, `reach_px` 300, table `right, left, down, up`):

| | margin cells known |
|---|---|
| at the start | playfield rows 105,600 / 105,600 (100%) |
| one screen up (`U70`) | **playfield rows 105,600 / 105,600 (100.0%)**; all rows 106,200 / 144,000 |
| main's code, same walk | 0 / 105,600 (0.0%) |

In the app (`z1-up`: the table found from the first screen, saved through the card, cold cache, 32:9):
scout started 1, finished 1, aborted 0, 12,739 scout frames, agreement 99.7% over 378,294 shared cells;
margins 106,200 of 144,000 at the start and **106,200 after walking one screen up** (the playfield rows
105,600 of 105,600; the other 600 are in the bar's rows). Second visit, a new app over the same cache:
scouts started 0, skipped 1, margins full at live frame 16.

## 4. Agreement with the game

Zelda 1, `z1_scouted_screens_against_what_the_game_shows`: a second rig WALKS to each screen and its
picture at rest (sprites, the status bar's rows and the twenty junk rows left out) is compared with the
scouted canvas at the same world cells.

| screen (rig walked) | cells compared | agree | same shapes under a palette swap |
|---|---|---|---|
| the screen above the first | 39,272 | **99.98%** | 99.98% |
| the one to the right of it | 44,898 | 85.57% | **100.00%** |

The second is a forest after brown trees: the game loads its own palette as the flip ends, the stitcher
writes only while the camera moves (every game, untouched), so the cells carry the palette of the flip
and every tree is one colour index off; the shapes are all right. It is the one screen of the 98% bar
that misses, by colour, not by structure.

A Link to the Past (`alttp_two_axes_on_the_road`): a second machine that only walked the same legs,
every cell both know: level layer 99.97% to 100.00% over 55,201 to 58,331 cells depending on the walk
(the same with X passes alone), the rain layer and the HUD layer 100.00%. With the Y passes the scouted
canvas knows every cell the walker saw (`walked-only` 0, 3,859 with X passes alone).

## 5. Honest gaps

* A Link to the Past: a live walk a screen up or down could not be scripted in the first area (section 1:
  camera window ~160 px, walls, a text box), so no vertical margin gain was measured there. The machinery ran: the finder's table carries `down, up`
  (`alttp-up-FIND`, the app's own flow), the road scout runs 10 passes with 4 branches, the canvas grows
  to 926 x 1,410, agreement 99.7% over 873,902 shared cells in the app. The margins are the X passes'
  (57,344 of 121,856 in the app's primary layer, the same as on main). The shot
  `docs/screens/w35/alttp-up-32x9-on.png` is Link walked up as far as the road lets him (`U150`: he
  barely moves); the left 113 px of the margin are black: the area's edge.
* Where the in-app finder stands decides its table. Its trials start at the player's first idle after
  the session: on Zelda 1 with the `Z1_WALK` session that was x `0x3c`, y `0x8d` of the first screen
  (trees above), and the table stays `right, left`. From the first screen's middle (`discover_game`'s
  snapshot) it gets `down, up`.
* Beyond the first walls a sweep's walker dies (Zelda 1's monsters) or flips away: what it stitched
  before stays, what is beyond is black, not guessed. The far 12 px at the left of the Zelda 1 shot are
  that.
* The stitcher's rule that writes only while the camera moves (the palette above) and the twenty junk
  rows under the first screen are still in every Zelda 1 canvas.
* A second visit of A Link to the Past: margins full (57,344) at live frame 15 as on main; the canvas'
  own cells (the live picture's 57,344 among them) are all back at frame 28 (main: 14). Not understood
  further than the SNES live path being the same code (the shot picture of the committed `alttp-scout`
  was not re-committed: its table is two-axis now, so it is another run).

* **Back down.** Walking one screen up and then back down to the first screen: the tracker names that
  screen a different scene, the live canvas is a new one (256 x 240) and the margins are 0% until that
  scene's own scout runs (the app starts one; the cache makes a later visit instant). Measured
  (`RF_WALK="U70 w100;D70 w100 D70 w100"`): up 100%, back down 0%. Zelda 1's first screen has no screen
  below it, so "one screen down from the start" does not exist there; this return trip is the nearest.
* **Reach on the SNES.** `margin_covered` compares the camera word with the canvas' world x, two scales,
  so a sweep there ends early (limited to bands it spans 2,540 px past the area edge, seen); left, a gap.
* **The shots, looked at.** `zelda1-up-32x9-on.png`: the live picture in the middle with its status bar
  (the 64 rows over both margins are black); left, the screen to the left of the screen above (tan
  ground, rows of brown rock items, green trees top and bottom), right, a grid of round green trees; no
  seam at either join, Link and the monsters only in the picture, a 12 px black strip at the far left.
  `alttp-up-32x9-on.png`: the road area, the left 113 px black (the area's edge), cliff ledges and
  boulders to the right, rain only in the live picture.
* The live walk in A Link to the Past meets a telepathy text box and trees: that is why no live vertical
  walk was measured there, not a proof none exists (W35-04 scouted a 160 px corridor).

## 6. Regression (one-axis tables, release, one at a time, main's number in brackets)

| game | table dirs | width | agreement |
|---|---|---|---|
| SMW `discover_smw` | right, left | 5120 | 100.0% (100.0%) |
| SMB3 | right, left | 2816 | 99.6% (99.7%) |
| MM2 | right, left | 2560 | 99.0% (99.0%) |
| MMX | right, left | 2820 | 100.0% (100.0%) |
| Super Metroid | right, left | 512 | 99.8% (99.8%) |
| Castlevania courtyard | right, left | 768 | 100.0% (100.0%) |
| ALttP `discover_alttp` (from the road) | right, left | 1024 | 99.8% (99.8%) |
| Zelda 1 `discover_z1` | right, left, **down, up** | 768 | 99.8% (right, left: 99.8%) |

App shots, re-run on the final code, main's run of the same shot in brackets where I ran it:
`cv-scout` 985 scout frames, 100.0%, margins 75,618 (W35-08: identical), second visit 16;
`smw-left` 1,071 frames, 100.0%, margins 282,807 (W35-08: identical); `z1-scout` 2,138 frames, 99.7%,
margins 106,200 (W35-09: identical); `scout-nes` SMB3 99.8% over 372,663 cells (99.7% over 372,660), MM2
3,680 frames, 98.8% over 1,847,615 cells (3,680, 98.8%, 1,847,615: the same); `scout-mm2-climb` 1,267
frames, 94.9% over 744,188 cells (the same). Tests: rf-enhance 476 passing (two new), the gate is below.

## 7. Second visit

Zelda 1 `z1-up`, a second app over the same cache and profile folder: scouts started 0, skipped 1,
margins 106,200 (the whole of them) at live frame 16. `alttp-up`: 0 and 1, margins 57,344 at frame 15.

## 8. Run

```
cargo test --release -p retroforge --test w35_17_vertical -- --ignored --nocapture z1_margins_after_walking_up   # ~35 s
cargo test --release -p retroforge --test w35_17_vertical -- --ignored --nocapture z1_scouted_screens_against_what_the_game_shows
cargo test --release -p retroforge --test w35_17_vertical -- --ignored --nocapture z1_discovery_adds_down_and_up  # ~20 s
cargo test --release -p retroforge --test w35_17_vertical -- --ignored --nocapture alttp_two_axes_on_the_road    # ~4 min
RF_DIRS=right,left ...                                                                                           # the one-axis baseline
RF_SCREENS_ONLY=z1-up / alttp-up (w30_screens, scripts: shot.sh)                                                 # ~1 min / ~4 min
```

## 9. W35-19: a blocked trial is not a verdict (the in-app Find gap of section 5)

Observation only. Cause (checked by the in-app run, table `right, left` before): `walk_vertically` tried
Up, then Down, only from the snapshot where the finder's trials start, the player's first idle (Zelda 1:
x `0x3c`, y `0x8d`, trees above). Up was blocked there (extent under the floor), Down has no screen below
the first one, so the table never got `down, up`; from the first screen's middle it did.

Fix (generic, `scout_discovery.rs`): when both halves fail from the snapshot, the same two trials run
again from where a 120-frame sidestep (`SIDESTEP_FRAMES`: Right, then Left) leaves the machine
(`start_from_here`, as W35-08's midway Left); the first start that grows both ways decides. A sidestep
that dies (scene change) is skipped. Only the NES `walk` recipe with a Left verdict reaches this code.

| | before | after |
|---|---|---|
| in-app Find (`z1_in_app_start_gate`: power-on, play, Find pressed in the field from Link's first idle) saves | `right, left` | `right, left, down, up` |
| Find answer, wall time after pressing Find (paced, the play script included) | 41.3 s | 41.8 s |
| `z1_margins_after_walking_up` (that table) | | start 105,600/105,600 playfield rows; after `U70` 105,600/105,600 (100%) |
| `z1_scouted_screens_against_what_the_game_shows` | | screen above 99.98% (same shapes 99.98%); the one to its right 85.57% (shapes 100.00%) |

Regression, discovery re-run (dirs, agreement): SMW right, left, 100.0%; SMB3 right, left, 99.7%; MM2
right, left, 99.0%; MMX right, left, 100.0%; Super Metroid right, left, 99.8%; Castlevania right, left,
100.0%; ALttP `discover_alttp` right, left, 99.8% (its `down, up` come from the SNES vertical-room path,
untouched: only the NES walk code changed); Zelda 1 `discover_z1` right, left, down, up, 99.8%. The sidestep
trials only run where the first ones fail, so they cost time only for games that were going to end without
Y passes; the platformers above were unchanged.

Run: `cargo test --release -p retroforge --test w35_11_start_gates -- --ignored --nocapture z1_in_app_start_gate` (~3 min).
