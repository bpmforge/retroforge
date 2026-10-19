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

## W34-06 as built (2026-10-19)

Lighting and the shadow map: `rf-renderer/src/lighting.rs` (pure), the shader's
lit path, `DioramaPass::set_lighting`, and the shell wiring in `lens_view.rs` /
`app.rs`.

- **The sun.** One `Sun` from the lens's time of day (the slider, or the wall
  clock when "follow the clock" is on), mirrored for side view. The day runs
  0.23 to 0.82 so it follows the grade's dawn and dusk keys: elevation
  `70 sin(pi u)` floored at 10 degrees, azimuth +60 to -60 (rises screen-right,
  sets screen-left); outside it a dim cool moon at 20 degrees. The plan's
  `10 + 60 sin(pi t)` over the whole 0..1 day would put a 45 degree sun at 0.8,
  which the grade already paints as night, so it was not used. Colour is the
  grade tint pulled halfway to white (the lens grade multiplies the picture by
  the same tint; a full tint here would grade twice). Weather dims it (rain 0.6,
  snow 0.8). `lit = colour * (ambient + (1 - ambient) * intensity * max(n.l,0)^0.7
  * shadow)`; noon on level ground is exactly 1, so noon is as bright as the
  unlit picture. The 0.7 exponent is a deliberate softening of Lambert so a low
  sun still shows.
- **Normals without a vertex change.** The vertex layout and every builder are
  untouched; the fragment shader takes the face normal from the screen-space
  derivatives of world position (`NORMAL_SIGN` is pinned by a test: level
  ground must come out +Y). Box sides take a flat 0.8 albedo; ground, box tops
  and side-view fronts take their texture; sprites are lit like the ground
  under them and only look up the shadow.
- **Shadow map.** Orthographic, depth-only, from the sun: 1024^2 below a 768 px
  wide target, 2048^2 from there (the 1024x896 case). Fitted each frame by
  casting the frame's four corner rays into the mesh's box, so it covers what the
  camera sees and snaps to whole texels (no shimmer as the camera glides); only
  the depth range needs the whole mesh. Occluders: boxes, wedges and
  billboards, the last alpha-tested at 0.5 (a sprite casts by its own
  silhouette). The flat floor and the contact ellipses do not cast. The main
  pass does a 3x3 PCF over a comparison sampler with a slope-scaled bias that
  grows with the texel. The contact ellipse stays at 0.25/0.45 of its alpha as
  ambient occlusion; with the map off it is back at full strength.
- **Off is off.** `set_lighting(None)` (the default, and what every golden and
  `mode7_plane` uses) is the original shader path: the pinned goldens did not
  change.
- **Budget fallback.** `ShadowGate`: the pass's own render time over a 120
  frame window; a p95 above 8 ms drops the map (Lambert and full contact
  ellipses stay) until `reset_shadow_budget`. The app's 16.67 ms gate for the
  whole Diorama is unchanged and remains the last resort.
- **Lens handoff.** While the Diorama is the picture on screen (no peek, no
  painter, camera Original) `lens_view::without_shadows_over_diorama` forces the
  lens's 2D shadows off and skips cutting the sprite layer for them; the flat
  view keeps them. Finding: in this tree the lens output is only ever the flat
  picture (`diorama_texture` is uploaded straight from the pass), so bloom,
  tilt-shift, grade and weather do not run over the 3D image at all; HD2D_PLAN
  section 3's "keep running over the 3D image" is not wired, and was not part of
  this ticket.
