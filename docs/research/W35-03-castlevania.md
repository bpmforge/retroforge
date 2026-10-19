# W35-03: Castlevania (NES), scout settings found by observation (evidence)

Clean-room: everything below was watched in the emulator (the finder and the probes in
`crates/retroforge/tests/w35_03_castlevania.rs`, ROM-gated, `#[ignore]`). D-021 respected: no
disassembly, no decomp-derived rows; every row of the table carries `source = "observed: auto
(W33-07)"`. The shipped `profiles/nes/castlevania/profile.toml` was not touched (the screens test
compares its bytes before and after; it has no `[scout]`, none was added by hand). ROM: `Castlevania
(USA).zip`. Release builds, one process at a time, cold canvas cache (fresh `TMPDIR`) for first visits.

## 1. Where free play starts, what the stage is

Script `CV_TO_STAGE` = `w400 S6 w600`: Start at frame 400, the intro walk, Simon in the courtyard at
frame ~1,006 (stage 1, TIME 0294). `$18` reads 1 (title), 3 (Start pressed), 4 (the intro), 5 (play).
Simon walks one pixel a frame (`R60` = 60 px). Block 1 (the courtyard) is three screens: the camera
runs 0 to `0x200`, Simon reaches the door at `$0040/41 = 0x02DB`, the screen stops, he walks into the
door and `$28` (the block counter, 0 then 1) steps; the entrance hall follows. Observed, not looked up.

* Scene cuts at the door: the scene key changes from the courtyard's to the hall's at the door and
  nowhere else in the courtyard (`cv_acceptance`: block 0 -> one scene, block 1 -> another, the sets
  are disjoint). The death/respawn in the hall (below) is a cut of its own (`None` while it settles).
* Danger: the courtyard is quiet (hp stays `0x40` for 644 px of walking). The hall is not: hp falls
  from `0x40` to 0 in the first ~1,600 frames of walking right (8 per hit, enemies not identified;
  a hit moves Simon back a few px), Simon dies near `$0040 = 0x0516` (frame 3,466), lives 4 -> 3, and the
  game restarts the block. The scout therefore needs to stay out of the hall or stay unhurt.

## 2. What the finder named (`observe_cv`, one scripted session)

Session `CV_PLAY` (2,534 frames): left to the wall, six standing jumps (`A`), right to the door with a
jump every 60 frames, through the door, four more walks, stand.

| Row | Address | Evidence |
|---|---|---|
| camera X (16-bit) | `$002E` (low) | held the scroll register's low byte on 100% of 576 scrolling frames; `$00FD` (the profile's cited row, the register's mirror) is the same value |
| player X | `$0040` (+ `$0041`) | stepped with the camera on 100% of 576 scrolling frames, kept moving on 293 frames the camera stood still (the left wall, the start), up on Right, down on Left. The profile's cited row, confirmed |
| player Y | `$003F` | read `0xB0` (176) standing, moved within 6 frames of a jump press in 13 of 15 presses |
| start gate | `$001B = 0`, `$0017 = 0x80` | see section 5: bytes that sit still, not a mode |
| hold | `$003F = 0` | see section 3: the in-app route names it, the headless route does not |

Not in the table, observed: `$0018` (game mode 1/3/4/5) and `$0028` (the block counter) are not gate
candidates (the counter differs between the courtyard and the hall, and the gate must hold in the
neighbouring room); `$0045` hp, `$002A` lives, `$0071` hearts, `$0042` timer as in the profile.

## 3. What the trials found out (the failures are the useful part)

