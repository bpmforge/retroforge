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
  the console: widescreen (no longer, W37-06, section 9), an HD pack, a shader, fog, the lens, the diorama, comparison/peek, a
  recording or screenshot, run-ahead, de-flicker, the sprite overlay. An intermediate frame would
  bypass those stages, so it is not drawn around them.
* The row ("Smoother motion") is generic, NES only for now (SNES too since W37-06), Enhanced-only and does not exist in
  Accuracy Mode. Its label is `rf_enhance::interpolation::label()`: `invents: true`, claim naming the
  one-frame delay, shown under the switch.
* No float in the composer (`alpha` is 0..=255); loops are `for` over ranges or iterators (law 8).

## 9. Generic: palette cycling, widescreen, SNES (W37-06, 2026-10-26)

Section 8 built the NES path for one recipe-aware game. W37-06 makes it generic: it no longer needs a
recipe, it runs with widescreen on, and it covers the SNES. The honesty contract of section 6 is unchanged
(opt-in, off on a fresh install, absent from Accuracy Mode, one frame of labelled latency, never while
recording, replay or golden capture, the simulation and every console frame untouched).

### Palette cycling (NES)

The layers now carry colours (`0x00RRGGBB`), not palette indices, so each dot is drawn with the colour it
had in the frame it is taken from: "frame N's pixels with N's palette". `interpolate::palette_verdict`
refuses a palette change unless it is cycling: at most two of the 15 background entries differ, neither is
the backdrop (`$3F00`, mirrored at `$3F10`). Sprite entries may change freely (star power): sprites are N's
own pixels. A dot that differs between the frames only by being one of the cycled colours counts as the same
dot in the background-fit test. Fades and flashes (three or more background entries, or the backdrop) are
still refused. SMB 1-1: 91.3% of pairs and 91.6% of the steady running ones (the question-block shimmer,
entry 13, was every refusal on steady pairs) became 98.9% and 100.0%.

### Widescreen (NES)

The Presenter holds the WIDENED picture. When console frame N+1 arrives the shell widens it first (its
margins composed from its own canvas, VRAM or level image, with the W36-06b enemy strips drawn in), then
`interpolate::widen` embeds the console's layers in it, then the Presenter swaps in the older one. (Swapping
the console frame first and widening it after would have put frame N's centre between frame N+1's margins,
a one-frame seam every frame.) The margin dots are background, so the same per-line scroll moves them and
the dots it uncovers come from N+1's wide picture. **Margin enemy strips move with the scroll** (they are
part of the margin background), not held: an object standing in the world is exact, one walking drifts from
its true place by its own speed times alpha until the next console frame redraws it. Measured in one frame
pair of SMB 1-1 a right-margin goomba stood still on the screen between consecutive console frames while the
ground moved 3 dots, so the intermediate showed it 2 dots off for one picture: held would have been exact for
that object and 2 dots wrong for the typical walker. Not resolved; the choice is the better on average.
Sprites that straddle the console's edge move by their own OAM delta inside and by the scroll in the strip.

### SNES

Nothing about an SNES frame needs a recipe. Captured presentation-only, off unless the setting is effective,
never in a save state, nothing written back:

| What | Where it comes from |
|---|---|
| Layer (BG1-4, sprite, backdrop) and priority of every dot | `FrameBuffer::layer_codes` from `PpuPixel` (rf-renderer) |
| OAM entry of every sprite dot | `FrameBuffer::sprite_ids` (existing) |
| Scroll of every background, brightness and backdrop colour of every row | `FrameBuffer::line_regs` from `CoreSink::line_registers`, asked for only while on |
| Mode, BG3 bit and the hazards of every latched row | `rf_snes::ppu::Ppu::line_facts` (reads the completed-frame latches, writes nothing) |
| Priority order of every mode | `rf_snes::ppu::priority_slots` |

A backdrop dot that colour math adds a sub-screen BACKGROUND to is recorded as that background
(`LAYER_SUB`): Super Mario World draws its far hills that way (BG2 on the sub screen, "add" over the
backdrop), and without it the hills would sit still while everything else moved.

`interpolate_snes::compose_snes` moves each LAYER by its own per-line scroll and re-stacks them in the
mode's own priority order: the dot at (x, y) shows layer L if frame N's dot at (x + sx_L, y + sy_L) was L (else
N+1's, off N's picture), the front-most claim wins, sprites are N's pixels moved by their own OAM delta and
take part in the order by their priority level. A dot nothing claims takes N+1's picture, then N's, then the
row's backdrop. The colour of a dot is the colour the console showed where it came from, so brightness and
colour math travel with their layer.

**HUD bands are fixed by construction**: BG3's status bar, or an IRQ-split band whose scroll register is
rewritten to the same value, has a zero step, so its dots are gathered from themselves.
`SnesStats::fixed_rows` counts the rows where every visible layer stood still while others moved.

Refusals (the pair is repeated), each from a named source: mode 7, hires or pseudo-hires, mosaic, a window
hiding part of a main-screen layer, interlace, forced blank or direct colour, a register written
mid-line (all from the PPU's latched line facts: a row with one is HELD from N when they are the minority of
rows, the pair is refused with the reason when they are the majority); brightness differing on any row or the
backdrop colour on more than an eighth of them (fade, flash); a layer's scroll jumping more than 24 dots;
layers that are not their shifted selves (a cut); not consecutive. Colour math itself is not refused: it is
carried by the dots, and a gradient that changes is the backdrop test's business. What a front layer or a
sprite hid in both frames is not captured, so the sliver a sprite or a layer edge moves over is the one
place the result is a guess (measured: 0.95% of dots differ from frame N+1 when a steady SMW pair is
composed at alpha 255, worst pair 2.6%).

Widescreen on the SNES: when the core itself widens the lines the frame is simply wider and the same gather
works; when the shell widens it, `SnesLayers::widened` marks the margin dots and moves them with the layer
that has the most visible dots.

### Measured

| Pair set | Interpolated | Notes |
|---|---|---|
| SMB 1-1, all pairs | 98.9% (1978/1999), was 91.3% | refusals: 17 scroll jumps, 3 palette changes, 1 cut |
| SMB 1-1, steady running pairs | 100.0% (1190/1190), was 91.6% | |
| SMB 1-1 at 32:9 with the scout canvas and live-enemy strips, 16 s in the app | 100.0% (954/954) | no refusals |
| SMW Yoshi's Island 1, steady pairs (level, camera moving) | 100.0% (289/289) | the other 1,081 pairs of the 2,399 are the level's fades, mosaic wipes and loads: refused |
| SMW Yoshi's Island 1, all pairs of a 2,400-frame play | 57.1% (1370/2399) | Blanked 573, Fade 214, Mosaic 227, MidLine 15 |

Cost per intermediate frame, release build: NES 256x240 about 240 microseconds, NES 856x240 (32:9)
about 1.0 ms in the app, SMW 256x224 about 0.5 ms; capture on the core thread about 8 microseconds per SNES
frame, 17 per NES frame.

Not done: the pass-through list still holds for HD packs, shaders, the lens, fog, peek, comparison and the
Diorama view (their stages run after the console picture and an intermediate frame would bypass them), and
for run-ahead, de-flicker and the sprite overlay (their pictures are not the plain one).