- **Cost, and where the time was.** The W34-05 note put "2.3 to 2.5 ms in the
  readback" down to the synchronous GPU wait. Timing the stages shows it was
  not the GPU: `device.poll(PollType::Wait)` returned on a timer, in multiples
  of ~1.5 ms (1.53 / 3.07 ms), even when the previous frame's work had finished
  long before. Polling without sleeping (`PollType::Poll` in a short spin until
  the map callback fires) removes it; this alone took the pass from 3.0-3.9 ms
  p95 to about 1 ms and applies to the unlit pass too. The same fix probably
  applies to `superfx_gpu.rs`'s readback (not touched here). An opt-in
  double-buffered readback (`set_async_readback`, one frame of latency, off by
  default; the app does not use it) was built as well and measured.
  `cargo test --release -p rf-renderer --test w34_00_diorama_cost --
  --ignored diorama_pass_cost_with_shadows_w34_06`, M4 Max, machine shared
  (load 30 to 50), W34-05 scene (window-fit camera, 4x MSAA, moving mesh), 300
  frames per row, ms p50 / p95 of mesh + pass + readback:

  | 1024x896 | p50 / p95 |
  |---|---|
  | unlit | 0.67 / 1.44 |
  | sun, no shadow map | 0.68 / 0.93 |
  | sun + shadow map | 0.84 / 1.45 |
  | sun + map, double-buffered readback | 0.37 / 0.72 |
  | unlit, plus 8 ms of other CPU work between frames | 0.89 / 1.17 |
  | sun + map, plus 8 ms of other work | 1.39 / 2.38 |
  | sun + map, double-buffered, plus 8 ms | 0.32 / 0.38 |

  512x448, sun + map: 0.62 / 0.84. The shadow pass costs about 0.15 to 0.5 ms
  at p50 and fits the 3 ms bar with room. Before the poll fix the same
  harness read 2.77 / 3.51 unlit and 2.78 / 4.39 with the map at 1024x896.
  A paced loop that *sleeps* between frames reads slower (about 3 ms) because
  the GPU and core clock down while idle; that is a property of the harness, not
  of the pass.
- **Proofs.** `rf-renderer/tests/diorama_light.rs` (8): a 32 px box on level
  ground at time 0.3 / 0.5 / 0.8 casts a shadow whose darkened-pixel centroid
  (map on versus map off, same sun) lands left of the box foot in the morning,
  right of it in the evening, shortest at noon and longest at dusk; a
  half-transparent billboard casts a smaller shadow than an opaque one; level
  ground at noon is as bright as the unlit picture (pins the normal sign);
  removing the sun returns the original pixels exactly; the budget fallback
  drops the map and keeps Lambert; lit frames are deterministic; a steady lit
  scene creates no GPU resource after the first frame; the double-buffered
  readback lags exactly one frame. `lighting.rs` has 17 unit tests and
  `lens_view.rs` 2 for the sun and the handoff.
- **Shots** (`docs/screens/w34/`): `diorama-light-alttp-t30`,
  `-t50`, `-t80` (Link's house; the bed is the one raised block, and its shadow
  falls down-left in the morning, nearly vanishes at noon and runs long and
  down-right at dusk, which is also dim), `diorama-light-smw-on` (time 0.3: the
  shadows of Mario and the winged block fall on the backdrop plane behind them)
  and `-smw-t50` (noon: Mario's shadow falls on the face of the ground block,
  the block's on the hill behind).

## W34-06b as built (2026-10-19)

The lens over the 3D image, and the readback poll fix everywhere.

- **Lens over the 3D view.** `App::store_diorama_result` runs `LensPass` over
  the finished Diorama frame when the 3D view is the picture on screen
  (`diorama_view_active`: no peek, no painter, Original camera) and the lens
  is effective, and uploads that to the texture. `DioramaRender.rgba` stays the
  raw pass output (accuracy checks and fingerprints read it). 2D lens shadows
  are forced off there (`without_shadows_over_diorama`; no sprite layer is
  passed), bloom, tilt-shift, grade and weather run as in the flat view. The
  flat-frame lens no longer runs while the Diorama is shown (its output was
  discarded). The lens shares `lens_budget` and `lens_failed` with the flat path.
- **Tilt-shift uses screen rows, not depth.** The Diorama's pitched camera
  maps depth onto rows, and getting the depth buffer to the lens would need a
  second readback of a resolved, MSAA depth target; the band looks the same.
  The grade stays the lens's own and the preset still leaves it off (the shots
  tick it on).
- **Poll stall, everywhere.** `gpu::read_buffer_sync` (used by lens, fog,
  shader chain, scale, composite, original pipeline and the Super Fx GPU path)
  now polls without sleeping for up to 30 ms (`gpu::wait_for_map`, shared with
  the Diorama pass), then falls back to the bounded `Wait`; completion is
  still verified through the map callback. `w34_06b_readback_cost`, M4 Max,
  200 frames, ms p50 / p95 before -> after: lens 256x224 1.77/1.87 ->
  0.67/1.00, lens 1024x896 2.47/3.67 -> 1.83/2.54, fog 1024x896 2.79/5.04 ->
  1.87/2.45, shader chain 1024x896 2.37/2.57 -> 1.32/1.59, Super Fx GPU 4x
  MSAA4 2.72/4.39 -> 1.75/2.13, 3x no MSAA 2.21/2.35 -> 1.01/1.57.
