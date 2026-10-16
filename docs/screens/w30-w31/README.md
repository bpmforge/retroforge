# W30 / W31 screenshots (2026-10-16)

Captured headlessly with `crates/retroforge/tests/w30_screens.rs`
(`#[ignore]`d; ROM-gated). Each pair is the feature **off** then **on**, on
**consecutive frames** of the same paused moment (off at frame k+1, on at
frame k+2), not the identical frame. The Super FX pair is the other way
round (on first, then off) because the HD path needs a few frames to have a
completed GSU job. Frames are the user's own dumps; no ROM data is stored
here beyond these rendered frames.

| File | Game | What it shows |
|---|---|---|
| `hdmode7-4x-off.png` / `-on.png` | F-Zero (USA) | Native 256x224, nearest-upscaled to 1024x896, vs HD Mode 7 at 4x with supersampling. In the "off" image every 4x4 block is one colour; in "on" 59% of the ground blocks carry sub-pixel detail. Near the car the track texture's own texels are big, which is correct magnification. |
| `widescreen-16x9-*.png`, `widescreen-32x9-*.png` | Super Mario World (USA), Yoshi's Island 1 | 4:3 vs 16:9 (400 wide) and 32:9 (800 wide), in a level after walking right and back (re-shot 2026-10-16; the first set was the title-screen demo). Margins from **VRAM** (what the console has loaded, 512 dots wide), captured live with Mario standing still. 16:9 is real level edge to edge. At 32:9 the outer ~144 dots each side are sky and, low down, black, because the console holds no more of the level than 512 dots. The stitched-canvas margins that should fill those are not tour quality for this game yet (offset repeats, HUD stitched in, no sky layer); see the follow-up. |
| `widescreen-16x9-fixed.png` | Super Mario World (USA) | The 16:9 title-screen frame after the colour-math fix (kept as the record of that fix). |
| `lens-off.png` / `-on.png` | Super Mario World (USA), overworld map | Lens and lighting: bloom, tilt-shift, soft shadows, grading at time 0.2, no weather. Subtle at these strengths: 99% of pixels change, by a mean of about 9 levels in 255. |
| `hd2d-look-off.png` / `-on.png` | RF-Scroller (in-repo NES test game) | "HD-2D look" preset in Game-Aware mode. **Substitute:** no A Link to the Past heights are painted yet, and NES Zelda decodes its level only in play (past name registration). The fixture's level is a flat floor, so the 3D view is a thin strip; the follow camera also looks off-centre (worth a look). |
| `superfx-hd-3x-off.png` / `-on.png` | Star Fox (USA), intro | Native (nearest 3x) vs Sharper Super FX 3D at 3x. |
| `superfx-hd-3x-zoom.png` | Star Fox (USA) | A 64x64 native-pixel crop around polygon edges: native at 8x nearest (left) vs the HD picture (right). Slanted edges are smooth; one dark-blue edge shows a sawtooth artifact, and in the full frame one-pixel lines between panels come out dotted. |
| `superfx-gpu-3x-msaa4-zoom.png` | Star Fox (USA), intro | W31-05: the same 64x64 crop at 8x: native (left), CPU 3x (middle), GPU 3x with 4x MSAA (right). The sawtooth is gone and edges are antialiased; loose 1-px plotted lines stay stepped. |
| `superfx-3d-drawing.png` | Stunt Race FX (USA) (Rev 1) | A race with the 3D drawing (fixed by W30-00), native. |
| `contact-sheet.png` | all | Rows top to bottom: HD Mode 7, widescreen 16:9, widescreen 32:9, lens, HD-2D, Super FX HD, then the Super FX zoom with the Stunt Race FX frame. Left = off, right = on. |

## Product tour set (2026-10-16)

Split comparisons: the left half is OFF, the right half is ON, with a white
divider. Use these for the tour.

| File | Game | What it shows |
|---|---|---|
| `compare-superfx-gpu.png` | Star Fox (USA), intro | Native (nearest 4x) vs Sharper Super FX 3D at 4x on the GPU with 4x edge smoothing (MSAA). Both halves are the **same frame**: the native side is the exact frame the HD picture was made from. |
| `superfx-gpu-native.png`, `superfx-gpu-4x-smooth.png` | Star Fox (USA) | The two full frames behind that split. |
| `superfx-gpu-3x-sharp.png`, `superfx-gpu-3x-smooth.png` | Star Fox (USA) | 3x on the GPU without and with edge smoothing (consecutive frames). |
| `superfx-gpu-zoom.png` | Star Fox (USA) | 64x64 native pixels at 8x: native, 3x no smoothing, 3x with 4x smoothing. Edges go from steps to clean antialiased lines. Thin 1-px plotted lines stay stepped, and corners stay on the game's whole-pixel grid (W31-06). |
| `compare-hdmode7-4x.png` | F-Zero (USA) | Native vs HD Mode 7 at 4x (consecutive frames). |
| `compare-lens.png` | Super Mario World (USA) | Lens and lighting off vs on (subtle by design; consecutive frames). |
| `compare-widescreen-16x9.png`, `compare-widescreen-32x9.png` | Super Mario World (USA), Yoshi's Island 1 | 4:3 (top, centred) over widescreen (bottom), a few frames apart. |

Not in the tour yet: **HD-2D** (no game has painted heights yet; the
fixture shot is a flat strip).
