# W32-09: the level scout on the NES (Super Mario Bros. 3, Mega Man 2)

Clean-room: every address below was found or confirmed by running the game in
this repo and watching RAM against the core's `ScrollWrite` events; no
disassembly-derived map and no emulator source was used. The probes are
`crates/retroforge/tests/w32_09_scout_nes.rs` (ROM-gated, `--ignored`):
`smb3_events`, `smb3_ram`, `smb3_gate`, `smb3_death`, `smb3_cam`,
`smb3_wrap`, `smb3_restored`, `smb3_scout_probe`, `mm2_ram`,
`mm2_scout_probe`. Level reached headlessly with scripted pad presses (NES
bits: A 0, B 1, Select 2, Start 3, Up 4, Down 5, Left 6, Right 7; the GUI's
Enter = Start, X = A).

## 1. The scroll source and its timing

The composite stitcher (`canvas_accum` + `ScrollTracker`) places a frame from
that frame's `Scanline`/`ScrollWrite` events. SMB3 World 1-1, per frame:
`S0-191 IRQ S192 W(x,0)@193 .. W(0,192)@194 W(0,239)@194 S194-239 VBL
W(cam,0)@240 W(cam,239)@240`. The scroll the game writes in vblank, at the
END of frame n's slice, draws frame n+1 (`$00FD` after frame n equals that
trailing write's x on **103 of 103** moving frames; the previous write
(`drawn[f]`) matches nothing). `ScrollTracker` already carries a frame's last
write into the next frame's first band, so the NES composite path has the
W32-08 rule built in and **`ScrollLag` is not applied**. Check: consecutive
scout frames, shifted against each other, match 99.5% of their pixels at a
shift equal to the stitcher's frame-position step (4 px, 10 frames of 10).

Three defects of the existing NES composite path showed up the moment a scout
walked past 256 px (all fixed, with tests):

* **x wraps at 256, not 512.** The events carry the camera's low byte only
  (no nametable bit): x runs 253, 255, 1, 3. `WorldAxis::advance` (modulus
  512) read 253 -> 3 as 250 px backwards, so every 256 px of level landed on
  top of the last (live frame position 212 -> 19 over a plain walk).
  `WorldAxis::advance_x` reads a move that is implausibly long as a 512 wrap
  but small as a low-byte wrap (at most 32 px) as the latter; a jump of 300
  is still a cut.
* **Left 8 dots.** SMB3 clips the leftmost 8 dots (backdrop colour). A column
  scrolling past the left edge was repainted with sky by the frame that had
  it clipped, so the whole stitched level came out as sky. `stitch_frame` now
  lets a backdrop dot in those 8 columns fill only an unknown cell.
* **A one-row split band** (row 193, fine x different every frame) was
  stitched at the primary band's position: a dotted line across the canvas's
  top row. A non-primary band under 8 rows is now HUD.

The status bar (rows 194-239 on SMB3) is a HUD band by the existing
zero-delta rule and never reaches the canvas; Mega Man 2 has none.

Also found: the app's `set_widescreen` asked a NES core for a 512-wide frame;
the NES draws 256, so its picture sat at the left of the buffer and the
margin fill centred it 128 dots off. A NES request is now 256 wide.

CHR banks (SMB3 is MMC3): rf-nes does not expose mapper state, so bank
switches cannot be counted. The IRQ at row 192 swaps banks for the status bar
every frame, and the stitched tiles and palettes are right at 4 px/frame. At
8 px/frame they are not (pink blocks come out green, content misplaced): the
game updates palettes/columns on a schedule of its own, so a faster scout
draws a wrong level. Stitching final pixel indices (not tiles) means a
bank switch cannot corrupt the canvas, only a game too far behind its own
camera.

## 2. Super Mario Bros. 3, World 1-1

