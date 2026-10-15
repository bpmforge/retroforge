# W31-01 — Star Fox's polygon map in GSU RAM (observation only)

Date: 2026-10-15. Clean-room: everything below comes from watching our own
emulator run the user's dump of *Star Fox (USA)*: the GSU's RAM reads and
writes (with the instruction address that made each), its `GETB` reads from
ROM, and every `PLOT`. No UltraStarFox, no leaked source, no disassembly,
no RAM map. Builds on W30-00 (Super FX output fixed) and W30-10 (vertex
records found). Report only; nothing built.

## The map

| What | Where | Format |
|---|---|---|
| Projected vertex table | GSU RAM `0x07A2`, up to 64 records (`0x07A2-0x0921`), rebuilt for each object every render job | 6 bytes: `x` (s16), `y` (s16), `w` (u16) |
| Third word `w` | — | low byte = **clip outcode**: bit 0 above the top, bit 1 below the bottom (inferred), bit 2 left, bit 3 right, bit 4 behind the camera / near plane (projected x, y then meaningless). High byte = a 5-bit value (22-31 seen) that rises with draw order (Spearman ρ 0.74-0.80 over three jobs): a quantised depth or depth-cue level, nearer = larger. Not proven which. |
| Face lists | **ROM**, read with `GETB` (ROMBR banks `0x0E`, `0x11`, `0x14`, `0x00` in the scenes watched) | a record tagged `0x14`, then the vertex count, colour and normal fields, then one byte per vertex: an **index into the vertex table** (record = `0x07A2 + index × 6`) |
| Current polygon | GSU RAM `0x0134` = vertex count; `0x0982+` = work buffer of `(x, y)` word pairs | the polygon the edge walker fills, after clipping; the walker's setup code reads its corners from here |

Face lists are not in RAM: the models' faces are cartridge data that the
GSU reads straight from ROM each frame. What RAM holds is the projected
vertices (table) and, per polygon, the clipped corner list (work buffer).

The per-polygon sequence observed, every time:

1. the face record's count is written to `0x0134`;
2. for each index byte, the vertex record at `0x07A2 + i×6` is read
   (x, y, w) and its x, y copied into the work buffer at `0x0982`;
3. polygons with any outcode bit set are clipped in the work buffer
   (vertices may be added; after clipping only the low byte of x and y is
   used — 8-bit screen coordinates);
4. the edge walker reads the buffer and fills spans with `LOOP`/`PLOT`
   (one fill routine per scene; three were seen: `B060`, `A75D`, `D017`).

The 3D viewport is **224 x 170** pixels (every plot fell inside x 0-223,
y 0-169).

## Proof: polygons rebuilt from RAM against the GSU's own pixels

Each polygon was rebuilt from RAM alone (the work buffer, or the vertex
table via the face indices), rasterised with a plain scanline fill (left
edge `floor`, right edge `floor`, bottom row excluded), clipped to the
viewport, and compared with the pixels the GSU actually plotted for that
polygon (pixel intersection-over-union).

| Scene | Polygons | Pixel IoU (all pixels) | Median per polygon | Polygons ≥ 0.95 IoU |
|---|---|---|---|---|
| Intro, frames 900-915 (3 render jobs) | 418 | **0.990** | 0.996 | 296 (71%) |
| Attract-mode play, from frame 2700 | 60 | **0.995** | 0.998 | 58 (97%) |

From the vertex table + face indices alone (before clipping), the intro
scored 0.985 IoU (347 polygons); clipped polygons need the work buffer.
Polygons under 95% are almost all tiny (a few pixels), where one
rounding difference on an edge is a large fraction. 51 + 20 polygon
setups produced no pixels (culled or fully clipped) and were not scored.

## Recommendation

**A high-resolution polygon renderer is feasible**, as a presentation-only,
profile-driven enhancement:

- Everything a re-rasteriser needs is observable per render job: the
  polygon order (the GSU's own painter order), each polygon's corners after
  clipping, its colour (COLR) and dither mode (POR), and the viewport.
  Re-filling those polygons at 2x-4x gives smooth edges and fill without
  touching the simulation.
- **Limit:** the table stores integer screen coordinates, so corners stay
  snapped to the original pixel grid; edges between them become smooth,
  corners do not move. True sub-pixel corners (and widescreen beyond the
  224-wide viewport) would need the pre-projection coordinates. RAM at
  `0x0600-0x07A1` holds 6-byte records written just before projection by
  code at `A926`/`A93F`/`A958` that look like camera-space x, y, z — the
  natural next spike if sub-pixel or widescreen 3D is wanted.
- The hook points are game-specific (count at `0x0134`, buffer at `0x0982`,
  table at `0x07A2`), so they belong in Star Fox's profile, not engine code.
- Other GSU titles (Stunt Race FX, Vortex) will have their own maps; this
  method (log RAM/ROM/PLOT per job, rebuild, compare) carries over.

Suggested build order if Brad wants it: (1) a core-thread observer that
records the per-job polygon stream from the profile-declared addresses;
(2) a GPU re-rasteriser that draws that stream at N× over the original 3D
window; (3) a by-eye pass on Star Fox, then the camera-space spike.