1. **The camera is movement-driven, not position-driven.** Poking `$0040` moves the poked byte (`+24`,
   `$40: 0x7C -> 0x94`, stays) and the camera does not follow. All 2,048 work-RAM bytes were poked
   `+24` and `+40` in turn from the middle of the courtyard (`explore_followed`): no byte makes
   `$002E`/`$00FD` move. So `player_move` cannot scout Castlevania; the word the finder names is a
   position the game keeps for collisions (the profile's note on `$0040` says as much).
2. **A hold that fooled the trials.** `PlayerMove` + hold `$003F` (Simon poked to the top of the screen)
   made the canvases grow past two screens: 94,724 cells in 300 frames. Run with the X step set to 0
   (no poke at all) it grows the same: poking Y to the top makes the game walk Simon to the right by
   itself, one pixel a frame. It passed every trial, and a Left pass from the middle of the courtyard
   walked right too (the table was useless to the left; found by stepping the scout by hand,
   `explore_left`). The trials now run the winning `PlayerMove` recipe once more with `step_px = 0` and
   refuse it when the canvases still grow sideways (`scout_discovery`, width not area: a hold may move
   a vertical camera on its own: SMW, MMX and Super Metroid do).
3. **`Walk` wins.** The scout holds the pad Right: 94,923 cells in 300 frames, 3 screens in 681 frames,
   hp untouched in the courtyard. The table: `strategy = "walk"`, `step_px = 1`, `dirs = ["right"]`
   (the walk recipe is right only, as for Mega Man 2: nothing tells a walk that Left is safe),
   `stall_frames = 90`. The headless route (`discover_cv`) writes two gate rows and no hold; the in-app
   route (`ScoutFind`, `cv_acceptance`) writes `player_y = $003F` and `hold = $003F = 0` ("kept going
   where the same scout without it was cut short") and no gate. With a Walk the hold is not used to
   scroll; whether it keeps Simon out of the hall's enemies is not shown here (the scout never entered
   the hall).
4. **The status bar hid the camera from the player finder.** Castlevania writes the scroll register six
   times a frame: four with the camera's value, then two zeros for the status bar at the top. The
   finder read the last write, a zero: the camera never "moved", the player finder saw 0 scrolling
   frames and named nobody. It now also compares each write with the one in the same place last frame
   (when the frame wrote as many) and takes the first that changed (`camera_delta`;
   `a_status_bar_write_after_the_camera_does_not_hide_the_camera`, which fails without the change).
5. **A table that said 60 stalled at its first frame.** The skeleton wrote `stall_frames = 60`; the
   trials had run with 90. Simon walks 4 px per frame in the scout's `player_move` trials and 1 px in a
   walk, and the camera starts after ~50 frames of the first. The scout stalled after 244 frames at
   256 px wide (variant `stall90`: 768 wide, 99.9%). The skeleton now carries the trials' own value.
6. **Two screens was too much to ask in 300 frames.** The trial floor of 2.0 screens refused every
   recipe: a walk of one pixel a frame shows 1.5 screens after 300 frames, 80 of them before the
   camera starts. The floor is 1.4 screens (a trial that did not follow shows less than one: 0.8 for
   the no-hold `player_move`).

## 4. Acceptance numbers

| Check | Result |
|---|---|
| Table found, no `[scout]` | yes (`cv_acceptance` through `ScoutFind`; `discover_cv` through the trials) |
| Saved to the user profile path through "Save scout settings" | 2 rows (`player_y`, `hold`) plus the scout block, written to `RETROFORGE_CONFIG_DIR/retroforge/profiles/nes/castlevania/profile.toml`; shipped profile bytes unchanged (asserted) |
| Courtyard scouted, in the app, from the saved profile | started 1, finished 1, aborted 0; 352 scout frames from the middle of the courtyard, 52,376 cells merged; pass Right; rung `walk@1px`; agreement **99.9% over 134,190 shared cells** (headless driver from the start: 768 x 240, 100% over 198,673; `cv_acceptance`: 768 wide, 100%) |
| Room cuts | courtyard and hall are two scenes (`0xd0b9837c4d836535`, `0x88b70e7ee9580f6a`), no scene spans both; the death/respawn is a cut of its own |
| Second visit | new app, same cache and profile folder: first sample at live frame 16 already has the 104,867 remembered cells, margins 53,927 of 144,000 known (the same as after the scout), scouts started 0, skipped 1 |
| Regression, discovery tests | SMW 5120/5120 px (agreement 100.0%), SMB3 2816/2816 (99.7%), MM2 2560/2560 (98.7%), MMX 2820 px (100.0%), Super Metroid corridor 512 (99.7%): all identical to before |

Shots (uncommitted): `docs/screens/w35/cv1-32x9-{off,on}.png`, 856 x 240. OFF: the 256 px game picture
in the middle (x 289 to 545), the HUD above it, both margins black. ON: the right margin is filled with
the rest of the courtyard: the tree line and the iron fence continue across the join, a second statue
on its pedestal, the courtyard wall rising into the castle gate on the right edge, the cobbled ground
continuous to x ~815; no seam is visible at this size. Not good: the left margin is still black (the
scout is right only and the live game has not been there; at the start of a stage it is outside the
level anyway), the strip right of the gate is black (outside the block), the status bar is not
repeated in the margins (it stays above the middle picture), and the picture was taken at one instant
(the torches' flames and Simon are not animated in the margins).

## 5. The gate finder (generic, all games)

Ticket item 1. The old score (steady share of the level times the share of other frames that differ)
could not rank a mode byte above bytes that merely sit still: Super Metroid `$0998` read 8 on 97% of
the corridor and was refused outright (it takes 8 values while the player plays; the cap was 3), and
Super Mario World's `$0100 == 0x14` read 0x13 for the first 29 frames of the level (a fade-in after the
scene key already said "level") and fell under 95%.

Now (`scout_finder`): the finder watches the scene cuts (a change of scene key, the settling frames,
30 frames after) and keeps the RAM 120 frames before a cut was noticed and 60 after it (a mode byte
moves before the picture does: SMW writes "leave the level" ~100 frames before the key changes).
Each candidate gets: **cover** (the share of cuts its value differs across, half over all cuts and half
over the cuts between playing and not), **sync** (the share of its changes made at a cut), **steps**
(changes per cut it changed across: a state machine steps through several states, a datum a level sets
changes once), **calm** (changes away from any cut: a stack byte or a pointer changes dozens of
times), with the old `differs` and the doors, as one `contrast`; ranking is by contrast, then bytes that
let go in a door. The steady-share test excuses the frames of a cut's zone, and the played-values cap is
8. The survival checks (`gate_survival`, the vertical room) are unchanged. `start_when` rows are written
at confidence 0.7 (steady share x (0.5 + contrast/2); it was 0.9 of constancy x differs).

Chosen gate (the two `start_when` rows of `discover_*`), before -> after:

| Game | Before | After | Widths / agreement |
|---|---|---|---|
| SMW | `$09B9`, `$09BA` | `$0065`, `$0057` | 5120, 100.0% -> same |
| SMB3 | `$84`, `$8C` | `$07C8`, `$07DF` | 2816, 99.7% -> same |
| MM2 | `$05`, `$0464` | `$036D`, `$002A` | 2560, 98.7% -> same |
| MMX | `$01BB`, `$01BD` | `$0700`, `$0702` | 2820, 100.0% -> same |
| Super Metroid | `$00D2`, `$0E55` | `$1986`, `$00D5` | 512, 99.7% -> same |

Honest result: none of the chosen bytes is a game-mode byte with a name. What changed: Super Metroid
`$0998` (the real mode byte, known from the profile) is now a candidate (rank 13 of 23; it was refused
for taking 8 values in play), the stack bytes (`$1FF8`...) that a first version of the score put first
are gone, and `observe_*` prints each candidate's cover/sync/steps/off-cut numbers (`RANK` lines). SMW
`$0100` passes the steady-share test now (the fade-in is excused) but its 14 changes away from cuts
(the title and map animate it) put it below the 200 the test prints once the calm term is applied
(it was rank 4 before that term; the term is what removed the stack bytes). The top bytes are not
modes because one session shows one or two levels: a byte a level sets once (`$0057`, `$0065` in SMW:
per-level data that changes at every cut) looks the same as a mode with the evidence available; two
levels of the same game would tell them apart (the mode byte reads the same in both). Castlevania's
`$0018` mode is not a candidate at all (see section 2).

## 6. Not done

* A Left pass for a walk (nothing in the session says a left walk is safe), so Castlevania's margins
  fill ahead, not behind; the entrance hall (hp 0 in ~1,600 frames); stairs between blocks later in
  the game; the safe hold is untested against the hall's enemies.
* The gate is still not a named mode byte (section 5).

## 7. Run

```
cargo test --release -p retroforge --test w35_03_castlevania -- --ignored --nocapture cv_acceptance   # ~40 s
RF_SCREENS_ONLY=cv-scout  (w30_screens, scripts: shot.sh)                                           # ~45 s
cargo test --release -p retroforge --test w35_03_castlevania -- --ignored --nocapture observe_cv      # RANK lines
cargo test --release -p retroforge --test w33_07_discovery -- --ignored --nocapture discover_smw      # etc.
```
