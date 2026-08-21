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

## 7. Decision

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
