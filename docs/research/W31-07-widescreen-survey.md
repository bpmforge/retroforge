# Widescreen in other emulators: survey (2026-10-16)

Clean-room: user docs, wiki pages, READMEs and press coverage only; no
emulator or game source was read.

## Four approaches people use

| Approach | Who | How | Limit |
|---|---|---|---|
| Draw more of what the console already holds | bsnes-hd (SNES), Genesis Plus GX Wide (Genesis) | Render extra columns from the tilemap/VRAM the game already loaded. bsnes-hd: per-layer modes (auto horz+vert, crop edges, limit to lines above/below N), sprite modes (safe / unsafe / clip / off), "ignore window" outside the 4:3 area, a fill colour for the widened area (auto / colour / black), a marker (lines / darken) showing the extra area, Mode 7 only by default. Aspects 16:9, 16:10, 2:1, 21:9. GPGX Wide: "extra columns" per game (10 for 16:9, more for some titles). | Only as wide as VRAM (SNES tilemaps max 512 dots), pop-in at the edges, HUD repeats, sprites the game culls off-screen never appear. Nobody offers 32:9 this way. |
| Patch the game to use the wider screen | Vitor Vilela's Super Mario World widescreen patch (needs bsnes-hd), Sonic widescreen patches for GPGX Wide | The game itself streams a wider tilemap and keeps sprites active in the widened area (SMW: 352x224, the game's panning camera treats the view as wider; single-screen levels widened by hand; bosses fixed one by one). | One hand-made patch per game; still being finished (sprite types at the border, bosses). Highest quality. |
| Stitch what the camera has seen | wideNES (ANESE, NES) | Track the scroll registers frame to frame (handling the 8-bit wrap), place each frame on a growing map, show the map around the live picture; status-bar splits need special handling. | Only areas already visited; HUD and split-screen tricks are the hard part. This is RetroForge's stitched canvas. |
| Read the level data | RetroForge's DecodedLevel source (profiles) | Decode the level from the game's data, place it under the live camera. | Per-game format knowledge; clean-room limits what can be used. |

## bsnes-hd's widescreen convention (sneslab "Widescreen" page)

- 16:9 = 352x224 (+48 a side), 21:9 = 448x224 (+96 a side), 8:7 pixel aspect kept.
- Sprite X is read as -128..+384 (>= 384 wraps by -512); sprites already
  handling the left edge adapt easily.
- Window positions map by `2x - 128` when widescreen is on (00 = -128,
  80 = +128, FF = +383); window HDMA works at any aspect.
- The extra area is invisible on real hardware, so patched games still run
  everywhere.

## What this means for RetroForge's 32:9

1. **Stitch per layer, not the composited frame.** Our stitched canvas
   records the finished picture at the main layer's scroll. SMW's hills
   (BG2) scroll at a different rate, so stitching the composite repeats
   terrain at the wrong offset and loses the sky: the artefacts in
   `target/screens-probe/widescreen-32x9-canvas.png`. One canvas per BG
   layer, each placed by its own scroll registers (layer extraction already
   exists), then composited with priorities, fixes that. Most likely root
   cause; needs checking against the probe.
2. **The HUD is its own layer on the SNES** (SMW's status bar is BG3 with its
   own scroll). Per-layer stitching drops it naturally; the zero-delta "HUD
   band" guess stops being needed there.
3. **Borrow bsnes-hd's options:** a fill colour for unknown areas (auto
   backdrop colour instead of black), sprite mode (clip at the 4:3 edge vs
   draw), a marker showing the widened area (good for the tour), and
   per-layer on/off.
4. **Support the bsnes-hd widescreen convention** (sprite X -128..384,
   window `2x - 128`, 352/448-wide output) so user-supplied widescreen
   patches like the SMW one run with real gameplay in the margins. The
   convention is documented on sneslab; the patch itself is the user's.
5. **32:9 ahead of the camera** can only come from a decoded level or a
   patched game; stitching can only show where the player has been.

## Sources

- sneslab, Widescreen: https://sneslab.net/wiki/Widescreen
- libretro docs, bsnes-hd beta core options: https://docs.libretro.com/library/bsnes_hd_beta/
- PCGamesN, bsnes-hd in RetroArch: https://www.pcgamesn.com/emulation/bsnes-hd
- libretro, Genesis Plus GX Wide: https://www.libretro.com/index.php/genesis-plus-gx-wide-now-available/
- Hackaday, Modifying a SNES ROM to be widescreen (SMW): https://hackaday.com/2021/06/22
- Kotaku, Super Mario World widescreen: https://kotaku.com/super-mario-world-is-now-playable-in-widescreen-1847138334
- Hackaday / Boing Boing on wideNES: https://boingboing.net/2018/08/28/widenes-see-outside-the-viewp.html
- ANESE README (wideNES): https://github.com/daniel5151/ANESE