- **Proofs.** `retroforge/tests/lens_over_diorama.rs`: lens off shows the raw
  render; shadows-only is the identity over the 3D image; bloom + grade +
  tilt-shift change the uploaded pixels while the raw render does not.
  Shots (`docs/screens/w34/hd2d-full-{alttp,smw}-{off,on}.png`, plus
  `-3d-no-lens`), uncommitted.

## W34-07 as built (2026-10-19)

Whole-level heights from the canvas, persisted; the Diorama at 32:9; the
ALttP floor edge. Code: `retroforge::level_heights` (new), `live_learn`,
`diorama_learned`, `app.rs` (`wide_ground`), `rf-enhance` `canvas/mod.rs`
(`queue_extra` / `fetch_extra` / `scene_merges`), `core_thread.rs`.

- **The re-tile cannot be literal, so it is a bridge.** A learner key is an
  FNV of VRAM bit planes plus the palette's CGRAM words
  (`tile_capture::hash_rows` / `palette_hash`, now shared); a canvas cell is a
  CGRAM index (`palette_base + palette * colours + colour`) plus the id of the
  palette snapshot it was seen under. Both halves come back: the colour numbers
  of an 8x8 block are written back into bit planes and hashed by the same
  function. `level_heights::tests::a_stitched_frame_rekeys_to_the_keys_the_
  capture_gave_it` captures a synthetic frame's grid, stitches the same frame
  and asserts every whole block comes back with the capture's key (flips,
  scroll phases, four palettes). Colour 0 is transparent (the layer's canvas has
  no cell there), so a block with such dots is keyed as if they were 0 and is
  "open": looked at again when more of it is seen. 16x16 character layers,
  hires, Mode 7 and the whole NES are not handled (the NES canvas stores the
  6-bit master colour, from which neither the pattern bytes nor the palette
  number come back); the NES gets the persisted table and call only.
- **Scout cells carry no palette id** (the scout stitches with no per-row
  palettes on the SNES; `canvas/merge.rs` says so). They are read under the
  layer's dominant snapshot. Palette cycling or an area with another palette
  therefore reads as an unknown key (flat), never as a wrong raised one.
