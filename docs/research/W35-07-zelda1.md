# W35-07: The Legend of Zelda (NES) at 32:9

Observation only (CLAUDE.md law 5, D-021): every figure below is a measurement of the running game
through the harness; nothing comes from a disassembly. ROM-gated runs are in section 7.

## 1. What the shot showed before (W35-04)

`[decode] kind = "room_grid"` has no sizes, so the decoded source fills nothing; the scout had no
table: the finder names no player (section 4), a patient walk reached two screens at 82.9%
agreement and its deltas were rejected at 52% and 57%. The margins were black.

## 2. The 82.9%: ranked causes, and which one it was

1. **A band's Y after a `$2005` split (verified, fixed).** In free play the game draws one band
   (`0..240`, scroll `(0, 0)`); during a screen flip (a 64 frame scroll, 4 px a frame, the nametable
   bit carried in the 9-bit X: 0 to 256) it splits at row 63: the status bar `0..63` at `(0, 0)`
   and the playfield `63..240` at `(x, 0)`. The `ScrollWrite` of a `$2005` write carried `t`'s Y,
   0. Hardware copies Y from `t` to `v` only on the pre-render line (nesdev "PPU scrolling":
   "Without the second write to $2006, only the horizontal portion of v will be reloaded from t"),
   so the picture keeps drawing from `v`: row 63 shows nametable row 63. `ScanlineBand::y_at` took the
   event's Y at face value and put the playfield's first row at world row 0, 63 rows high. Evidence:
   the live canvas after one flip (`live_walk` in `w35_07_zelda1.rs`, frames 1840 to 1910) held
   the new screen 63 rows above the old one (no status bar on it, the tree rows starting at row 0),
   the picture then disagreed with the canvas for 45 frames (`STALE_FRAMES`) and the canvas was
   replaced by the new screen alone (512 wide, then 256 wide at origin 256 at frame ~1950). That is
   what the scout's self-check saw as 82.9%: the frames of the flip "agree with themselves and not
   with the live screen before it".
2. Palette or attribute differences between screens: `agree` compares palette indices, not
   colours, and the second screen's agreement after the fix is 100.00%: not it.
3. The sprite-zero split itself and the scroll lag: the bands are right (`(0, 63)`, `(63, 240)`),
   only the Y was wrong: not it. Enemies and Link are sprites, holes in the canvas: not it.

Fix (`crates/rf-nes/src/ppu/scroll.rs`): the event of a `$2005` write while rendering, inside the
picture, carries the Y of the raster: the picture's top row as the scroll registers last placed it
(`raster_top_y`, set where `t`'s Y reaches `v` at the pre-render copy) plus the row the write takes
effect on, instead of `t`'s Y. Two guards, both found by the RF-Scroller fixture (section 6):
`v` is NOT read (a game that streams tiles through `$2007` while the picture is drawn leaves it
anywhere: reading it made the world Y drift 32 rows a frame there and cut the scene), and a `t` that
held an address (`$2006`) at the pre-render copy is not a scroll (`t_is_scroll`), so the top stays
where it was. Vblank and blanked-screen writes keep `t`'s Y (the next frame's scroll). Neither
field is machine state (excluded from the save state, `state.rs`). The scroll tracker's carry for a
frame with no write is now the top row's Y (the last band's Y less the rows above it), not the
split's. Tests: `dollar_2005_*`, `a_dollar_2006_commit_mid_picture_*`, `a_t_that_holds_an_address_*`
(rf-nes: 401 passing), the `compute_bands` carry (`a_mid_frame_scroll_write_...`).

## 3. The status bar

Outside a flip the bar is part of the one band, so it was stitched into the canvas at the camera's
world x: the neighbour's margin would have shown a second bar. The scroll tracker now remembers the
rows a bar of real height took (a non-primary band that stood still, zero-delta rule, seen in a
split, anchored to the top or the bottom of the picture) and cuts those rows out of every later
band of the scene, split or not, per scene (cleared at a cut, `ScrollTracker::hud_rows`,
`a_bar_seen_in_a_split_is_cut_out_of_...`). The pieces keep the Y the split would have written. The neighbour screens therefore have no status bar
rows: the margins show black there. The first screen's own bar (stitched before the first flip was
seen) stays in the canvas under the live picture.

## 4. Discovery: the player is not a word the game obeys

Zelda moves its camera only in flips. Link's X (`$70`) steps by minus the camera's step while it
scrolls (Link stands still in the world), so the finder names nobody (it did not before either), and
the in-app finder never started its trials (`Observed::ready` wants a player and a Y). Changes:

* `Observed::camera_only`: no player named and the camera has moved on 384 frames: a `walk`
  needs neither (NES only; `scout_find.rs`). A platformer names its player in a few dozen frames, so
  this does not pre-empt a `player_move` table (SMB3, MM2, CV runs below).
