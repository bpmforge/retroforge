# Design: Frame Interpolation (experiment)

Closes traceability gap **G5** ("frame interpolation named but
undesigned"). Written under ticket W8-09, whose first criterion makes this
document a **gate**: no implementation may precede it.

## 1. What is being proposed

Generating intermediate frames between the console's own, so that motion
appears smoother on a display refreshing faster than the emulated machine.
The SNES and NES produce ~60 unique frames per second; a 120 Hz or 144 Hz
panel repeats each one. Interpolation would instead synthesise the frames
in between.

This is the only enhancement in the project whose output is **entirely
invented**. The sprite-limit bypass reveals sprites the hardware computed
and dropped; the stitcher reveals pixels the machine really drew, earlier;
de-flicker reconstructs a sprite from OAM snapshots that genuinely
happened. An interpolated frame corresponds to no machine state at all. It
is therefore held to the honesty contract at its strictest
(`crate::experiments::HonestyLabel`, `invents: true`).

## 2. What interpolation can preserve

- **Apparent smoothness of continuous motion.** Scrolling backgrounds and
  sprites moving at a constant velocity are exactly the case where a
  midpoint is a good guess.
- **The accuracy-exact stream itself.** Interpolation is strictly
  downstream of `CoreSink`; the real frames pass through untouched and can
  still be hashed, recorded, and compared. Every existing golden keeps
  working, because interpolation never edits the frames it sits between.

## 3. What interpolation cannot preserve

This is the section that decides the feature, and every item below is a
property some real game depends on.

### 3.1 Input latency

Interpolating between frames N and N+1 requires **having** N+1, so frame N
cannot be displayed until N+1 exists. That is a full extra frame of
latency — ~16.7 ms — added to every input, permanently, in exchange for
smoothness. For an emulator whose users measure input lag, this is not a
side effect; for many it is disqualifying on its own.

Extrapolation (predicting N+1 from N−1 and N) avoids the delay and pays
for it in wrongness: it guesses, and it is most wrong exactly when motion
changes, which is when the player is looking.

### 3.2 Deliberate sprite flicker

NES games flicker sprites *on purpose* to work around the 8-per-scanline
limit; the flicker is how the game shows nine objects. Blending
consecutive frames turns that into semi-transparent ghosting — an effect
the game never had, replacing one it intended. Note this is the opposite
problem to FR-ENH-002's de-flicker, which *reconstructs* the dropped
sprite; interpolation would instead average the two states into something
that is neither.

### 3.3 Hard cuts

A scene change between frames N and N+1 must not be interpolated: a
cross-fade appears where the game had an instant cut. **This one is
tractable**, because the project already detects scene changes for the
stitcher (`rf_enhance::scene_identity::compute_scene_id`,
`scene_tracker`). A frame pair spanning a scene-id change is not a
candidate.

### 3.4 Colour-domain effects

Palette cycling, colour math, and per-line CGRAM writes (the gradient
RedSpaceHDMA draws) are changes in *colour*, not motion. Interpolating
them produces intermediate colours that existed in no frame, and does so
most visibly on the effects games use precisely because they are eye-
catching.

### 3.5 The HUD boundary

A static status bar over a scrolling playfield interpolates badly at the
seam: the playfield wants midpoints, the HUD wants none, and a whole-frame
blend gives the HUD a shimmer. **Also tractable**: W8-06 lands HUD band
detection (`rf_enhance::hud`), so the HUD's scanline range is known and can
be excluded.

## 4. Interaction with the determinism invariant

**Interpolation must never feed back into the simulation, and structurally
cannot if it lives where this design puts it.**

- It sits **downstream of `CoreSink`**, in the renderer or the shell. Cores
  never import upper layers (ARCHITECTURE §6), so a core cannot observe
  that interpolation is happening.
- It consumes the accuracy-exact frame stream and emits display frames. No
  emulator state is read or written.
- **Replays are unaffected.** A replay is an input log against a
  deterministic machine; interpolation changes neither.

There is one real interaction, and it is with the *player* rather than the
machine: §3.1's added frame of latency changes when input is sampled
relative to what is on screen. That is a human-facing change, so the rule
is: **interpolation must not alter the input-sampling schedule**, and a
session running it must say so.

## 5. Where it would live

```
core ──▶ CoreSink ──▶ [accuracy-exact frames] ──▶ interpolator ──▶ display
                                  │
                                  └──▶ goldens, replay, mode-invariant hash
```

The branch matters: everything that verifies the emulator reads the
accuracy-exact stream, and only the display path sees interpolated frames.
This is the same shape `overlay_scanline` and `sub_scanline` already use —
a parallel channel that cannot displace the reference.

## 6. Honesty contract

- Opt-in, off on a fresh install (law 6).
- Absent from Accuracy Mode entirely, not merely disabled.
- Labelled `invents: true`, with a claim naming the added latency, because
  a smoothness feature that quietly costs a frame of input lag is the kind
  of trade a user must opt into knowingly.
- Never active during a recording, a replay, or a golden capture.

## 7. Decision (W8-09, superseded by §8 on 2026-10-25)

**Not implemented under W8-09, and the reason is §3.1.** A full frame of
input latency is a poor trade for smoothness in an emulator, and it is not
a cost that better engineering removes — it is inherent to interpolating
toward a frame you must first possess. The alternative, extrapolation, is
wrong exactly when the picture changes.

What *is* delivered is the part that is unambiguously correct and that any
future implementation needs first: the **candidacy test** — given two
consecutive frames, may this pair be interpolated at all?
`rf_enhance::interpolation` answers it from scene identity (§3.3) and HUD
bands (§3.5), and refuses on the cases above. Building that now means the
question "is this frame pair safe?" is settled and tested before anyone
writes a blender, rather than being discovered afterwards from a bug
report about cross-fading cutscenes.

### Preconditions for revisiting

1. A measured latency budget that the project is willing to spend, stated
   in ms and agreed rather than assumed.
2. Extrapolation evaluated against real motion, with an honest account of
   its failure mode at direction changes.
3. Per-region interpolation (playfield only, HUD excluded) demonstrated,
   since a whole-frame blend is already known to be wrong by §3.5.

## 8. Implementation (W36-07, D-028, 2026-10-25)

Brad's D-028 ruling meets precondition 1: **one frame of latency (about 17 ms), disclosed in the UI,
opt-in, off on a fresh install, absent from Accuracy Mode.** Precondition 3 (per-region, never a
whole-frame blend) is how it is built. Precondition 2 (extrapolation) is not needed: the design
interpolates between two frames it holds, so it never guesses ahead.

### Where the code lives

| Piece | Where | Thread |
|---|---|---|
| Per-line scroll, background colour under every dot, OAM index per dot (presentation-only reads of the PPU, excluded from save states, off by default) | `rf-nes` `Ppu::line_scroll`, `Ppu::bg_under`, the sink's sprite ids | core thread, read after a real frame |
| `capture` (reads the above into `Layers`), carried on `FrameMsg::interp` | `retroforge::frame_interp` | core thread |
| `Presenter` (holds the last two console frames, shows the older one, composes between) | `retroforge::frame_interp` | UI thread, downstream of the channel |
| `compose`, `detect_hud`, refusals | `rf_enhance::interpolate`, `rf_enhance::interpolation` | pure functions |

Nothing is written back to the machine, `FrameMsg::rgba` is still the console's own frame when it
leaves the core thread (the Presenter swaps in the older one on the UI side), and the capture is off
unless the setting is on, so the accuracy-exact stream, goldens, replays and the mode-invariant
hashes are untouched. It is never on while a video recording runs or a screenshot is taken.

### How an intermediate frame is made (alpha 0..255 of the gap, fixed point)

* **Background.** Frame N's background colour under every dot (including the dots a sprite covered),
  shifted by `alpha` of the scroll the PPU applied between N and N+1, **line by line**. The scroll
  source is the PPU's own per-line scroll at dot 1 of each picture line (the loopy `v` register less
  the two prefetched tiles, plus fine X), not the camera RAM and not the `$2005` write events: the
  RAM runs about two frames ahead of the picture (`which_frames_oam_does_the_picture_come_from`), so
  its delta is the wrong pair of frames, and a per-line value is exact for a split-scroll game by
  construction (a status bar above the split has a different, constant value than the playfield
  below it, so it does not move). Columns the shift uncovers are taken from N+1's background (the
  same world, drawn by the machine), never blended.