- **The capture's block grid is one line off the picture, and the alignment is
  measured, not assumed.** Geometry (`grid_phase`: the frame's canvas origin
  minus the grid's offset) put every block of SMW one dot-row off the canvas's
  own: of ~7,600 live cells compared, none matched. A 64-way vote over live
  cells (`Calibration`: read the canvas at every shift of -4..3 dots both ways,
  count the cells whose key equals the capture's) decides the shift per scene
  and layer in about 0.7 ms; on SMW it is (0, -1) every time. After it, 5,713
  of ~7,600 live cells match exactly (75 %), 909 match in palette only (a sprite
  or the HUD covers part of the block, or the block has colour-0 dots), 936 are
  unkeyed (off the stitched canvas). This also means the live lattice that
  `Lattice::from_learned` builds sits one picture row off the drawn picture
  (`tile_capture` takes `vofs + y0`); not changed here.
- **Where it runs.** On the core thread beside the learner and the canvas model
  (`LiveHeights::level_pass` -> `LevelTracker::observe`): a pass every 45
  frames, and at once after a scout delta is merged (`scene_merges`; the game is
  usually paused while the scout works, so frames alone would not come). It
  looks at blocks that are not finished only, then turns keys into heights with
  a table lookup per block, wholesale (a tile the guard forgot or demoted never
  stays raised on the canvas). The result (`HeightCanvas` plus a `LevelReport`)
  rides every `HeightsFrame` as an `Arc`, not only when it changes: the UI's
  pending-frame cap drops frames, and an update carried by a dropped frame
  would be lost for good.
- **Persistence** (all through the canvas model's one `Cache`, written by its
  `flush` and its drop; opening a second index over one root is not safe):
  the ROM's `TileHeightTable` (`tile-heights-v1`, restored into
  `HeightLearner::with_table` on the first captured frame), a heights block per
  scene (`heights-canvas-v1`, a sibling entry as W34-03 made it; restored on
  entering the scene and replaced by the first pass) and the camera call
  (113 bytes with its entry header, only when watched from play: gravity or
  constant speed at confidence 2 or more). A remembered call is drawn from at
  once but never fed to the learner (so a menu is not learned from) and is
  dropped the moment the game's own evidence is sure; a profile's hint wins.
  Written when stats changed and 300 frames passed, on leaving a scene, and on
  Shutdown.
- **Widescreen Diorama.** `refresh_learned_diorama` no longer clears at a
  frame wider than 256 when the level's heights belong to the scene on screen:
  the ground is the flat view's own margin fill with the sprites taken out
  (`App::wide_ground`, a second `widen_margins` for the pixels only), the
  lattice is `Lattice::from_level` over the whole wide picture
  (display column X is canvas `origin.x + X - (out_w - 256) / 2`, as
  `fill_margins_keeping`), billboards shift with the plane
  (`side_billboards_wide`), the camera frames the wide picture. Side view only.
- **A bug found on the way:** `SetWidescreen` grew the frame buffer but not the
  layer extraction, so Diorama on with widescreen on handed the UI layers 256
  wide beside a 512-wide frame (epaint's size assertion took the app down; the
  layers also held the left margin, not the picture). Fixed in `core_thread`;
  `tests/w34_07_widescreen_layers.rs` fails without it.
- **ALttP floor edge (D2).** The black beyond the room was the clear colour
  around a plane that ended at the picture. The top-down floor now goes on
  `EDGE_MARGIN_CELLS` = 16 cells past the picture on every side in the room's
  own edge pixels (`LiveGrid::with_margin`, `padded_ground` already repeats
  edges); the camera is still held to the room. The ground also stops at the
  rows the game shows (224 of the host's 240; the 16 black ones were part of
  the plane). 8 cells left a band at the far edge; 16 did not.
- **Not done / limits.** NES whole-level (see above), 16x16 layers, top-down
  from the canvas, 32:9 outside a side view; the Diorama does not raise HUD
  layers (D7 is W34-08). Turning 32:9 on while the map is showing and then
  entering a level leaves the level's canvas offset by the map's scroll (the
  world axes carry on across the scene change), so a restored canvas lands
  beside the live one; the shots turn 32:9 on inside the level, as
  `shot_persist` does. Pre-existing, not changed here.
- **Verified: a tile learned once is known level-wide.** SMW YI1, learned in
  one session at 4:3 (2 solid kinds: the ground tiles; 121 kinds drawn), applied
  in the next to the scouted canvas (624,164 cells merged by the scout, 11.7 s
  wall, unchanged: the scout stitches only): 662 solid blocks over the level, 630
  of them in cells the scout saw first. Visited area alone would be the ~32
  on screen.
- **Costs** (M4 Max, release): a pass over a 16,000-block canvas (5,120 x ~200
  dots) 2.4 ms the first time, 0.1 to 1.7 ms after (it skips finished blocks); a
  rebuild of the heights from the keys 0.1 ms; the alignment vote 0.7 ms once
  per scene; the scout's wall time did not change (it takes no grids). Diorama at
  32:9 in a 1700x760 window, pacing off, 10 x 30-frame chunks per state: 53.7 fps
  off, 53.0 on, the pass 2.04 ms p95 at 1371x672. Stored: scene chunk 68 KB,
  heights block 16.5 KB, table 8.5 KB (1.6 k keys), orientation 113 B.
- **Shots** (`docs/screens/w34/`): `diorama-level-smw-32x9-off`
  (the flat 32:9 frame), `-on` (the 3D view: the ground row stands as blocks
  across the right margin the scout filled, out to the edge; the left is the
  level's start, so nothing is there; the diagonal slope and the hills stay flat
  because only the ground tiles were learned), `-tilted` (the same frame from a
  raised, turned camera, where the thickness of the scouted ground shows),
  `-second-visit` (a fresh session, no play, no scout: the same picture), and
  `diorama-level-smb3-second-visit` (NES: ground slab on the first frames, from
  the restored table). `diorama-top-alttp-on` was re-shot for the floor edge.
  Second visit, SMW: with 32:9 switched on when the level's sky first shows
  (frame 70 from the X press), the level heights reach the UI 3 frames later with
  662 solid blocks (630 scouted), 0 scouts started; 4:3 raised cells from the
  restored table alone at frame 63. SMB3 1-1: blocks stand 2 frames after Diorama
  goes on.

## W34-08 as built (2026-10-19)

Layer separation, the flat HUD and entity billboards (D3, D4, D7). Code:
`retroforge::diorama_layers` (new: the split, the HUD overlay, entities),
`retroforge::hud_watch` (new: which part of the picture is HUD),
`rf-renderer::diorama_side` (atlas, back planes, behind-the-background
sprites), `kind::PLANE`, and the wiring in `live_learn` / `app.rs`.

- **Which layer drew each dot.** The core thread records it per pixel
  (`LayerInfo::ids`, 61 KB a frame, only while Diorama captures) next to the
  learner's scroll ratio per layer (`HeightLearner::layer_ratios`, travel over
  the fastest layer's) and the HUD verdict. A dot the main screen leaves to the
  backdrop can still be a layer: one the game puts on the SUB screen and
  blends in with colour math. That is Super Mario World's hills (the PPU
  metadata calls 80 % of a level frame "backdrop"); `layer_ids` reads the sub
  screen's own pixel at that dot (`FrameBundle::sub`, captured while Diorama is
  on) and gives the dot to that layer. Without it the hills and the sky were
  one plane.
