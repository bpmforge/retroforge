# W35-08: left passes for the scout

Observation only (CLAUDE.md law 5, D-021): every figure is a measurement of the running game through
the harness. Release builds, one process at a time, cold canvas cache. ROM-gated runs are in section 6.

## 1. Audit: where does a scout pass one way only?

The pass machinery was never one-way: `Worker::run` runs every direction of the plan, each from
the restored snapshot, each merged into unknown cells only (`scout.rs`), and the app gives a scene
`plan.dirs_for(axis)` (the table's directions on that axis, else right+left / down+up). What differs is
what the discovered or shipped TABLE says:

| game | strategy | X dirs the table gives | cause |
|---|---|---|---|
| SMW, SMB3, MMX, Super Metroid, ALttP | `player_move` (poke the player) | right, left (skeleton and hand tables) | none: both ways, verified (section 6) |
| Zelda 1 | `walk` | right, left (W35-07 walks left from the start) | none |
| Castlevania | `walk` | right only | the Left trial ran from the stage's start, where the camera sits at its left stop |
| Mega Man 2 | `walk` | right only (found AND the shipped hand table) | same trial from the level start; the hand table copied it |

Ranked causes for "right only", verified:

1. **The Left trial is judged at the wrong place (verified, fixed).** `discover` ran the Left trial from
   the snapshot the finder took, a level start or a screen with nothing to its left: Castlevania
   from its snapshot grew 72,436 cells (1.2 screens, under the 1.4 floor of 86,016), Mega Man 2
   61,440 (one screen: no growth). A forward-only camera and a camera that was at its stop look the
   same there. Evidence: the same recipe from the middle of the courtyard walks left for 3 screens
   (section 4).
2. **A walk's Left was never asked of `walk` until W35-07, and only from the start (so cause 1).**
3. **Mega Man 2's walker dies without its hold (verified, shaped the fix).** A first version of the
   midway walk ran with no hold: Right died after 168 frames (a pit). The Y hold the winning
   recipe carries (`$04A0`) keeps him alive; the trial uses the champion hold.

Not a cause: `Scout::pokes`/the stop rule (symmetric), `can_scout` (`bands` games can always move
either axis), the ladder (a failed pass n>0 does not give up the scout).

## 2. Fix (generic, `scout_discovery.rs`, `scout.rs`)

After the winning `walk` recipe and its hold are chosen, when the Left trial from the start did not
grow: walk Right for `MIDWAY_FRAMES = 480` (alive, same hold), then start a second scout FROM THERE
(`Scout::start_from_here`: a snapshot of the walking machine, seeded with the walking scout's own
accumulated scene tracking; seeding it with the model of the level start made the new place a scene cut
on its first frame, a trial that "died" at frame 1) and run Left. `extent >= floor` and not died:
the table says `dirs = ["right", "left"]`; otherwise it stays `["right"]` (a forward-only camera such
as SMB1's walks nowhere back, so nothing is hard-coded: it is detected, and the report line says
which). `player_move` tables already always carry both; their passes are unchanged.

Budget per pass: unchanged. A pass is the table's `frame_budget` (3000) at most, ends after
`stall_frames` (260 for a walk) without growth. Discovery pays at most 480 + 300 more frames on one
extra scout machine (a second, a few seconds). In the app the Left pass costs what a Right pass did:
Castlevania mid-courtyard 985 scout frames for both passes, Mega Man 2 2,317 for both (the Right
alone was 2,056: +261 for the Left).

Up/Down: the scene's axis rule (one axis per scene, `scene_axis`) already gives vertical scenes
down+up (Mega Man 2's climb, Super Metroid's shafts) through `can_scout`; unchanged. A top-down
game (ALttP, Zelda 1) scouts the axis the scene scrolls on; its other axis would need a second canvas
axis per scene, not cheap, not done.

## 3. Mega Man 2's shipped table

`profiles/nes/mega-man-2/profile.toml` says `dirs = ["right"]` by hand (W32-09: "nothing tells a walk
that Left is safe"). Left from the middle with the hold now has evidence (section 6: right+left, 99.0%
against 98.7% right alone), but the shipped file was NOT touched here (a profile change is its own
review). The discovered table has both.

## 4. Castlevania, left, against a walked rig (`cv_left_vs_walked`)

The session walks right ~2 screens more (`R60 R+A30` x2) before the finder's snapshot, so the
snapshot is mid-level. The table found in app mode (`ScoutFind`): `walk`, `dirs = ["right", "left"]`.
The scout from that snapshot: canvas 768 x 199, passes Right (door, a scene cut, as before) and
Left, 985 scout frames, self-check agreement 100.0%. A second rig restored to the same snapshot walks
left to the wall and its canvas is compared cell by cell with the scout's: 136,904 cells known to
both, 136,904 agree (100.00%), none known to the walk only.

## 5. Shots (uncommitted PNGs, `docs/screens/w35/`)

* `cv1-32x9-{off,on}.png`, 856 x 240. Off: the 256 px picture in the middle, black either side. On: the
  left margin (289 px) shows real level: trees, the iron fence, dead trees at the very left edge,
  a statue on its pedestal, the cobbled ground, continuous across the join to the live picture; the
  right margin the tower and wall of the gate, black past x ~635 (the end of the block). Margins known
  75,618 of 144,000 cells (53,927 before, Right only). Not good: the status bar is not repeated in the
  margins (the top 48 rows are black), no sprites (Simon, flames) in the margins, and the strip right
  of the gate is black (outside the block).
* `smw-left-32x9-{off,on}.png`, 800 x 224 (a new `smw-left` shot: Mario run right through the
  session, snapshot mid-level, shipped hand table `right, left`). Off: flat blue either side of the
  picture. On: the left margin shows the level behind Mario (more of the pale blue hills, green
  bushes, the ground, a Yoshi coin at the edge, a slope at the far left), the right margin ahead (the
  tall pale hills, the diagonal green pipe, the raised dirt step); no seam. Sprites (Mario, the
  enemy) only in the live picture. Scout 1,071 frames, agreement 100.0% over 269,525 shared cells,
  margins 282,807 cells known.

## 6. Regression (discovery tests, release, one at a time)

| game | table dirs | width | agreement |
|---|---|---|---|
| SMW `discover_smw` | right, left | 5120 | 100.0% (unchanged) |
| SMB3 | right, left | 2816 | 99.7% (unchanged) |
| MMX | right, left | 2820 | 100.0% (unchanged) |
| Super Metroid | right, left | 512 | 99.8% (99.7%) |
| ALttP | right, left | 1024 | 99.8% |
| Zelda 1 `z1_scout_vs_walked` | right, left | 768, three screens 100% each | 99.8% |
| MM2 `discover_mm2` | right -> **right, left** | 2560 (100% of the hand table) | 98.7% -> 99.0% |
| Castlevania `cv_acceptance` | right -> **right, left** | 768 | 100.0% |

The non-walk games' code path is untouched (the new trial is gated on `Walk`); their numbers are a
re-run. Second visit (`cv-scout` shot): first sample at live frame 15 with 152,828 cells remembered, scouts
started 0, skipped 1. Accuracy Mode untouched (scout/discovery only). Tests: `a_scout_started_from_here_...`
in `scout.rs` (lib), `cv_left_vs_walked` (ROM-gated).

## 7. Run

```
cargo test --release -p retroforge --test w35_08_left_passes -- --ignored --nocapture cv_left_vs_walked  # ~30 s
cargo test --release -p retroforge --test w33_07_discovery -- --ignored --nocapture discover_mm2         # ~40 s
RF_SCREENS_ONLY=cv-scout / smw-left (w30_screens, scripts: shot.sh)                                      # ~40 s / ~60 s
```
