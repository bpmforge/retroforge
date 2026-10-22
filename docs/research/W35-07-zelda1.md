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

## 8. W35-09: a second neighbour screen each side

At 32:9 (856 px) the margin is 300 px each side, so the first neighbour (256 px) left a 44 px strip black
at each far end. The walk stopped there because Link's path was blocked, not because the camera was
(probe `z1_pass_probe`): going right he reaches the next screen, walks to x `$50` and stands against a
tree, 260 frames, stall. Observation only (D-021); no game bytes read beyond the finder's own.

Generic change (`scout.rs`, `app.rs`): `ScoutPlan::reach_px` is how far past the live picture the
margins reach, per side, derived by the app from the aspect (`(output width - 256) / 2` = 300 here;
0 = not asked, as in discovery and every other caller). A `walk` pass that has STALLED (the stall
rule's count, 260 frames without growth) while the canvas does not yet reach `reach_px` past where
the pass began (`margin_covered`: canvas edge >= start camera + 256 + reach on the right, <= start
camera - reach on the left) no longer ends: it sidesteps (`sidestep_pad`): up for 8 frames, forward
(the pass's direction) for 160, down for 16, forward 160, up 24 ... 8 sidesteps, then gives up and the
stall rule ends the pass. Any growth (a screen flip) resets the stall count, so a later blockage gets its
own zigzag; covered margin or a spent zigzag leaves the old behaviour. X passes only (a vertical scene
is untouched), pad bits only (the NES pad). Budget: the pass is still bounded by its `frame_budget`
(3000) and the zigzag is bounded by itself (8 sidesteps = 288 + 1280 = 1568 frames at most per
blockage). Measured: the Right pass crosses the blocked tree line on its first sidestep (frame ~740
of the pass, canvas 768 wide, then stalls 260 frames at the next screen: 1,044 frames), the Left pass
likewise (1,098 frames); the whole scout is 2,054 to 2,138 frames (897 before), the same worker thread,
a few seconds. A game whose margin is already covered, or whose pass ends by budget or a scene cut,
behaves as before. A path that stays blocked through all 8 sidesteps ends like before and the strip
stays black (honest).

Result (`z1_two_screens_vs_walked`, `w35_09_zelda_reach.rs`, ROM-gated; the discovered table with
`reach_px` 300): canvas 1280 x 241, origin -512: the start screen and TWO each side. A second rig
walked the same screens by hand (round the tree line: up 12 at x `$50`; left side up 12 at x `$90`) and
the five 256 px columns agree 100.00% each (45,056 playfield cells a screen, none known to one only);
self-check agreement 99.8%.

In the app (`z1-scout`, cold canvas cache, 32:9, the card's saved table): scout started 1, finished 1,
aborted 0, passes ended `[Stalled, Stalled]`, 2,138 scout frames, agreement 99.7% over 378,294 shared
cells; margins known 106,200 of 144,000 cells (it was 90,624): every playfield row (64..240) of both
margins, none of the status-bar rows (the 64 top rows are black on purpose, section 3; no bar in the
margins). Shot `docs/screens/w35/zelda1-32x9-on.png` (uncommitted), looked at: both 44 px far-end
strips hold real terrain, not black: left, the forest's edge and sand continuing the left screen (the
tree rows at the top and bottom); right, the next screen's rock face (brown) at the top right and
bottom right, sand between; no seam at either join; Link only in the live picture. Second visit:
first sample at live frame 16 with 106,200 margin cells, scouts started 0, skipped 1.

Regression, one process at a time, release: discovery has `reach_px` 0, so by construction its numbers
are a re-run: SMB3 2816 px / 99.7%, MM2 2560 px (100% of hand) / 99.0%, SMW 5120 px / 100.0%,
Castlevania courtyard 768 px / 100.0%, `z1_scout_vs_walked` 3 screens 100% each (all as in section 6
and W35-08). The app path (`reach_px` 300) shots: `cv-scout` 985 scout frames, agreement 100.0%,
margins 75,618 cells, second visit 15 frames / skipped (identical to W35-08), `smw-left` 1,071 frames,
100.0% (identical), `scout-nes` SMB3 (player_move) 99.6%, MM2 (walk, hand `dirs = right`) 2,050
frames 98.7%; `scout-mm2-climb` (vertical, untouched) re-run. Changed PNGs of the other shots were
reverted.

Known flake (test harness, not the scout): `z1-scout`'s second visit can fail "started a scout" (about 4
runs in 10 here, 0 in 5 before this change). Cause, verified with a debug log (removed): the first
visit's `open_rom_path` leaves the earlier core thread (ThreadId 5) running; it owns its own `rf-cache`
index in memory, and a late `put` from it (a different scene, ~13 KB) persists an index that lacks the
scouted scene the second core thread (ThreadId 6) had put, so the next app finds no entry. A longer
first visit gives it more time; the app proper has one core thread. Rerun when it hits.

Run:
```
cargo test --release -p retroforge --test w35_09_zelda_reach -- --ignored --nocapture z1_two_screens_vs_walked   # ~20 s
cargo test --release -p retroforge --test w35_09_zelda_reach -- --ignored --nocapture z1_pass_probe             # RF_DIR=left RF_REACH=300
RF_SCREENS_ONLY=z1-scout (w30_screens, scripts: shot.sh)                                                         # ~25 s
```

## 9. W35-10: the z1-scout flake, fixed (core thread handoff)

Root causes, ranked, and how each was checked:
1. (the bug) `open_rom_path` assigned `self.core = Some(new)`, dropping the old `CoreHandle` with no `Shutdown`
   and no join. Verified by reading: the core loop drains commands with `cmd_rx.try_iter()`, which ignores a
   hung-up sender, and it only stops on `Shutdown` or when an event send fails (next frame). So the old thread
   lives on with its own `rf-cache` handle. The new regression test fails without the fix (first thread still
   reachable after the second open), passes with it. Quit-to-library sent `Shutdown` but did not join; app exit
   did neither.
2. (consequence) its final flush (canvas model drop) then ran after the new thread's: last writer wins.
3. (amplifier) `Cache::persist_index` rewrote the whole `index.bin` from the in-memory index, so a stale
   writer dropped rows another writer added, although the entry file was safe on disk (the write was already
   temp-then-rename). Not the trigger on its own with one thread, but it turned a leak into a lost scene.

Fix: `RetroForgeApp::stop_core_and_join` (Shutdown, then poll `is_finished` up to 5 s, logged and detached if
exceeded; the time is kept in `last_core_join`). Called by `open_rom_path` before the spawn, by quit-to-library
and by `Drop` (exit). `persist_index` now folds in on-disk rows whose entry file exists and which the writer
does not hold (its own win; evicted rows have no file, so none are resurrected), then renames into place.
Accuracy Mode untouched (no core, renderer or cache-format change).

Measured: ROM switch join 7.5 ms (NES fixture to SNES fixture, release); `z1-scout` (cold TMPDIR, one at a
time, release) 10/10 passes, 25 s each, peak RSS 300 to 368 MB (was failing about 4 in 10).
Tests: `w35_10_core_handoff.rs` (first thread exited after the second open, join time, exit ends the thread),
`rf-cache` `a_stale_writer_cannot_drop_a_newer_writers_entry`.