* `WALK_STALL_FRAMES = 260`: a `walk` trial's patience is a screen of walking at a pixel a frame.
  The 90 frames of the other recipes ended the walk 30 frames short of the edge (the 120 frames of
  W35-04 were this).
* A `walk` that scrolled right is tried to the left too and the table says `dirs = ["right", "left"]`
  when that also grew (a forward-only game keeps `right`).

The discovered table (no hand `[scout]` in the shipped profile, unchanged):
`strategy = "walk"`, `step_px = 1`, `stall_frames = 260`, `dirs = ["right", "left"]`; no rows. No gate
cleared the 0.7 confidence floor (`gates kept []`), so the table has no `start_when`: the scout
starts as soon as it is allowed to (60 settle frames), in any scene it meets, a title screen included.
That is an honest gap: the best candidates (`mode_score` 0.38) did not tell play from the menus.

## 5. Result

Headless (`z1_scout_vs_walked`): the table's scout on the first overworld screen gives a canvas
768 x 240 (the screen either side of the first), 897 scout frames, self-check agreement 100.0%.
Against what the game itself showed (a second rig walks right, back, then left through the flips
and its live canvas is compared cell by cell, playfield rows, 45,056 cells a screen): left screen
100.00%, first 100.00%, right 100.00%, same extents, no cell known to one only.

In the app (`z1-scout` in `w30_screens`, cold canvas cache, widescreen 32:9, the card's "Save scout
settings" into the user profile folder, the shipped profile byte-identical): the scout runs once
(started 1, finished 1, aborted 0), the margins fill, and a second visit over the same cache has
them at live frame 16 with no scout started (skipped 1).

Shots (`docs/screens/w35/zelda1-32x9-{off,on}.png`, 856 x 240, uncommitted). Off: the 256 px picture
in the middle, black either side. On: left of the picture the screen to its left, right the
screen to its right (tree rows that continue the live screen's edge); the top 64 rows of both
margins are black (the status bar's rows, deliberately empty); a 44 px strip at each far end is
black (the scout reached one screen each side: the walks stop at trees, the next screen is not
known). Link is in the live picture only.

## 6. Other games (generic code changed)

`scroll.rs` (every NES `$2005` split), `scroll_tracker.rs` (bar memory, carry), `scout_discovery.rs`
(`walk`), `scout_find.rs`, `scout_finder.rs`. NES discovery re-run one at a time, release, after the
final change:

| game | before | after |
|---|---|---|
| SMB3 `discover_smb3` | 2816 px, hand table 2816 | 2816 px (100% of hand), agreement 99.7%, `player_move`, table unchanged |
| MM2 `discover_mm2` | 2560 px, hand 2560 | 2560 px (100% of hand), agreement 98.7%; `walk` table now `stall_frames = 260` (was 90), `dirs` still `right` (its Left trial did not pass the floor) |
| Castlevania `cv_acceptance` | courtyard 3 screens | 768 px, 100.0%; `walk`, `stall_frames = 260`, `dirs` right |

Shots `cv-scout`, `scout-nes`, `scout-mm2-climb` re-taken: the margins are pixel-identical to the
committed ones (6 to 163 pixels differ in each, all inside the live 256 px picture: sprite
timing, the "off" shots differ by as much); the changed PNGs were reverted, not part of this work.
Fixture: three `scout.rs` tests on RF-Scroller (a synthetic ROM whose vblank work overruns into the
picture, FORMAT.md "Known defects") depended on the old placement. With the playfield on its own
rows the 2 px player_move scout agrees with the live canvas on 91.9% of the 57,600 cells they share
(95.8% before, over the 95% bar by 0.8 points): the ladder goes on to walk, which agrees on all of
them. `a_step_the_game_cannot_keep_up_...` and `an_accepted_scout_remembers_its_rung_...` now say
walk and carry the reason; `a_nes_scout_on_rf_scroller_...` passes unchanged. The world-Y drift
this fixture showed under a `v`-reading first version is the reason for the two guards in section 2.
SNES games are untouched by construction (`Walk` exists only where the console is placed by bands;
`camera_only` is NES-only; `scroll.rs` is the NES core). The cached canvases of NES games with a
`$2005` split, made before this change, carry the old row placement; the registration check
replaces such a canvas after 45 frames of disagreement.

## 7. Run

```
cargo test --release -p retroforge --test w35_07_zelda1 -- --ignored --nocapture z1_scout_vs_walked   # ~15 s
cargo test --release -p retroforge --test w35_07_zelda1 -- --ignored --nocapture live_walk           # probe, RF_SCRIPT
cargo test --release -p retroforge --test w35_04_zelda -- --ignored --nocapture discover_z1           # ~15 s
RF_SCREENS_ONLY=z1-scout (w30_screens, scripts: shot.sh)                                              # ~25 s
```
