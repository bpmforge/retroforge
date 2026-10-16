# Next plans (written 2026-10-16, coding paused for the token budget)

Everything needed to resume without re-discovery. Tickets are in
`plan.json` (W31-07, W31-08, W32-01..06).

## 1. State of main

- `main` at the merge after 0caf96f, pushed to origin (Gitea) and github.
- Last full green run: **2839 passing** (74e2fa4, `cargo test --workspace`).
  Since then: two timing tests hardened (`menu_pauses`, `toasts_and_modals`,
  pass alone) and docs only.
- **Open:** the stop hook reported a failing `cargo test` on main at about
  16:10Z; the diagnosing run was stopped before it finished, so the failing
  test is **not identified**. The machine was at load 30+ from an external
  28-34 GB VM. First step next session: when the VM is idle run
  `cargo test --workspace --no-fail-fast > log` once and read the
  `FAILED` lines. Suspects, most likely first: (1) another wall-clock test
  starved by load (same pattern as the two already hardened; look for fixed
  sleeps or `0..N` step loops checking `frame_count_for_test`); (2) the
  W31-05 GPU tests competing for the Metal device with other binaries;
  (3) a real regression from today's job-carry or still-camera fixes (both
  had full green runs after them, so least likely).
- Post-merge `scripts/local-gate.sh` was not run today (load); run it once
  the suite is green.

## 2. W31-07: per-layer widescreen canvas (parked)

**Why:** the 32:9 stitched canvas in Super Mario World repeats terrain at a
wrong offset, paints HUD pieces into the world and loses the sky
(`target/screens-probe/widescreen-32x9-canvas.png`). The canvas stitches
the composited frame at one layer's scroll, but SMW's hills/sky layer
scrolls at a different rate and the status bar is its own layer
(docs/research/W31-07-widescreen-survey.md).

**Branch:** `feat/W31-07-layer-canvas` (d0e5fd2, pushed, not merged,
unbuilt). In: new `crates/retroforge/src/layer_canvas.rs` (475 lines) and
the start of the core_thread wiring. Not done: the margin compose in
`app.rs::widen_margins`, tests, verification.

**Design (the brief the work was started from):**
1. One canvas + one wrap-aware scroll accumulator per BG layer the
   widescreen policy widens (`req.policies.decide_all(stepper.bg_layer_views())`,
   `Decision::widens()`). Layer n's canvas takes only pixels whose
   `PpuPixel.layer == Background(n)`, placed by layer n's own
   `hofs/vofs` (mod tilemap size, like `snes_scroll_for_stitcher`).
   Covered pixels are holes and never erase known cells. Non-widened
   (HUD) layers get no canvas. One shared scene id.
2. Ship them with the existing canvas snapshot (`CoreEvent::CanvasSnapshot`,
   throttle near core_thread.rs:1520) plus each layer's frame position on
   `FrameMsg` beside `canvas_frame`.
3. Margin pixel = front-most layer with a known cell (fixed BG1 > BG2 > BG3 >
   BG4 order is acceptable if documented; better: the order seen in the 4:3
   picture), else the row's backdrop colour (palette index 0), not black.
   HUD rows unchanged. Old composite canvas stays for NES and the map view
   (app.rs ~4648).
4. Tests: two layers at different scroll rates land at their own offsets;
   holes do not erase; HUD layer not stitched; backdrop fallback; existing
   canvas tests green.
5. Verify: `RF_SCREENS_ONLY=widescreen RF_SMW_RUN_FRAMES=240
   RF_SMW_BACK_FRAMES=120 cargo test --release -p retroforge --test
   w30_screens -- --ignored --nocapture`, then look at
   `target/screens-probe/widescreen-32x9-canvas.png`. If it is good, switch
   the tour shot from forced VRAM back to the canvas (w30_screens.rs,
   `force_vram_margins_for_test`).

**Cheap extras from bsnes-hd's options** (same ticket or a small follow-on):
backdrop fill colour (auto / colour / black), a marker for the widened area
(lines / darken, good for the tour), sprite mode (clip at the 4:3 edge vs
draw), per-layer on/off.

## 3. W31-08: "another level of HD, with antialiasing" (Brad, 2026-10-16)

What each HD path does today and what the next level is:

| Path | Today | Remaining jaggies | Next level |
|---|---|---|---|
| Sharper Super FX 3D (W31-03/05) | GPU triangles at 2-8x, MSAA 4x (8x falls back to 4x on this Metal adapter), ~2.8 ms/frame at 3x release | 1-px plotted panel lines stay stepped; corners on the whole-pixel grid; a 1-native-pixel right-edge bias approximation | (a) draw degenerate polygons / PLOT runs as coverage-AA lines (analytic line coverage in the fragment shader) instead of 1-px quads; (b) sub-pixel corners from the camera-space records (W31-06, research first); (c) optional supersample on top of MSAA (render 2x the target, box-down) where 8x MSAA is missing |
| HD Mode 7 (W30-02) | 4x with supersampling | distant-ground shimmer, texel aliasing near the horizon | more samples per pixel near the horizon (distance-weighted), mip/anisotropic-style filtering of the Mode 7 texture, measured per frame |
| 2D layers and sprites | nearest / integer scale | staircase edges on diagonals and curves | opt-in pixel-art-aware edge smoothing per layer (HUD layers excluded), from published algorithm descriptions only (xBR / ScaleFX family docs, papers); per-layer so text and HUD stay crisp |
| Everything | - | temporal crawl | later: temporal AA is a poor fit (GSU draws at ~15-20 fps, sprites flicker); keep spatial |

Acceptance for every row: zoom-crop proof (native | current | new) like
`superfx-gpu-zoom.png`, release-build ms per frame, Accuracy Mode
untouched, 1x paths byte-identical where they are today.

Order: (a) Super FX AA lines (smallest, visible in the tour shot) -> HD
Mode 7 horizon -> W31-06 research -> 2D smoothing.

## 4. W32: game patching (future state)

Full design: `docs/design/GAME_PATCHING.md`. Order: W32-01 soft-patching
(IPS/BPS/UPS, opt-in, separate saves) -> W32-03 bsnes-hd widescreen
convention (needs W31-07) -> W32-02 inspector -> W32-04/05 authoring
assistant -> W32-06 homebrew loop.

## 5. Things noticed, not ticketed

- Frame-stepping while paused used to flip widescreen margins between
  canvas and VRAM; fixed by the still-camera fix (74e2fa4) for the same
  reason. Worth one check in W31-07's verification.
- HD-2D follow camera looked off-centre in the fixture shot
  (docs/screens/w30-w31/hd2d-look-on.png).
- Tour shots still missing: HD-2D (needs a game with painted heights), a
  good 32:9 (needs W31-07).
