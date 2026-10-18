# W33-08: vertical and 8-way scouting (evidence)

Everything here was observed on the dumps in `~/Games/Roms` with the probes in
`crates/retroforge/tests/w33_08_vertical.rs` (ROM-gated, `#[ignore]`) and the
screens harness `tests/w30_screens.rs` (`RF_SCREENS_ONLY=scout-mm2-climb`).

## 1. Mega Man 2, Air Man's shaft

The first horizontal section ends at camera X $0900 (world x 2304). With Mega
Man's Y held high (the W32-09 hold, `$04A0 = $20`) he floats over the end of
it; let go of the hold and he falls, and the screen scrolls DOWN by itself
(about 4 px a frame, world y 8 to 240, `$20` the screen counter `$09 -> $0A`).
That is the "vertical climb" of the brief: the scene tracker (W33-09) cuts it
into its own section scene at frame 2214 (`sections = "axis"`), joined to the
first by a continuous edge. It lands on a cloud platform (Mega Man's Y byte
`$04A0` = `$54`), with a ladder at the top left.

| Probe | Result |
|---|---|
| Pad Up / Down / Up+A on the landing | nothing: the game scrolls only for a player who leaves the screen |
| `$04A0` poked to `$00` | nothing (he is put back at `$0C`, the top of the screen) |
| `$04A0` poked to `$F8` and held | `$37 = 3` at once, the scroll starts about 30 frames later, `$20 = $0B`, world y 480 |
| `$04A0 = $54 + 4` every frame (from what the game holds) | nothing: standing on the platform the game puts it back, every frame |
| `$04A0` poked from a RUNNING TARGET (`+2`, `+4`, `+8` a frame, kept in the scout) | works at all three: the target passes `$F0`, `$37 = 3`, the scroll starts 25-35 frames later |
| `$0460` (X) poked to `$80`, Y untouched | he walks off the platform and falls: the same scroll |

So the recipe is `player_move` on the Y byte with the poke made from the
scout's own target (`Scout::y_target`), and the hold on `$04A0` (which pins him
at the TOP of the screen, for the horizontal section) released on a Y pass,
because the recipe row names the same byte. Standing time before the first new
row is 40 frames at 4 px and about 100 at 2 px (target to `$F0` plus the
game's own latency), longer than `stall_frames` (60): a Y pass gets a lead-in
(`Y_LEAD_IN_FRAMES` = 300) before the stall rule counts, until the canvases
have grown past the first frame's. One screen below the landing (`$20 = $0B`)
the section turns sideways again (camera X +5) and there is nothing further
down to stitch.

Scouted by the real driver (worker thread, ladder, self-check, merge), from the
landing: passes `Down, Up`; the 8 px probe reads 98.9 % ("slow"), its first
delta 92.2 % and is REFUSED, the pass runs at 4 px and is accepted
(`player_move@4px`), overall agreement 94.9 % (the refused 69.9 % / 73.2 %
deltas are a later edge near the live screen, dropped by the W33-06 rule);
the live canvas of the section goes from 256 x 416 to 261 x 656 cells (a screen
taller; the 5 px sideways are the game's own, not the scout's). In the app (32:9,
cold canvas cache): 61 440 cells merged, started 1, finished 1, aborted 0.

The 32:9 shots (`docs/screens/w30-w31/scout-mm2-climb-32x9-{off,on}.png`) show
the landing: the 256 px game picture in the middle, the margins either side a
flat sky blue in both, no garbage and no wall (the screen has no wall to
show). Off and on differ only in sprite animation: one frame of 32:9 shows the
rows the screen is on, and the vertical scout fills the rows ABOVE and BELOW
it, which come on screen as the screen scrolls. The number that shows the
scout worked is the canvas height above, not a picture.

## 2. The axis the scout follows

`SceneTracker::scrolls_vertically()` (new) names the axis of the current
section. A first sighting is NOT enough: Super Mario World's Yoshi's Island 1
settles its camera vertically before Mario walks, which named a vertical level
and sent the first run of the regression pass down and up (width 256 instead of
5120). The axis now counts only when the section was entered by a switch from
the other axis (after 256 px on it) or has travelled 256 px along its own
(`AxisTracker::confirmed`). Cost: a level that is vertical from its first
frame is scouted along X until it has scrolled 256 px, and a scene is scouted
once; a vertical-first game needs the scene to be re-entered (or the scout
re-armed) after that. Not measured here: no such game was reached.

## 3. Super Mario World

No vertical level is reachable by the scripts (Yoshi's Island 1 and its pipe
room only). The SNES half of the Y machinery (a 16-bit player Y word,
`camera_y` `$1C`, the two `hold` rows on the same bytes) is checked in Yoshi's
Island 1 with an UP pass (`smw_up_pass_releases_the_hold_and_moves_the_player_y_word`):
Mario's Y goes from 344 to 64 (the hold would pin it at 150), the camera's Y
word stays at 192 (the level does not scroll vertically), the pass ends
"Stalled". The SMW `[scout]` table gains `camera_y $7E001C`, `player_y
$7E0096`, `player_y_hi $7E0097` (the same bytes as the existing rows).

## 4. A bug the shot found (W33-09)

`apply_profile_match()` runs in `open_rom_path` BEFORE the core thread is
spawned, so its `SetSceneKey` went nowhere; the scout's plan (app state) carried
the key and the live canvas model did not. The scout's tracker saw a "key
change" on its first frame and every NES scout in the app ended at frame 1
(`started 1 finished 0 aborted 1`; the old shot assertion
`!finished || cells > 0` passed on exactly that). The key is now kept
(`scene_key_spec`) and sent again once the core exists.
With a working scout the horizontal NES shots (`scout-smb3`, `scout-mm2`) merge
0 cells: a live canvas below the top registration confidence (a player who has
not moved since the level began) takes nothing from a scout
(`merge_unknown_from`, W33-04). That is by design; the shot now asserts a clean
finish and reports the cells.
