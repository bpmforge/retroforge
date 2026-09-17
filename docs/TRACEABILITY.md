# RetroForge — Spec Traceability Matrix

Date: 2026-07-06. Audits the repository's Phase-0 document set against the
founding specification (the user's original two-part brief). Every concrete
requirement in the spec is a row. Status: **COVERED** (designed + traceable
to a doc §/SRS id/ticket), **PARTIAL** (addressed but incomplete or thin),
**MISSING** (no trace). Strictness: "mentioned in passing" = PARTIAL.
Gap register at the end lists every non-COVERED row with a proposed fix.

Abbrev: ARCH=docs/ARCHITECTURE.md, EC=design/EMULATION_CORES.md,
ER=design/ENHANCEMENT_RUNTIME.md, GP=design/GAME_PROFILES.md,
PL=design/PLUGINS.md, SS=design/SAVE_STATES.md, RD=design/RENDERER.md,
DB=design/DEBUGGER.md, SRS/TEST/ROAD/MVP/SCOPE/NG=docs/<name>.md.

## 1. Project goals (14/14 covered)

| Goal | Where | Status |
|---|---|---|
| Working NES core | SRS FR-CORE-020..027, EC §2, W1/W2 | COVERED |
| Working SNES core | SRS FR-CORE-030..037, EC §3, W6/W7 | COVERED |
| Both cores under one frontend | ARCH §3, FR-FE-004 | COVERED |
| Preserve original behavior by default | FR-MODE-003, NG #13, CLAUDE.md law 6 | COVERED |
| Optional enhancement mode | FR-MODE-001/003 | COVERED |
| Per-game enhancement profiles | GP, FR-PROF-* | COVERED |
| Game-specific data extraction modules | ER §4 decoder families, FR-ENH-005 | COVERED |
| GPU-accelerated rendering | RD, FR-REND-001 | COVERED |
| Widescreen + ultrawide display modes | FR-REND-004, RD §3, NG #12 (honest limits) | COVERED |
| Reduce/eliminate sprite flicker | ER §2, FR-ENH-001/002 | COVERED |
| Smoother scrolling where possible | RD §3 sub-pixel camera, RD §8 sync-to-display, SCOPE P8 smooth-camera | COVERED |
| Full-level/extended-map visualization | ER §3/§4, FR-ENH-005/006 | COVERED |
| Overlays + future AI features | FR-REND-006, FR-AI-*, ER §6 | COVERED |
| Plugin architecture, evolvable without core rewrites | PL, FR-PLUG-*, one-way layering ARCH §3 | COVERED |

## 2. Architecture layers (spec's 11-layer + 15-layer lists)

| Layer | Crate / doc | Status |
|---|---|---|
| App shell / Frontend-UI | `retroforge` bin, FR-FE | COVERED |
| Emulator Core API / HAL | `rf-core-api`, ARCH §5 | COVERED |
| NES core / SNES core | `rf-nes`, `rf-snes` | COVERED |
| Cartridge & mapper | `rf-cart` + mapper traits in cores (EC §2.4/§3.5/§4) | COVERED |
| Audio system / AV timing | `rf-audio`, ARCH §8 pacing, RD §8 | COVERED |
| Renderer | `rf-renderer` | COVERED |
| Enhancement runtime | `rf-enhance` | COVERED |
| Game profile runtime | `rf-profiles` | COVERED |
| Plugin/mod runtime | `rf-plugin-sdk` | COVERED |
| Debugger/inspector | `rf-debugger` | COVERED |
| AI pipeline | `rf-ai` (contract in ER §6) | COVERED |
| Asset cache | `rf-cache` | COVERED |
| Save/replay | `rf-state`, SS | COVERED |
| Test harness | `rf-harness`, TEST §2 | COVERED |

## 3. NES requirements (18/18)

| Req | Where | Status |
|---|---|---|
| 6502 CPU (incl. unofficial ops) | EC §2.1, FR-CORE-020/021, W1-01 | COVERED |
| PPU | EC §2.2, FR-CORE-022/023, W1-04/05 | COVERED |
| APU | EC §2.3, FR-CORE-024, W2-01 | COVERED |
| Controller input | EC §2.5, FR-FE-003, W1-07 | COVERED |
| Cartridge loading; iNES + NES 2.0 | FR-CORE-010, EC §4, W0-02 | COVERED |
| NROM, MMC1, UxROM, CNROM, MMC3 first | FR-CORE-025, EC §2.4 (coverage table), W2-02/03 | COVERED |
| Save RAM | FR-CORE-012, W2-04 | COVERED |
| Save states | FR-STATE-001..007, SS | COVERED |
| Frame stepping | FR-DBG-004, DB §1 | COVERED |
| CPU/PPU/APU debugging | DB §1-§3 | COVERED |
| Pattern-table viewer | FR-DBG-001, DB §3 | COVERED |
| Nametable viewer | FR-DBG-001, DB §3 | COVERED |
| Palette viewer | FR-DBG-001, DB §3 | COVERED |
| OAM/sprite viewer | FR-DBG-001, DB §3 (limit-occupancy bar) | COVERED |
| Mapper debugging | DB §1 (mapper breakpoints) + §2/§3 event viewer | COVERED |
| Deterministic replay | FR-STATE-006, SS §3 | COVERED |

## 4. SNES requirements (18/18)

| Req | Where | Status |
|---|---|---|
| Ricoh 5A22 CPU | EC §3.1, FR-CORE-030, W6-01/02 | COVERED |
| PPU1/PPU2 model | EC §3.3, FR-CORE-033 | COVERED |
| S-SMP/S-DSP audio | EC §3.4, FR-CORE-031/036 | COVERED |
| DMA + HDMA | EC §3.2, FR-CORE-032 | COVERED |
| VRAM, CGRAM, OAM | EC §3.3, DB §3 SNES columns | COVERED |
| BG modes 0-6 | EC §3.3, FR-CORE-033 | COVERED |
| Mode 7 | EC §3.3 (fixed-point matrix), FR-CORE-034 | COVERED |
| Windowing / mosaic / color math | EC §3.3 | COVERED |
| Interrupts (NMI, H/V IRQ) | EC §3.1 | COVERED |
| LoROM + HiROM | EC §3.5, FR-CORE-035 | COVERED |
| DSP-1 (HLE) | EC §3.5, FR-CORE-038, D-010, W14-18/W14-19 | PARTIAL |
| Battery saves | FR-CORE-012, EC §3.5 SRAM maps | COVERED |
| Save states | FR-STATE-*, TEST §6 | COVERED |
| Debug visualizers (SNES-specific) | DB §3 (Mode 7 view, CGRAM, HDMA lanes) | COVERED |
| CPU/APU/PPU tracing | DB §2 (bsnes-convention format) | COVERED |
| Common games first, then expand | ROAD P7 exit, EC §3 NTSC-first | COVERED |
| Enhancement chips: honest deferral (DSP-1 lifted 2026-09-17, HLE) | FR-CORE-035, FR-CORE-038, NG #10, D-010, EC §3.5 refusal diagnostic | COVERED |

## 5. Rendering — original + enhanced pipelines (16 + honesty)

| Req | Where | Status |
|---|---|---|
| Exact original frames, res/timing/palette/clipping | RD §2, FR-REND-002 | COVERED |
| Preserve flicker + sprite limits in Accuracy | EC §2.2, MVP checklist ("Accuracy still flickers") | COVERED |
| GPU acceleration | RD §1 (wgpu 30) | COVERED |
| Integer scaling / clean pixel scaling | RD §2, FR-REND-002 | COVERED |
| CRT shaders | RD §4 | COVERED |
| LCD shaders | RD §4 ships crt/scanlines/xbrz/sharp-bilinear — no lcd-grid pass named | PARTIAL |
| Widescreen + ultrawide layouts | RD §3, FR-REND-004 | COVERED |
| Optional background extension | ROAD P8 (bsnes-hd per-layer policies), ARCH §2 honesty row | COVERED |
| Optional sprite de-flicker | ER §2 | COVERED |
| Optional HUD separation | ER §4/§7, FR-ENH-007 | COVERED |
| Optional tilemap reconstruction | ER §3 stitcher | COVERED |
| Full-level map rendering (profile-gated) | ER §4, FR-ENH-005 | COVERED |
| Layered rendering (BG/sprites/HUD/overlays separate) | ER §7 SceneGraph, RD §3 | COVERED |
| Future AI upscaling + sprite replacement | RD §3 asset substitution, FR-AI | COVERED |
| Renderer abstraction → Vulkan/Metal/DX/WebGPU | RD §1 (wgpu covers all four) | COVERED |
| Honesty: full level ≠ generic | ARCH §2 table, NG #11/12 | COVERED |

## 6. Full-level / top-down examples (spec's two worked examples)

| Req | Where | Status |
|---|---|---|
| Side-scroller: locate level data, metatiles, screens, collision, objects, palettes, tile banks | GP §2 (`decode`, `collision`, `entities`, `chr_bank_reg`), ER §4 steps 1-3 | COVERED |
| Reconstruct scene graph; render wide/full view; sim runs normally; player+actives drawn over | ER §4 steps 4-5, FR-ENH-006, MVP checklist | COVERED |
| Optional future/offscreen objects w/ honesty | GP `offscreen_valid`, ER §4 spawn-point rule | COVERED |
| Top-down: rooms, maps, stitching, expanded view, smooth transitions | ER §4 top-down variant, `room_grid` family | COVERED |

## 7. Loading-screen strategies (5/5 + guard)

| Strategy | Where | Status |
|---|---|---|
| 1 Fast-load (safe, known periods) | ER §5.1, GP `[loading.wait_loops]`, FR-ENH-008 | COVERED |
| 2 Predecode caches | ER §5.2, rf-cache | COVERED |
| 3 Game-profile preload | ER §5.2/§4 | COVERED |
| 4 Seamless transitions (render-side) | ER §5.3 | COVERED |
| 5 Native-enhanced (explicit mods only) | ER §5.4, FR-ENH-009 | COVERED |
| Never remove blindly | ER §5.1 (generic detection = debug tool only) | COVERED |

## 8. Anti-flicker (9/9)

| Req | Where | Status |
|---|---|---|
| Original eval in Accuracy | EC §2.2 (buggy overflow incl.) | COVERED |
| Optional sprite-limit bypass | ER §2.1, FR-ENH-001, W3-05 | COVERED |
| Priority preservation | ER §2.1 + §7 composer rules | COVERED |
| Temporal reconstruction | ER §2.2, FR-ENH-002 | COVERED |
| OAM analysis / flicker detection | ER §2 SpriteHistorian | COVERED |
| Per-game configuration | GP `[antiflicker]` | COVERED |
| Safety vs intentionally hidden sprites | ER §2 safety a-c, RISKS R-03 | COVERED |
| Debug compare visualization | FR-DBG-006, RD §5 | COVERED |
| Test cases incl. intentional blink | TEST §7 | COVERED |

## 9. Memory/CPU/GPU expansion rules (honesty model)

| Req | Where | Status |
|---|---|---|
| No pretend-more-RAM-for-games; emulator-side resources only | ARCH §1/§2, CONSTRAINTS §1 | COVERED |
| Larger render targets / decoded caches / precomputed maps / AI caches / metadata stores / object DBs / plugin memory | ARCH §7 rf-cache row, RD §3, DB §4, GP | COVERED |
| Enhancement state in enhanced save states | SS ENHC chunk, FR-STATE-007 | COVERED |
| Original memory map unless explicit patch | ER §5.4, FR-ENH-009, NG #13 | COVERED |
| Deterministic baseline loop; parallel host work list (post-proc, audio, shaders, AI, decode, stitching, compression, replay, traces, ROM analysis, precache) | ARCH §6 worker list + single core thread | COVERED |
| Never parallelize core CPU/PPU/APU timing | ARCH §6, EC §1 | COVERED |

## 10. GPU architecture checklist (15)

Original frame ✓ (RD §2) · tile-based rendering ✓ (RD §3 atlases) · layer
extraction ✓ (W3-03) · sprite extraction ✓ · render-to-texture ✓ · shader
chains ✓ · upscaling filters ✓ · CRT ✓ / **LCD PARTIAL (see §5)** · AI
upscaling hooks ✓ (asset substitution) · widescreen ✓ · ultrawide ✓ · debug
overlays ✓ · object outlines ✓ (OverlayCmds; hitbox example E7-S1) ·
collision overlays ✓ (collision decode + heatmap quads) · heatmaps ✓ ·
full-level visualization ✓. — 14 COVERED, 1 PARTIAL.

## 11. AI enhancement features (spec's 14-item list)

| Feature | Where | Status |
|---|---|---|
| Real-time sprite/background upscaling | Deliberately re-scoped: offline pack gen + runtime cached substitution (NG #8, ER §6, RD §3) — spec's "must be optional/cached" satisfied | COVERED (honest deviation) |
| Sprite-sheet reconstruction | ER §6 v1 targets | COVERED |
| Group sprites into animation sets | Not explicit anywhere | MISSING |
| Identify repeated assets | Hash-based identity ER §6, RD §3 | COVERED |
| Offline/async generation, cache by rom+asset+model+settings | FR-AI-001/002 | COVERED |
| Object-aware enhancement | Only implicitly via profile entity knowledge | PARTIAL |
| Frame interpolation | RD §8 names it (rf-ai, Phase 8+), no design | PARTIAL |
| HUD detection / text-box enhancement | ER §6 "later" | COVERED (future-listed) |
| AI replacement art packs (user-reviewable) | FR-AI-004, SCOPE AI row | COVERED |
| AI widescreen background extension | ER §6 outpainting (profile-gated, labeled approximate) | COVERED |
| AI-assisted ROM RE / symbol labeling / level-data identification | Not designed; debugger annotation flow is manual | MISSING |
| AI translation overlays | ER §6, SCOPE | COVERED |
| AI accessibility overlays | ER §6, SCOPE | COVERED |
| Local-first, no cloud required, external opt-in | FR-AI-003, NG #6 | COVERED |

## 12. Game profile fields (spec's ~20-field list)

ROM hash ✓ (multi-hash identity) · title ✓ · region ✓ · revision ✓ ·
console ✓ · **mapper/chip requirements — PARTIAL (no schema field; rf-cart
detects, but profile can't assert)** · memory addresses ✓ (`memory_map`) ·
ROM offsets ✓ (`rom_map`) · tile decode rules ✓ · level decode rules ✓ ·
object decode rules ✓ (`entities`) · palette rules ✓ · camera rules ✓ · HUD
layout ✓ · widescreen behavior ✓ · anti-flicker settings ✓ · compat flags ✓
(`capabilities`) · script hooks ✓ (`[plugins]`) · debug annotations ✓
(labels/notes/sources per entry + DB §4 export) · user mod overrides ✓
(`[mods]` + `profiles.d`) · data-driven TOML ✓ · complex games via plugins ✓
(`custom` decoder → plugin). — 21 COVERED, 1 PARTIAL.

## 13. Plugin capabilities (spec's 15-item list)

Inspect CPU memory ✓ · PPU state ✓ · VRAM/OAM/CGRAM ✓ · frame events ✓ ·
scanline events ✓ (explicit perf-gated cap) · read ROM ✓ (`RomView`) ·
decode assets ✓ · add overlays ✓ · replace rendered layers ✓
(`replace_layers`) · replace sprites ✓ (Scene layers/packs) · add debug
panels ✓ (`panel()`) · **export maps — PARTIAL (plugin filesystem cap is
`cache_dir` only; no user-dir export path)** · record traces ✓ · **add
custom controls — MISSING (no input/hotkey capability in PL §2)** · patch
memory in mod mode ✓ (ledgered) · no silent behavior change ✓. Sandboxing ✓
(tiers: in-process gated now, wasmtime later — honest about it, PL §1).
— 13 COVERED, 1 PARTIAL, 1 MISSING.

## 14. Frontend features (spec's 21-item list)

ROM library ✓ (FR-FE-001) · per-game settings ✓ (FR-FE-002) · save states ✓
· rewind eventually ✓ (FR-STATE-008, P8) · screenshot ✓ (FR-FE-005) · video
recording eventually ✓ (FR-FE-007, P3/P8) · input remapping ✓ (FR-FE-003) ·
controller ✓ · keyboard ✓ · shader selection ✓ (E4-S1) · display scaling ✓ ·
accuracy/enhancement toggles ✓ (FR-MODE-001) · debug mode ✓ · tile viewer ✓
· sprite viewer ✓ · memory viewer ✓ (FR-DBG-002) · trace viewer ✓ (DB §3) ·
profile editor ✓ (FR-FE-004 panel; full editor P9) · plugin manager ✓
(FR-FE-004) · enhancement comparison view ✓ (FR-REND-005) · side-by-side ✓.
— 21/21 COVERED at requirements level. **Note: a dedicated UI/UX design doc
(layouts, workflows, panel specs) did not exist at audit time; it now does —
docs/design/FRONTEND_UI.md (screens, wireframes, UX principles, phasing).**

## 15. Developer tooling (spec's 17-item list)

ROM inspection ✓ (FR-DBG-007) · hashing ✓ · header parsing ✓ · mapper
inspection ✓ (rom inspect + event viewer) · CPU trace ✓ · PPU trace ✓ ·
frame capture ✓ (RD §6) · VRAM dumps ✓ · OAM dumps ✓ · palette dumps ✓ ·
tilemap export ✓ · sprite-sheet export ✓ (SCOPE tooling row) · level-data
exploration ✓ (decode preview, E6-S2) · **pattern matching — PARTIAL (hex
viewer goto/find only; no ROM-wide byte-pattern search tool)** · watchpoints
✓ · breakpoints ✓ · Lua/WASM scripting ✓ · full-level map export ✓
(FR-DBG-007). — 16 COVERED, 1 PARTIAL.

## 16. Testing requirements

| Req | Where | Status |
|---|---|---|
| NES: CPU instr, PPU timing, mapper, known test ROMs, golden frames, audio, roundtrip, determinism | TEST §4 table + §6 | COVERED |
| SNES: CPU, DMA/HDMA, PPU modes, APU, golden, Mode 7, roundtrip, determinism | TEST §5 + §6 | COVERED |
| SNES: cartridge-mapping tests | Implied by gilyon/boot gates; no explicit LoROM/HiROM mirror-map fixture row | PARTIAL |
| Enhancement: original unchanged / separately tested / toggleable / side-by-side / versioned profiles / state versioning | TEST §6-§7, FR-MODE-002, FR-ENH-010, NFR-008 | COVERED |
| Phase exit gates | TEST §8, ROAD per-phase | COVERED |

## 17. Deliverables 1-20 and design-doc sections 1-25

All 20 first-message deliverables trace: architecture (ARCH), stack
(TECH_STACK), repo structure (workspace + ARCH §7), module-by-module (ARCH
§7 + design/*), core APIs (ARCH §5), schemas (GP §2), roadmap (ROAD), MVP
(MVP), stretch (MVP §4), risks (RISKS), testing (TEST), debug tools (DB),
renderer (RD), enhancement engine (ER), plugins (PL), save states (SS),
full-level example (ER §4 + GP example), anti-flicker example (ER §2), AI
integration example (ER §6 + RD §3), pseudocode both loops (ARCH §8).
All 25 second-message sections trace likewise (exec summary ARCH §1;
non-goals NG; text diagram ARCH §3; first-10 tickets → plan.json's 39;
example profile GP §2; example plugin interface PL §3). Two thin spots:

| Item | Status | Note |
|---|---|---|
| §18 Performance strategy | PARTIAL | Real content exists but distributed (NFR-002/003, TEST §9, DB §6, ARCH §6, RD §8) — no single section to point at |
| Deliverable 6 "enhancement metadata" schemas | PARTIAL | Profile schema + plugin manifest + cache keys done; replacement-pack manifest format deferred to P8 without a schema sketch |

## 18. MVP bullets (second message) — 9/9 COVERED

Run basic NES ROM · render frames · accept input · save states · debug
viewers · load enhancement profile · render overlay · one enhanced-rendering
demo on PD/homebrew ROM · code scaffolding plan (workspace scaffolded,
plan.json, PLAYBOOK commands) — all in MVP.md §2-§3 + repo state.

## 19. Technical constraints (spec) — 8/8 COVERED

Deterministic emulation (CONSTRAINTS §1) · baseline independent of
enhancement (mode invariant) · opt-in (FR-MODE-003) · no hardcoded
copyrighted names/assets (NG #16, FR-PROF-006) · hash-identified
user-provided ROMs (rf-cart) · clean-room docs (CONSTRAINTS §2, GP
`sources` required) · no ROM distribution (NG #5) · maintainability/
testability (CONSTRAINTS §3, R-13 machinery).

## 20. Gap register

Totals: **~200 rows audited — 190 COVERED · 8 PARTIAL · 2 MISSING.**

| # | Gap | Status | Proposed fix |
|---|---|---|---|
| G1 | LCD-grid shader absent from first-party shader list | PARTIAL | Add `lcd-grid` pass to RD §4 list + W3-02 acceptance |
| G2 | AI sprite→animation-set grouping not designed | MISSING | Add to ER §6 capture step + FR-AI row (pack-builder groups by OAM adjacency/animation cadence) |
| G3 | AI-assisted RE (symbol labeling, level-data identification) absent | MISSING | Add "assisted-annotation" future item to ER §6 + DB §4 (suggest labels from access patterns); P9+, keep manual flow primary |
| G4 | Object-aware enhancement only implicit | PARTIAL | One FR-AI row: enhancement may key on profile entity kinds (per-object pack variants) |
| G5 | Frame interpolation named but undesigned | PARTIAL | Acceptable for P8+; add one paragraph to RD §8 or an rf-ai job sketch when scheduled |
| G6 | Profile schema lacks mapper/chip requirement field | PARTIAL | Add optional `meta.mapper`/`meta.chips` assertion validated against rf-cart detection (schema v0.1) |
| G7 | Plugin capability for custom controls/hotkeys missing | PARTIAL→add | Add `input_bindings` capability to PL §2 (register hotkeys/virtual buttons, remap-visible) |
| G8 | Plugin map/data export limited to `cache_dir` | PARTIAL | Add host-mediated `export` API (user-picked dir) to PL §3 instead of widening fs caps |
| G9 | SNES cartridge-mapping test fixture not an explicit TEST §5 row | PARTIAL | Add row: libSFX-built LoROM/HiROM mirror-map fixture, golden RAM block |
| G10 | Performance strategy distributed; pack-manifest schema deferred | PARTIAL | Optional: 1-page docs/PERFORMANCE.md index; add pack-format sketch to SCOPE asset row or a P8 design stub |

None of the gaps touch MVP scope or the architecture's load-bearing
decisions; G1/G6/G7/G8/G9 are one-line-to-one-section edits, G2/G3/G4/G5/G10
are future-phase design additions.

## 21. Gap resolution — 2026-07-06 (same day)

All 10 gaps addressed: G1 lcd-grid added (RENDERER §4 + W3-02 acceptance) ·
G2 animation-set grouping designed (ENHANCEMENT_RUNTIME §6) · G3 AI-assisted
RE added as assist-only P9+ item (ER §6) · G4 object-aware per-entity pack
variants (ER §6) · G5 frame-interpolation experiment slotted into W8-01 with
design-first note · G6 `[meta.requires]` mapper/chips added to profile schema
(GAME_PROFILES §2) · G7 `input_bindings` capability added (PLUGINS §2) · G8
host-mediated `export` API noted (PLUGINS §2) · G9 LoROM/HiROM mirror-map
fixture row added (TESTING §5) · G10 docs/PERFORMANCE.md index created +
pack-manifest folded into W8-01. Frontend UI design gap closed by
docs/design/FRONTEND_UI.md. Result: 0 MISSING, 0 unaddressed PARTIAL.
