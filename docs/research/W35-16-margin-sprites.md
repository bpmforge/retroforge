# W35-16: sprites and enemies in the 32:9 margins

Observation only (CLAUDE.md law 5, D-021): every figure is a measurement of the running game through the
harness (`crates/retroforge/tests/w35_16_margin_sprites.rs`, release, one process at a time). Hardware claims
cite fullsnes "SNES PPU Sprites (OBJs)" and nesdev "PPU OAM" / "PPU sprite evaluation".

The question: the 32:9 margins show background layers only. What can a margin show of the game's OBJECTS, and
which source is honest?

## 1. What the hardware lets a game say about an off-screen object

* **SNES.** An OAM entry has a 9-bit SIGNED X (-256..255). An object can hang off the left by up to its width,
  and off the right by at most `width - 1` (X = 255). It can never sit at X >= 256: the entry has no way to say
  "100 px right of the picture". A game that wants an object further out has to leave it out of OAM.
* **NES.** X is an unsigned byte, the PPU stops drawing at dot 255 and a sprite cannot be left of 0: the tail
  of a sprite at X = 249..255 is the only thing the hardware ever clips on the right (at most 7 columns), and
  nothing on the left.

So an OAM-driven approach has a hard ceiling of `width - 1` px (7 NES, up to 63 SNES) past an edge, and only
for an object that is ALREADY partly on the picture.

## 2. Measurement: how far past the edge do real games put objects?

Scripted play (the same scripts the scout tests use), one OAM decode per frame, screen px, objects whose
vertical span is on the picture. "Straddler" = an entry partly inside 0..255 and partly outside.

| game | frames | OAM entries seen | straddlers (entry-frames) | max overhang right | max overhang left | frames with a straddler |
|---|---|---|---|---|---|---|
| Super Mario World, the scripts' first level (SNES) | 921 | 9,426 | 183 | 15 px | 15 px | 82 |
| Super Mario Bros. 3, 1-1 (NES) | 930 | 5,752 | 46 | 7 px | 0 | 25 |
| Castlevania, stage 1 (NES) | 1,690 | 35,282 | 0 | 0 | 0 | 0 |
| A Link to the Past, road session (SNES) | 9,304 | (not totalled) | 2,732 | 15 px | 15 px | 923 |
| Super Metroid, Ceres (SNES) | 1,115 | (not totalled) | 0 | 0 | 0 | 0 |

(`dump_smw`, `dump_smb3`, `dump_cv`, `dump_alttp`, `dump_sm`.) The games cull objects to the picture themselves: SMW's overhang tops out
at 15 px although the hardware allows 63, A Link to the Past's too; Castlevania and Super Metroid (Ceres) never write a sprite past the edge at all.

SMW also writes entries that are wholly OFF the picture (534 entry-frames). They are of two kinds, and
neither is an enemy approaching: two entries parked at X = -64/-68, Y = -15 for ~27 frames each (slots
123/124), and, from frame ~667, a 4 x 4 block of 16 px entries at X = -180..-239 moving right at 2 px a frame (a
large scripted object, not a spawn). This is the trap for "none appear where the game has no object": a rule
that paints every entry would paint these in the left margin. **The rule used (bsnes-hd's `safe`, written
down in `rf_enhance::widescreen::ObjPolicy`): an entry is painted past the edge only when part of it is on the
picture.**

## 3. The approaches

