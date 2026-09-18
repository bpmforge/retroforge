# Design: Enhancement Wave 16 — Diorama, atmosphere, walls, local AI

Tickets **W16-01..W16-09**. Source: `retroforge-vision-review.html` (three-tier
Diorama plan, PixelRamp precedent, engine prerequisites, 2026-09-17) and
`retroforge-studio-ideas.html` (natural mode, walls pop up, atmosphere
layers, local AI paths A/B, candidate tickets, same date), both commissioned
by Brad; `retroforge-concept-shots.html` renders what each feature looks
like. Brad's go: *"file all of these and lets get to a full plan on doing
all of these. But lets get to doing the work"* (2026-09-17). Execution order
is interleaved with UX_WAVE_15.md in `docs/ROADMAP.md` §"Waves 15-16
execution order".

## 1. Purpose and rulings

This wave turns the enhancement platform's existing per-pixel identity,
per-layer scroll telemetry, and scene graph (ENHANCEMENT_RUNTIME.md §7) into
three visible features — atmosphere layers, a 3D "walls pop up" Diorama, and
local AI upscaling — plus the shared plumbing (benchmark harness, frame-
budget gate) all three need to ship honestly rather than as an unbounded
frame-time regression.

Two decisions from `docs/DECISIONS.md` govern the wave. **D-011**: box art
/ metadata fetch is allowed, opt-in, under NON_GOALS #6 — unrelated to this
wave except as the precedent that "opt-in external data" is not
automatically a NON_GOALS violation; cited since both rulings landed the
same day. **D-012**: local AI is in scope for real-time enhancement and
upscaling on hardware that can hold the frame budget (Apple Neural
Engine/Metal, a capable discrete GPU), hardware-gated at start-up, shader
chain as the fallback everywhere else; the offline Upscale Studio path
(AI_UPSCALING.md, W8-10) stays the universal path. First ticket is a
benchmark spike (W16-01) because no honest 60 fps number for an ESRGAN-
class model at 256x240 exists in the literature — see §7.

**Natural-mode confirmation.** The studio-ideas review asked whether a
"natural mode" needed building; it does not. Accuracy Mode already is it:
cycle-accurate core, enhancement runtime unsubscribed, original render
pipeline, fresh install boots there by CLAUDE.md law 6. Sprite flicker and
slowdown are the real hardware limits reproduced, not a defect.

**Mode-picker wording, routed to W15.** The one change this review found —
label Accuracy in the mode picker with "as on the original console, flicker
and slowdown included" so it doesn't read as a developer setting — is UX
copy, routed to `docs/design/UX_WAVE_15.md` §8 rather than ticketed here.

## 2. The Diorama tiers

