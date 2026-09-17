# RetroForge — Software Requirements Specification (SRS)

Status: Phase 2 baseline · 2026-07-06
Traces: `docs/ARCHITECTURE.md` (§ refs), `docs/design/*`, `plan.json` tickets.
Priorities: **MVP** (must exist to prove the architecture), **P1** (Phase 1-4),
**P2** (Phase 5-7), **P3** (Phase 8-9 / future).
Verification methods: **TR** = test ROM via rf-harness, **CI** = automated CI
check, **UT** = unit test, **GF** = golden-frame compare, **M** = manual.

Requirement convention: "shall" = binding; each row is testable in isolation.

## 1. FR-CORE — Emulation cores

### 1.1 Shared core contract

| ID | Requirement | Pri | Verify | Crate |
|---|---|---|---|---|
| FR-CORE-001 | Each core shall implement the `EmulatorCore` trait (load, reset, run_frame, step, save_state, load_state, state_view, config) per ARCHITECTURE §5. | MVP | UT | rf-core-api |
| FR-CORE-002 | Cores shall be deterministic: identical state + identical `InputFrame` sequence shall produce identical state (full state hash) on every frame. | MVP | CI | rf-nes, rf-snes |
| FR-CORE-003 | Cores shall contain no wall-clock reads, host RNG, or thread-dependent behavior. | MVP | CI (lint + review) | rf-nes, rf-snes |
| FR-CORE-004 | Cores shall emit video as indexed pixels + source metadata (palette index, layer, sprite id, priority, `dropped_by_limit` flag) via `CoreSink`, never pre-composed RGB. | MVP | UT | rf-core-api |
| FR-CORE-005 | Cores shall support cycle-granular `step` (instruction, scanline, frame) for the debugger. | MVP | UT | rf-nes, rf-snes |
| FR-CORE-006 | Event emission shall be subscription-masked so an unsubscribed core pays no per-event allocation cost. | P1 | UT + bench | rf-core-api |

### 1.2 Cartridge loading (rf-cart)

| ID | Requirement | Pri | Verify | Crate |
|---|---|---|---|---|
| FR-CORE-010 | The loader shall parse iNES and NES 2.0 headers, and SNES LoROM/HiROM headers (with copier-header stripping when `size % 8192 == 512`). | MVP(NES)/P2(SNES) | UT | rf-cart |
| FR-CORE-011 | The loader shall compute CRC32/MD5/SHA-1/SHA-256 over the normalized (header-stripped) image, RA/No-Intro compatible. | MVP | UT | rf-cart |
| FR-CORE-012 | Battery-backed SRAM shall persist to a sidecar file keyed by normalized hash and load on next run. | P1 | CI roundtrip | rf-cart |
| FR-CORE-013 | Unknown mapper/chip shall fail with a diagnostic naming the mapper number/chip, never a crash. | P1 | UT | rf-cart |

### 1.3 NES core (rf-nes)

