# W35-02: Super Metroid, scout settings found by observation (evidence)

Clean-room: everything below was watched in the emulator (the finder and the
probes in `crates/retroforge/tests/w35_02_super_metroid.rs`, ROM-gated,
`#[ignore]`), D-021 respected: no disassembly, no decomp-derived RAM rows, every
row of the table carries `source = "observed: auto (W33-07)"`. The shipped
`profiles/snes/super-metroid/profile.toml` was not touched (the screens test
checks its bytes before and after). The profile's old cited rows ($0911/$0915,
$0AF6/$0AFA) are confirmed or explained below.

ROM: `Super Metroid (Japan, USA) (En,Ja).zip`. Release builds, one process at a
time, cold canvas cache (fresh `TMPDIR`) for every first visit.

## 1. Where free play starts

Ceres Station is the soonest (a new game starts there; Crateria would need the
whole Ceres escape first). Script (`sm_to_free_play`), frames at 60 Hz:

| Step | Frames | Seen |
|---|---|---|
| boot, Start, Start, A, A, Start, A | ~1,100 | option screen ("START GAME" / "ENGLISH TEXT"), then the first story page; `$0998` reads 1, 4, 2 on the way |
| story pages, A every ~150 frames (55 presses) | ~8,200 | `$0998 = 0x1e`; a cursor block waits for A at the end of each page, so the pages do not advance by themselves |
| Ceres cutscene | | `$0998 = 0x1f`, `$079F = 6` (Ceres), `$079B = 0xDF45` |
| free play | frame ~9,500 (script waits to 11,800) | `$0998 = 8`; Samus at `$0AF6,$0AFA = 0x0080,0x0048` on a platform at the top of a shaft |

Rooms met (all `$079B` values, observed): `0xDF45` the shaft (one screen wide,
camera Y 0 to 0x200, Samus falls from ledge to ledge), `0xDF8D` the corridor
(two screens wide: camera X 0 to 0x100, a lab with capsule pods), `0xDFD7` the
next. Doors: `$0998 = 0x0b` for about 90 frames, camera scrolls a screen.

Script traps found: plain Right stops at a step 0x80 px into the corridor (jump
with A held to climb); Left from the corridor's entry walks out the door back
into the shaft; the shaft needs R/L/Down taps to drop from ledge to ledge; the
exit door is up on the right at the bottom, reached by two `R+A` hops.

## 2. The table the session found

One scripted session (`SM_SHAFT` then `SM_PLAY`: shaft, door, corridor, six
standing jumps, right and left, stand still), fed to `retroforge::scout_find::
ScoutFind` exactly as the core thread feeds it. 40 to 52 s of wall time from the
first frame of the session to the table, headless.

| Row | Address | Evidence |
|---|---|---|
| `camera_x` (u16) | `$7E00B1` | held the BG1 H-scroll register's low byte on 100% of 386-405 scrolling frames; the carry check found its high byte |
| `camera_y` (u16) | `$7E00B3` | held the V-scroll low byte on 92% of 278 frames the screen scrolled vertically (the shaft) |
| `player_x` | `$7E0AF6` (high `$0AF7`) | 70% of 386 moving frames matched the camera over 16-frame windows (exact frame steps: 64%), up on Right (488 frames), down on Left (226), 418 frames moving with the camera still, 8 frames the wrong way |
| `player_y`, `player_y_hi` | `$7E0AFA`, `$7E0AFB` | moved within 6 frames of the jump press in 10 of 10 presses; confirmed by a vertical pass (below): the shaft's canvas reached 1.9 screens |
| `start_when` | `$7E0024 = 0`, `$7E05F5 = 0` | weak, see section 6 |
| `hold` | none | all 16 candidates were inert in these rooms (identical extent with and without), so none is written |

