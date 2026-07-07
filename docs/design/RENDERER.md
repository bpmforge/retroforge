# Design: Renderer (`rf-renderer`)

Scope: the wgpu-based presentation stack — original and enhanced pipelines,
shader chains, compare mode, capture, and headless CI rendering. Consumes
`FrameBundle` video (indexed pixels + metadata) and, in Enhanced mode, the
`SceneGraph` defined in ENHANCEMENT_RUNTIME.md §7. Never touches core state.

## 1. Stack and device management

- **wgpu 30** (pin major; budget the quarterly bump — see rust-stack
  research §1). One `Device`/`Queue` shared with egui via `egui-wgpu`; the
  game view is an egui paint callback targeting our own render graph, so
  debug panels and game output composite in one surface without copies.
- Surface config: prefer `Fifo` (vsync) present mode; `Mailbox` optional.
  sRGB swapchain; HDR (`color_space`, wgpu 30) is a stretch config for the
  enhancement pipeline only.
- All passes run on the render thread against triple-buffered `FrameBundle`s
  (ARCHITECTURE §6): the core is never blocked by GPU work; a slow GPU drops
  presentation frames, not emulation frames.

## 2. Original pipeline (Accuracy/Compatibility modes)

```
indexed frame (256×240 / 512×478 w/ line-width tags)
  → R8Uint texture upload (+ per-line width/emphasis metadata buffer)
  → palette pass: LUT texture (console palette × emphasis/greyscale variants) → RGBA8 target
  → scale pass: integer scale + aspect (8:7 PAR for NES/SNES NTSC, user override)
  → optional shader chain (§4)
  → blit to surface (letterboxed)
```

Rules: nearest-neighbor unless a filter pass says otherwise; integer scaling
default-on with fractional fill as a user option; overscan crop (default
224-line NTSC view, full 240 optional) applied at the scale pass. The
palette LUT is data (checked-in `.pal` assets + generator tool), so palette
research doesn't touch shaders.

## 3. Enhanced pipeline

Renders the `SceneGraph` back-to-front into a **virtual canvas** whose size
is decoupled from console output (ultrawide 21:9/32:9, zoomed-out, or
full-map), then applies the camera transform and shader chain.

- Each `SceneLayer` kind has a dedicated sub-renderer:
  - `OriginalFrame` — the §2 output as a texture (also used by compare mode).
  - `ExtractedBg` / `SpriteSet` — instanced quads over tile/sprite atlases
    built from the indexed data + palette groups; `dropped_by_limit` sprites
    render only here (never in §2). Priority order preserved via depth or
    painter's order within the layer.
  - `StitchedCanvas` / `DecodedLevel` — large tiled textures streamed from
    `rf-cache` (chunked 512×512 pages, LRU residency; a full level fits
    comfortably — even 4096×960 NES levels are trivial for any 2016+ GPU).
  - `HudPinned` — re-composited at screen anchor after camera transform.
  - `OverlayCmds` — immediate-mode draw list (lines/rects/text/heatmap
    quads) for plugins/debugger.
- Camera: 2D affine (translate/zoom), animated by the composer (smooth pans
  for room transitions). Sub-pixel camera motion is allowed — layers are
  real textures, so scrolling smoothness is a camera property, decoupled
  from the game's 8px-quantized scroll writes.
- Asset substitution: before atlas upload, tile/sprite hashes probe
  `rf-cache` for enhanced variants (AI packs / artist packs); hit ⇒ bind the
  HD atlas page (integer scale factor per pack, e.g. 4×), miss ⇒ original
  pixels. Fallback is per-asset, per-frame, and free (hash is already
  computed for cache keys).

## 4. Shader chain

- WGSL passes with a common interface: `tex_in, sampler, params: UBO →
  tex_out`. Chain description is data (per-game/user settings): e.g.
  `[crt-easymode, vignette]` or `[xbrz4]` or `[]`.
- Ship first-party: `nearest`, `sharp-bilinear`, `scanlines`,
  `crt-easymode`-class, `xbrz`-class upscaler. Parameters exposed as typed
  UBO fields with UI-generated controls (name/range annotations in a small
  manifest per shader).
- Not a goal: full RetroArch slang-shader compatibility. A converter for
  simple single-pass GLSL presets is a Phase 8 stretch item; document this
  honestly in the UI.

## 5. Compare / side-by-side mode

Both pipelines run in the same frame (original always runs anyway — it's the
§2 path): compare mode presents split-screen (draggable divider) or A/B
blink. Original output also feeds the de-flicker debug diff view
(ENHANCEMENT_RUNTIME §2). Cost is one extra scale pass — negligible.

## 6. Capture points

- Screenshot: post-palette (raw 1×), post-scale, or post-chain (as-seen) —
  user picks; PNG via async readback (`map_async`, never stalls render).
- Video (Phase 8): wgpu readback ring → encoder worker (start with PNG/APNG
  sequence + ffmpeg external; in-process encoder later).
- Golden-frame CI taps the **post-palette 1× buffer** — deterministic,
  scale/shader-independent, byte-hashable.

## 7. Headless mode

`rf-renderer` compiles without a window: device from any adapter (Metal on
the M2 CI host, Lavapipe/llvmpipe fallback in Linux CI), render into an
offscreen texture, read back, SHA-256 hash → `rf-harness` compares against
golden hashes. The palette pass is bit-exact by construction (integer LUT
lookups, no filtering, no sRGB math on the 1× buffer), so golden hashes are
stable across backends; anything after the 1× buffer is *not* hashed in CI
(shader output may differ per driver — validated by eyeball + reference
images with tolerance instead).

## 8. Pacing interaction

Render thread presents at display rate; emulation paces on the audio clock
(ARCHITECTURE §8). 60.0988 Hz (NES) / 60.0988-ish (SNES NTSC) vs 60/120/144
Hz displays: default = audio-driven with occasional dup/drop frame;
optional "sync to display" mode adjusts emulation speed ±0.5% (classic
libretro trick) for perfectly smooth scrolling at the cost of pitch-exact
audio. Frame interpolation for >60 Hz displays is an rf-ai/Phase 8+ concern,
not a renderer primitive.
