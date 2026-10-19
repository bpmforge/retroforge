# HD-2D: automatic heights, real shadows, then Diorama fixes (W34 plan)

Planning only (2026-10-18, main at 363ce19, W33 complete). Read-only review of
`crates/rf-renderer/src/{diorama,diorama_mesh,lens}.rs`, `crates/rf-enhance/src/
{canvas,scene_graph,level_view,sprite_historian,scroll_tracker}.rs`,
`crates/retroforge/src/{app,enhanced_view,live_heights,hd2d_preset,profile_builder,
core_thread,scout_discovery}.rs`, `docs/design/{ENHANCEMENT_WAVE_16,STITCHING_PLAN,
ENHANCEMENT_RUNTIME,NEXT_PLANS}.md`, `docs/STATUS.md`, `docs/screens/w30-w31/*`.
Line numbers are from those files at this commit. Estimates are marked as such
and name the harness that confirms them.

Goal (Brad): the fan "HD-2D A Link to the Past" look (tilted camera, depth,
lighting, day/night, weather) for many games from emulation alone, with heights
found automatically, no painting; clean-room (observation only, no decomp).

---------------------------------------------------------------------------

## 1. Critique of today's Diorama / HD-2D

### 1.1 What works (keep)

- The pass itself: a pure CPU mesh builder (`diorama_mesh.rs:152-230`,
  `245-323`) plus a thin wgpu pass (`diorama.rs:719-1168`) with a tested camera
  (`camera_matrices`, `diorama.rs:165-677`; `follow_centres_the_given_world_
  point_on_screen` at `:637` proves the follow matrix is right). W30-05b fixed the
  mirror and the feet anchor (`diorama_mesh.rs:755-784`).
- Live-screen walls from the tilemap, locked to the picture by `scroll mod
  tile_px` (`live_heights.rs:32-40`, `227-251`): the right shape for a
  per-frame tile grid; the automatic heights reuse it.
- The height painter and `[heights]` schema (`profile_builder.rs:304-430`,
  `rf-profiles schema.rs:549-600`): keep as the manual override over learned
  heights, never the only source.
- The HD-2D preset is settings-only and reversible (`hd2d_preset.rs:102-135`).
- Budget gate reuse (`app.rs:1431`, `fog::BudgetGate`), accuracy invariant test
  (`tests/diorama_camera_live.rs:90`): the harness the new work plugs into.
- W33 gives what W16 §3 said was missing: whole-level per-layer canvases with
  palette index, priority, palette id, Live/Scout source, confidence and
  persistence (`canvas/mod.rs:1-56`, `stitcher.rs:49-60`), and the scout fills
  whole levels.

### 1.2 What is broken, with the cause