- **Planes (D4).** The picture is pulled apart into one slot per plane of a
  single stacked texture (the pass keeps its two textures): the playfield
  (blocks grow from it, plus every layer scrolling at 90 % or more of its
  speed or with no ratio yet), one plane per slower layer at
  `(1 - ratio) * 48 px + 0.5 px * layer` behind it, and the backdrop dots as the
  sky 24 px behind the furthest. Each back plane is scaled about the eye
  straight in front of the target (`SideScene::ref_eye`), so from the front it
  covers exactly what it covers flat and the planes part company only when the
  camera moves. A back plane's own pixels are carried down through what is in
  front of it (nearest of its own above in the column, else below, else along
  the row), because the console only kept what won: without that a swinging
  camera shows slivers of the plane behind through the ground. Pixels a sprite
  covered are filled from their row, so the player is not drawn twice.
  `kind::PLANE` is shaded like ground but never casts into the shadow map: the
  first build cast, and each plane darkened the one behind it in the shape of
  its own outline (a test pins it, and fails if it casts).
- **HUD (D4, D7).** `HudWatch` (two 240-frame windows) calls a layer HUD when
  it is not the playfield, its dots sit in one band at an edge (under three
  quarters of the rows, within 16 rows of the top or bottom), its scroll never
  moved, and either the playfield did or the two are scrolled apart (a room that
  never scrolls: ALttP). It calls a band HUD when the playfield's widest band of
  a layer stops short of an edge and the band reaching that edge keeps its
  scroll (the NES split under SMB3's level; the little bands a split writes in
  between belong to it). Super Mario World's status bar is BG3 rows 9 to 36;
  SMB3's is rows 193 to 240; ALttP's is BG3 at the top. The canvas model's own
  `hud_layers` was not usable here: it only exists while widescreen stitches
  the scene (`TileGrid::hud` was false on every captured grid). HUD dots are
  taken out of the scene (filled from their row, a fully covered row from the
  nearest row) and kept as a `HudOverlay`, composited over the finished 3D image
  AFTER the lens, by mapping the whole picture onto the whole image (nearest),
  so zoom and follow cannot crop it and pitch cannot move it. It is not lit,
  lensed, shadowed or weathered. The learner never raised HUD grids (only the
  playfield grid is extruded); D7's synthetic-layer test was already
  `a_hud_grid_is_never_raised`.
- **Billboards (D3).** Touching sprites are one entity (`cluster`, the W34-02
  clustering) and one upright billboard standing at its bottom edge on the
  height field, as before; behind-the-background sprites cluster apart and
  stand just behind the playfield plane (so the plane's opaque dots and a Tall
  box hide them through the depth buffer, and they show through its holes).
  Both kinds are shadow casters (billboards were already, alpha-tested). The
  top-down builder takes the same entities.
- **Contract change.** `DioramaRender::rgba` (and the lensed copy that is
  uploaded) now carry the flat HUD, composited last; W34-06b's "stays the raw
  pass output" holds for the 3D part only, and a fingerprint over `rgba`
  includes the HUD. Frames with no HUD, and every other path (decoded level,
  Mode 7, wide ground, layering off), are byte-for-byte what they were.
