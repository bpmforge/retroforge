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

### 3a. Diorama pass — `SceneLayer::Geometry` (ticket W16-06)

A standalone pass (`rf_renderer::diorama::DioramaPass`), not a shader-chain
stage — like the fog pass (§4's common interface is one input texture;
this one needs a ground texture, a sprite-cutout texture, and a real
vertex buffer). Consumes `SceneLayer::Geometry` (produced in `rf-enhance`
from `DecodedLevel.collision` via `solidity_mask`/`geometry_layer`) plus
the sprite layer, resolved by the app shell (`crates/retroforge/src/
enhanced_view.rs::compose_diorama`) exactly like every other cross-crate
layer (§3's own "shell-mediated" split).

- **Mesh**: `rf_renderer::diorama_mesh::build_vertices` is a pure CPU
  function — a textured ground plane, solid tiles extruded into boxes
  (textured top face, flat-shaded side faces), sprites as upright
  billboards at their OAM/entity-table footprint with a soft procedural
  contact shadow underneath. Emitted **already sorted back-to-front**
  (farthest tile row first) because this pass, like every other one in
  this crate, has no depth attachment — painter's algorithm, not
  `DepthStencilState`.
- **Camera**: fixed pitch (52°) and fixed vertical FOV, framing the whole
  tile grid; view/projection matrices are a 64-byte UBO
  (`shaders/diorama.wgsl`'s `Camera`), built by hand-rolled column-major
  4x4 math (`rf_renderer::diorama::camera_matrices` — this crate has no
  matrix-library dependency). Only the camera pitches; input and
  hit-testing stay strictly 2D and are untouched by this pass.
- **Sprite source**: the already-extracted sprite-only layer
  (`rf_renderer::layers::LayeredFrame::sprite_rgba`, ticket W3-03's
  existing pipeline) — the simplest correct source per the ticket brief,
  over building a second per-sprite atlas.
- **Budget gate**: reuses `rf_renderer::fog::BudgetGate`/`DISABLE_P95_MS`
  directly (§8) rather than inventing a second threshold.
- **Honesty**: Diorama tier two (§2 of `docs/design/
  ENHANCEMENT_WAVE_16.md`) is Game-Aware-only, gated on a profile whose
  decode actually produced a collision mask; the badge names it
  ("Diorama: walls") only when effective.

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
- **Shipped as of ticket W3-02a**, completing §4's first-party list:
  `crt` (CRT-class), `lcd-grid` (LCD-grid-style), `xbr` (xBR-class). Each
  carries a `ShaderManifest` with `license`, `basis` and `authorship`, and
  each WGSL file opens with a PROVENANCE block deriving every term it
  computes — gaussian beam profile + aperture mask + gamma round-trip for
  the CRT; cell-gap geometry + subpixel stripes for the LCD; a single
  diagonal-edge comparison with a luma-weighted distance for the upscaler.
  The upscaler's `basis` names Hyllian's xBR (MIT) as the permitted family
  and explicitly rules out **xBRZ (Zenju, GPL-3.0)** and **libretro GPL
  ports**; it is not a transcription of Hyllian's either, and is
  deliberately simpler (one 2×2 comparison, where real xBR uses a larger
  neighbourhood and a multi-level rule table) so the "-class" label is
  accurate rather than modest.
- **Authorship caveat, recorded rather than buried (W3-02a, 2026-08-17).**
  These three were written by an LLM which may have been trained on
  GPL-licensed shader sources, so G-42's literal test — "must not have the
  sources open" — does not mean the same thing for such an author as for a
  human. Rather than assert a test that cannot be verified, each file
  *derives* its arithmetic from stated reasoning a reviewer can check line
  by line, and each manifest says so. A human licence review knows exactly
  what to audit. `cargo deny` cannot see inside a `.wgsl`, so the parts of
  the law that CAN be mechanised are asserted in
  `every_shader_manifest_records_its_provenance`.
- **Tolerance, calibrated at last (W3-02a).** Measured on Metal against
  the CPU oracles: CRT and LCD-grid both show a **1-LSB** max channel
  delta (from `pow` and `smoothstep` evaluating differently on GPU and
  CPU), so they earn `channel_delta: 1` — this crate's first nonzero
  threshold, and with it **zero** pixels mismatch, so
  `max_mismatch_fraction` stays 0.0. **xBR came back byte-exact** (its
  arithmetic is comparisons and one `mix(_, _, 0.5)`, no transcendental)
  and therefore keeps `channel_delta: 0`: giving it an unearned allowance
  is the vacuity W3-01b refused when it shipped nearest at 0. Each
  threshold is mutation-verified through the real GPU pass — rendering a
  *different* shader in its place is rejected with max channel deltas of
  131, 64 and 252 respectively.
- Not a goal: full RetroArch slang-shader compatibility. A converter for
  simple single-pass GLSL presets is a Phase 8 stretch item; document this
  honestly in the UI.

## 5. Compare / side-by-side mode

Both pipelines run in the same frame (original always runs anyway — it's the
§2 path): compare mode presents split-screen (draggable divider) or A/B
blink. Original output also feeds the de-flicker debug diff view
(ENHANCEMENT_RUNTIME §2). Cost is one extra scale pass — negligible.

**Implemented in ticket W3-04.** `rf_renderer::compare` holds both
presentations as pure functions over two RGBA buffers — `compose_split`
and `blink_shows_original` — with no `egui` and no GPU, the same shape
`retroforge::enhanced_view` uses: the branching is what has bugs, so the
branching is what is tested headlessly and the UI only paints the result.

**"Synced frame" (FR-REND-005) is structural here, not maintained.** The
two halves are the *same* `FrameMsg`/`FrameBundle` pair: the enhanced half
is what the shell displays (`FrameBuffer::overlay_scanline` paints the
enhancement over the accuracy frame), and the original half is
`FrameBundle::video`, documented as "always the accuracy-exact stream:
assembled from `video_scanline` only, never `overlay_scanline`". One
frame, two renderings, identical geometry — so there is no scaling policy
to invent and no second fetch that could land a frame apart. If the two
geometries ever disagree the pair is dropped rather than composed
misaligned.

**The split composes a buffer rather than clipping two draws**, which a UI
toolkit would find more natural. Composing is what lets the compare view
answer FR-FE-005: what is on screen and what a capture of it would contain
come from one function and cannot disagree. It also makes the divider
testable by reading pixels instead of by driving a UI.

**Cost is gated**, following W3-03a: the two buffers are cloned only while
compare mode is on, or for the single frame a screenshot is pending.
Re-adding an unconditional per-frame clone for a view that is off by
default would have undone that ticket a day later.

## 6. Capture points

- Screenshot: post-palette (raw 1×), post-scale, or post-chain (as-seen) —
  user picks; PNG via async readback (`map_async`, never stalls render).
- **PNG encoding is dependency-free (ticket W3-04, `rf_renderer::png`).**
  This workspace has no image codec and TECH_STACK has no row for one, so
  rather than quietly widen the dependency graph to write a screenshot,
  PNG is encoded directly: zlib permits **stored** (uncompressed) deflate
  blocks (RFC 1951 §3.2.4), so a conformant file needs only CRC-32 and
  Adler-32, both implemented from their specifications. **The honest
  trade:** files are roughly raw-RGBA sized (~245 KB for a 256×240 frame
  rather than the ~10-40 KB a real encoder manages) — fine for a
  screenshot a user takes deliberately, wrong for anything per-frame,
  which is why §6's Phase-8 video capture must NOT be built on it. If
  screenshots ever need to be small, that is the moment to add a real
  codec *and* its TECH_STACK row. Verified against three independent
  decoders (`file`, macOS `sips`, Python `zlib`), not only its own
  round-trip test.
- Video (Phase 8): wgpu readback ring → encoder worker (start with PNG/APNG
  sequence + ffmpeg external; in-process encoder later).
- Golden-frame CI taps the **post-palette 1× buffer** — deterministic,
  scale/shader-independent, byte-hashable.

## 6a. Failure fallback (FR-REND-007, ticket W3-01a)

`rf_renderer::fallback` implements "renderer failures shall fall back to
the original pipeline, never abort emulation", and the design turns on one
fact about wgpu: **validation errors are reported out-of-band.** A shader
that fails to compile does not return `Err` from `create_shader_module` —
it reaches the device's uncaptured-error handler, whose default behaviour
is to panic the process. So `guarded()` wraps GPU work in a
`wgpu::ErrorScope` and converts a captured validation error into an `Err`.
Without that seam FR-REND-007 is unimplementable, because the failure it
names never becomes a value you can branch on.

Three outcomes, and each says what actually happened rather than
collapsing to a boolean:

| `RenderPath` | When | Frame drawn? |
|---|---|---|
| `Enhanced` | palette → scale → chain all ran | yes |
| `OriginalFallback` | the shader chain failed | yes, unshaded |
| `RecoveredOnOriginal` | the device was lost and **recreated** | yes, unshaded |
| `Failed` | device lost and could not be recreated | no — caller keeps emulating |

`FallbackRenderer` adds the two things a real caller needs: device
**recreation** (loss is recoverable — driver reset, GPU switch, suspend —
so "no picture, forever" would meet the letter of "never abort emulation"
and none of its intent), and a **sticky shader-disable** (a chain that
failed to compile will not start compiling; retrying it 60 times a second
pays the failure cost forever).

**On not hanging.** W3-01 stalled at a 600-second watchdog twice, both
times here. This module awaits exactly one future, and it is already
resolved when created — verified against the vendored wgpu 29.0.4 source,
not assumed: `backend/wgpu_core.rs`'s `pop_error_scope` ends
`Box::pin(ready(scope.error))`. `Device::set_device_lost_callback` is
deliberately **not used anywhere in this crate**; it is the one API whose
contract permits never firing.

**On the tests being real.** Device loss is injected with
`Device::destroy()`, and shader failure with WGSL that genuinely does not
compile, fed through `try_enable_shader` — the same production path a
user-selected shader (W3-02a) will arrive by. Nothing is compiled out and
no GPU is stubbed: each test first asserts the *enhanced* path works on
the same objects, so a fallback that was always taken would fail. Measured
on Metal: fallback in ~50-120 µs against a 1-second budget.

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