B1. **Side-view games are laid flat on the ground.** The mesh maps level Y to
world Z (`diorama_mesh.rs:24-39`) and a sprite's feet to `center_z = world_y +
height` (`enhanced_view.rs:739-746`, `1066-1068`). For a side-scroller the sky
becomes "ground" and the floor row is a strip. That is the fixture shot
(`hd2d-look-on.png`): the ground texture is transparent where the decode drew
nothing (`enhanced_view.rs:526` "Transparent, not black"), so only the one floor
row of RF-Scroller (48x14 metatiles, profile.toml:286-288) is visible, lying
flat, its solid tiles extruded as 16 px boxes (`scene_graph.rs:106`
`WALL_HEIGHT_EIGHTHS = 8`). No height table fixes this; every proof game except
ALttP / NES Zelda is side-view. Orientation is the first problem to solve.

B2. **The follow camera is framed against the whole level, not the viewport.**
`placement` solves the distance from the level's bounding sphere and divides by
zoom (`diorama.rs:349-350`), so "zoom 1.6x" (`hd2d_preset.rs:27`) means a
different distance for every level (a 5000 px SMW level is miles away). On the
fixture the whole-level fit shows ~760 world px of a 768 px level, so the
game's own 256 px viewport is the middle third of the 3D image: the level's
left edge sits a third of the way in with void to its left, the right side is
cut by the image edge, and the picture reads as "off-centre"
(NEXT_PLANS.md:128). The follow target itself is where the matrix tests say.
A second framing change comes with it: the flat view is drawn with the 8:7
TV stretch while the 3D image is drawn `DisplayGrid::exact` with "no TV
stretch" (`app.rs:16825-16829`), so toggling HD-2D changes the on-screen
aspect and letterboxes the 256x240 render inside the play rect. The
decoded path also centres on a fixed 256x240 viewport (`level_view.rs:46`,
`enhanced_view.rs:783-788`) regardless of the SNES's 224 rows.

B3. **No depth buffer** (`diorama.rs:820-824`, `diorama_mesh.rs:9-22`): painter's
order with a Manhattan sort (`diorama_mesh.rs:282-300`). Correct enough for
boxes seen from behind, wrong for a sprite between two walls, and it rules out
shadow receivers. A shadow map needs a depth attachment anyway.

B4. **Rendered at frame size, then NEAREST-upscaled.** `refresh_live_diorama`
renders `out = (width, height)` (`app.rs:5485-5500`, the decoded path likewise
`:5422-5437`) and the texture is uploaded `NEAREST` (`app.rs:5532-5536`). At 4x
the 3D view is a blocky 256x224 image; box edges and tilted billboards alias.
`superfx_gpu.rs` is the in-repo precedent for display-resolution + MSAA.

B5. **Per-call GPU resources and a synchronous readback** (`diorama.rs:982-1158`,
`read_buffer_sync` at `:1158`): textures, buffers and bind group are created
every frame. ~2 ms at 256x240 (STATUS W16-13). At 1024x896 the readback alone
is ~3.7 MB/frame (estimate; W34-00 measures it with the W16-01 harness).

B6. **Lens shadows are 2D and land in the wrong place on the 3D view.** The lens
runs after `select_active_view` picked the diorama as `displayed`
(`app.rs:4719-4760`), and its shadow is a screen-space offset blur of the flat
frame's sprite layer (`lens.wgsl:201-208`, `lens.rs:296-301`,
`fit_sprite_layer` accepts the same-size diorama image). The HD-2D preset turns
those shadows on (`hd2d_preset.rs:23, 108`). The contact ellipse in the pass is a
fixed 1.3x blob with no light (`diorama_mesh.rs:483-502`) and side faces are a
flat 0.55 (`:101`), so time of day changes nothing in 3D.

B7. **Heights keyed on the 10-bit character number only** (`live_heights.rs:16-18,
93-97`). A tileset reload (VRAM DMA between levels) renumbers tiles, so a table
painted in one level raises the wrong walls in the next. Pattern-bytes + palette
is the stable key (the HD-pack precedent: `hdpack.rs:63-68` `TileData::ChrRam`).

B8. **Live path is SNES-only and refuses widescreen** (`app.rs:5477-5484`:
`width == 256`, `msg.snes` only). Diorama and the W33 margins are mutually
exclusive, and the NES has no live path although `FrameMsg::vram`/`palette_ram`
(`core_thread.rs:366-370`) already carry the nametables every frame.

B9. **HUD tiles are walled.** `live_heights` has no HUD exclusion; SMW's BG3
status bar becomes a row of walls pitched with the level. `canvas::hud::
hud_layers` (`canvas/hud.rs:83`) and `hud::detect` (`hud.rs:260`) already
classify HUD layers/rows.

B10. **HDMA per-line scroll ignored** (`live_heights.rs:44-46`): parallax bands
are walled at the end-of-frame scroll, while `FrameBundle::lines` (W33-01) now
carries per-line registers.

B11. **Availability policy.** The "diorama" row is Game-Aware + `NeedsProfile`
(`enhance_ui.rs:229-235`) and the preset says "paint one" (`hd2d_preset.rs:77-79`).
Learned heights need no profile; by ENHANCEMENT_WAVE_16 §2 that is tier 1
(Enhanced mode), and NON_GOALS #11 requires "visited only, labelled
approximate". Proposal in §6; Brad's call.

B12. **The 3dSen objection** (WAVE_16 §5): "tile clusters into 3D with no
authoring" failed. What failed was inferring geometry from tile *art*. This plan
infers it from *play*: where the player stands, stops and is hidden, which the
emulator sees exactly. Art statistics are a low-weight tie-breaker only, and
every unknown tile stays flat (the honest fallback). That is a different claim.

---------------------------------------------------------------------------

## 2. Automatic heights

### 2.1 Signals available without game knowledge

| # | Signal | Source today | Tells us |
|---|---|---|---|
| S1 | Player support | player sprite box (OAM) + its Y over frames; input (`core_thread.rs:1065` `Arc<SharedInputFrame>`, UI-readable) | side-view: the tiles under the feet when a fall ends = floor tops. Top-down: tiles the feet overlap while moving = walkable |
| S2 | Blocking | input held toward a tile, player X/Y stops, camera stops | the tile is solid on that side |
| S3 | Hidden behind | SNES tilemap priority bit, NES OAM "behind BG" attr, `PpuPixel::priority` (`video.rs:45-47`, hardware bits), sprite pixels lost under BG in `bg_rgba`/`sprite_rgba` | a tile that hides the player is taller than him (top-down: tall wall/tree; side-view: foreground pillar) |
| S4 | Never overlapped | per tile, frames seen vs frames any sprite footprint overlapped it, with S2 evidence on a neighbour | solid mass |
| S5 | Layer order and scroll ratio | `LayerCanvases` per BG, `ScrollTracker`/`WorldAxis`, `atmosphere.rs` | BG2+ slower than BG1 = backdrop plane (set back, never extruded); atmosphere = fog |
| S6 | Vertical traversal | player Y changes linearly with X while supported (side-view slopes); top-down: Y drift while crossing a tile row with no input Y (stairs) | ramp class, slope sign |
| S7 | Art statistics | pattern-hash runs over the canvas (vertical runs >= 3 and horizontal runs >= 3 = block fill; horizontal-only with empty above = platform); a pattern whose opaque/transparent boundary is a diagonal is a slope candidate that confirms S6 | low-weight prior only |
| S8 | Whole-level canvas | `SceneCanvases` incl. Scout cells (`canvas/mod.rs`) | re-tile the canvas by pattern+palette so learned keys apply level-wide |
| S9 | Orientation | scroll axis statistics (`scene_tracker.rs:105-109`), gravity test on the player (dy accelerates downward with no input vs constant-speed dy under Up/Down) | TopDown vs SideView |

The player: the sprite cluster whose world motion answers the pad (press Right,
it moves right within 2 frames; releases stop it) and that stays near the
screen centre. The W33-07 finder's `PlayerCandidate` rows and a profile
`[player]` (when present) are accepted first; the pad test is the generic path.
Clean-room: observation of the running game only.

### 2.2 Data structures (rf-enhance, pure data)

```
// rf-enhance/src/heights/mod.rs (new)
TileKey   { pattern: u64 /* hash of the 8x8 or 16x16 pattern bytes, flips
             normalised */, palette: u16 /* hash of the tile's colours */,
             layer: u8, cell_px: u8 }