- **Not done / limits.** D4's "atmosphere layers stay fog" is not addressed:
  fog / rain / weather layers are whatever the picture drew (a fast layer joins
  the playfield plane; a slow one gets a plane) and the fog pass is untouched.
  Widescreen (W34-07's wide ground) stays ONE plane with
  its old set-back and its HUD in the picture: the canvas margins carry no
  per-dot layer, so W34-09 has to bring them. 16x16 hires, Mode 7 and the
  decoded-level path are untouched. The HUD call is a heuristic (a layer that
  only draws a band at an edge and never scrolls); a wrong call flattens a band
  and learns nothing. A sprite's shadow on a far plane lands displaced and
  reads as a floating blob at some sun angles (physically right: the plane is
  48 px behind). The planes' set-back is fixed (48 / 24 px), not scaled by the
  tile size.
- **Cost** (M4 Max, release, machine shared; whole-app diorama gate p95 =
  split + mesh + pass + readback + the HUD of the previous frame, 240 frames
  per row, pacing off, layers and one plane alternating in the same binary).
  Default window (image 585 x 448): SMW 1.91 / 1.70 ms vs 1.74 / 1.51 on one
  plane; SMB3 (585 x 480) 1.74 / 2.78 vs 1.54 / 1.38; ALttP 1.82 / 1.97 vs
  1.38 / 2.26. At the 4x window (image 1170 x 896): SMW 3.37 / 3.43 vs 2.02 /
  1.81; ALttP 3.32 / 2.86 vs 1.92 / 2.09, so about +1.3 ms at 4x, over D6's 3 ms
  line at 1170 x 896 (the pass alone, W34-06b: 0.93 to 1.45 ms at 1024 x 896).
  Timed on their own: the split 0.41 ms (256 x 224, three planes), the HUD
  composite 0.12 ms at 1170 x 896, and the four-slot atlas against a one-picture
  ground in the pass (`atlas_upload_cost`, same mesh, 1170 x 896) 0.16 to 0.2 ms
  more (p50 1.83 -> 1.99, p95 2.25 -> 2.44). That is about 0.7 of the ~1.3 ms;
  the other ~0.6 is not measured and is probably the fill of three more
  full-image planes at 4x MSAA. `fps` is the core's (about 100 for SMW, 330 for
  SMB3, 127 for ALttP) and does not move.
- **Proofs.** `rf-renderer/tests/w34_08_layers.rs` (4): a back plane matches
  the flat picture from the front and shows parallax at yaw 25 (one plane
  shows none); a behind sprite is hidden by an opaque plane, shows only through
  a hole, and a Tall box hides it there; a plane casts no shadow on the plane
  behind (fails when it casts). `retroforge/tests/w34_08_layers_app.rs`: the
  status-bar rows of the Diorama are identical at pitch 30 and 75 and at zoom
  100 and 250 with follow (and differ with layering off), planes change the
  picture, a layer map of the wrong size is ignored. Units: `diorama_layers`
  (12), `hud_watch` (9), `layer_ids` (1), `layer_ratios` (1), side builder (3).
- **Shots** (`docs/screens/w34/`; sun fixed at 40 %).
  What is in them, as seen: **SMW** `-on`: status bar flat and sharp across the
  top, hills and clouds on their own plane behind the ground (the hills read
  as one slab behind the grass, lit and shadowed separately from the sky),
  Mario's shadow lands on the hills as a displaced grey blob, a sliver of void
  at the left and bottom edge where the picture's own edge shows under the
  pitch; `-yaw15` against `-oneplane-yaw15`: the hills and clouds slide against
  the ground and the sky plane's edge shows on the right (parallax), where the
  one-plane picture only skews; `-pitch-lo`/`-pitch-hi` against
  `-oneplane-pitch-hi`: the status bar is the same pixels, on one plane it
  tilts with the picture; `-zoom` against `-oneplane-zoom` (preset 160 %,
  following Mario): the status bar is whole, the one-plane view crops it.
  **SMB3** (NES, one background layer): only the sky is a separate plane (24 px
  behind), so `-yaw15` shows the sky plane's edge sliding from the bushes' and
  nothing else moves; the status bar is flat at the bottom in `-on` and
  `-pitch-hi`, identical, and tilted with the floor in `-oneplane-*`. **ALttP**
  `-on`: hearts, magic meter and item box flat and sharp at the top, Link one
  billboard with his shadow on the floor; `-oneplane` is the W34-07 look, the
  HUD floating over the room as a tilted band. Soft smudges above the HUD are in
  both (not from this ticket). Files: `diorama-layers-{smw,smb3,alttp}-{off,on}`,
  `-smw-{yaw15,oneplane-yaw15,pitch-lo,pitch-hi,oneplane-pitch-hi,zoom,
  oneplane-zoom}`, `-smb3-{yaw15,oneplane-yaw15,pitch-hi,oneplane-pitch-hi}`,
  `-alttp-oneplane`.

## W34-09 as built (2026-10-19)

The widescreen Diorama (D2 at 16:9 / 21:9 / 32:9), the layers in the canvas
margins, and the 4x cost. Code: `retroforge::app` (`widen_margins_layered`,
`widen_margins_for_frame`, `wide_ground`, `display_grid`),
`retroforge::diorama_layers`, `diorama_learned`, `play_view`,
`rf_enhance::canvas::compose` (`margin_pixel_layer`, `margin_color_layer`,
`front_layer_in`), `rf-renderer` `diorama_side` / `diorama.wgsl`.