Not in the table, observed: `$0911` (the profile's cited layer-1 X) steps with
the camera on 285 of 286 frames and never when the camera stands still: it is a
camera copy, not the player (the player finder's wall test refuses it, as it
should). `$0915` changes only with the vertical scroll. `$0AF6` and `$0AFA` are
the profile's cited player rows: confirmed. `$0998` (game state) read 8 on 97%
of the corridor's frames, 0x0b in the door and 1/2/4/0x1e/0x1f before play;
`$079B`/`$079F` room and area as above. The camera finder names `$B1`/`$B3`
(the scroll register shadows) before `$0911`/`$0915`; the scout reads either.

Vertical recipe: the scout's Down/Up passes poke `$0AFA/$0AFB`. The shaft
scouted with the table above reaches 256 x 706 cells (three screens), agreement
99.7% over 312,410 shared cells (rung `player_move@8px`).

## 3. Acceptance numbers

| Check | Result |
|---|---|
| Table found with no `[scout]` | yes (`sm_acceptance`, `find_sm`) |
| Saved to the player's profile through "Save scout settings" | 7 rows, written to `RETROFORGE_CONFIG_DIR/retroforge/profiles/snes/super-metroid/profile.toml`; shipped profile bytes unchanged |
| Corridor scouted, in the app, from that saved profile | started 1, finished 1, aborted 0; 559 scout frames, 48,295 cells merged; passes Right, Left; rung `player_move@4px`; agreement **99.8% over 617,626 shared cells** (headless driver: 512 x 224, 99.7% over 371,853) |
| Room cuts | the shaft (`0xDF45`) and the corridor (`0xDF8D`) are two scenes (`7249495ea27b7f9e`, `6cca1eda9ade6898`); no scene spans two rooms; the door's fade is a short scene of its own (90 frames, `$0998 = 0x0b`) |
| Second visit | new app, same cache and profile folder: first sample at live frame 14 after the snapshot already has the 40,150 remembered cells, margins 20,075 of 121,856 known (the same as after the scout), scouts started 0, skipped 1 |
| Regression, discovery tests | SMW 5120/5120 px, SMB3 2816/2816, MM2 2560/2560 (100% of the hand tables); Mega Man X 1024 -> 2820 px (500,989 cells, 100% agreement: its Y word `$0BB0` is now named and a hold found) |

Shots (uncommitted): `docs/screens/w35/sm-32x9-{off,on}.png`, 800 x 224.
OFF: the 256 px game picture sits in the middle (x 272 to 528), Samus at the
left wall by the entry stairs, both margins a plain dark-to-blue gradient (the
backdrop; nothing has been stitched). ON: the left margin is the same gradient
(the room has nothing left of its entry wall); the right margin is filled with
the rest of the corridor: two more capsule pods, the floor plates and the stairs
up to the far door, continuing the ceiling pattern across the join without a
visible seam. Not shot: the shaft at 32:9 (its margins are walls on both sides;
the gain is vertical and only shows as canvas height, 256 x 706, as in
W33-08). Not checked in the picture: the pods' animation at the join.

## 4. What the generic code needed (each with a test)

1. **Player X over windows.** Samus walks 1.5 px a frame and the camera eases
   after her: exact frame-by-frame steps matched 64% (bar 85%), so no player was
   named. `scout_finder` now also accepts a word whose changes add up to the
   camera's travel over 16-frame windows (>= half of the windows; windows with a
   cut are skipped). Test `a_camera_that_eases_still_names_the_player` (a camera
   two frames late, steps 1,3,1,2,1: the exact test alone gets under 85%).
2. **Y.** The first run named joypad shadow bytes (`$8B`...; a flag a press sets
   moved by 127 within a frame) and refused `$0AFA` because the shaft made it
   fall in 2 of 7 control windows. A Y byte now has to change on 5 frames of the
   window (`a_button_flag_is_not_the_player_y`) and may fall in two of five
   control windows. This is also why Mega Man X now has a Y and a hold.
3. **Gate scene cap.** More than six scenes before the level (the story pages,
   the menus) sent the level to the shared histogram and the finder returned no
   gate at all. Eviction folds the quietest scene into the shared histogram
   (`a_level_after_many_scenes_still_has_a_gate`). Doors inside a scene (27
   frames of `$0998 = 0x0b` in the corridor) made the level 97.4% constant: the
   bar is 95%, bytes must hold in the neighbouring rooms, be gone in doors, take
   at most three values while playing; ties go to bytes that let go in a door.
4. **Vertical confirmation.** The Y recipe can only be tried in a room that
   scrolls vertically, and the session meets one only by chance:
   `ScoutFind` keeps a second snapshot (and the live model) at the first stand
   in a room the scene tracker calls vertical; `scout_discovery::Vertical` runs
   a Down/Up pass per Y candidate from it and names the one that scrolls the
   screen furthest. Without it `player_y` was never written (the hold trials
   were inert in the corridor).
5. **Gate survival.** A junk gate stopped the scout for good: `$0029 = 0` held on
   97% of the corridor and read 128 in the shaft, and the scout never started
   there (600 s, started 0; without the gate: 256 x 706, 99.7%). The table now
   keeps only gates that stay true through a scouting run, and through one in
   the vertical room (`gate_survival`).
6. `CoreCommand::ApplySnapshot`: the screens harness needs to start a session in
   a room 11,800 frames into a SNES game (`ApplyState` is NES-only). A snapshot
   of either console, restored on the core thread.

## 5. What failed, honestly

* The gate finder is weak on this game. The bytes named (`$0024`, `$05F5`) are
  not mode bytes with a name; `$0998 == 8` is among the candidates but the
  finder cannot tell it from bytes that sit still through play and differ in
  menus. Three evidence changes helped (section 4.3) and survival removes the
  harmful ones; it does not make the survivors meaningful. The rows say "seen
  in one level of this session". Baseline gates for SMW, SMB3, MM2 and MMX were
  junk in the same way.
* No safe hold: Ceres's rooms have nothing that hurts a floating Samus, so the
  trials cannot see one. A game with hazards will.
* The vertical room has no 32:9 shot (its margins are walls), only numbers.
* Scouting the corridor from its door fills the right margin; the left margin of
  this room is outside it, so the shot cannot show a room BEHIND.
* Not done: Castlevania, Zelda (later parts of the ticket); Crateria (the
  landing site needs the Ceres escape first).

## 6. Run

```
cargo test --release -p retroforge --test w35_02_super_metroid -- --ignored --nocapture sm_acceptance   # ~4 min
RF_SCREENS_ONLY=sm-scout  (w30_screens, scripts: shot.sh)                                             # ~2.5 min
cargo test --release -p retroforge --test w33_07_discovery -- --ignored --nocapture discover_smw       # etc.
```