| ID | Requirement | Pri | Verify | Crate |
|---|---|---|---|---|
| FR-CORE-020 | The 2A03 CPU shall pass SingleStepTests `nes6502` JSON vectors including cycle-by-cycle bus activity, for all official opcodes (MVP) and stable illegal opcodes (P1). | MVP/P1 | UT | rf-nes |
| FR-CORE-021 | The CPU shall match the golden `nestest.log` trace byte-exactly (PC/A/X/Y/P/SP/CYC) from $C000 start. | MVP | CI | rf-nes |
| FR-CORE-022 | The PPU shall render per-dot with correct frame timing (odd/even frame skipped dot, pre-render line) and pass blargg `ppu_vbl_nmi` (all 10 sub-ROMs). | P1 | TR | rf-nes |
| FR-CORE-023 | Sprite evaluation shall replicate hardware (8-sprite/scanline limit, sprite-0 hit, buggy overflow flag), passing blargg sprite_hit and sprite_overflow suites. | P1 | TR | rf-nes |
| FR-CORE-024 | The APU shall pass blargg `apu_test` and `dmc_dma_during_read4`; mixer shall implement the non-linear formulas (NESdev APU Mixer). | P1 | TR + audio RMS | rf-nes |
| FR-CORE-025 | Mappers NROM(0), MMC1(1), UxROM(2), CNROM(3), MMC3(4) shall be supported — ≈91.5% of the licensed NA library — plus **AxROM(7)**, taking coverage to ≈93% (amended 2026-08-06 at Brad's request, ticket W2-17, after a real mapper-7 title was refused; the original five remain the MVP/P1 commitment). MMC3 shall pass `mmc3_test_2` IRQ suites. | MVP(NROM)/P1(rest) | TR | rf-nes |
| FR-CORE-026 | The core shall run RF-Scroller (in-repo fixture, W2-10) and Alter Ego (freeware, fetch-only — D-008) without visual or logic faults for a scripted 5-minute input log. | P1 | GF replay | rf-nes |
| FR-CORE-027 | AxROM(7) and Action 53(28) support shall extend coverage to ≈96%. | P2 | TR | rf-nes |

### 1.4 SNES core (rf-snes)

| ID | Requirement | Pri | Verify | Crate |
|---|---|---|---|---|
| FR-CORE-030 | The 5A22 (65C816) shall pass SingleStepTests `65816` JSON vectors, then gilyon/snes-tests `cputest` on-ROM. | P2 | UT + TR | rf-snes |
| FR-CORE-031 | The S-SMP (SPC700) shall pass SingleStepTests `spc700` vectors and boot the IPL ROM handshake. | P2 | UT + TR | rf-snes |
| FR-CORE-032 | DMA and HDMA shall pass undisbeliever DMA/HDMA edge-case ROMs (golden-frame). | P2 | GF | rf-snes |
| FR-CORE-033 | The PPU shall implement BG modes 0-7, windowing, mosaic, and color math, verified per-feature against PeterLemon/SNES PPU ROMs. | P2 | GF | rf-snes |
| FR-CORE-034 | Mode 7 (incl. HDMA-driven perspective) shall pass PeterLemon Mode 7 ROMs. | P2 | GF | rf-snes |
| FR-CORE-035 | LoROM and HiROM mapping shall be supported; DSP-1 is emulated per FR-CORE-038; all other enhancement chips (SA-1, Super FX, DSP-2/3/4, Cx4, S-DD1, SPC7110, ST01x, ExHiROM) remain explicitly deferred and shall produce the FR-CORE-013 diagnostic. | P2 | UT | rf-snes |
| FR-CORE-036 | The S-DSP shall produce audio passing SPC timing suites; BRR decoding shall be sample-exact. | P2 | TR + audio RMS | rf-snes |
| FR-CORE-037 | The core shall run RF-Scroller-S (in-repo SNES fixture, via W6-00) without faults for a scripted 5-minute input log. | P2 | GF replay | rf-snes |
| FR-CORE-038 | The DSP-1 coprocessor (NEC uPD7725-based DSP-1/DSP-1B, ~13 titles incl. Super Mario Kart and Pilotwings) shall be emulated at command level (HLE, snes9x-style) — the documented ~30-command set with its fixed-point Mode-7/vector/rotation math — over the real DR/SR bus protocol; verified by UT against vectors from the published command documentation, never from a ROM. DSP-2/3/4 share the coprocessor-nibble header bit and are not distinguishable from DSP-1 by header alone, so they are identified in the profile/rom-manifest layer (W14-19) and recorded as known-wrong rather than run through the DSP-1 HLE. | P2 | UT | rf-snes |

## 2. FR-MODE — Operating modes

| ID | Requirement | Pri | Verify | Crate |
|---|---|---|---|---|
| FR-MODE-001 | The application shall expose exactly five mode presets: Accuracy, Compatibility, Enhanced, Research/Debug, Game-Aware (ARCHITECTURE §4). | MVP | M | retroforge |
| FR-MODE-002 | **Mode invariant**: for identical ROM + input log, Accuracy and Enhanced modes shall produce identical per-frame core state hashes. Enhancement shall be observation-only. | MVP | CI | rf-enhance |
| FR-MODE-003 | Enhancement features shall be opt-in; a fresh install shall boot in Accuracy-equivalent defaults. | MVP | UT | retroforge |
| FR-MODE-004 | Compatibility-mode fast paths shall be individually documented and covered by the compatibility test list; any fast path that fails a test ROM shall be demoted from the preset. | P1 | CI | rf-nes, rf-snes |
| FR-MODE-005 | Game-Aware mode shall activate only when a profile matches the normalized hash; absence of a profile shall degrade to Enhanced with generic features only. | P1 | UT | rf-profiles |

## 3. FR-REND — Rendering

| ID | Requirement | Pri | Verify | Crate |
|---|---|---|---|---|
| FR-REND-001 | The renderer shall run on wgpu with original and enhanced pipelines selectable per frame. | MVP | M + GF | rf-renderer |
| FR-REND-002 | The original pipeline shall convert indexed frames to RGB via console palette LUTs and scale with integer scaling and aspect-correct modes. | MVP | GF | rf-renderer |
| FR-REND-003 | The renderer shall support a shader chain (none, clean scalers, CRT) applied post-composition. | P1 | M | rf-renderer |
| FR-REND-004 | The enhanced pipeline shall composite `SceneGraph` layers back-to-front (ENHANCEMENT_RUNTIME §7) into arbitrarily sized render targets including ultrawide aspect ratios. | P1 | GF | rf-renderer |
| FR-REND-005 | Side-by-side compare (original | enhanced, synced frame) shall be available whenever the enhanced pipeline is active. | P1 | M | rf-renderer |
| FR-REND-006 | Debug overlays (plugin/debugger `DrawCmd`s) shall render topmost in both pipelines. | MVP | M | rf-renderer |
| FR-REND-007 | Renderer failures (device loss, shader compile error) shall fall back to the original pipeline, never abort emulation. | P1 | UT | rf-renderer |

## 4. FR-ENH — Enhancement runtime

| ID | Requirement | Pri | Verify | Crate |
|---|---|---|---|---|
| FR-ENH-001 | Sprite-limit bypass shall render `dropped_by_limit` sprites while core-side OAM evaluation (sprite-0 hit, overflow flag) remains hardware-exact. | P1 | TR + GF | rf-enhance |
| FR-ENH-002 | Temporal de-flicker shall reconstruct sprites present in ≥k of the last N OAM snapshots, with scene-change invalidation and per-game exclusions (ENHANCEMENT_RUNTIME §2). | P1 | GF cases | rf-enhance |
| FR-ENH-003 | The generic map stitcher shall accumulate visited background regions per scene (wideNES technique) with per-scanline scroll-band handling and persist canvases via rf-cache. | P1 | UT + GF | rf-enhance |
| FR-ENH-004 | Stitched-canvas views shall visually distinguish unvisited area (fog); the UI shall never imply unvisited geometry is known. | P1 | M | rf-enhance |
| FR-ENH-005 | Profile-driven decoders (`metatile_screens`, `room_grid`, `tilemap_direct`) shall reconstruct full levels as pure functions of ROM bytes, cached by (rom, level, decoder settings). | P2 | UT + GF | rf-enhance |
| FR-ENH-006 | Enhanced camera modes (ultrawide crop, zoom-out, full-map) shall track profile-declared camera/player addresses and draw live sprites over reconstructed scenes. | P2 | GF | rf-enhance |
| FR-ENH-007 | HUD separation shall activate only from profile rules or a verified split heuristic, pinning HUD regions during enhanced camera movement. | P2 | GF | rf-enhance |
| FR-ENH-008 | Loading fast-forward shall trigger only on profile-declared wait loops (PC + condition), disabling frame pacing without altering simulation. | P2 | CI | rf-enhance |
| FR-ENH-009 | Game-logic patches (mods) shall be off by default, individually user-enabled, ledger-logged, and recorded in save states. | P2 | UT | rf-enhance |
| FR-ENH-010 | Every enhancement feature shall be independently toggleable at runtime without restart. | P1 | M | rf-enhance |
| FR-ENH-011 | Every enhancement heuristic shall implement the trust ladder (D-004): states shadow (detect + record, never act) → advisory (badge suggests) → active; fresh install all-shadow; per-game state persisted by normalized hash; profiles may pin states. | P1 | UT | rf-enhance |
| FR-ENH-012 | Heuristic contradiction events (safety auto-re-enable, scene-cut resets, blink-period violations) shall be recorded to a local per-game report card surfaced in the Enhance workspace; suppressing a safety heuristic per-game shall require a stored justification that auto-reopens when the trigger recurs in a new scene context. | P1 | UT + M | rf-enhance |
| FR-ENH-013 | Every shipped heuristic shall have a red fixture (RF-Scroller scene or dedicated ROM) that MUST trigger it; a heuristic change that stops firing on its red fixture shall fail CI. | P1 | CI | rf-harness |

## 5. FR-PROF — Game profiles

| ID | Requirement | Pri | Verify | Crate |
|---|---|---|---|---|
| FR-PROF-001 | Profiles shall be TOML conforming to schema v0 (GAME_PROFILES §2), validated by `retroforge-tool profile validate` in CI. | MVP | CI | rf-profiles |
| FR-PROF-002 | Profile matching shall use normalized-hash identity with per-revision overrides; mismatched revisions shall not partially apply. | MVP | UT | rf-profiles |
| FR-PROF-003 | Every `memory_map`/`rom_map` entry shall carry a `source` citation (clean-room provenance); validation shall fail without one. | P1 | CI | rf-profiles |
| FR-PROF-004 | The loader shall reject newer schema majors and warn on unknown keys. | P1 | UT | rf-profiles |
| FR-PROF-005 | User overrides (`~/.retroforge/profiles.d/`) shall layer over shipped profiles with precedence visible in the profile inspector. | P2 | M | rf-profiles |
| FR-PROF-006 | Profiles for commercial games shall contain only facts (addresses, rules) — CI shall reject binary assets in `/profiles` except under homebrew titles with license files. | P1 | CI | repo |
| FR-PROF-007 | Community-submitted profiles/packs/plugins shall pass license-gated intake (D-005): SPDX license + provenance metadata required; missing/unknown licenses and denylisted content (NC assets, GFDL text, GPL-derived shader code) rejected; nothing activates without passing. | P3 | CI | repo |

## 6. FR-PLUG — Plugins and scripting

| ID | Requirement | Pri | Verify | Crate |
|---|---|---|---|---|
| FR-PLUG-001 | The plugin host shall enforce manifest-declared capabilities (PLUGINS §2); undeclared API calls shall error at the boundary. | P1 | UT | rf-plugin-sdk |
| FR-PLUG-002 | Lua scripting (mlua) shall expose read-memory, frame/event callbacks, and overlay drawing with BizHawk-style naming. | P1 | UT + M | rf-plugin-sdk |
| FR-PLUG-003 | `write_memory` capability shall require per-plugin user enablement and route through the visible write ledger. | P1 | UT | rf-plugin-sdk |
| FR-PLUG-004 | Script errors and plugin panics shall be contained per-callback (auto-pause/disable), never aborting emulation. | P1 | UT | rf-plugin-sdk |
| FR-PLUG-005 | Per-frame plugin time budgets shall throttle over-budget plugins with a UI warning. | P2 | UT | rf-plugin-sdk |
| FR-PLUG-006 | A WASM (wasmtime component) host shall provide sandboxed plugin execution once the API stabilizes. | P3 | UT | rf-plugin-sdk |

## 7. FR-DBG — Debugger and tooling

| ID | Requirement | Pri | Verify | Crate |
|---|---|---|---|---|
| FR-DBG-001 | Viewers: pattern table, nametable/tilemap, palette, OAM/sprite (NES MVP; SNES adds Mode 7 and CGRAM viewers); event viewer (frame timeline) and audio channel scopes at P1 (DEBUGGER §3). | MVP/P1/P2 | M | rf-debugger |
| FR-DBG-002 | Memory viewer with live edit (Research mode only), watchpoints (read/write/exec), and breakpoints driving core `step`. | MVP(view)/P1(watch) | UT | rf-debugger |
| FR-DBG-003 | CPU trace logging in nestest format (NES) and analogous 65C816/SPC700 formats, with ring-buffer capture and export. | MVP | CI | rf-debugger |
| FR-DBG-004 | Frame stepping and run-to-scanline from the UI. | MVP | M | rf-debugger |
| FR-DBG-005 | Annotation store (address → label/type/notes/source) exporting directly into profile `memory_map`/`rom_map` skeletons. | P1 | UT | rf-debugger |
| FR-DBG-006 | Anti-flicker diff view: original vs reconstructed OAM per frame. | P1 | M | rf-debugger |
| FR-DBG-007 | Developer CLI (`retroforge-tool`): rom hash/inspect, header parse, trace capture, VRAM/OAM/palette dump, tilemap export, profile validate, level/map export for supported profiles. | P1 | CI | tools |

## 8. FR-STATE — Save states and replay

| ID | Requirement | Pri | Verify | Crate |
|---|---|---|---|---|
| FR-STATE-001 | Save states shall use the chunked TLV `.rfstate` container (SAVE_STATES §2), taken at frame boundaries only. | MVP | UT | rf-state |
| FR-STATE-002 | State roundtrip (save → load → run N frames) shall be hash-identical to uninterrupted execution. | MVP | CI | rf-state |
| FR-STATE-003 | Loading a state for a different normalized ROM hash shall be refused with a diagnostic. | MVP | UT | rf-state |
| FR-STATE-004 | Unknown optional chunks shall be skipped with a warning; missing core chunks shall fail loudly naming the chunk; chunk version bumps require a migration or an explicit error. | P1 | UT | rf-state |
| FR-STATE-005 | Golden `.rfstate` fixtures from each release shall load in CI forever (or fail the build). | P1 | CI | rf-state |
| FR-STATE-006 | Replay logs (`.rfreplay`) shall record header (rom hash, start type, core config) + per-frame inputs + periodic state hashes; CI shall replay and assert final hashes. | MVP | CI | rf-state |
| FR-STATE-007 | Enhanced-mode states shall load in Accuracy mode (enhancement chunks ignored) and vice versa. | P1 | CI | rf-state |
| FR-STATE-008 | Rewind via delta-compressed snapshot ring, user-enabled. | P3 | UT | rf-state |

## 9. FR-FE — Frontend

| ID | Requirement | Pri | Verify | Crate |
|---|---|---|---|---|
| FR-FE-001 | ROM library: scan user directories, identify by normalized hash, show profile/enhancement availability badges. | P1 | M | retroforge |
| FR-FE-002 | Per-game settings persisted by normalized hash (mode, shaders, enhancement toggles, input map). | P1 | UT | retroforge |
| FR-FE-003 | Input: keyboard (winit) + gamepads (gilrs) with remapping UI and per-game overrides. | MVP(kbd)/P1(pad) | M | rf-input |
| FR-FE-004 | Dockable panels (egui_dock): game view, debug viewers, compare view (MVP core set); profile editor and plugin manager panels at P1 (full editors P9 — MVP.md excludes plugin-manager UI). | MVP(core)/P1 | M | retroforge |
| FR-FE-005 | Screenshot capture (original and enhanced buffers separately). | P1 | M | retroforge |
| FR-FE-006 | Audio path: cpal stream + SPSC ring + rubato dynamic rate control; underruns surfaced as a diagnostic counter. | MVP | UT + M | rf-audio |
| FR-FE-007 | Video recording of either pipeline. | P3 | M | retroforge |

## 10. FR-AI — AI enhancement hooks (future)

| ID | Requirement | Pri | Verify | Crate |
|---|---|---|---|---|
| FR-AI-001 | AI jobs shall run only via the async `Enhancer` contract, consuming exact extracted assets (indexed pixels + palette) and producing rf-cache entries; zero AI on the frame path. | P3 | UT | rf-ai |
| FR-AI-002 | Enhanced-asset lookup shall key on (rom hash, asset hash, model id, settings hash) with instant fallback to original assets. | P3 | UT | rf-cache |
| FR-AI-003 | AI features shall function fully offline with local models; external providers shall be per-provider opt-in. | P3 | M | rf-ai |
| FR-AI-004 | AI output packs shall use the same replacement-pack format as hand-made packs and be user-reviewable before activation. | P3 | M | rf-ai |

## 11. NFR — Non-functional requirements

| ID | Requirement | Pri | Verify |
|---|---|---|---|
| NFR-001 | Determinism is a release gate: the full determinism CI suite (FR-CORE-002, FR-STATE-002/006, FR-MODE-002) shall block merge on failure. | MVP | CI |
| NFR-002 | *Provisional targets, to be re-baselined after Phase 1 benchmarks:* NES core ≤2 ms/frame, SNES core ≤8 ms/frame, Accuracy mode, single core of an Apple-M-class laptop. | P1/P2 | bench |
| NFR-003 | Enhanced mode with de-flicker + stitching + shaders shall sustain 60 fps on the same hardware; enhancement work exceeding budget degrades (skips) rather than stalling the core thread. | P1 | bench |
| NFR-004 | Cold start to interactive library ≤2 s; ROM load to first frame ≤500 ms (no profile) on reference hardware. | P1 | bench |
| NFR-005 | No cloud services required for any feature; no telemetry without explicit opt-in. | MVP | M |
| NFR-006 | The repository shall never contain copyrighted ROMs or ROM-derived assets; test ROMs arrive via hash-verified manifest fetch (TESTING §3). | MVP | CI |
| NFR-007 | `unsafe` code requires a `// SAFETY:` comment and is warned by workspace lints; hot-path exceptions need review sign-off. | MVP | CI |
| NFR-008 | All public formats (profile schema, .rfstate, .rfreplay, plugin manifest) are versioned from first release. | MVP | CI |
| NFR-009 | Platforms: macOS (primary dev), Windows, Linux — CI builds all three from Phase 3 onward. | P1 | CI |
| NFR-010 | Path containment (D-006): every externally influenced path (library scan, profiles.d references, plugin filesystem caps, export destinations, cache dir) shall be realpath-resolved and contained to its declared root; symlink escapes refused with a diagnostic; library scan is symlink-loop-safe. | P1 | UT |
| NFR-011 | First-party code ships `MIT OR Apache-2.0`; every third-party dependency shall carry a permissive licence (MIT / Apache-2.0 / BSD-2-Clause / BSD-3-Clause / Zlib / ISC / public-domain-equivalent). Copyleft (GPL/LGPL/AGPL/MPL/SSPL) and NC/ND-restricted assets are denied by default and CI shall fail the build on violation. Apache-2.0-only dependencies (winit, cpal) carry NOTICE attribution; dual-licensed dependencies record an explicit licence election (libzstd → BSD-3). A permissive licence outside the enumerated list may be admitted only as a **documented per-crate exception** naming the crate and the reason (never by widening the global allowlist), so the allowlist keeps mirroring this requirement one-to-one — e.g. Unicode-3.0 for `unicode-ident`, pulled in transitively by `serde_derive`. Emulation cores are first-party clean-room implementations from hardware documentation — no ported or transcribed third-party emulator source. | MVP | CI |