- **Which layer drew a margin dot.** The compose that fills the margins from
  the canvases (`margin_pixel`) already found the front-most known cell; it
  now also says which layer's canvas it came from (`margin_pixel_layer`), by
  the core's own rule inside the 256 dots: the main screen's front-most layer,
  else the sub screen's where the row blends it in (SMW's hills), else the
  backdrop. `widen_margins_layered` returns that beside the picture as a layer
  map of the WIDE picture (the core's ids for the 256 hardware dots in the
  middle, the compose's in the margins, the backdrop where nothing is known).
  The core records its map for the 256 dots only (`bundle.video` is the
  centre of a widened line), so the map's size is `256 x rows` there, not the
  core frame's; `refresh_learned_diorama` checks that.
- **One compose per frame.** The flat view composes the margins first; while
  a layered Diorama is wanted it keeps that picture and map (`wide_cache`) and
  the Diorama stands on them. Before, the Diorama composed the margins a second
  time (1.9 ms mean at 32:9 in a 1829 x 448 window). The sprites stay in the
  cached picture; `split` takes them out by the ids' `LAYER_SPRITE`
  (`sprite_rgba` may now be empty), so a layer-less frame (layers off) still
  takes the old second compose with the sprites removed.
- **Planes in the margins.** `split` runs on the wide picture: every plane
  (playfield, one per slower layer, sky) has its margin dots on its own slot,
  the carry-down and the row fill work on the wide width, and a margin dot no
  layer knew is the row's backdrop colour on the sky plane. The HUD is the
  game's own 256 dots (`LayerSplit::hud_cols`, `without_hud`'s `hud_cols`):
  a HUD band's rows reach only the middle columns, the margins of those rows
  stay in the scene, and the overlay is the wide picture's size with alpha
  only in the middle, so `composite` lands it centred and unstretched.
  Heights are W34-07's (`Lattice::from_level`). Sprites are the live game's.