TileObs   { key: TileKey, priority: bool, flip: (bool, bool) }
TileGrid  { origin_world: (i64, i64), cols, rows, cell_px, offset: (u8, u8),
            cells: Vec<TileObs>, layer: u8, hud: bool }   // one per BG layer
SpriteBox { world: (i32, i32), w, h, behind_bg: bool, cluster: u8 }
Orientation { TopDown, SideView, Unknown }
Evidence  { seen: u32, floor_top: u32, blocked: [u32; 4], overlapped: u32,
            hides_player: u32, slope: i16 /* signed run votes */ }
Learned   { class: Floor|Wall|Tall|Platform|Ramp(i8)|Backdrop|Unknown,
            height_eighths: u8, confidence: u8 /* 0..=3, W33-04's model */ }
TileHeightTable { map: HashMap<TileKey, (Evidence, Learned)> }   // per ROM
HeightGrid { cols, rows, cell_px, offset, class, depth, confidence }  // per frame
HeightCanvas { origin, cols, rows, cell_px, cells: Vec<Learned> }  // per scene
```

`HeightLearner::observe(frame: &[TileGrid], sprites: &[SpriteBox], player:
Option<&SpriteBox>, pad: PadSnapshot, orientation: Orientation)` folds evidence;
`HeightLearner::grid_for(&TileGrid) -> HeightGrid` reads it back. Rules (all
observation, all per key, so a tile learned once is known everywhere):

- floor_top += 1 when the player's dy hits 0 after >= 2 frames of falling and
  the tile touches the feet row (side-view), or the feet overlap it while moving
  (top-down: that is `overlapped`, meaning walkable, height 0).
- blocked[dir] += 1 when input is held toward the tile for >= 3 frames, the
  player does not move, and no sprite overlaps that tile.
- hides_player += 1 when the player's sprite pixels are missing under the tile
  while OAM says he is there (SNES priority bit or NES behind-BG attr set).
- Class and height: Floor (overlapped >> blocked) = 0; Wall (blocked or
  never-overlapped with a blocked neighbour) = 8; Tall (hides_player) = 16-24;
  Platform (floor_top with no blocked sides, side-view) = 8 toward the camera;
  Ramp(sign) from S6 with slope in eighths per tile.
- Confidence 0-3: up 1 per 30 agreeing observations, down 1 per 3
  contradictions (same numbers as `canvas/registration.rs` so there is one
  ladder). Below 2 the renderer keeps the tile flat. Unknown = flat.
- Painted `[heights]` rows win over learned ones; learned rows are written to
  the player's own profile only when he presses "Keep these heights" (the
  W33-07 `source = "observed:"` convention).

Orientation: SideView when the scroll axis is X-dominant and the gravity test
passes; TopDown when Up/Down move the player at constant speed; a profile
`[camera].mode = "side_scroller"` (RF-Scroller declares it, profile.toml:157)
settles it at once. Unknown renders flat.

### 2.3 Where it lives (layer boundaries)

- **Core thread (retroforge/core_thread.rs):** build the compact `TileGrid`s
  here, the only place the PPU may be read, and ship them on `FrameMsg` gated
  by a `CoreCommand::SetTileGridCapture(bool)` like `SetSnesDebugCapture`
  (`core_thread.rs:822`, `1553`). rf-enhance may not depend on rf-snes/rf-nes
  (`sprite_historian.rs:17-27`, validate-arch rule 3), and `tilemap_entry` is
  `rf_snes::debug`, so decoding cannot move into rf-enhance. Size: 33x29 cells x
  4 layers x 16 B = 61 KB/frame worst case vs the 64 KiB VRAM clone the live
  path needs today; NES: nametables + attributes + CHR hash via the HD-pack
  placement path (`hd_render.rs:43-60`, `SetTileCapture`).
- **rf-enhance::heights:** learner, tables, canvas re-tiling, persistence
  (rf-cache key `tile-heights-v1/<rom sha256>` and a `heights` block in the
  scene chunk, versioned like `canvas-chunk-v2`).
- **rf-renderer:** `DioramaScene` grows `class`/`slope` slices;
  `diorama_mesh` gets a side-view builder, wedges for ramps, greedy merge of
  equal-height runs; the pass gets depth, MSAA, lighting, shadow map.
- **retroforge (shell):** mediation only (`enhanced_view.rs` compose fns), the
  readout ("Heights: 312 tiles learned, 71 % of this screen"), the
  availability row.

### 2.4 NES vs SNES

| | NES | SNES |
|---|---|---|
| tile key | 8x8 CHR pattern bytes (CHR-RAM hashed, CHR-ROM index + bank-independent hash), palette from the attribute table at 16x16; learn at 16x16 metatile granularity (the unit NES games think in) | tilemap entry: character, 3-bit palette, priority, flips; 8x8 or 16x16 per BGMODE (`live_heights.rs:221`); key on pattern bytes so VRAM reloads do not matter |
| priority | none per tile; sprite-behind-BG is the OAM attr (S3 from the sprite side) | tilemap bit 13 per tile + OAM priority |
| layers | one (layer 0); parallax only by split scroll (bands) | up to 4; BG order by mode; sub-screen |
| scroll | `ScrollTracker` bands per line | per-line registers (W33-01) |
| modes | — | modes 5/6/7 excluded (`live_heights.rs:44`) |
| sprites | 8x8/8x16, 64 entries (`FrameMsg::oam`) | `decode_oam` sizes per `$2101` |

### 2.5 Heights become the mesh

- TopDown (today's XZ plane): floor at 0; Wall boxes `height_eighths`; Tall
  boxes taller than the player's billboard, which the depth buffer then hides
  behind them correctly; Ramp = wedge (`push_wedge`, 2 triangles top + 2
  sides) rising along the slope sign; Backdrop layers drawn at y = -tile_px
  under the floor with their own scroll (the "floor is a hole" look only where
  the floor layer is transparent).
- SideView (new builder `build_vertices_side`): the level is the vertical XY
  plane at z = 0; Floor/Wall/Platform tiles extrude toward the camera by
  `depth` (one tile by default); Backdrop layers at z = -k * tile_px by scroll
  ratio (parallax measured, not guessed); billboards stand in XY with their
  feet on the block's front face top edge; the camera pitches 10-25 degrees
  and yaws +-15 (the HD-2D side-section look). Ramps are sloped block tops.
- Merging: adjacent cells with equal class/height merge into one box
  (greedy row runs) so a 33x29 grid is a few hundred boxes, not ~1000.

---------------------------------------------------------------------------

## 3. Shadows and light

**Light model.** One `Sun` (new `rf-renderer/src/lighting.rs`) derived from the
same `time_of_day` the grade uses (`lens.rs:174-175, 246-290`): elevation
`10 + 60 * sin(pi * t)` degrees for t in day, azimuth sweeping -60 to +60
degrees across the day (sun rises screen-right/top, sets screen-left in
top-down; mirrored for side-view so shadows fall on the ground plane the
player stands on); night = a moon at 20 degrees, cool tint, ambient up.
Colour = `grade_for(t).tint`, ambient 0.45 noon to 0.25 night, Lambert on box
faces (replaces the flat `SIDE_SHADE`). Weather dims the sun (rain 0.6, snow
0.8) and the lens keeps drawing the particles.

**Shadow map, not screen space.** A screen-space offset blur (today's lens
shadow) cannot land on a height field: the shadow of a sprite on a wall two
tiles behind it is a projection question. Pass 1: orthographic depth-only
render from the sun over the followed viewport's world box (1024^2, 2048^2 at
>= 3x), occluders = boxes, wedges and alpha-tested billboards (so a sprite's
silhouette shadows the floor and the wall behind it). Pass 2: the diorama
pass samples it with slope-scaled bias and 3x3 PCF (9 taps; Poisson 8 at 4x) for
soft edges, over ground, box tops, sides and billboards (a sprite under a tree
is shaded). The contact ellipse survives at 0.25 as cheap ambient occlusion and
is the fallback when the budget gate turns the map off.

**Sprite footprint.** From OAM: cluster touching sprites of one palette into an
entity (the `sprite_historian.rs` position+palette identity, not `sprite_id`),
one billboard per entity instead of one per OAM entry (`enhanced_view.rs:1051`
makes one per `SpriteEntry` today), alpha from the sprite layer pixels so the
silhouette is the real one.

**Cost budget (estimates; W16-01 harness, `docs/evidence/gpu-passes.json`).**
Geometry is tiny (<= ~500 boxes after merging, <= 128 billboards): shadow pass
~0.2-0.4 ms; main pass at 1024x896 with 9-tap PCF ~0.6-1.2 ms on the M4 Max;
target <= 3 ms total at 4x, behind the shared `BudgetGate`. The number is
measured in W34-00 before the shader is written.

**Interaction with the lens.** When the diorama is the displayed view the lens
runs with `shadows` forced off (3D shadows replace them); bloom, tilt-shift,
grade and weather keep running over the 3D image. The flat look keeps the lens
shadows unchanged. The preset sets `lens_shadows` per view accordingly
(`hd2d_preset.rs:108`).

---------------------------------------------------------------------------

## 4. Diorama fixes, each with its acceptance

| # | Fix | Acceptance |
|---|---|---|
| D1 | Follow fits the viewport: distance solved for the 256x224/240 window (or the widescreen window), target = player feet when known else viewport centre, critically-damped smoothing (<= 2 px/frame overshoot), the game camera's own position is the target when the player is unknown | `follow_centres_the_given_world_point_on_screen` extended: the target lands within 2 px of screen centre on a 5000-px level at zoom 1.0..4.0; the fixture shot has the floor through the vertical centre |
| D2 | Framing at every aspect: `out = play rect size`, aspect from it, the 3D image fills the play rect at the flat view's own aspect (no letterbox jump on toggle), 16:9/21:9/32:9 ground from the W33 canvas margins (`compose::margin_pixel`) with unknown cells = row backdrop | screenshot at 4:3, 16:9, 32:9 on SMW: no empty wedge inside the known canvas; `widescreen_aspects.rs`-style test asserts the 3D image width equals the play rect |
| D3 | Billboards: OAM clustered into entities, 8x16 and SNES sizes, flips, behind-BG sprites behind Tall boxes via depth | unit test: a 2x2 cluster of 8x8 OAM entries is one billboard; a behind-BG sprite's pixels are hidden by a Tall box in the render |
| D4 | Layer separation: backdrop BG planes set back by measured scroll ratio, atmosphere layers stay fog, HUD layers composited flat on top in screen space | SMW: hills (BG2) visibly behind the ground at yaw 15; the status bar is flat and unmoved at every pitch |
| D5 | Depth buffer + MSAA 4x + display-resolution render; persistent textures/buffers; one encoder per frame | pinned goldens re-pinned once; `diorama_camera.rs` sprite-visibility tests unchanged; zoom crop like `superfx-gpu-zoom.png` |
| D6 | Perf: p95 at 1024x896 under 3 ms with shadows; vertex buffer reused when the grid is unchanged (scroll-only = translate) | `gpu-passes.json` row; `BudgetGate` never trips in a 10-minute SMW run |
| D7 | HUD: HUD rows/layers never raised or pitched | SMW BG3 / SMB3 status bar not in the height grid (unit test on a synthetic HUD layer) |
| D8 | Orientation: SideView builder; Unknown renders flat and says so in the readout | fixture: RF-Scroller renders as a vertical level with blocks toward the camera, not a strip |

---------------------------------------------------------------------------

## 5. Proofs per ticket

- **Ground truth first.** RF-Scroller and RF-Scroller-S carry real collision
  tables (`scene_graph.rs:109-164` `geometry_layer`). A headless harness runs a
  scripted 60 s (walk right, jump, walk back) and scores the learner against
  the collision mask: precision/recall on solid, per class. Mutation checks:
  disabling S1 must drop recall; shuffling tile keys must drop precision.
- **Screenshot pairs** (`tests/w30_screens.rs` pattern, consecutive frames,
  off/on, plus a split compare): top-down = A Link to the Past (SNES; needs the
  scripted intro to reach play, the `w30_screens.rs` snapshot/`step_from` infra
  and `tests/profile_refind.rs`'s scripted play) and NES Zelda (decodes only
  past name entry, same scripting); side-view = Super Mario World (YI1) and
  Super Mario Bros. 3 (1-1). Each shot's README row states tiles learned, % of
  screen covered, confidence histogram.
- **fps cost**: release, unpaced median of 600 frames, at 1x and 4x, with and
  without shadows (the W33-results.md table format), and the `gpu-passes.json`
  rows.
- **Accuracy invariants**: `diorama_camera_live.rs:90`'s "changes the diorama,
  never the accuracy frame" extended to heights and shadows on/off; the
  mode-invariant corpus gains the HD-2D entry; the only core commands are the
  capture switches (read-only snapshots, like `SetSnesDebugCapture`); Accuracy
  Mode cannot turn any of it on (law 6; `hd2d_preset.rs:10-11`).
- **Determinism**: learner output is a pure function of the observation
  sequence (test replays a recorded sequence twice and compares tables).

---------------------------------------------------------------------------

## 6. Phased tickets (W34), ordered by value

### W34-00 Measure first (2 pt)
Baseline: diorama cost at 256x224 and at the play-rect size with readback
(B5); shadow-map cost with a stub pass in the W16-01 harness; the collision
oracle harness on RF-Scroller; how many frames of play ALttP, SMW and SMB3 need
before 80 % of on-screen tiles have an observation (instrumented count only, no
rendering). Output: `docs/research/W34-00-heights-baseline.md`.
Risk measured: readback at 4x; VRAM/tile-grid capture cost (< 0.3 ms target).

### W34-01 Tile grids on the frame (5 pt)
`TileGrid` per BG layer built on the core thread for NES and SNES, keyed by
pattern hash + palette, with priority and HUD flags, per-line scroll honoured
(B7, B8, B10). Gated capture; accuracy hash unchanged (test as W33-01 did).
Acceptance: NES and SNES fixtures produce identical grids to the tilemap read
(`live_heights::character_grid`) for every cell; 61 KB/frame max; cost row.

### W34-02 Player and orientation (5 pt)
Entity clustering from OAM; the pad-correlation player detector; gravity test;
`Orientation`; profile hints accepted first. Acceptance: on RF-Scroller,
RF-Scroller-S, SMW, SMB3 and ALttP the player cluster is found within 120
frames of play; orientation correct on all five; a pure unit test replays a
synthetic OAM+pad trace.

### W34-03 HeightLearner (8 pt)
rf-enhance::heights with S1-S4, S6, S7 and the confidence ladder; per-ROM
cache; side/top rules. Acceptance (oracle harness): >= 90 % precision and >=
70 % recall on solid tiles after the scripted 60 s on both fixtures; unknown
tiles are flat; determinism test; cache round-trips.

### W34-04 Side-view diorama and the learned-heights wiring (8 pt)
`build_vertices_side`, wedges, greedy merge; shell composes from `HeightGrid`
for NES and SNES live paths; painted `[heights]` overrides; availability row
and the readout; the HD-2D card no longer says "paint one". Acceptance: the
fixture shot is a vertical level with blocks (D8); SMW YI1 shows ground and
pipes as blocks after 30 s of play; Accuracy invariant test green. Decision
needed (Brad): row moves to Enhanced with an "approximate, visited only" badge
per WAVE_16 §2 / NON_GOALS #11, or stays Game-Aware.

### W34-05 Depth, display resolution, camera (5 pt)
D1, D2 (4:3 first), D5, D6's buffer reuse. Re-pin `diorama_golden` once.
Acceptance per the table in §4; cost row at 4x.

### W34-06 Lighting and shadow map (8 pt)
`lighting.rs` Sun from time of day, Lambert faces, shadow pass + PCF, lens
shadow handoff, contact AO fallback. Acceptance: ALttP shot at t = 0.3 / 0.5 /
0.8 shows the sun moving (shadow direction and length change, verified by
centroid of the darkened pixels in a test scene); p95 <= 3 ms at 1024x896 on
this machine; budget gate falls back to contact shadows.

### W34-07 Whole-level heights from the canvas (5 pt)
Re-tile `SceneCanvases` (Live and Scout cells) by pattern+palette, apply the
table, persist a `heights` block per scene; the camera can pitch over scouted
ground beyond the viewport. Acceptance: SMW YI1 32:9 diorama shows blocks in the
scouted right margin; second visit restores them with no play.

### W34-08 Layer separation, HUD, billboards (5 pt)
D3, D4, D7. Acceptance per §4.

### W34-09 Widescreen diorama (3 pt)
D2 at 16:9/21:9/32:9 from the canvas margins (B8). Acceptance per §4.

### W34-10 Tour and docs (2 pt)
Screenshot set (§5), `docs/screens/w34/README.md`, user-guide chapter,
ENHANCEMENT_RUNTIME §8 "Heights and light", STATUS entry with numbers.

Order: 00 -> 01 -> 02 -> 03 -> 04 -> 05 -> 06 -> 07 -> 08 -> 09 -> 10. W34-04
is the first visible payoff (side-view games stop being strips); W34-06 is the
look Brad asked for; 07-09 widen it.

### Risks
- The player detector fails on games with no player (menus, cutscenes):
  learner idles; render stays flat. Acceptable.
- Pattern-hash keys collide across palettes that mean different things (SMW's
  blue vs green pipes are the same shape): keyed on palette too, with a
  pattern-only fallback at confidence 1.
- Animated tiles (water, coins) churn keys: rate-limit learning to keys seen
  >= 30 frames; W32-07-level-sources.md §3 rows 12-13 list the known cases.
- SNES capture cost: measured in W34-00; the compact grid replaces the 64 KiB
  clone, so it should be lower than today's live path.
- Readback at 4x may force the egui-wgpu render-to-texture path (no readback)
  earlier than planned; W34-00 decides.
- Policy (B11) can block W34-04's visibility in Enhanced mode; the ticket
  ships either way, the row placement is the only open decision.

---------------------------------------------------------------------------

## W34-04 as built (2026-10-18)

- **Builder.** `rf-renderer::diorama_side` (re-exported by `diorama_mesh`):
  `build_vertices_side` stands the picture upright (world Y = picture height
  minus picture Y), extrudes solid cells toward the camera, merges equal-depth
  cells into rectangles, draws exposed top / left / right faces only, and
  stands billboards at the middle of the depth of the cell under their feet.
  Ramps are wedges. The camera is `camera_matrices::side_placement`: it frames
  the game's own window (not the level), with the user's tilt scaled by 0.35
  (52 degrees becomes about 18). `build_vertices` and the pinned goldens are
  untouched.
- **Learner on the core thread.** `retroforge::live_learn::LiveHeights` owns
  the scene tracker (NES camera), `PlayerTracker` and `HeightLearner` while
  `SetTileGridCapture` is on (Diorama ticked outside Accuracy Mode) and ships
  a `HeightsFrame` on `FrameMsg::heights`. A profile's `[camera].mode` reaches
  it through `SetOrientationHint`, re-sent to every new core.
- **Shell.** `retroforge::diorama_learned` turns the playfield grid plus its
  `HeightGrid` into a lattice and picks the builder: SideView upright,
  TopDown the old floor plan, Unknown flat (the readout says "flat until the
  camera angle is known") unless a painted `[heights]` table is present, which
  replaces the learned heights whole (it is SNES-only and keyed on character
  numbers, so it is not merged cell by cell). The decoded-level path
  (RF-Scroller) takes the same side builder when the orientation is SideView.
- **D-023.** The Diorama row is generic (Enhanced or Game-Aware, no profile);
  the badge line reads "approximate, visited areas only" unless the heights are
  decoded or painted. The HD-2D card no longer points at the Profile Builder.
- **Not done here.** The backdrop is one plane set back by `1 - ratio` of two
  tiles from the measured scroll ratio; separate per-layer textures (so hills
  and the sky sit at different depths) are W34-08 (D4). The learned table is
  not persisted yet (rf-cache `tile-heights-v1` is written by nobody in the
  shell), so every session starts cold.

---------------------------------------------------------------------------

## W34-05 as built (2026-10-18)

- **D5, the pass.** `DioramaPass` has a `Depth32Float` attachment and 4x MSAA
  (`superfx_gpu.rs` pattern: probe, resolve, `StoreOp::Discard`) and renders at
  whatever size the shell asks for (clamped to the device limit). Everything
  it owns is kept between frames: the ground and sprite textures (recreated on
  a size change only), the camera buffer, one vertex buffer that only grows (a
  mesh identical to the last is not uploaded again), the MSAA/resolve/depth
  targets, the readback buffer and the bind group. One encoder per frame. The
  mesh is still emitted back-to-front (blending order); depth settles what
  that order gets wrong. A fragment with nothing to show (`a < 0.02`) is
  `discard`ed, so a transparent sprite texel cannot hide what is behind it, and
  shadows draw with a second pipeline (tested, never written, nudged toward the
  camera). The readback un-premultiplies silhouette pixels (the resolve averages
  coverage over a transparent clear), so edges keep their colour.
  `diorama_depth.rs` proves each of these (near quad wins when drawn first,
  transparent texel, MSAA partial coverage vs 1 sample, sizes, no new GPU
  resources after the first frame).
- **Re-pins.** `diorama_golden` and `mode7_plane_golden` (it renders through
  the same pass) moved once: the pass's output changed, the default camera path
  (`view_proj_camera`, `fit_distance`) did not, and
  `the_default_camera_renders_exactly_what_render_always_did` still holds.
- **D1, the camera.** `camera_matrices::window_placement` solves the distance
  by bisection so the game's own window (256 x the rows it shows, divided by
  the zoom) fits the frame on the floor (`WindowPlane::Ground`, top-down) or on
  the upright picture (`WindowPlane::Upright`, side view), the target at the
  exact centre; the near plane scales with the distance so depth steps stay
  small on a 5000 px level. Fit numbers (window corners in NDC, top / bottom):
  side view at the old 18 degrees and 50 degree FOV, x 0.996 / 0.77, y 0.95 /
  -0.73; at the shipped 13 degrees and `WINDOW_FOV_Y_DEG` 30, x 1.0 / 0.89, y
  0.97 / -0.87. So `SIDE_PITCH_SCALE` went 0.35 -> 0.25 and the window cameras use
  a 30 degree FOV. Top-down at the default 52 degrees: x 0.75 / 1.0, y 0.59 /
  -0.78 -- the near edge fills the width, the far edge is three quarters of it,
  the window is 68% of the frame height. That is the keystone of a tilted
  floor under a *contain* fit; a *cover* fit (near edge touches, far edge
  cropped) would remove the side wedges at the cost of the top few percent --
  left for Brad. The target is the player's feet from the W34-02 track (held
  for two seconds when the detector drops out), else the middle of the window,
  through `CameraSmoother`, a critically damped spring (no overshoot from rest or
  when a walking target stops; a jump over a third of the screen snaps).
  At zoom 1 on the live path the window is the whole screen, so the follow is
  visible from zoom > 1 (the HD-2D preset is 1.6).
- **D2, framing (4:3 first).** The 3D image is drawn into the rect the flat
  view uses (same `DisplayGrid::for_frame`, pixel aspect, scale mode) and
  rendered at that rect's size in physical pixels (cap 2048 on the long edge),
  from the previous frame's layout (one frame of lag on a resize, the console
  frame size before the first layout). The 8:7 TV stretch comes from framing
  with `aspect / pixel_aspect`: the picture is framed as if pixels were square
  and the GPU stretches it into the wider frame, so toggling Diorama does not
  move the picture (`diorama_live_view.rs` asserts the rect and the aspect).
  The window is the rows the flat view shows (an SNES frame without overscan
  draws 224 of 240; without this the diorama had a 16 row black band). 16:9 /
  21:9 / 32:9 ground from the W33 margins is W34-09, not done here.
- **Cost.** `cargo test --release -p rf-renderer --test w34_00_diorama_cost --
  --ignored diorama_pass_cost_after_w34_05`, M4 Max, machine shared with other
  jobs (load 3 to 20), window-fit camera, mesh build + camera solve + pass +
  readback, ms p50 / p95. "Still" uploads the same mesh every frame, "moving"
  a different one.

  | Size (readback) | 4x MSAA still | 4x MSAA moving | 1 sample still | 1 sample moving |
  |---|---|---|---|---|
  | 256x224 (224 KB) | 2.31 / 2.74 | 2.31 / 2.82 | 1.84 / 2.54 | 2.30 / 2.93 |
  | 512x448 (896 KB) | 2.44 / 3.04 | 2.01 / 3.10 | 1.94 / 2.77 | 2.49 / 3.11 |
  | 1024x896 (3.5 MB) | 2.20 / 3.00 | 2.88 / 3.91 | 2.13 / 2.78 | 3.02 / 4.34 |
  | 2048x1792 (14 MB) | 3.32 / 4.53 | 3.29 / 4.44 | 3.02 / 3.95 | 3.04 / 3.95 |

  The 1024x896 4x row sits at D6's 3 ms bar *before* W34-06 adds a shadow pass;
  W34-00 measured 1.9 to 2.5 ms p95 with no depth and one sample. W34-00 put
  the ~1.6 ms floor down to creating resources; it did not move when they were
  reused. Timed inside the pass at 1024x896: CPU prep 0.2 ms, submit 0.1 ms,
  and 2.3 to 2.5 ms in the readback, of which the un-premultiply was 0.5 ms
  (0.25 after testing four pixels at a time) and the row copy 0.1 to 0.3 ms.
  The rest is the synchronous wait for the GPU. An async, double-buffered
  readback (one frame of latency) or render-to-texture is the next lever and
  is outside this ticket.

  In the whole app (unpaced, 10 interleaved chunks of 30 frames per state,
  Diorama off / on, and the app's own gate sample p95 for build + render +
  readback): at the 4x window (`RF_SCREENS_WINDOW=1300x1100`, image 1170x896,
  8:7): SMW 103.6 / 100.4 fps, p95 3.48 ms; ALttP 136.1 / 128.2 fps, p95
  2.16 ms. At the default test window (image 585x448, *not* 4x): SMW 103.6 /
  101.0, p95 1.85 ms; SMB3 (585x480) 373.2 / 320.6, p95 3.29 ms. The gate trips
  at 16.67 ms p95 (`fog::DISABLE_P95_MS`). D6's ten-minute run: SMW at the 4x
  window for 36 000 frames (630 s, 4x image 1170x896, RSS peak 509 MB):
  fps off / on 110.7 / 104.9, gate p95 3.62 ms, Diorama still rendering at the
  end (a trip would have dropped it to flat; the harness reads the end state,
  not every frame).
- **A finding for the learner (fixed in W34-03b).** The SMW shot's learned
  table depended on the timeline of the scripted play: 1500 frames gave 16 to
  32 raised cells, 1800 and 2100 about 790 (hills and the status bar extruded
  into spikes). Instrumented per signal and per key, the causes were: (1) S3
  "hides the player" on a fade/wipe layer, one key over 28% of BG3, drawn over
  Mario during the level exit, learned Tall (265 raised cells), and the
  votes were counted once per cell, so a wall of one tile reached the bar four
  times too fast; (2) the playfield was "the widest band", so on a screen where
  only a fill or status layer was full height that layer became the playfield
  and was extruded; (3) S4 mass on fill keys, kept (it is how ground fill is
  learned) but now bounded by the global guard. Fixes: S3 skips blank cells
  (`TileCell::BLANK`, set by the capture) and fills (>= 25% of the grid) and
  votes once per key per frame; the playfield is the widest band among the
  layers that scroll with the camera; only that grid is ever raised
  (`grids_for`); a table raising more than 40% of the playfield is drawn flat,
  its keys forgotten and `GuardTrip` logged. SMW YI1 at 1500/2100/3600 frames:
  raised cells peak at 79 of 960 (was 183 to 267) and the hills and status bar
  are never raised.
- **Not done here.** Per-call ground upload is still a `write_texture` of the
  whole picture (the live path's picture changes every frame; the decoded
  path's level image does not and could be keyed); the decoded-level window is
  still a fixed 256x240; the diorama image is uploaded to egui as straight
  alpha every frame (a premultiplied path would skip a conversion).
