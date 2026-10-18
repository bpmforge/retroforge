# Better stitching for RetroForge's widescreen margins (W33 plan)

Status: plan only, 2026-10-17. Read-only review of `main` at 184ce48 (W32-09
merged). Nothing here is implemented. File:line references are to that
commit.

Goal (Brad): up to 32:9 on SNES and NES where the margins show the REAL level
left and right, seamlessly, for many games, with the live emulator untouched
and the whole thing opt-in.

The one-sentence verdict: the pipeline is structurally right (per-layer
canvases, a scout that lets the game draw its own level, unknown-only merge,
live always wins) but it throws away information the core already produces
(the sub-screen's per-pixel layer label, per-line scroll registers, per-line
CGRAM, the colour-math operator), carries two canvas systems that disagree on
scene and placement, and needs hand-found RAM addresses for every game. The
fixes are mostly transport and consolidation, not new theory.

---

## 1. Critique of the current architecture

### 1.1 What is right (keep)

* **Per-layer canvases placed by each layer's own scroll**
  (`crates/retroforge/src/layer_canvas.rs:221-267`). This is the correct
  model; parallax falls out of it. The row-by-row front-to-back evidence
  (`row_orders`, layer_canvas.rs:315-372) is a clever stand-in for priority
  the core could state outright, but it works.
* **`Option<PpuPixel>` cells, never a guessed fill** (`crates/rf-enhance/src/
  stitcher.rs:24-33`). Honesty is the whole product; keep it.
* **One scroll-timing rule** (`ScrollLag`, layer_canvas.rs:84-121) with the
  measurement behind it (docs/research/W32-07-scout-smw.md §1: 39 110 of
  39 110 cells agree after the fix). Keep the rule and the test
  `the_composite_and_layer_canvases_place_the_same_content_at_the_same_world_x`
  (layer_canvas.rs:1051-1078).
* **Scout on a worker, live core only ever read, deltas by dirty box,
  unknown-only merge** (`crates/retroforge/src/scout.rs:23-36, 869-931`;
  `layer_canvas.rs:420-433` for the merge). Law 4 holds by construction: the scout owns its
  `EmuStepper`; the core thread does one `snapshot()` (scout.rs:1051).
* **Profiles describe, the user enables** (`crates/rf-enhance/src/
  widescreen.rs:236-258`). Law 6 is encoded in the type.
* **Observation-only RAM rows with evidence in the notes** (profiles/snes/
  super-mario-world/profile.toml:150-188; W32-07/W32-09 research notes).
  This is the clean-room discipline; the auto-discovery below must produce
  rows of exactly this shape.

### 1.2 What is fragile or wrong (cite and fix)