| Byte | Evidence |
|---|---|
| `$00FD` | = the vblank scroll x (low byte of the camera) on 103/103 moving frames. |
| `$0012` | High byte of the camera: 0 below 256, +1 at each 256 (0 -> 1 -> ... -> 5 on the walk). |
| `$0090` / `$0075` | Mario's X, low / high. Walking: 0x18..0xd7, then 0x09 with `$75` 0 -> 1; camera = X - 0x80 from X >= 0x80. Poking both moves Mario and the camera follows. |
| `$00A2` | Mario's Y: 0x80 on the ground, 0x42..0x80 in a jump, 0x9f once fallen. |
| `$000F` | 1 for 230 frames of standing in the level from 80 frames after A; 0 on the map; 0 in the death hang after a goomba hit (diff of 25 bytes, this the simplest). It reads other values (0xf1, 0xee ..) once a scout's Mario is hit, so it is a "stable and alive" gate, not a flag with a documented meaning. |

Natural walking dies: the first goomba (frame ~46) and then the first pipe's
piranha plant (x 0x15e).

Strategy (b), move the player: poking `$90/$75` +step with neutral input.

| Recipe | Result |
|---|---|
| X only | Mario reaches the first goomba in 8 frames and dies; the camera never moves. |
| X + hold `$A2` = 0x40 | Survives the goomba, dies in the first pipe's plant (camera stuck at 0xe0). |
| hold `$A2` 0x30..0x3c | Same. |
| hold `$A2` 0x10 | Camera follows to the end of the 256-px wrap, scout stops "SceneChanged" there (the wrap defect above). |
| hold `$A2` 0x28 (or 0x20), step 4 | **Works.** 2816 px (the whole of 1-1 up to the bonus-room end), 727 frames, both passes end by "no new level for 60 frames". |
| step 8 | Reaches the same width but the game cannot keep up: wrong palettes, misplaced tiles. Step 4 is right. |

Camera Y does not follow a high Mario here (`$FC` stays 0xef).

## 3. Mega Man 2, Air Man's stage

| Byte | Evidence |
|---|---|
| `$001F` / `$0020` | Camera X, low / high screen. `$1F` = the vblank scroll x on 414/414 moving frames; `$20` 0 -> 1 at 256. |
| `$0460` / `$0440` | Mega Man's X, low / high (0x80 .. 0xcf, 0x1e + `$440` 1; X - camera = 0x80 while scrolling). |
| `$04A0` | Mega Man's Y: 0x74 on the first platform, 0xf0 after a fall into a pit. |
| `$06C0` | Health, 0x1c at the start; 0x1a after the hits of the run that fell, and still 0x1a dead: **not** an alive flag. |

Recipes: writing the camera (`$1F`) moves the picture but the game streams
nothing: the first 512 px repeat for 2672 px (rejected). Poking X moves Mega
Man and not the camera: it follows only a player who walks. So the profile
strategy is `walk`: no pokes of X, Right held on the scout's pad, Y held. A
held Y of 0x70..0x74 stops at the first obstacles (178 frames), 0x60 at 302,
0x40 at 434; **0x20 floats above all of them: 2050 frames, 2560 px, ended by
the stall rule** where the camera stops (the end of the first section, with
the platform visible at the right edge). Not checked beyond: vertical
sections, doors and boss rooms are never reached.

The scout starts only at full health (`$06C0` = 0x1c): a hurt Mega Man is not
scouted. The cost is about 2050 frames (~10 s of worker time).

## 4. What the margins show (docs/screens/w30-w31/scout-{smb3,mm2}-32x9-*.png)

Taken in the GUI harness right after entering the level, the live game paused
once the worker runs. Off: black either side of the 256-dot picture. On: the
right margin continues the level (SMB3: the pink, blue and green blocks and
the pipe, contiguous with the picture's right edge; MM2: the platform, then
two Air Tikki). The left margin is black in both: the level starts at the
left edge, nothing is left of it. SMB3's status bar rows are not extended
(black under the margin). One small white sliver near the top of SMB3's right
margin may be a clipped cloud edge. The scout ran 788 frames (two passes) on
SMB3 and 2050 on MM2, ~494k and ~553k cells merged.
