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
Exit criteria (**amended 2026-08-05 — Brad, on the Phase 1 gate's finding;
this list is AUTHORITATIVE for Phase 1 exit, and `docs/TESTING.md` §8
summarizes it rather than restating it**):
- SingleStepTests nes6502 vectors 100% (official ops; illegal ops ≥ the set
  nestest covers).
- **nestest golden log byte-exact** (PC/A/X/Y/P/SP/CYC vs nestest.log).
- Frame stepping works.
- Determinism: 2× 10k-frame runs, identical per-frame hashes over
  **reachable** state (CPU registers, WRAM, OAM, PRG-RAM, `master_cycle`,
  `frame_count` — `EmuStepper::state_hash`'s enumerated set).

**What was removed, and why it is not a weakening.** The first Phase 1 exit
gate (2026-08-05, `docs/STATUS.md`) found that two criteria could not be
satisfied by *any* amount of Phase 1 work, because the tickets that
implement them are Phase 2: "two NROM homebrew titles boot" is W2-10 +
W2-11, and full-machine-state determinism needs `save_state`, which is
W2-04 — itself behind W2-01b → W2-01a. Satisfying Phase 1 as previously
written required **31 of Phase 2's 71 points first**, which inverts the
phase order the gate exists to enforce (R-05).

Both were therefore moved *down* to Phase 2, not dropped — and Phase 2's
exit criteria already covered them in **stronger** form ("RF-Scroller plays
start-to-finish and Alter Ego plays by hand"; "save-state roundtrip +
replay determinism suites green"), so the removal loses no coverage. The
determinism criterion keeps its 10k-frame scale, which is the part that
catches drift a short run hides; only its *state scope* is staged, because
a hash cannot cover PPU/APU internals before the core can serialize them.

## Phase 2 — NES compatibility (weeks)

Goal: the ~91.5% mapper set + audio + states.
Key tickets: W2-02 (MMC1/UxROM/CNROM), W2-03 (MMC3 + A12 IRQ), W2-01
(frame counter, channels, DMC DMA), W2-05 (cpal + rtrb + rubato rate
control), W2-04 (rfstate container + roundtrip + save-state UI), W2-07
(hash-identified ROM library), W2-06 (gamepad + remap), W2-08 (settings
screens), W2-09 (nightly CI tier + bench baseline).
Exit criteria:
- blargg instr_test-v5, cpu_timing_test6, cpu_interrupts_v2, ppu_vbl_nmi,
  sprite_hit_tests, oam_read, apu_test, mmc3_test_2 all pass headless
  (**several of these are fetched but executed by nothing today — W2-12
  owns wiring them into `local-gate.sh` + the evidence file; see the Phase 1
  gate entry in `docs/STATUS.md`**).
- Save-state roundtrip + replay determinism suites green, and determinism
  extended to **full machine state** (PPU + APU included, reachable once
  W2-04 lands `save_state`) — inherited from Phase 1's 2026-08-05 amendment.
- **Two NROM homebrew titles boot** (Alter Ego + a neslib fixture) —
  inherited from Phase 1's 2026-08-05 amendment; subsumed by, and weaker
  than, the RF-Scroller/Alter Ego criterion immediately below, which stays
  the real bar.
- RF-Scroller (in-repo fixture, W2-10) plays start-to-finish and Alter Ego
  plays by hand without visible faults.

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
re-entrant canvases), W5-01 (RF-Scroller: identity + RAM map),
W5-02 (level decoder family + decode goldens), W5-03 (full-level view +
live overlay demo), W5-06 (authoring hot-reload loop), W5-04 (MVP
acceptance pass), W5-05 (release v0).
Exit criteria: MVP acceptance checklist (MVP.md) passes end-to-end on
RF-Scroller + one non-profiled game (stitcher-only ultrawide; Alter Ego or
a second fixture).

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
sampled; RF-Scroller-S (in-repo SNES fixture) plays; save states roundtrip.

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
