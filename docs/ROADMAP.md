# RetroForge — Roadmap

Status: Phase 0 complete → Phase 1 ready · 2026-07-06
Ticket IDs `W<phase>-<n>` live in `plan.json`. **A phase's tickets may not
start until the previous phase's exit criteria pass** (RISKS R-05). Durations
assume AI-agent-driven implementation with maintainer review; they are
planning ranges, not commitments.

## Phase 0 — Research & architecture ✅ (this doc set)

Goal: decisions made, contracts written, workspace scaffolded.
Exit: research briefs + ARCHITECTURE + design/* + SRS/TESTING/plan.json
committed; workspace compiles; CI green. **Done 2026-07-06.**

## Phase 1 — NES core MVP (order of weeks)

Goal: first pixels from a deterministic NROM machine.
Key tickets: W0-02 (iNES/NES2.0 parse + normalized hashing), W1-01 (6502
against SingleStepTests `nes6502` vectors — all official + illegal ops,
cycle-by-cycle bus), W1-02 (bus + NROM + DMA), W1-03 (nestest golden
trace), W1-04 (per-dot background PPU), W1-05 (sprite eval + sprite-0),
W1-07 (input + replay log), W0-03 (rf-harness runner + $6000 protocol +
golden-frame hash), W1-06 (window + frame blit + frame stepping).
Exit criteria:
- SingleStepTests nes6502 vectors 100% (official ops; illegal ops ≥ the set
  nestest covers).
- **nestest golden log byte-exact** (PC/A/X/Y/P/SP/CYC vs nestest.log).
- Two NROM homebrew titles boot (Alter Ego + a neslib fixture, per
  TESTING §8); frame stepping works.
- Determinism: 2× 10k-frame runs, identical per-frame state hashes.

## Phase 2 — NES compatibility (weeks)

Goal: the ~91.5% mapper set + audio + states.
Key tickets: W2-02 (MMC1/UxROM/CNROM), W2-03 (MMC3 + A12 IRQ), W2-01
(frame counter, channels, DMC DMA), W2-05 (cpal + rtrb + rubato rate
control), W2-04 (rfstate container + roundtrip + save-state UI), W2-07
(hash-identified ROM library), W2-06 (gamepad + remap), W2-08 (settings
screens), W2-09 (nightly CI tier + bench baseline).
Exit criteria:
- blargg instr_test-v5, cpu_timing_test6, cpu_interrupts_v2, ppu_vbl_nmi,
  sprite_hit_tests, oam_read, apu_test, mmc3_test_2 all pass headless.
- Save-state roundtrip + replay determinism suites green.
- Nova the Squirrel plays start-to-level-3 by hand without visible faults.

## Phase 3 — Renderer modernization (weeks)

Goal: wgpu pipelines + the indexed-pixel contract paying off.
Key tickets: W3-01 (device/surface/original pipeline, integer/aspect,
headless golden frames, device-loss fallback), W3-02 (WGSL chain: CRT,
scanline, lcd-grid, xBRZ-class), W3-03 (BG/sprite layer extraction from
pixel metadata), W3-04 (side-by-side original/enhanced), W3-05
(sprite-limit bypass + de-flicker prototype), W3-06 (3-OS CI builds).
Exit criteria: golden-frame suite runs on CI in both pipelines; bypass
demonstrably removes flicker on a test scene while Accuracy mode is
pixel-identical to Phase-2 goldens; 60 fps sustained with shader chain on
M-class hardware.

## Phase 4 — Enhancement framework (weeks)

Goal: the platform part — events, profiles, overlays, invariant.
Key tickets: W4-01 (CoreSink event bus + subscription masks + mode-
invariant CI), W4-03 (runtime + SceneGraph composer + stitcher), W4-02
(TOML schema v0 + loader + validator), W4-04 (mlua host + BizHawk-shaped
bindings + Lua console), W4-05 (mode toggles + honesty badge + per-game
settings), W4-06 (debug viewers + annotation store), W4-07 (retroforge-
tool CLI), W4-08 (rf-cache store).
Exit criteria: mode-invariant test green in CI; profile matches by
normalized hash and toggles features; Lua script draws an overlay from live
RAM reads; enhancement state serializes into ENHC chunks.

## Phase 5 — Game-aware prototype (weeks) → **MVP** (`docs/MVP.md`)

Goal: prove the thesis on open-source homebrew.
Key tickets: W4-03 (scroll telemetry + IRQ split + scene hashing +
re-entrant canvases), W5-01 (Nova the Squirrel: identity + RAM map),
W5-02 (level decoder family + decode goldens), W5-03 (full-level view +
live overlay demo), W5-06 (authoring hot-reload loop), W5-04 (MVP
acceptance pass), W5-05 (release v0).
Exit criteria: MVP acceptance checklist (MVP.md) passes end-to-end on Nova
the Squirrel + one non-profiled game (stitcher-only ultrawide).

## Phase 6 — SNES core MVP (months — R-01)

Goal: 65C816 machine boots test ROMs.
Key tickets: W6-00 (phase-entry refinement — splits the 13-pt set),
W6-01 (65C816 SingleStepTests vectors), W6-02 (bus, LoROM/HiROM,
MDMA/HDMA basics, auto-joypad), W6-03 (PPU modes 0/1, OAM, first
frames), W6-04 (SPC700 vectors + boot-ROM handshake).
Exit criteria: 65816 + spc700 vector suites 100%; gilyon cputest/spctest
pass; libSFX-built fixture ROMs render golden frames; input works.

## Phase 7 — SNES compatibility (months)

Goal: the commercial mainstream plays.
Key tickets: W7-01 is the phase-entry planning ticket — it expands into:
PPU modes 2-6 + mosaic + color math + windows, Mode 7 (+ HD-Mode-7-class
internal resolution — same math, more samples), S-DSP (BRR, echo,
gaussian), DMA/HDMA edge cases, SNES save states, PAL timing config,
NES AxROM/Action 53 (~96% coverage), PeterLemon/undisbeliever golden
frames.
Exit criteria: PeterLemon CPU/PPU/Mode-7 golden set green; 3 designated
plain-LoROM commercial titles (user-supplied) playable start-to-credits
sampled; Nova the Squirrel 2 plays; save states roundtrip.

## Phase 8 — Advanced enhancements (months, parallelizable)

Widescreen per-BG-layer policies for SNES (bsnes-hd model) + NES profile
tier; temporal de-flicker (OAM history reconstruction); HUD separation
(profile-driven + heuristic); HD pack loader with Mesen-compatible identity
+ pack builder; offline AI pack pipeline (ort/ONNX, cache-keyed, review UI);
rewind; room-stitching for top-down profiles; smooth-camera experiments
(profile-gated entity interpolation).
Exit criteria: each feature ships with its own golden/A-B tests + per-game
override knobs + honesty-contract UI labels.

## Phase 9 — Ecosystem (ongoing)

Profile editor UI; plugin SDK stabilization (wasmtime component tier, WIT
world, capability sandbox); pack/profile/mod distribution format
(community-safe: no ROM data, license metadata required); documentation
site; example plugins; Mesen HD-pack import; possible flattened libretro
export (NON_GOALS #9 revisit).
Exit criteria: a third party ships a profile + pack + script without
touching Rust or asking us questions.

## Dependency spine

```
P1 NES core ─ P2 compat ─ P3 renderer ─ P4 framework ─ P5 MVP demo
                                              │
P6 SNES core ─ P7 SNES compat ────────────────┤
                                              └─ P8 advanced ─ P9 ecosystem
```
P6 may start once P4 is gated (harness + contracts reusable); P8 items gate
individually on P5/P7 as relevant.
