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

**Integer scaling vs. 8:7 PAR, resolved (ticket W3-01b):** these two rules
pull against each other — 8:7 is not an integer ratio, so a single scale
factor applied to both axes cannot be simultaneously integer *and* land on
exactly 8:7. Two resolutions were on the table: (a) integer-scale both axes
by the same whole number, then apply a second, separate horizontal-only
stretch to correct the aspect; or (b) integer-lock only the vertical axis
and let the horizontal axis carry the (non-integer) PAR correction
directly, in the same pass. **Chosen: (b).** The vertical axis is the one
[`Overscan`](../../crates/rf-renderer/src/scale.rs) crops (224 vs. 240
lines), so keeping it integer keeps every cropped source scanline landing
on an exact whole number of output rows with no seam; the horizontal axis
was already going into non-integer territory the moment 8:7 entered the
picture, so there is nothing extra lost by computing it directly as
`y_scale * 8/7` rather than integer-scaling it first and immediately
undoing that with a second stretch. (a) and (b) produce the *same final
output size* either way (`out_width` is `source_width * y_scale * 8/7`
under both) — (a)'s second pass is pure overhead for an identical result,
so (b) is strictly simpler: one pass, one shader, one shared rounding rule
between the GPU pass and its CPU reference oracle (§7). Implemented in
`rf-renderer::scale` (`ScaleGeometry`, `FillMode::IntegerLocked`,
`ParRatio`); the 8:7 "user override" is exposed as a `ParRatio` the caller
supplies (`ParRatio::SQUARE` for square pixels, or any other ratio) — a
settings-UI control for it is app-shell (`crates/retroforge`) scope, not
built by this ticket.

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
  `crt-easymode`-class, `lcd-grid` (handheld/LCD look), `xbr`-class
  upscaler. Parameters exposed as typed UBO fields with UI-generated
  controls (name/range annotations in a small manifest per shader).
- **Shader licensing law (design review G-42):** the upscaler is based on
  Hyllian's **xBR (MIT)** — never xBRZ (Zenju, GPL-3.0) or libretro GPL
  ports. sharp-bilinear and lcd3x are public domain and may be ported
  directly. The CRT and LCD-grid looks are **behavior-spec clean-room**:
  whoever writes the WGSL must not have the GPL/unlicensed libretro
  sources open; UI labels say "-class"/"-style", never claiming to *be*
  the libretro shaders. Every shipped shader records its provenance in
  its manifest.
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
a local macOS dev host, **OpenGL via llvmpipe** in Linux CI), render into an
offscreen texture, read back, SHA-256 hash → `rf-harness` compares against
golden hashes. The palette pass is bit-exact by construction (integer LUT
lookups, no filtering, no sRGB math on the 1× buffer), so golden hashes are
*derived* to be stable across backends by that construction — not yet
independently observed on more than one backend (this doc/ticket's own
implementation was verified on Metal only; CI's first green run on llvmpipe
is what turns "should be identical" into "is identical," and a red run
there is a cross-backend question to investigate, not necessarily a code
regression). Anything after the 1× buffer is *not* hashed in CI (shader
output may differ per driver — validated by eyeball + reference images with
tolerance instead).

**The tolerance mechanism (ticket W3-01b):** `rf-harness::tolerance`
(`compare_with_tolerance`, `ToleranceConfig`, `DEFAULT_TOLERANCE`) is that
"reference images with tolerance" instrument. The "reference image" side is
a CPU oracle (`rf_renderer::scale::render_scaled_reference`) computed by
the same formula, at the same `f32` precision, as `shaders/scale.wgsl` —
not a checked-in binary asset — the same "independent oracle, not a
codified GPU output" shape §7's palette-pass golden hash already pairs
with `palette_index_to_rgb`. The metric is two numbers applied together: a
per-pixel `channel_delta` (how far any one channel may drift and still
count as "matching" — kept at `0` for a nearest-neighbor pass, since a
correct sample is either exactly right or a different texel entirely, not
"slightly off") and a `max_mismatch_fraction` (how many pixels, as a
fraction of the frame, are allowed to fail that check at all — this is the
number that actually absorbs cross-backend float noise, and is also kept
at `0.0`: on this ticket's own Metal run the GPU scale pass and its CPU
oracle came back byte-identical, 0 of 65,632 pixels, so there was no
observed noise to size a nonzero allowance against — see
`crates/rf-harness/tests/scale_pass_tolerance.rs` for the measured numbers
and its four calibration mutations, each of which must fail this exact
threshold). On tolerance-check failure the test dumps both buffers as
`.ppm` files (`rf-harness::tolerance::dump_ppm` — no image-codec dependency
needed to write one) for the "eyeball" half of this section's own name.

**Backend choice, decided (ticket W3-01, 2026-08-07):** `.github/workflows/ci.yml`
sets `WGPU_BACKEND=gl` + `LIBGL_ALWAYS_SOFTWARE=1` — OpenGL via **llvmpipe**,
not Vulkan via Lavapipe (an earlier draft of this section named "Lavapipe/
llvmpipe" as if interchangeable; they are different backends, Vulkan vs
OpenGL, on the same underlying software rasterizer project). llvmpipe is
chosen because it is what `ubuntu-latest`'s stock Mesa provides without
extra `apt` packages, and because the original pipeline's palette pass (the
only thing CI golden-hashes) needs nothing Vulkan-only: it is a plain
render pipeline (fullscreen-triangle vertex stage, `textureLoad` fragment
stage against an `R8Uint` sampled texture) with no compute-shader
requirement, so GL 3.3-class software rendering is sufficient. Lavapipe
remains a documented fallback option if a future pass needs a Vulkan-only
feature, but is not the CI default.

## 8. Pacing interaction

Render thread presents at display rate; emulation paces on the audio clock
(ARCHITECTURE §8). 60.0988 Hz (NES) / 60.0988-ish (SNES NTSC) vs 60/120/144
Hz displays: default = audio-driven with occasional dup/drop frame;
optional "sync to display" mode adjusts emulation speed ±0.5% (classic
libretro trick) for perfectly smooth scrolling at the cost of pitch-exact
audio. Frame interpolation for >60 Hz displays is an rf-ai/Phase 8+ concern,
not a renderer primitive.
