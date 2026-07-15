# RetroForge — Scope

Status: Phase 0 · 2026-07-06
Phases refer to `docs/ROADMAP.md`; requirement IDs to `docs/SRS.md`.

## 1. Scope by subsystem

| Subsystem | In scope (v1 = Phases 0-9) | Later / conditional | Out (see NON_GOALS) |
|---|---|---|---|
| NES core | 2A03 CPU+APU, per-dot PPU, NROM/MMC1/UxROM/CNROM/MMC3 (~91.5% of licensed NA library), then AxROM (~96%) | FDS, MMC5, VRC family | Everything past ~96% coverage as a goal |
| SNES core | 65C816/5A22, DMA/HDMA, PPU modes 0-7 (Mode 7, windows, mosaic, color math), S-SMP/S-DSP, LoROM/HiROM, battery saves | ExHiROM; SA-1, Super FX, DSP-1 | One-off chips (Cx4, S-DD1, SPC7110...) |
| Modes | Accuracy / Compatibility / Enhanced / Research-Debug / Game-Aware presets + CI mode-invariant | — | — |
| Renderer | wgpu original + enhanced pipelines, integer/aspect scaling, WGSL shader chain (CRT, xBRZ-class), layered scene-graph composition, ultrawide, side-by-side, headless golden-frame mode | HDR output (wgpu 30 supports), custom user shaders | slang/GLSL preset compat in v1 |
| Enhancement (generic) | Sprite-limit bypass + auto-re-enable heuristic, temporal de-flicker, wideNES-style stitcher (scroll telemetry, IRQ/HDMA split detection, scene hashing, re-entrant canvases), HUD heuristics | MappyLand-style smarter segmentation | Generic full-level or widescreen-gameplay promises |
| Enhancement (game-aware) | Profile schema v0, decoder families (metatile_screens, room_grid, tilemap_direct), camera/entity/HUD rules, RF-Scroller (NES) + RF-Scroller-S (SNES) in-repo fixture profiles (D-001), fast-load wait-loops | Curated profiles for flagship commercial titles (SMB1, Zelda — formats already documented by community; facts only, user ROMs) | Hardcoded game names in runtime code; third-party assets as gate fixtures |
| Asset replacement | Mesen HD-pack-compatible identity (tile bytes + palette + conditions), pack builder (record-while-playing), pack loader | Mesen `hires.txt` import; parallax/audio pack features | — |
| AI | Offline pack generation (local ESRGAN-class via ONNX), cache keyed (rom, asset, model, settings), review UI | Widescreen outpainting, translation/accessibility overlays, external providers (opt-in) | Real-time neural upscaling |
| Scripting/plugins | Lua (mlua) with BizHawk-shaped API (memory domains, bus callbacks, gui draw, frame events), capability manifests, native in-tree plugins | wasmtime component sandbox (Phase 9), plugin marketplace format | Stable native ABI before Phase 9 |
| Debugger | Breakpoints/watchpoints, CPU/PPU/APU trace, pattern/nametable-tilemap/OAM/palette viewers, event viewer, memory editor, annotation store → profile export | Assembler/live patching, symbol import (FCEUX .nl, Mesen .mlb) | — |
| Save/replay | Versioned chunked states (.rfstate), BK2-style input-log replay (.rfreplay), determinism CI, golden state fixtures | Rewind (Phase 8), TAS editor UX | — |
| Frontend | egui/eframe + egui_dock shell: library (hash-identified), per-game settings, input remap (keyboard+gilrs), mode toggles, panel docking, screenshots | Video recording, controller GUI themes, localization | Mobile/web/console UIs |
| Tooling | `retroforge-tool`: hash, header parse, ROM inspect, profile validate, map/sprite-sheet/tilemap export; `scripts/fetch-test-roms.sh` | Standalone profile editor app | — |
| Distribution | GitHub releases: macOS (Apple Silicon primary), Windows, Linux builds | Package managers (brew, winget) | App stores |

## 2. MoSCoW summary

**Must (MVP, Phases 0-5)** — NES core to blargg-CPU/PPU-vbl gate; NROM+MMC1;
save states + replay determinism; original pipeline + scaling + one CRT
shader; debug viewers (pattern/nametable/OAM/palette) + frame stepping;
enhancement runtime with stitcher + ultrawide + overlay API; profile loader
+ RF-Scroller full-level demo; side-by-side compare; mode-invariant CI.

**Should (Phases 6-7)** — SNES core to gilyon/PeterLemon gates; Mode 7 + HD
Mode 7-class internal resolution; UxROM/CNROM/MMC3; Lua tier; annotation →
profile export; MMC3 IRQ-dependent titles playable.

**Could (Phases 8-9)** — de-flicker temporal reconstruction tuning, HD pack
builder + import, offline AI pack pipeline, rewind, widescreen per-layer
policies (SNES), room-stitching top-down profiles, wasmtime plugins, profile
editor UI, accessibility/translation overlay prototypes.

**Won't (v1)** — see `docs/NON_GOALS.md`: netplay, mobile/web, libretro
core, enhancement chips, real-time AI, multi-system support.

## 3. Scope-change rule

Anything moving between columns above requires: an entry in this file's
history, a ROADMAP.md phase adjustment, and re-validation of the affected
plan.json tickets. The honesty contract rows in `docs/ARCHITECTURE.md` §2
may only be relaxed with new research evidence.