**(a) Live OAM overhang.** Draw an entry's off-picture part where the game put it. The SNES core already does
the drawing: `rf-snes` `obj.rs` composes a widened line with sprites at their 9-bit X plus the pad
(`WidenMask.obj`), and the shell then throws it away wherever the stitched canvas knows the margin
(`fill_margins_keeping`: world first, native only where the canvas is unknown; compose.rs: "Sprites are never
known in a margin"). So on SNES it is "keep the core's own overhang", no core change. On the NES the core draws
256 dots only; the 7-px tail would need a new core channel (see section 6).
* Trade-offs: exact (it is the game's own pixel at the game's own position, error 0 by construction); zero
  divergence; costs one byte-per-dot capture and one pass over the margin dots. But it can only complete an
  object that is already entering: the lead it gives is `overhang / speed` frames (SMW: <= 15 px).

**(b) A hidden copy, re-run with the live game, objects at true world positions.** The copy needs its camera
(and so its player) elsewhere, or it renders the same OAM the live game does. Objects it shows are
therefore a DIFFERENT machine's: spawn and cull decisions the game takes from the player's and camera's
position differ, enemies that react to the player move differently, and the copy must be resynchronised from
the live state every few frames. Cost: a second core at full speed on a worker thread for the whole session
(the scout's own worker costs a quarter of the live fps under load, W32-08), plus input mirroring that cannot be
exact once the copy's player stands elsewhere. It fails "none appear where the game has no object" by
construction (it shows objects the live game does not have), so it would carry the D-023 approximate badge and
more. Not built.

**(c) Scout-time sprites frozen into the canvas.** The scout deliberately never takes sprites (scout.rs, law 4
section): an enemy it woke is not an enemy the live game has, and a frozen enemy is stale the moment it is
placed (SMW YI1's walkers cover several px a frame). Wrong by design. Not built.

## 4. Recommendation

**(a)**, opt-in, default off, SNES now. By the evidence it is the only approach whose every pixel is the game's
own, and the measurements say what it buys: it completes an object that is half over the edge (up to the
game's own overhang: SMW 15 px) and nothing more. **The acceptance as worded ("enemies show in the margin
BEFORE they enter, at the position they then enter at") cannot be met by any single-machine approach, because
the information is not in the machine: games cull before they write OAM.** (b) is the only route to "before",
and by the argument above it trades exactness for a copy that is wrong in the ways that matter. The measure
that is honest for (a): early px at first paint (the overhang) and the frames of lead it is, per object;
position error at the crossing is 0 (same entry, same frame); and no margin pixel on a frame with no entry
over an edge.

No D-023 badge: nothing here is learned or guessed. The control says what it is: "only the part of an object
the game placed past the picture's edge".

## 5. Implementation (SNES)

* `rf-renderer` `FrameBuffer::set_sprite_id_capture`: per dot, the OAM index of the sprite that won it
  (`PpuPixel::sprite_id`), 255 for none. Off (default) allocates and writes nothing.
* `rf_enhance::margin_sprites` (pure, console-free): `edge_straddlers` (the rule above, a 128-bit set over
  `(index, x, width)`), `mask_of` (ids and set to a 0/1 mask) and `paint` (copy the core's own dot into the
  margin where the mask says so; the 256 hardware dots and a patch's whole width are never touched).
* The core thread builds the mask from the frame's ids and the OAM (`EmuStepper::snes_oam_view`, a borrow) only
  while the toggle and widescreen are on, and sends it in `FrameMsg::margin_sprites`.
* The app paints it after `fill_margins_keeping` and only when the toggle is on (and the Diorama, which
  composes its own ground, is not).
* Toggle: `margin_sprites` in the per-game settings (written only when on), a checkbox under the Widescreen
  row beside the aspect picker. Off is the old code path: no capture, no mask, no paint.

## 6. Results (SNES, `measure_smw`; shots `docs/screens/w35/margin-sprites-{smw,alttp}-{off,on}.png`)

The widened frame the app asks the core for (512 dots), the app's own mask over it, SMW's scripted first level (921
frames of the play script; a Banzai Bill and walking enemies cross the edges):

* **Lead, per object.** 12 distinct objects crossed an edge (the Banzai Bill's four 16 px columns counted
  once; 27 entry episodes in all). First overhang at first paint: 1, 1, 2, 7, 13, 14, 14, 14, 14, 14, 15, 15 px;
  screen speed 1-3 px a frame, so the object is shown 0.5 to 14 frames early, median 7 (about 0.1 s). That is
  the whole "before": the game writes the entry only once it is that far on.
* **Position error where the object enters: 0 px, by construction and checked.** All 36,194 painted dots lie
  inside their own OAM entry's box (0 outside); the OAM read after the frame names exactly the sprites the
  frame drew (0 sprites with dots on both sides of the edge missing from the straddler set).
* **None where the game has no object: 0 frames** painted a margin dot without an entry over an edge (the
  parked entries and the block sliding in from X = -180 stay out: 18,497 margin sprite dots unpainted).
* **Off is the old picture.** Never-sent and sent-false give an empty mask on every frame, and the core's
  picture is byte-identical in all three cases (`w35_16_margin_toggle`, on the repo's SNES fixture). At 32:9 in
  the app, on against off from the same snapshot: 0 differing dots in the console's 256 (SMW and ALttP), 928 (SMW)
  and 104 (ALttP) dots differ in the margins, and off again equals off exactly.
* **Cost with it on, per frame:** the core thread's mask (OAM decode plus 114,688 id lookups) 40 us; the UI
  thread's paint 41 us on the busiest frame (960 dots); the capture inside `FrameBuffer::video_scanline` is one
  more pass over the line. About 0.1 ms of a 16.7 ms frame. Off: nothing runs.
* **RSS flat:** 20,000 mask-and-paint frames, 131 MB before and after; over the 921-frame play 118 to 131 MB
  (the finder and canvas model of the test rig, not this feature: the capture is one fixed 112 KB buffer and
  the mask is freed each frame).
* **What the shots show** (left to look at, described as seen). SMW: the Bill is sliced down its body at the
  picture's right edge with the margin a flat blue (no scout ran); with the toggle on its black body and red
  end-band continue about 15 px into the margin, then the blue resumes. ALttP: the soldier at the
  bottom right is cut in half at the edge in the off shot; on, the right of its body and its shield continue
  into the green margin (104 dots). Nothing else in the margin changes: they are not scouted levels, so the
  margins are plain backdrop either side of a small completed sprite, which is exactly what the evidence says
  this can give.

## 7. Not done, and why

* NES (SMB3's 7 px): the NES core renders 256 dots only (W32-09). The tail would need the PPU to emit the
  clipped columns on a new `CoreSink` channel; 7 px of a sprite already half over the edge is the whole prize
  (Castlevania measured 0), so it is left out and the toggle says it is SNES-only.
* BG priority in the margin: the core's widened line decides sprite-over-BG against ITS widened BG; the
  margin's BG is the stitched canvas. A sprite that the hardware would put behind a foreground tile (BG1
  priority) can show in front of canvas foreground within the <= 15 px overhang.
* Entries wholly off the picture (section 2): never painted.