**F1. The SMW far hills are already labelled; the bundle drops them.**
`crates/rf-core-api/src/video.rs:75-90` (`SubPixel { palette_index, layer,
op, fixed, main_black }`) and `crates/rf-snes/src/ppu/mod.rs:1286-1323`
show the sub-screen winner's `PixelLayer` is computed per dot and emitted
through `CoreSink::sub_scanline` (`crates/rf-core-api/src/core.rs:152`,
received at `crates/retroforge/src/core_thread.rs:1200`). `FrameBundle`
carries only `video` (main screen), so `LayerCanvasAccumulator::observe_frame`
(layer_canvas.rs:247-259) never sees BG2 on the lines where SMW puts it only
on the sub screen, and `ScoutSink` (scout.rs:299-321) does not implement
`sub_scanline` at all. The labelling work the brief asks about ("sub-screen
labelling in the core") is done; what is missing is transport. Caveat:
`crates/rf-snes/src/core.rs:455-463` renders the sub line only when
`line_color_math_active(y)`; that is exactly the set of lines where the hills
are visible, so it is the right set.

**F2. Mid-frame (HDMA) scroll is available per line and unused.**
layer_canvas.rs:18-21 says a layer whose scroll changes mid-frame "is
stitched at its first value: fullsnes does not let us do better from register
snapshots". But `crates/rf-snes/src/ppu/mod.rs:123-134` (`LineState { hofs:
[u16; 4], vofs: [u16; 4], .. }`) latches every layer's scroll per visible
line (ticket W7-07), and `line_state_for_test(y)` (ppu/mod.rs:911) already
reads it. The stitcher samples `bg_geometry()` once per frame
(`crates/retroforge/src/stepper.rs:1455-1467`, core_thread.rs:466-482). Any
game with HDMA parallax (common on SNES) is stitched wrong today, and the
scroll lag rule exists partly to paper over "which register read drew this
line". Per-line scroll in the bundle removes the question: the core knows
what each line was drawn with.

**F3. Sky banding and black below ground come from a vote, not from state.**
`row_backdrops` (layer_canvas.rs:566-595) takes the most common displayed
colour among a row's `Backdrop` pixels and returns `None` for a row with no
visible backdrop pixel; `app.rs:12128-12133` then falls back to
`resolve_index(0, palette)`, which in SMW is CGRAM 0 = black. The banding in
`docs/screens/w30-w31/compare-widescreen-32x9.png` is this vote changing
row to row; the black under the ground is the `None` case. The exact answer
per row exists: CGRAM entry 0 from `line_palette(y)` (ppu/mod.rs:882), the
colour-math fixed colour from `sub_scanline`'s `fixed_color`, and the
`ColorMathOp` for backdrop (video.rs:60-73). Reconstruct, do not vote.

**F4. Remembered indices are resolved with the CURRENT row palette.**
app.rs:12120-12133: `resolve_index(cell.palette_index, palettes.get(row))`.
A cell stitched under one CGRAM state (palette cycling, a per-area palette
load, a fade) is coloured with whatever CGRAM holds now. On a fade-in the
whole margin pulses; in a level with two palettes the far part is wrong.

**F5. Two canvas systems with two placement rules and two scene paths.**
`canvas_accum.rs` (composite, bands from `ScrollWrite` events, scene tracker,
persistence, the map view, the NES scout path `scout.rs:327-336, 675-725`)
versus `layer_canvas.rs` (per layer, registers, no persistence, SNES scout
path). To drive the scene tracker on the SNES, `observe_frame_with_scroll`
(canvas_accum.rs:112-153) injects a fake `ScrollWrite` event. Every bug so far
(widened-line bundle offset 55508af, still-camera HUD flag 74e2fa4, scroll
lag b4fd601, x-wrap at 256 d1a16f5) had to be reasoned about twice.

**F6. Scene identity is a 64 px jump plus an empty bank state.**
`crates/rf-enhance/src/scene_tracker.rs:122-170`: a cut is `world_jump >
MAX_CONTINUOUS_STEP_PX` or a change in `mapper_bank_state`, which
core_thread always passes as `&[]` (canvas_accum.rs:95-100). Doors, pipes
and room transitions that happen to land within 64 px of the old camera are
NOT cuts, so two rooms share one canvas; a long fall in a vertical level that
exceeds 64 px in one frame IS a cut. The scout's own `JUMP_PX = 64`
(scout.rs:198) duplicates the rule with the same blind spots.

*W33-09 measured and fixed this (`docs/research/W33-09-scenes.md`): the NES
scene id was ONE constant for a whole session because the edge hash is flat on
NES palette indices, title/map/level cuts land at scroll 0 and were never seen,
and a Y wrap at 240 read as a 236 px jump. Scenes are now keyed by the
profile's level id, else by the settled picture, and cuts need the picture to
disagree with the measured scroll.*

**F7. HUD detection is three different heuristics.** Zero-delta bands and
`MIN_BAND_ROWS` (scroll_tracker.rs:159-161, 364-397), `hud_layers` by
"never moved while another did, and draws a band" (layer_canvas.rs:294-307),
and the bsnes-hd `AutoBoth` policy (widescreen.rs:194-224). The SNES core
knows per line which layers are on the main screen (`LineState.tm`, ppu/
mod.rs:160-162) and the profile may declare HUD rows (`declared_hud`,
app.rs:12083). One classifier with the core's facts first, heuristics second.

**F8. Scout plans are hand-found per game** (profile `[scout]` tables) and the
recipe cannot say "vertical", "both axes", "clear this scroll lock" or
"abort when this level id changes". `ScoutPlan::pokes` (scout.rs:202-221)
moves X only; `ScoutDir` is Left/Right only; `camera_matches`
(scout.rs:1159-1170) compares a camera word to BG1 only.

**F9. The scout's acceptance is "it grew".** `StopRule` (scout.rs:238-275)
stops on stall/budget/jump, and `MIN_USEFUL_FRAMES` (scout.rs:787) rejects a
too-short run, but nothing checks that scout cells AGREE with live cells
where both exist. The W32-07 note measured 100 % agreement by hand; the
W32-09 note found that 8 px/frame on SMB3 produces wrong palettes and
misplaced tiles. That measurement must be in the loop, not in a document.

**F10. Persistence covers only the composite canvas** (`crates/rf-enhance/src/
persistence.rs`, `canvas_accum.rs:163-169, 273-283`). The per-layer canvases
and every cell the scout earned are lost at exit; the second visit to a level
scouts it again.

**F11. Unverified: the 125 px backtrack offset** (docs/design/NEXT_PLANS.md
§2, 2026-10-16 evening). The W32-08 lag fix probably closed it, but no note
says so. Ticket 0 below re-shoots the repro before anything else.

### 1.3 Delete or merge

* Merge `canvas_accum.rs` into the per-layer model (NES = one layer). Delete
  `observe_frame_with_scroll`'s event injection, `fork_for_scout` /
  `rewind_trackers`, the `Composite` struct and `step_composite` in scout.rs,
  `ScoutPlan::composite`, and `FrameScrolls::composite`. The map view reads
  layer 0.
* Delete `row_backdrops` once F3's reconstruction lands.
* Keep `rf_enhance::stitcher::Canvas` as the cell store; move
  `layer_canvas.rs`'s pure parts (merge, crop, front pixel, order) into
  `rf-enhance` where ARCHITECTURE.md §6 puts the stitcher ("scroll tracker +
  map stitcher" belongs to rf-enhance, not the bin). The bin keeps only the
  core-thread driver and the stepper reads.

---

## 2. Target design

### 2.1 Data model: one `SceneCanvases` for both consoles

```
SceneCanvases {
  scene: SceneKey,                       // §2.6
  layers: [LayerCanvas; 4],              // NES uses [0]
  palettes: PaletteTable,                // §2.4
  rows: Vec<RowBackdrop>,                // §2.5, per world y
  confidence: Confidence,                // §3.4
}
LayerCanvas { cells: Canvas<Cell>, dirty: Option<Rect>, motion: Motion }
Cell { palette_index: u8, priority: u8, palette_id: u16, source: Live|Scout }
```

Decisions, with reasons:

* **Store palette indices, not resolved RGB.** RGB loses palette cycling and
  cannot be recomposed through colour math. Indices plus a palette snapshot id
  (§2.4) reproduce the picture exactly and stay small (4 bytes/cell).
* **Do not store tiles + attributes from VRAM/tilemaps.** The W32-09 note
  (docs/research/W32-09-scout-nes.md §1) shows why: MMC3 swaps CHR banks per
  frame for the status bar and `rf-nes` does not expose mapper state;
  "stitching final pixel indices means a bank switch cannot corrupt the
  canvas". Tile capture would need bank state, CHR-RAM hashing and animated-
  tile normalisation (W32-07-level-sources.md §3 rows 12-13) for a ~3x
  memory saving nobody needs: a 20-screen SMW level at 4 layers is 5120 x 224
  x 4 x 4 B = 18 MB worst case, usually far less because BG3/BG4 are sparse.
  Tilemap reads stay a debug/validation tool (`rf_snes::debug::tilemap_entry`).
* **`source: Live|Scout`** on every cell, so the margin can be rendered
  "live only" for the acceptance test (§3.3) and so a live frame landing on
  a scout cell can be counted as a match or a mismatch before it overwrites
  it (`blit_row`, stitcher.rs:202-215, already lets live overwrite; the flag
  is what turns that overwrite into an observation).
* **`priority`** from `PpuPixel.priority` (video.rs:45-47) so §2.3's compose
  can honour tile priority bits, not only layer order.

### 2.2 Capture: what the core must put in the bundle (rf-core-api change)

Three additions to `FrameBundle` (all metadata, law 4-clean, Accuracy Mode
never reads them):

1. **`sub: Vec<Option<(Vec<SubPixel>, u16)>>`** per line, exactly what
   `sub_scanline` already delivers to the renderer (frame.rs:205). The
   per-layer stitcher writes a sub-screen `Background(n)` pixel to layer n's
   canvas like a main-screen one. Fixes F1 with zero new PPU work.
2. **`line_scroll: Vec<[(u16, u16); 4]>`** per line from `LineState.hofs/
   vofs` (ppu/mod.rs:127-128), plus `tm`/`ts` per line. SNES only; the NES
   keeps `ScrollWrite` bands (they are the per-line truth there). Fixes F2
   and reduces `ScrollLag` to a fallback for bundles without per-line data
   (the NES tracker already carries the vblank write into the next frame,
   W32-09 note, so it is rarely needed there either).
3. **`line_backdrop: Vec<RowBackdropState>`** = `(cgram0_bgr555, fixed_bgr555,
   backdrop_op, brightness)` per line, from `line_palette(y)` (ppu/mod.rs:882)
   and the line's `ColorMath`. Fixes F3.

Cost: ~224 x (256 x 4 B + 16 B + 8 B) = ~235 KB/frame extra when widescreen is
on; nothing when off (gate on the `WidescreenRequest`, the way `layers` is
gated at core_thread.rs:2103). Measure in W33-01 (target: < 0.3 ms/frame).

Rejected alternatives: (a) a scout-side per-layer render via
`render_scanline_masked` (ppu/mod.rs:1404) costs a second render per layer
and is `&mut Ppu`, so it can never be pointed at the live machine; (b)
reading tilemaps directly needs CHR/bank state the API does not expose (F-
note above) and still misses colour math. Both are strictly worse than
transporting what the PPU already computed.

### 2.3 Placement and compose

* **Placement = registers (bands on NES, per-line scroll on SNES), per
  line.** `WorldAxis::advance_x` (scroll_tracker.rs:241-259) stays; make the
  512-vs-256 wrap a per-game profile hint (`scroll_wrap = 256|512`) with the
  heuristic as default.
* **Image registration as a cross-check, not a placer.** Each frame, for the
  primary layer, correlate the new line block against the canvas at the
  register-predicted offset and at +-1..+-8 px. The W32-09 note's "99.5 % at
  the stitcher's own step" is the healthy signature. A sustained best match
  away from the prediction lowers `Confidence.placement` (§3.4) and logs the
  offset; it never moves cells (a wrong auto-shift is worse than a visible
  seam). This is also the detector for F11.
* **Compose per world pixel, per row**, in `rf-enhance`: take the front-most
  main-screen layer by (priority, mode layer order from `bg_mode` and
  `bg3_priority` in `LineState`), then the sub-screen layer at the same world
  position, then apply the row's `ColorMathOp` with the row's palette
  snapshot. `row_orders` becomes the fallback when the per-line state is
  absent (NES, or an old cache).
* **HUD/splits.** One classifier: (1) profile `declared_hud` rows; (2) SNES:
  a layer whose per-line `tm` bit is set only on a band of lines and whose
  per-line scroll never moves while another layer's does; (3) NES: the
  zero-delta band rule as today. Output: per world row, which layers are
  HUD, and the HUD rows are never widened (as `keep_native_row` does now).

### 2.4 Palette snapshots

`PaletteTable: Vec<[u16; 256]>` per scene, keyed by hash of the line's CGRAM
words plus brightness; a new line palette is appended on first sight, cells
store the 16-bit id. SMW uses a handful per level; a palette-cycling game
adds one per cycle phase (bounded: a 64-entry table per scene is ~32 KB).
NES: the table holds the 32-byte palette RAM equivalent. The margin resolves
`cells[x].palette_index` with `palettes[cell.palette_id]`, never the live
row palette. A fade (brightness < 15) is excluded from capture (do not stitch
a half-faded frame) and excluded from compare.

### 2.5 Seam and backdrop

* Backdrop per world row is reconstructed from `RowBackdropState` (§2.2 item
  3): colour = `math(cgram0, fixed, op)` under the row's brightness. The sky
  gradient SMW draws with HDMA on the fixed colour reproduces exactly, row by
  row, with no vote.
* Below ground, nothing is known: show the row's backdrop (never black) and
  let the scout fill it; the "known/unknown" line is also drawn optionally as
  bsnes-hd's "marker" for the tour.
* At the 4:3 edge there is no blend: the picture's own pixels and the canvas
  are the same indices resolved the same way, so a seam is a placement bug,
  not something to hide. Only allowed softening: a 2 px dither on a row whose
  canvas palette id differs from the live row palette id (a palette swap in
  progress).

### 2.6 Scene graph: rooms, doors, vertical and 8-way levels

* `SceneKey = (rom_sha256, patch_sha256?, level_id?, scene_hash)`. When a
  profile declares a `level_id` RAM word (an observation-found row, like
  `$0100` on SMW or `$0727` on SMB3), its value joins the key and a change in
  it is a hard cut; the perceptual hash stays as the fallback.
* Vertical and 8-way: nothing in the canvas is horizontal-only
  (`WorldAxis` already runs on y). The gaps are the scout (§3.2, Y moves) and
  the compositor's margin mapping, which already takes `(dx, dy)`.
* A scene graph `Vec<(SceneKey, Edge)>` records transitions (camera jump,
  level id change, bank change) with the world positions either side, so a
  room entered by a door can later be placed next to its neighbour. Rooms
  are shown in the margin only when the edge is a continuous scroll;
  a door edge shows the room's own wall/backdrop.

### 2.7 Persistence (second visit is instant)

* `rf-cache` chunk v2: per `(SceneKey, layer)` canvas + the palette table +
  row backdrops + confidence, `SETTINGS_HASH = "canvas-chunk-v2"`
  (persistence.rs:25 is v1; v1 entries simply miss). Scout cells persist with
  `source = Scout`. Serialise layers and palettes in index order; never walk a
  `HashMap` (the determinism argument in stitcher.rs:39-47 applies).
* Flush on scene change and every 30 s (today: `RequestCanvasSnapshot`
  piggy-back, core_thread.rs:1599-1620); restore on scene entry before the
  settle frames expire, and skip the scout when the restored canvas already
  covers both sides of the camera by >= the margin width.
* Cache is derived, user-local pixel data; it is not ROM bytes and is not in
  git (law 5).

**Built (W33-05).** One entry per `(ROM sha256, scene id)` rather than per
layer: all four layers, the palette table, confidence, the scene's edges and a
`scouted` flag, framed with magic, version and CRC32 and zstd-compressed
(`rf_enhance::persistence`, `SETTINGS_HASH = "canvas-chunk-v2"`). Restored on
the first stitch of a scene, written when the game cuts away from it, on the
5 s snapshot flush (only if the cells changed) and when the model drops. A
scene whose scout finished is restored with `scouted` set and the scout driver
starts nothing for it. An entry past 48 MiB stored drops layers 3, 2, 1 in
that order; across scenes `rf-cache` evicts whole least-recently-used entries
to its cap. Measured: SMW YI1 3.69 MB raw, 66.7 KB stored; SMB3 1-1 1.83 MB raw,
34.3 KB stored. Cells are written `Live`; the `Scout` source tag arrives with
W33-06.

### 2.8 How the scout plugs in

Unchanged in principle (snapshot, worker, deltas, unknown-only merge), with:
(a) the same bundle (sub-screen, per-line scroll) through `ScoutSink`;
(b) a `ScoutAxis { X, Y }` and `ScoutDir` of four directions; (c) merge
tagging `source = Scout`; (d) the agreement self-check of §3.3 computed on
the worker before a delta is sent; (e) one scout path, the per-layer one.

---

## 3. Many games with little per-game work

The three profiles with `[scout]` tables each took a research session of
RAM probing. The target is: a game with a `[camera]` row (the census already
finds those: profiles/nes/mega-man-2/profile.toml:3-7) gets a scout with no
further hand work, and a game with nothing gets a camera, a player, a gate
and a scout from one play session, all written as observation rows.

### 3.1 Auto-discovery (all by observation; rows carry `source = "observed:"`)

* **Camera finder: exists.** `rf_enhance::camera_finder::CameraFinder`
  (camera_finder.rs:249-311) scores every work-RAM byte against the scroll
  register; the census (`crates/rf-harness/tests/profile_census.rs`) and the
  in-app flow (`crates/retroforge/tests/camera_finder_flow.rs`) use it. Add
  the 16-bit pairing (which byte is the high byte: the one that steps when
  the low byte wraps, the W32-09 note's `$0012`/`$0020` evidence) and run it
  for Y as well.
* **Player finder by input correlation.** While the user holds Right (or the
  harness does), candidate words satisfy: within `[camera, camera + 255]` on
  >= 95 % of frames, step by the camera's step on >= 90 % of moving frames,
  and keep moving for a few frames when the camera stops at a wall (which
  distinguishes the player from camera mirrors like SMW `$1462`). The W32-07
  note found `$0094` with exactly these three tests by hand. Y: the word that
  changes on a jump (A/B press) and settles on landing.
* **Start gate by game-mode RAM stability.** Bytes that are constant across
  every in-level moving frame AND differ on the title/map/death frames the
  session also saw (the W32-07 `$0100 == 0x14`, `$0071 == 0`; W32-09
  `$000F == 1`). Score = (in-level constancy) x (fraction of non-level
  frames with a different value); take the top two as `start_when`. A death
  the scout causes is itself an observation: the byte that changes at the
  scout's abort frame is the "alive" gate.
* **Hold (safe-Y) finder.** Run a 120-frame trial scout holding the player's
  Y word at a row above the visible top (SMW 150, SMB3 0x28, MM2 0x20 were
  all "above the screen"); if the gate byte flips, retry two rows higher;
  if the camera never follows, drop the hold (some games freeze an
  off-screen player).
* **Scroll-lock finder (later).** A byte that flips at the frame the camera
  stops while the player keeps moving (boss doors, MM2 `$1B` per Data Crystal
  is a lead to verify, never a value to trust).

**Built (W33-07).** `rf_enhance::scout_finder` (the observer, pure and
unit-tested), `retroforge::scout_discovery` (the trials on a second machine)
and `retroforge::scout_find` (the core-thread half of the in-app action).

* **What the observer names.** Camera: `CameraFinder`, unchanged. Player X: a
  byte that steps by the camera's step on >= 85 % of the frames the camera
  moves, goes UP on Right and DOWN on Left, and keeps changing in the held
  direction on >= 6 frames the camera stands still (a session that starts
  with the camera not yet following gives those frames for free; a game that
  starts mid-screen needs a Left walk). Its high half is the byte that stepped
  +1/-1 in the frames the low half wrapped, anywhere in RAM (SMB3 `$75`,
  MM2 `$0440`, MMX `$0BAE`). Y: a byte that held still for 8 frames, moved
  within 6 frames of a face-button press, by 6..=127, in >= 50 % of the
  presses that found it quiet and never in the control windows opened with no
  press. Gate: a byte that holds one value on >= 98 % of the frames of the
  level (the scene with the most scrolling frames; the scene ids are the
  canvas model's, so W33-09's level ids decide where a profile has them) and a
  different value on >= 60 % of the frames of every other scene.
* **What the trials decide.** Which player word the game obeys (the others are
  copies it recopies), which Y byte holds the player out of harm's way, how
  high, and whether to `walk` instead: each recipe scouts 300 frames from one
  snapshot (`Scout::restart_with`), then the winning hold is tuned over six
  heights on 1100-frame trials (a hold too low catches on pipes that 300
  frames never reach; the highest height within 2 % of the best extent wins).
  SMB3 does not scroll for a player standing on the ground, so the mover and
  the hold are chosen TOGETHER (no hold: extent stays at one screen).
* **A bug this found.** A NES scout whose profile has `[scene]` rows ended
  every pass at its first frame: the live model is fed the level bytes, the
  scout's scene tracker was not, so its first frame was a "key change". The
  plan now carries the profile's scene key (`ScoutPlan::scene_key`).
* **Session recipe** (what a player is asked to do): play the level for a
  minute, walk right and left, jump a few times (>= 6 jumps), then stand
  still for a second. The app starts the trials when the player has stood
  still for 20 frames after that.

### 3.2 Generic strategy ladder with fallbacks

Try in order per scene, each with the self-check of §3.3; stop at the first
that passes; record the winner in the profile row `strategy_observed`:

1. `player_move` (+ hold Y) — worked on SMW, SMB3.
2. `walk` (hold the pad direction, hold Y) — worked on MM2, works when the
   camera follows only a walking player.
3. `camera_write` (write camera + every mirror the finder found) — the
   rarely-working one (SMW snapped back, MM2 streamed nothing), kept because
   some games do accept it.
4. Give up for this scene: margins show backdrop + stitched history only,
   `Confidence.scout = 0`, and the UI says "margins: visited areas only".

Per pass: `axis` X then Y; `dirs` all four; `step_px` auto-tuned from 8 down
to 1 until the agreement check passes (the W32-09 "8 px breaks SMB3's
palettes, 4 px is right" finding, automated). Forward-only engines (SMB1,
Contra) show up as a failed Left pass and are recorded as such.

### 3.3 Acceptance = scout vs live agreement, in the loop

Before a scout delta is merged, compare it against the live canvas on every
cell BOTH know (the W32-07 figure 39 110/39 110 is the bar): agreement =
equal (palette_index, palette_id). Rules:

* >= 99.0 % over >= 2 000 shared cells: merge, `Confidence.scout` += 1.
* 95-99 %: merge, but log and lower `step_px` for the next pass.
* < 95 % or < 2 000 shared cells after a full pass: discard the delta, mark
  the strategy failed for this scene, try the next rung.
* Then, continuously: every live frame that lands on scout cells is a free
  re-check (count mismatches on `source = Scout` cells the live frame
  overwrites); a mismatch rate above 1 % over 600 frames flips the scene's
  margins back to live-only and reports it.

**Built (W33-06).** `crates/retroforge/src/scout_ladder.rs` (the ladder and
`check_delta`, pure and unit-tested) and the worker in `scout.rs`.

* **Source bit.** `Canvas` carries a per-cell Live/Scout flag (a parallel
  vector, empty while every cell is Live). A merge tags the cells it fills
  `Scout` and never writes over a live cell or an earlier scout cell; a live
  write over a Scout cell replaces it and makes it `Live`. The registration
  check and the agreement count only `Live` cells as evidence (`get_live`), so
  wrong scout cells under a live frame are not a placement fault. Persistence
  v2 writes and reads the flag cell for cell.
* **Check.** Each delta is compared with a copy of the live canvas taken when
  the scout starts, per trusted layer, at the best vertical offset within one
  row (`scout_agreement_aligned`: SMB3 1-1 reads 90 % at offset 0 and 99.3 % at
  +1 against a standing live canvas, the tracker not following that game's
  vertical scroll; the offset is reported, never applied). The worst layer's
  verdict decides; the pass's FIRST judged delta decides the pass (merge:
  carry on; slow: merge and lower the step for the next pass; reject: discard
  and halve the step, then the next rung); a later reject only drops that
  delta, because where a late delta meets the live canvas again that is the
  live canvas's newest edge, half drawn (RF-Scroller: 99.9 % over the screen,
  86 % over the last 40 columns). Palette ids are compared only when both
  cells carry one (the SNES scout stitches with no per-row palettes yet, so
  its cells carry none).
* **A live canvas cannot judge what lies ahead.** The corruption of a too-fast
  scout (SMB3 at 8 px) sits in the columns AHEAD of the player, and a canvas
  of the standing screen agrees 99 % at 4 px AND at 8 px (measured). So a
  step is first probed: the same stretch scouted at half the step and
  compared cell by cell (8 vs 4 on SMB3: 85.7 %, rejected; 4 vs 2: 99.7 %).
  Probes are verified once per rung and step. A scene the live game has not
  drawn has nothing to compare with at all, and in the app that is the normal
  state of a NES level start: the scroll tracker calls a band whose scroll
  has not changed since the previous frame HUD (`ScrollTracker::observe`), so a
  player who stands still after the scene settled stitches nothing, and the
  scout reference held 0 cells in both NES shots (SMB3, MM2; SMW's per-layer
  path held 58 572, so its deltas are judged live: 100 % over 336 075 cells).
  There the probe is the check, and a scout is not refused for the live
  canvas's silence (rule 3 of §3.3 refused it; measured in the app harness,
  that kills every NES scout at a level start). A rung with no probe (walk, or
  a step already at the floor) on an empty live canvas is accepted blind; the
  live frames that later overwrite its cells are what would catch it (not done,
  see below). The ROM-gated driver tests build their live canvas from a fresh
  model, which does stitch a standing screen (the first frame of a scene), so
  they do exercise the live check on SMB3 (99.1 % over one screen).
* **Ladder.** player_move, walk, camera_write, give up; a rung with nothing
  to act on is skipped (player_move needs player addresses, walk a machine
  placed by bands, camera_write camera addresses); the profile's strategy goes
  first, `pin = true` limits it to that one, and the rung that finished a
  scene is kept with it and tried first by that scene's next scout. Step: the
  profile's value (8 without one), halved to a floor of 2 (a profile step
  below that is its own floor); a smaller step gets up to 4x the frame budget.
  A pass that stitches nothing (Left on a forward-only engine) ends, it is not
  a failed rung. Give-up is remembered with the scene.
* **Telemetry** (`ScoutTelemetry`): deltas merged / slow / rejected /
  unjudged, probes and probes rejected, agreed and shared cells (so the
  agreement percentage), gave_up, and `last_report` (every check with its
  agreement, the rung each pass finished on, the live cells it had to
  compare with); the harness prints them.
* **Not done.** The continuous re-check of §3.3 (count mismatches where a live
  frame overwrites a Scout cell, flip the margins to live-only above 1 % over
  600 frames) and `Confidence.scout`. The scout's cells carry no palette id on
  the SNES (they never did): its margins resolve with the current row palette.

### 3.4 Confidence score per game (shown in the UI, stored in the profile)

`Confidence { placement: 0-3, palette: 0-3, scout: 0-3, scenes_ok: n/m }`
derived from: registration residual (§2.3), palette-id churn, scout
agreement, and how many scenes reached rung 1-3 vs 4. The margin source
degrades with the score: 3 = full (scout + history), 2 = history only, 1 =
backdrop only beyond the VRAM width, 0 = VRAM margins as today. Never
garbage: a low score means less margin, not wrong margin.

### 3.5 When to give up gracefully

Per scene after `MAX_ATTEMPTS` (scout.rs:779) across all rungs; per game
when < 1 in 4 scenes pass; per session when the worker's wall time exceeds a
budget (default 60 s of scout per hour of play). All three leave the live
game unaffected and are reversible by the user ("Scout again").

---

## 4. Determinism and law constraints

* **Live core never touched (law 4).** The only new core-side code is
  metadata emission (per-line scroll, sub pixels, row backdrop state) on the
  existing `CoreSink` channels, computed from state the PPU already latches.
  The scout keeps its own machine (scout.rs:16-22). No new `&mut` path into
  the live stepper from any stitching code; the registration check reads the
  bundle only.
* **Accuracy Mode untouched (law 6).** All new bundle fields are gated on
  the widescreen request being present; `WidescreenPolicies::default()` is
  disabled; the golden-hash tests over `PpuPixel` indices are unaffected
  because nothing in `video` changes. `mode_invariant_*` must stay green.
* **Determinism invariant.** Canvases are a pure function of the bundle
  stream plus the scout's inputs; the scout is deterministic given the
  snapshot (restore_fidelity: 0 differing pixels, W32-07 note). Persistence
  serialises in index order; equality is `Vec` equality. The existing
  `stitcher_determinism` test extends to per-layer canvases.
* **No ROM bytes in git (law 5).** Caches live under the user's cache root;
  tests use RF-Scroller / RF-Scroller-S fixtures; ROM-gated tests stay
  `--ignored`. Research notes record addresses and ratios, never dumps.
* **Clean-room.** Every RAM row written by auto-discovery is an observation
  with its ratio in `notes`; a documentation lead (Data Crystal, SMW Central)
  may be named as a lead but a row is only emitted after the finder confirms
  it in play. No disassembly-derived data, no emulator source.

---

## 5. Phased ticket list (W33), by value

Harness for every proof: `RF_SCREENS_ONLY=widescreen RF_SMW_RUN_FRAMES=240
RF_SMW_BACK_FRAMES=120 cargo test --release -p retroforge --test w30_screens
-- --ignored --nocapture` (SMW), `w32_09_scout_nes` for SMB3/MM2, plus the
in-process timing numbers the W32-07 probes print. "fps cost" = release
build, same load, before/after, median of 600 frames.

### W33-00 Re-verify the backtrack offset and baseline the numbers (1 pt)
Measure first. Re-shoot the NEXT_PLANS §2 repro; run the placement test;
record live fps with widescreen 32:9 on/off, scout on/off, on SMW, SMB3, MM2.
Acceptance: a note with the five numbers and either "offset closed by
b4fd601" or a minimal repro. Risk: if it is open, W33-02's registration check
is the tool that finds it, so order W33-02 before W33-01 in that case.

### W33-01 Sub-screen and per-line state in the bundle (5 pt)
`rf-core-api`: `FrameBundle { sub, line_scroll, line_backdrop }` filled by
`FrameBundleBuilder` from the existing sink calls; `ScoutSink` forwards them.
`rf-snes`: expose `LineState` scroll/tm/ts and the row backdrop state through
the existing `CoreSink` (no new PPU render). Acceptance: the SMW 32:9 shot
shows the far hills (BG2) in both margins; the sub-screen channel is empty
when widescreen is off; fps cost < 0.3 ms/frame measured. Risk: bundle size
(cap per line, gate on request).

### W33-02 Exact backdrop and palette snapshots (3 pt)
Replace `row_backdrops` with the reconstructed row colour; add the
`PaletteTable` and `palette_id` per cell; resolve margins with the snapshot.
Acceptance: no banding in the SMW sky (every margin row's colour equals the
picture's row colour where both are backdrop: 224/224 rows); below-ground
unknown shows the row's backdrop, 0 black pixels in the margin; a fade-in
frame leaves the margin colours unchanged. Risk: palette table growth on
cycling games (cap at 256 ids/scene, measure on a water level).

### W33-03 One canvas model for both consoles (8 pt)
`rf-enhance::canvas` (moved from the bin): `SceneCanvases`, per-line
placement (SNES) / bands (NES) into layer canvases, HUD classifier of §2.3,
compose with priority and colour math. Delete the composite path and its
scout branch (§1.3). Map view reads layer 0. Acceptance: the SMB3 and MM2
scout shots are pixel-identical to today's (same cells), the placement test
passes for both consoles in one function, workspace green, the deleted
`observe_frame_with_scroll` has no callers. Risk: the map view and
`CoreEvent::CanvasSnapshot` consumers in app.rs (grep `ultrawide_canvas`,
app.rs:4029, 4193, 5666) need one adapter.

### W33-04 Registration cross-check and confidence (3 pt)
Per-frame correlation of the primary layer against the canvas at predicted
+-8 px; residual feeds `Confidence.placement`; margins degrade by score.
Acceptance: on SMW/SMB3/MM2 walks the residual is 0 on >= 99 % of frames; an
injected +3 px scroll error in a test is detected within 10 frames; the
score shows in the Enhancements panel. Risk: cost of the correlation
(bounded: 8 rows x 17 offsets x 256 px per frame; measure).

### W33-05 Persistence v2 and instant second visit (3 pt)
Chunk v2 per (scene, layer) with palettes, rows, confidence, scout cells.
Acceptance: quit and relaunch SMW YI1, margins full within 10 frames of the
scene settling, no scout started (telemetry `started == 0`); cache size per
level reported; determinism test extended. Risk: cache cap and eviction with
larger entries (`CANVAS_CACHE_CAP_BYTES`).

### W33-06 Scout self-check and strategy ladder (5 pt)
Agreement check on the worker before each delta (§3.3); rungs
player_move/walk/camera_write with auto `step_px`; `source = Scout` cells;
live overwrites scout. Acceptance: SMB3 at a forced step of 8 is rejected by
the check (today it merges wrong palettes) and the pass is retried at 4 and
accepted; the three existing profiles still finish both passes; telemetry
shows agreement % per scout. Risk: shared-cell count too small on a fresh
scene (use the live frames since settle; require 2 000).

### W33-07 Auto-discovery of camera pair, player, gate, hold (8 pt)
The finders of §3.1 as `rf_enhance::scout_finder`, driven from play (in-app
like the camera finder flow) and from the census harness; output =
observation rows in the profile skeleton. Acceptance: on SMW, SMB3 and MM2
with their `[scout]` tables REMOVED, one scripted play session yields a table
whose scout reaches >= 90 % of the width the hand-made table reached; on one
new game with only a `[camera]` row (Mega Man X: camera found 97 % in the
census) the scout fills at least one scene. Risk: player finder false
positives on camera mirrors (the "keeps moving at a wall" test is the
discriminator; measure it).

### W33-08 Vertical and 8-way scouting (3 pt)
`ScoutAxis`, four `ScoutDir`s, Y pokes/holds, Y camera word. Acceptance: a
vertical SMW level (or SMB3 1-2 style) fills the margins above/below and the
X margins at the level's side walls show backdrop not garbage; fps unchanged.
Risk: camera-Y follow rules differ (SMW's camera Y stayed at 192 with Mario
above the screen, W32-07 note): the hold must be released on the Y axis.

**Built (W33-08).** `ScoutAxis {X, Y}` and `ScoutDir {Right, Left, Up, Down}`;
the plan gains `camera_y`, `player_y`, `player_y_hi` (profile roles of the same
names); `pokes` runs on the axis of the pass and RELEASES every `hold` on the
player's Y word on a Y pass; a Y `player_move` pokes from the scout's own
running target (a player standing on a floor is put back on it every frame);
a Y pass has a 300-frame lead-in before the stall rule counts (Mega Man 2
scrolls down only after the poked Y has crossed `$F0` and the game's own
30-frame latency). The ladder is per axis (a Y pass starts at `player_move`
whatever the profile's X strategy, at the default step). The driver scouts the
scene along `SceneTracker::scrolls_vertically()` (new: a CONFIRMED axis only,
see `docs/research/W33-08-vertical.md` section 2) when the recipe has Y words
or a pad to walk, else along X as before. Mega Man 2's Air Man shaft:
416 -> 656 cells tall by the real driver, 4 px accepted after the 8 px probe
and delta were refused; horizontal widths unchanged (SMW 5120, SMB3 2816, MM2
2560, MMX 1024, same agreement). Found on the way and fixed: the app never
sent the profile's `[scene]` key to a core spawned after the profile match.

### W33-09 Scene graph: level ids, doors, rooms (5 pt)
`SceneKey` with the profile's `level_id`; transition edges; neighbour rooms
in the margin only across continuous-scroll edges. Acceptance: SMW pipe to
bonus room and back gives two scenes and the original's canvas is intact;
MM2's first vertical section is a separate scene joined by an edge; no
false cut on a 64+ px fall in a vertical level. Risk: level id rows exist
only where observed; the hash fallback stays.

### W33-10 Tour and docs (2 pt)
New `compare-widescreen-32x9.png` from the canvas, a scout contact sheet for
three games, GAME_PROFILES.md `[scout]` update, ENHANCEMENT_RUNTIME.md §3
rewritten for the per-layer model, research note with every number above.

Order: 00 -> 01 -> 02 -> 03 -> 04 -> 05 -> 06 -> 07 -> 08 -> 09 -> 10.
01+02 give the visible win (hills and sky) in one week; 03-05 remove the
double system and make the second visit free; 06-07 are the "many games"
work and depend on 03's single model; 08-09 widen the class of levels.

### Risks to measure first (in W33-00 and W33-01)

1. Bundle growth per frame with sub pixels and per-line state: if > 0.3 ms,
   send sub pixels only for lines where `line_color_math_active`, and
   per-line scroll as runs (most lines repeat the previous).
2. Whether `LineState` scroll really is what drew the line on every game
   (the W7-07 latching is tested on HDMA demos; verify on SMW's HDMA sky and
   a Mode 1 parallax game by the placement test).
3. The canvas memory bound at 4 layers x long levels, and the clone cost of
   `current_canvases()` (layer_canvas.rs:444-455) on the UI refresh; ship
   dirty rects to the UI instead of whole canvases if > 2 ms.
4. Agreement threshold calibration: run the W33-06 check on the three known-
   good scouts first and set the bar from their numbers, not from 99 % a priori.