Three tiers, unchanged from the vision review, each gating what a badge is
allowed to claim (FR-MODE honesty contract, NON_GOALS #11/#13):

| Tier | Needs | Honesty | Wave-16 coverage |
|---|---|---|---|
| 1, generic | No profile — layer-of-origin, per-layer scroll, palette | One toggle, mode badge, hold-to-peek | Not ticketed this wave (parallax/bloom/focus-band composite is a rf-renderer GPU-pass ticket the vision review flags as "build first" but leaves unfiled; see §11) |
| 2, profile-assisted | A curated profile: depth per plane, collision, entity kinds | Profile-gated, author credited, Game-Aware mode only | W16-03/04 (atmosphere), W16-05/06 (walls), W16-09 (Mode 7 as 3D) |
| 3, hand-authored | Art per title: HD packs, normal maps, hand-built geometry | Distinct mode, never blended into Accuracy, licence/author shown | W16-02 (Upscale Studio output feeds this tier; normal-mapped lit sprites and hand-built geometry are not ticketed) |

**The Accuracy invariant, restated for this wave specifically**: every
pass below (fog, 3D compositor, AI upscale) is an Enhanced/Game-Aware-only
overlay, sits behind the frame-budget gate (§8), and never runs when a
fresh install first boots (CLAUDE.md law 6; NON_GOALS #8's "no AI in the
frame path by default"). A player who never opens a settings menu sees
exactly what the console drew.

## 3. Per-frame data inventory

What exists today (from `crates/rf-core-api/src/video.rs` and the SNES
snapshot), and what this wave needs but the frame bundle does not carry yet.

**Exists:** `PpuPixel { palette_index, layer: PixelLayer, sprite_id,
priority }` per pixel, `PixelLayer` being `Backdrop | Background(u8) |
Sprite` with a core-defined plane index; `SubPixel { palette_index, layer,
op: ColorMathOp, fixed }`, `ColorMathOp` being `None | Add | AddHalf |
Subtract | SubtractHalf`, the SNES colour-math channel W16-03's detector
reads (both `video.rs`). Per-frame scroll writes per layer with landing
scanline, OAM rewrites, mapper IRQs, DMA starts, opt-in and zero cost
unsubscribed. SNES debug snapshot: full VRAM/CGRAM/OAM, the Mode 7 matrix
and offsets (`rf-snes/src/debug.rs::mode7_project`/`mode7_camera_corners`),
HDMA per line, DSP voices — **debug-only and SNES-only today**. HD-pack
tile identity by hash, already masked by real priority. Scene graph
(ENHANCEMENT_RUNTIME.md §7): `SceneLayer` variants including `ExtractedBg`
and `DecodedLevel`, both currently **shapes with no producer**.

**Missing, and gating this wave:** a **geometry scene layer** — no
`SceneLayer` variant carries a depth field or solidity mask; §7's enum has
no such member, W16-06 adds one. A **depth field per layer** — priority
(`PpuPixel::priority`) is an overlap-resolution ordering, not a distance;
nothing says "this plane is farther away." A **generic per-tile nametable
/ tile-id stream** — tile identity exists only in the hash-keyed HD-pack
path, not as a generic per-pixel/per-tile field. The **Mode 7 matrix on the
frame bundle** — real and tested (`debug.rs`), but reachable only through
the debug snapshot API, not the `FrameBundle`/`CoreSink` path renderer and
scene graph run on; W16-09 promotes it.

## 4. Atmosphere layers

**Detector signature (W16-03).** A SNES background plane is an atmosphere
candidate when it shows, together: half or additive colour math
(`ColorMathOp::Add`/`AddHalf` on that plane's `SubPixel`s), slow scroll
independent of the main plane, and low tile variety. Thresholds (scroll
delta ceiling, tile-variety ceiling, minimum colour-math coverage of the
plane's on-screen pixels) are documented in the implementing ticket's code,
not guessed here — W16-03's acceptance requires the thresholds be written
down wherever the detector lives.

**Trust-ladder rollout (D-004, ENHANCEMENT_RUNTIME.md §2a).** Shadow first:
detect and record to the per-game report card, zero render effect. Advisory
next: the Enhance workspace suggests, still no effect. Active: the fog pass
(W16-04) actually renders, and a profile may pin the plane directly (skip
detection, the same mechanism anti-flicker uses today).

**Red fixture.** An RF-Scroller-S variant (or a new dedicated fixture) must
trigger the detector in the test suite and must keep firing — FR-ENH-013's
rule that a heuristic's red fixture going quiet is itself a CI failure.

**`ExtractedBg`'s first producer.** W16-03 is also the first code that
constructs a real `SceneLayer::ExtractedBg { layer: BgLayerId }` — until
this ticket the variant is a shape nothing emits.

**Fog pass design (W16-04).** A WGSL post pass reads the atmosphere layer's
own pixels as a density map — not a synthetic fog volume — and renders
drifting, volumetric-style fog (froxel or screen-space) following the
layer's scroll telemetry. The original plane's mask stays underneath, so
the accurate silhouette is never hidden, only re-rendered. Driving fog
density from an emulator's own background layer is not attested anywhere in
the field per the studio-ideas review, so this ships under the same trust
ladder as any other heuristic-adjacent effect, and the badge names the
effect specifically ("Atmosphere: fog") rather than a generic "Enhanced."

**Two corrections carried from the review, binding on scope.** Super
Metroid's Norfair heat is palette cycling, not a translucent colour-math
plane — the W16-03 detector as specified will not find it, and a second
palette-cycle detector or an explicit profile flag is a separate, later
ticket, not silently covered by W16-03/04. Castlevania IV's rotating room
is Mode 7 geometry, not an atmosphere plane, and belongs with §5/§9
(walls/Mode-7-as-3D), not fog.

A Link to the Past's exact fog registers were not verified from a
technical source in the source review (two community sites failed TLS);
the detector is expected to find that plane empirically in shadow mode,
which is the point of shipping shadow-first rather than hand-tuning per
game.

## 5. Walls pop up

**Collision sources, cited.** The profile schema already has a
`[decode].collision` table (GAME_PROFILES.md §2, e.g. `collision = { table
= 0x2600, bits = "solid,platform,hazard" }`), and `metatile_screens`
already produces a per-metatile solidity byte into `DecodedLevel.collision`
— real, shipped code. Named commercial sources for a room-game collision
profile: A Link to the Past's tile attributes in WRAM bank `$7F`
(cross-checked by the snesrev reimplementation against the real ROM); The
Legend of Zelda's per-column overworld storage and collided-tile variable
(Data Crystal); Super Mario World's Map16 "acts like" table — each a
citable `source =` row under FR-PROF-003, not a transcription (facts-only
policy, CONSTRAINTS §2).

**`room_grid` extension (W16-05).** The `room_grid` family (the Zelda-style
room-game shape; `rf-enhance/src/decode/room_grid.rs`) has no `collision`
field today — `metatile_screens` is the only family that decodes one. This
is a bounded schema extension gated by GAME_PROFILES.md §2's "new decoder
fields are added when ≥2 games need the same shape" rule: the two
collision-sourced profiles this ticket ships (a top-down NES fixture plus
one documented commercial layout, no ROM bytes, per D-009's local-
verification-counts-as-evidence standard) are exactly that pair.

**Geometry layer (W16-06).** A `SceneLayer::Geometry` (or equivalent) with
a depth field and solidity mask, produced from `DecodedLevel.collision` —
the missing scene-graph producer named in §3.

**App-side 3D compositor, and why it's app-side.** `rf-enhance` and
`rf-renderer` are kept apart on purpose (ARCHITECTURE §3/§6, CLAUDE.md law
4): neither crate has an "edge" that owns both game-rule decoding and GPU
compositing. So extrusion, billboards, contact shadows and camera pitch are
new shell code in `crates/retroforge` over wgpu, consuming the geometry and
sprite-set scene layers, not a change inside either crate's boundary.
Concretely: solid tiles from the collision mask extrude into blocks (fixed
height), sprites stand as billboards at their OAM footprint with a
contact-shadow ellipse under each, and the camera pitches at a **fixed**
angle — input and hit-testing stay strictly 2D, matching the concept-shots'
"only the camera pitches" framing.

**NON_GOALS #11, visited-only.** "No generic 'show the whole level'
promise... generic tier shows visited terrain only" binds the geometry
layer exactly as it binds the stitched canvas: only visited/decoded screens
get extruded geometry, never a guessed one — the studio-ideas review is
explicit that a trace-based "visited-and-blocked" fallback, if ever built
generically, must be labelled approximate and is not this ticket.

**3dSen evidence.** The one project that tried to automate exactly this —
tile clusters into 3D geometry with no per-game authoring — gave up after
ten years and shipped hand-made, per-game profiles instead (vision review
§4/§6b). That is why Diorama tier two is profile-gated rather than promised
generically: the field's own attempt at "generic" concluded it isn't one.

## 6. Light rules

**Precedent.** PixelRamp (github.com/Artificial-Age/PixelRamp-SNES-
Remaster-Toolkit; prototype, ~40 stars, Windows-only D3D11, source not yet
published) does palette-aware upscale, bloom, distance light-cast and
per-object sprite rules in real time inside a patched Snes9x, with a
"Remaster Studio" for painting which sprite is a light source. It validates
that palette-aware upscaling plus a per-layer Z plus sprite-keyed light
casting reads as a convincing remaster with **no 3D geometry at all** — a
tier-one-plus-slice-of-tier-two result RetroForge's layer identity already
fits.

**Emitter rules in the entity table.** The proposed shape: a profile-
declared list of emitters keyed by tile hash or entity kind (an extension
of the existing `[entities]` table in GAME_PROFILES.md §2, which already
has `kind` fields), each carrying colour, radius and flicker — the sprite-
shadow idea inverted, sprites emit instead of occlude. Rendered as an
additive pass over the already-composited layers, the same colour-math
family (`ColorMathOp::Add`) the hardware itself uses for glow, composing
with §4's atmosphere pass rather than fighting it.

**Not ticketed this wave.** No W16 ticket implements light rules; §11
records it as a future-wave candidate.

## 7. Local AI

**Path A — Upscale Studio, offline (W16-01, W16-02).** Already designed in
`docs/design/AI_UPSCALING.md` (W8-10) and partly built: `rf-ai`'s pipeline
groups extracted tiles into animation sheets (§3 of that doc — one
`Upscaler::upscale` call per sheet so a walk cycle stays coherent), and
writes a Mesen-compatible HD pack keyed by tile hash. What W16-02 adds:

- A **model fetcher with a licence ledger** (script, hash-pinned, never
  vendored — same posture as the ROM/vector fetchers; a fetched weights
  file is an *artifact* under the `tests/rom-manifest.toml` precedent, e.g.
  `NoLicenseGrantFetchOnly`, NFR-011). OpenRAIL-class weights are refused
  outright — Real-ESRGAN (BSD-3) is the base; community pixel-art weights
  (OpenModelDB entries) must each be checked individually; Stable-Diffusion
  restyling is out on two counts, an OpenRAIL licence and palette drift
  between tiles with no coherence mechanism.
- **`ort` load-dynamic**, per AI_UPSCALING.md §5: `default-features =
  false, features = ["load-dynamic"]`, the runtime `dlopen`'d from a
  caller-supplied path, no build-time download, no vendored blob;
  `ort::init_from`, not the lazy default path (`expect()`s and aborts on a
  missing dylib).
- A **review screen** (Upscale Studio window): lists captured tiles,
  previews original vs. upscaled, approve/reject/replace each before the
  pack is written — the surface AI_UPSCALING.md §6 left undone on purpose
  ("`BuiltPack` returns images in memory... so a review step can sit
  between building and installing").
- Output is a **Mesen-compatible pack**, one format for AI packs and
  hand-authored artist packs (ENHANCEMENT_RUNTIME.md §6).

**Path B — real-time pass, hardware-gated (W16-07, W16-08).** Sits after
the compositor, strictly downstream of core state — determinism tests are
unchanged because nothing here can affect what the core computed
(ARCHITECTURE §3). Costs exactly one extra frame of latency; this is
disclosed in the status bar next to the mode badge, not buried in a
settings page. Enabled only when start-up hardware detection clears the
gate threshold (§8); everywhere else the option shows disabled with the
reason, and a fresh install always boots with the pass off (CLAUDE.md law
6, NON_GOALS #8).

Runtimes, ranked by risk (lowest first): **MetalFX** via `objc2-metal-fx`
(W16-08) — first-party, already used by a Bevy plugin, lowest risk on
Apple Silicon; spatial mode is the primary target, temporal is attempted
and either ships or its blocker is recorded. **burn-wgpu** — a neural model
on the same wgpu stack the emulator already uses, no second GPU API.
**`ort`/CoreML or CUDA** — a second GPU API alongside wgpu, higher
integration cost but the same runtime Path A already uses. Anime4K's wgpu
port (MIT) is a hand-tuned shader family, not a neural model — it belongs
with the cheap first-party shaders (RENDERER.md §4) and needs no gate.

**The missing benchmark, and why W16-01 is first.** No apples-to-apples
number exists for a 256x240 or 512x448 frame at 60 fps on Apple Silicon or
a consumer NVIDIA card; the nearest public data point is a desktop RTX 4090
at roughly 12-17 ms for a Real-ESRGAN-class model on a 480p frame — most of
a 16.7 ms budget, on hardware well above what most players own. So D-012's
gate threshold is set from a real measurement (W16-01), not a guess: the
harness times the shader chain, the compositor, and a stub neural pass at
256x240 and 512x448 on the developer's own machine, records p50/p95 as a
JSON evidence row the way `local-gate.sh` does, and runs a real ESRGAN-
class model through `ort` (CoreML EP, CPU EP as control) to get one
concrete data point. Docs then say plainly which models and hardware pass
today — no aspirational claim ahead of the measurement.

**Vendor SDKs deferred.** DLSS and FSR 4 have no Rust or wgpu bindings —
either would be a native-binding project of its own, out of scope here.

## 8. Frame-budget gate and benchmark harness

W16-01 builds the one piece of infrastructure §4, §5's compositor and §7's
Path B all sit behind: a benchmark harness (any GPU pass, timed per frame
at 256x240 and 512x448, JSON evidence row) and the resulting gate — a pass
is eligible for real time only under a measured p95 threshold. The fog
pass (W16-04), the real-time AI pass (W16-07) and MetalFX (W16-08) each
disable themselves when p95 exceeds that threshold and record the disable
on the report card; none invents its own threshold. The 3D compositor
(W16-06) has no `depends_on` on W16-01, but shares the same harness for its
own by-eye/perf check rather than inventing a second timing mechanism
(CLAUDE.md law 8, `docs/LESSONS.md` RF-L-11).

## 9. Mode 7 as 3D

W16-09 promotes the Mode 7 matrix and offsets from the SNES debug snapshot
(`rf-snes/src/debug.rs`) to the frame bundle as an optional cross-console
field, then has the same §5 3D compositor render the plane as a textured
ground at the hardware's own transform, camera pitched — bsnes-hd style, at
higher internal resolution. Deliberately the same compositor as §5 with a
different input (a hardware-computed transform instead of a decoded
collision grid), not a second 3D pipeline. Castlevania IV's rotating room
and a Mode 7 racing/flight title (Super Mario Kart already reaches its
title screen after DSP-1 HLE, D-010) are the by-eye targets.

## 10. Ticket map

| Ticket | Depends on | What |
|---|---|---|
| W16-01 | — | GPU benchmark harness; ESRGAN spike via `ort`; MetalFX timed if it builds; sets the frame-budget threshold |
| W16-02 | W16-01 | Upscale Studio: fetcher + licence ledger, `rf-ai`/`ort` pipeline, review screen, Mesen pack output |
| W16-03 | — | Atmosphere-layer detector, shadow mode, red fixture, `ExtractedBg` producer |
| W16-04 | W16-03 | Fog/steam WGSL pass driven by the atmosphere layer, behind the frame-budget gate |
| W16-05 | — | `room_grid` collision field; two collision-sourced profiles; solidity mask |
| W16-06 | W16-05 | Geometry scene layer + app-side 3D compositor: extrusion, billboards, shadows, fixed pitch |
| W16-07 | W16-01 | Real-time AI upscale pass, hardware-gated, one-frame latency disclosed |
| W16-08 | W16-01 | MetalFX spatial (and attempted temporal) path on Apple Silicon |
| W16-09 | W16-06 | Mode 7 matrix promoted to the frame bundle; compositor renders it as pitched 3D ground |

Execution order (interleaved with W15, `docs/ROADMAP.md` §"Waves 15-16
execution order", quick wins and foundations first): **W16-01 → W16-03 →
W16-02 → W16-04 → W16-05 → W16-06 → (W16-07, W16-08) → W16-09.**

## 11. Open questions

- **Showcase game.** Tier one looks best on real multi-plane parallax
  (Super Mario World, Kirby, Castlevania IV, Contra III); no title chosen.
  Not blocking any ticket, but the by-eye checks in W16-04/06/09 need one
  named.
- **Sixth preset vs. feature set.** Should Diorama be a mode preset of its
  own, or a feature set inside Enhanced/Game-Aware? Five presets exist
  today; unresolved. Each W16 ticket routes around it by gating on
  "Game-Aware mode only" rather than a named Diorama preset.
- **SA-1 / Super FX.** The coprocessor ruling that decides whether Yoshi's
  Island, Super Mario RPG, Star Fox and Kirby Super Star are ever
  reachable for this wave's showcase — tracked in compatibility work, named
  here because it gates which games can demonstrate tier two.
- **W16-09's fixture is not what its acceptance criteria imply.**
  `profiles/snes/example-mode7` is a schema-demonstration profile with an
  all-zero `[[identity]]`; its `[decode]` section (once `kind =
  "room_grid"`) was **removed** in W9-08 because nothing configured it and
  no Mode 7 fixture backs it (`docs/LESSONS.md` RF-L-11). There is
  currently **no Mode 7 render fixture ROM** anywhere in the repo — Mode 7
  coverage today is unit tests over `mode7_project`/`mode7_camera_corners`
  with synthetic matrices, not a bootable golden. W16-09 needs a real
  fixture built first (a small first-party Mode 7 test ROM, the RF-Scroller
  pattern) or its acceptance criterion corrected before the ticket is
  claimed — stated plainly here rather than left for the executor.
