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
cycle-by-cycle bus), W1-02, W1-04-bg (per-dot background), W1-04-spr
(sprite eval + sprite-0), W1-02, W1-07, W0-03 (rf-harness runner
+ $6000 protocol + golden-frame hash), W1-06-min (window + texture +
frame stepping).
Exit criteria:
- SingleStepTests nes6502 vectors 100% (official ops; illegal ops ≥ the set
  nestest covers).
- **nestest golden log byte-exact** (PC/A/X/Y/P/SP/CYC vs nestest.log).
- Alter Ego (PD) title screen renders; frame stepping works.
- Determinism: 2× 10k-frame runs, identical per-frame state hashes.

## Phase 2 — NES compatibility (weeks)

Goal: the ~91.5% mapper set + audio + states.
Key tickets: W2-02, W2-02, W2-02, W2-03 (A12 IRQ), W2-01 (frame
counter, channels, DMC DMA), W2-05-out (cpal + rtrb + rubato rate
control), W2-04 (rfstate container + roundtrip tests), W2-04
(rfreplay + divergence pinpointing), W2-07 (hash-identified ROM
library UI), W2-06-remap.
Exit criteria:
- blargg instr_test-v5, cpu_timing_test6, cpu_interrupts_v2, ppu_vbl_nmi,
  sprite_hit_tests, oam_read, apu_test, mmc3_test_2 all pass headless.
- Save-state roundtrip + replay determinism suites green.
- Nova the Squirrel plays start-to-level-3 by hand without visible faults.

## Phase 3 — Renderer modernization (weeks)

Goal: wgpu pipelines + the indexed-pixel contract paying off.
Key tickets: W3-01-core (device/surface/original pipeline), W3-01
(integer/aspect), W3-02 (WGSL chain: CRT, scanline, xBRZ-class),
W3-03 (BG/sprite layer extraction from pixel metadata), W3-04
(side-by-side original/enhanced), W3-05-1 (sprite-limit bypass +
auto-re-enable heuristic), W3-01-gpu (golden frames on CI, software
rasterizer on Linux runners).
Exit criteria: golden-frame suite runs on CI in both pipelines; bypass
demonstrably removes flicker on a test scene while Accuracy mode is
pixel-identical to Phase-2 goldens; 60 fps sustained with shader chain on
M-class hardware.

## Phase 4 — Enhancement framework (weeks)

Goal: the platform part — events, profiles, overlays, invariant.
Key tickets: W4-01 (CoreSink event bus + subscription masks), W4-03
(runtime + SceneGraph composer), W4-02 (TOML schema v0 + loader +
validator), W4-04 (draw-command API), W4-04 (mlua host + BizHawk-
shaped bindings), W4-05-invariant (CI: Accuracy vs Enhanced state-hash
equality), W4-05 (per-game settings persistence).
Exit criteria: mode-invariant test green in CI; profile matches by
normalized hash and toggles features; Lua script draws an overlay from live
RAM reads; enhancement state serializes into ENHC chunks.

## Phase 5 — Game-aware prototype (weeks) → **MVP** (`docs/MVP.md`)

Goal: prove the thesis on open-source homebrew.
Key tickets: W4-03 (scroll telemetry + IRQ split + scene hashing +
re-entrant canvases), W5-03 (enhanced camera over stitched canvas),
W5-01-profile (Nova the Squirrel: level decoder from its documented
format, camera/entity addresses), W5-02 (decoded-level scene layer +
live sprites over reconstruction), W5-04 (side-by-side demo mode +
capture).
Exit criteria: MVP acceptance checklist (MVP.md) passes end-to-end on Nova
the Squirrel + one non-profiled game (stitcher-only ultrawide).

## Phase 6 — SNES core MVP (months — R-01)

Goal: 65C816 machine boots test ROMs.
Key tickets: W6-02-snes (headers, LoROM/HiROM, normalized hashing),
W6-01-65816 (SingleStepTests vectors), W6-02 (MDMA/HDMA, auto-joypad),
W6-03-basic (modes 0/1, OAM, windows deferred), W6-04 (vectors +
boot ROM), W6-04-path, W6-03-snes (viewer providers).
Exit criteria: 65816 + spc700 vector suites 100%; gilyon cputest/spctest
pass; libSFX-built fixture ROMs render golden frames; input works.

## Phase 7 — SNES compatibility (months)

Goal: the commercial mainstream plays.
Key tickets: W7-01-modes (2-6, mosaic, color math, windows), W7-01
(+ HD-Mode-7-class internal resolution — same math, more samples), W7-01
(S-DSP: BRR, echo, gaussian), W7-01-edge, W7-01-snes, W7-01
(PeterLemon/undisbeliever golden frames).
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
