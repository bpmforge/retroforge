# W31-04 — Sharper Super FX 3D: proof and how to try it

Date: 2026-10-15. Builds on W31-01 (the polygon map), W31-02 (the observer)
and W31-03 (the re-rasteriser and compositor). Clean-room: observation of
our own emulator only. Presentation only: the game runs exactly as without
it (machine state identical with the observer on or off, W31-02).

## How it works

1. The core records each GSU render job as an ordered list of polygons
   (clipped corners, colour, dither mode) and single plots, from the
   addresses in the game's profile (`[superfx_polygons]`).
2. On the UI thread the latest jobs are rasterised at 1x and matched
   against the frame actually on screen: the 3D window's position and the
   palette colours are found from the screen itself, and a job is used only
   when at least 97% of its pixels agree. Otherwise the original frame is
   shown.
3. Only screen pixels that are the 3D picture are replaced (sprites and the
   HUD over it stay as they are). The polygons are redrawn at 2x-4x with the
   game's own dither pattern; silhouette pixels take the background colour
   where the hi-res polygon does not cover them, so outer edges are smooth
   too.

## Proof (Star Fox (USA), no input)

| | Intro (frames 850-1100) | Attract play (2600-2800) |
|---|---|---|
| 1x raster vs the GSU's own pixels (W31-02) | 99.58% exact colour, IoU 0.999 | 99.59% exact colour, IoU 0.997 |
| Frames where the redraw matched the screen (>= 97%) | 250 / 250 | 200 / 200 |
| Mean match score | 0.9959 | 0.9963 |
| Share of the 3D window replaced | 48.7% | 25.2% |
| 1x composite identical to the original frame | yes, 500 composites | yes, 400 composites |
| Cost at 3x (release) | ~1 ms p95; ~0.25 s once per scene to locate the window | same |

Reproduce: `RF_SUPERFX_POLY_ROMS="<Star Fox dump>" RF_SUPERFX_HD_OUT=<dir>
cargo test --release -p rf-harness --test superfx_polygon_observer -- --nocapture`
(writes native / 3x PNG pairs every 50 frames).

## Try it

1. Open Star Fox (the USA or Rev 1 dump; the profile ships in
   `profiles/snes/star-fox/`).
2. Switch the mode to **Enhanced** (Game-Aware works too).
3. Esc → **Enhancements** → turn on **Sharper Super FX 3D** ("HD Super FX")
   and pick 2x, 3x or 4x (default 3x).

Things to look for: smooth polygon edges and silhouettes; no flicker when
the redraw switches on or off (it falls back to the original frame when the
match drops); the HUD and sprites unchanged; occasional single dark specks
along steep edges (seen once in testing).

## Limits

- Corners stay on the original pixel grid (the game stores whole-pixel
  screen coordinates); the edges between them become smooth.
- Not combined with widescreen (skipped while widescreen is on), an HD pack,
  or HD Mode 7.
- Other Super FX games need their own `[superfx_polygons]` entry, found the
  same way as W31-01.