* **Sprites.** Frame N's own sprite pixels, moved by `alpha` of how far the same OAM entry moved by
  N+1. The movement is read from the picture (bounding box of the dots each OAM index won, in both
  frames), not from the OAM bytes (the OAM runs ahead of the picture too). With a recipe, entries
  whose pixels sit near one of its object places (enemy slots, the player: `Map::anchors`, from the
  recipe's RAM rows) are one object and all move by the median movement of the entries that have a
  counterpart; without a recipe each OAM entry moves by its own (this is the generic path W37-06
  will build a UI for). Lower OAM index stays on top; a behind-background entry is drawn only where
  the background is the backdrop colour.
* **New and unmatched sprites.** A sprite that exists only in N+1 is not drawn until N+1 (we only draw
  N's pixels). A sprite that exists only in N, or moved implausibly far (over 32 dots), or changed size
  between the frames, is drawn where N had it until N+1.
* **HUD.** Rows are excluded through `candidacy`: a top or bottom band, at most half the frame, whose
  per-line scroll equals its first (last) line's in both frames while the rest of the frame differs
  (a status bar over a scrolling playfield). Those rows, sprites in them included, are copied from N.
  A frame with no split has no HUD to exclude and nothing in it is wrongly held.

### Refusals (the pair is repeated: frame N stays on screen)

Not consecutive (a frame the UI never saw), palette changed (`$3F00-$3F1F` digest; star-power and
flash cycling land here), scroll step over 24 dots between frames (`LargeMotion`), a background that is
not N's shifted (`Discontinuity`: at least 80% of the dots must match; this is the cut detector, because
the NES edge signature behind `SceneId` is flat, `scene_identity` module doc), every row HUD, and layers
that do not describe a picture (`Malformed`).

### Presentation and honesty

* When console frame N+1 arrives the Presenter shows N (so the picture is one console frame late: the
  disclosed latency); each later repaint in the gap composes `alpha = elapsed / measured gap`. At
  60 Hz no repaint falls in a gap and nothing is composed (the tally reports those as untried).
* It steps aside, passing frames through untouched, whenever another stage rewrites the picture after
  the console: widescreen, an HD pack, a shader, fog, the lens, the diorama, comparison/peek, a
  recording or screenshot, run-ahead, de-flicker, the sprite overlay. An intermediate frame would
  bypass those stages, so it is not drawn around them.
* The row ("Smoother motion") is generic, NES only for now, Enhanced-only and does not exist in
  Accuracy Mode. Its label is `rf_enhance::interpolation::label()`: `invents: true`, claim naming the
  one-frame delay, shown under the switch.
* No float in the composer (`alpha` is 0..=255); loops are `for` over ranges or iterators (law 8).