- **Top-down at 16:9.** `usable` used to need a 256-wide frame or a
  side-view level, so ALttP at 16:9 turned the Diorama off. A floor plan on
  a widened frame now takes the lattice in the 256 dots' coordinates
  (`Lattice::shifted_right` by the margin), the wide picture as its floor
  (`TopFrame::screen` the wide size, `sprite_size` / `pads` for the sprite
  texture and the boxes), and `without_hud` on the wide map. Its walls come
  from the painted / learned grids of the 256 dots only; the floor in the
  margins is flat (the heights-from-the-canvas of W34-07 are side view only).
  `without_hud` also lifts the sprites off the floor now (Link was drawn
  twice, once on the floor and once as a billboard, in W34-08's shot too).
- **The play rect at 21:9 and 32:9 (a bug in the shell, found here).** The
  core's frame is capped at the PPU's 512 dots and `DisplayGrid::for_frame`
  folds any frame 512 wide or more as hires, so at 32:9 (and 21:9: 523) the
  rect was sized from a 256-dot picture: 4:3, the picture squeezed to a
  quarter of its width (the W34-07 note "the window crop stretches a wide frame
  to 4:3" was this). With widescreen on the play view now lays out by the
  widened picture (`display_frame_size`, `DisplayGrid::for_widened_frame`,
  never for exactly 512). `tests/w34_09_wide_diorama.rs` asserts the rect's
  aspect is the picture's at 16:9, 21:9 and 32:9 and that the Diorama image has
  the rect's aspect (it fails, 1.31:1 against 2.69:1, without the fix).
- **The planes stand on past the picture's edge** (W34-08's void at the left
  and bottom). `SideScene::overscan_px` (the app passes the plane's height)
  makes every plane a picture quad plus eight cells that repeat the picture's
  edge texels (half a texel in, so filtering never reads the transparent gap
  between two slots; each runs 1 px under the picture and a hair further back,
  because the picture's own edge pixels are blended half-way into that gap).
  A second W34-08 fault found on the way: with planes the plane was the
  frame's 240 rows but the atlas slot only the 224 the game shows, so the 16
  rows below the picture read the gap and the next slot (a band of sky under
  the ground); the plane is now `rows` tall.
- **Mario's shadow on the hills.** A back plane is `kind::BACK`: shaded like
  a plane (never a caster) and it receives no shadow, so a sprite's shadow
  falls on the plane it stands on and not displaced onto one 48 px behind.
  `rf-renderer/tests/w34_09_planes.rs` pins it (and fails, 2,000 px darkened,
  when the hills receive it at a low sun).
- **Cost: where it went.** Machine shared (load 2 to 12 during the runs; rows
  alternate in one binary, pacing off, 240 frames per row, SMW YI1, image 1170 x
  896). `w34_09_cost` (the pass alone on an SMW-shaped scene with three planes,
  mesh + upload + draw + readback): one plane 0.83 p50 / 1.52 p95 ms; three
  planes as W34-08 built them (planes receive the shadow) 1.11 / 1.65;
  three planes now 0.83 / 1.05; three planes without the shadow map 0.69 /
  0.93, unlit 0.68 / 0.91, 585 x 448 0.54, 64 x 64 (fixed cost) 0.42. So the
  W34-08 "unmeasured 0.6 ms" was the 3 x 3 PCF lookup on three full-image
  planes: skipping it on the back planes takes 0.28 ms off the median and 0.6
  off the p95, and the fill of three planes with the lookup gone is the same as
  one (the plane texture is opaque and noisy in the bench). A depth pre-pass was
  not needed. In the app, per frame (stage means): split 0.41 -> 0.30 ms
  (row-major carry-down, one slot index per pixel, one clear copy of the picture
  for every slot), HUD composite 0.12, mesh 0.01, the pass 1.5 to 1.8 (the
  harness shares the GPU with the UI). The app gate (split + mesh + pass +
  readback + HUD of the previous frame, and now the wide picture) at the 4x
  window, p50 / p95, W34-08 -> now: SMW layers 3.37 / 3.43 p95 -> 1.6 to 1.9
  p50, 2.1 to 2.9 p95 (four rows, 2.1 / 2.1 / 2.3 / 2.9), one-plane control
  1.3 to 1.4 p50, 1.6 to 2.7 p95; ALttP layers 3.32 / 2.86 p95 -> 1.8 to 2.2
  p50, 2.0 to 3.1 p95, control 1.4 to 1.9 p50, 1.6 to 3.1 p95. The p95 of a
  row moves by a millisecond with the machine's load (the control does too);
  the median is steady, so the target is met at p50 unconditionally and at p95
  when the machine is not loaded. Layers cost about 0.4 ms over one plane at
  4:3 (both rows have no wide picture there). At wide aspects the one-plane
  control is NOT comparable: it makes its own margin compose (`wide` 1.9 ms
  at 32:9) while the layered path rides the flat view's, which the gate does
  not count; the layered 32:9 row reading cheaper than one plane is where the
  compose is counted, not a win. 1371 x 672 (16:9): layers p50 2.1 p95 3.0;
  1829 x 448 (32:9): p50 2.5 to 2.6 p95 3.2 to 3.4, of which the split is 0.7.
  No GPU timestamps (the device is requested without the feature): "where it
  went" is pass by pass, with knobs toggled in `w34_09_cost` and CPU stage
  timers in the app, not a GPU profile.
- **Shots** (`docs/screens/w34/`; sun fixed at 40 %, a 1900 x
  800 window, so 16:9 is 1371 x 672 and 32:9 is 1829 x 448; SMW learned at 4:3
  in a first session, then a fresh start with the scout): `diorama-wide-smw-16x9-
  {off,on}`, `-32x9-{off,on}`, `-16x9-yaw` (yaw 15), `diorama-wide-alttp-16x9-
  {off,on}`; `RF_SCREENS_ONLY=diorama-wide` (`RF_WIDE_COST=1` for the cost
  rows, `RF_WIDE_NOSCOUT=1` to skip the scout).
- **Not done / limits.** `wide_cache` clones the wide picture every frame
  (3.3 MB at 32:9, about 0.1 ms; a refcount would drop it). VRAM-drawn margin
  dots between the 256 hardware dots and the core's frame width that no canvas
  knows get the backdrop id but keep their VRAM colour (ground-coloured dots on
  the sky plane, if it ever happens; the canvases normally know them). The
  floor of a top-down Diorama in the margins is flat
  (no walls from the canvas); the NES at 21:9 / 32:9 gets the same planes only
  as far as its one layer goes; 16x16 / hires / Mode 7 untouched; the stretched
  edge of the planes (the picture's last column or row repeated) shows at a
  steep pitch or yaw on a screen with something on the edge (the map screen
  shows tall streaks of the picture's edge colours); the cost at 32:9 is the
  split (0.7 ms) plus the pass, both over D6's 3 ms line at p95 on a loaded
  machine.
