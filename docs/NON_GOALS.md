# RetroForge — Non-Goals

Status: Phase 0 · 2026-07-06

Explicit exclusions. Each is a decision, not an oversight; revisiting one
requires a design doc, not a ticket.

## Product non-goals

1. **Not a multi-system frontend.** Two consoles: NES and SNES. No GB/GBA,
   Genesis, PSX, or "just one more core." RetroArch and ares own breadth;
   our value is depth on two machines that share an enhancement runtime.
2. **No netplay in v1.** Rollback netcode couples into core scheduling and
   input latching; designing it later is possible *because* determinism and
   input logs are first-class, but it is not on the roadmap through Phase 9.
3. **No mobile or console ports in v1.** Desktop (macOS/Windows/Linux)
   only. wgpu keeps doors open; we walk through none of them yet.
4. **No web build in v1.** wgpu/egui make WASM plausible; test-ROM CI and
   plugin sandboxing come first. Revisit after Phase 9.
5. **No ROM distribution, no ROM store, no "get games" UX.** User-provided
   ROMs only. Homebrew fixtures arrive via hash-verified fetch manifests
   (`docs/CONSTRAINTS.md` §legal).
6. **No cloud dependency, no accounts, no telemetry-by-default.** AI
   features are local-first; optional external providers are opt-in
   configuration, never a requirement (spec: "should not require cloud").
   Clarified 2026-09-17 (D-011): this permits fetching box art/metadata
   from libretro-thumbnails or similar community sources as an opt-in,
   off-by-default Settings toggle, no accounts — the same posture as any
   other external provider under this item.

## Technical non-goals

7. **Not cycle-accuracy-at-all-costs.** ares-level transistor fidelity is a
   non-goal. Accuracy is defined by the test gates in `docs/TESTING.md`
   (SingleStepTests vectors, blargg/gilyon suites, golden frames) — when
   those pass, we stop. ares remains our oracle, not our bar.
8. **No cloud AI, and no AI in the frame path by default.** *Amended
   2026-09-17 (D-012, Brad):* local AI is in scope for both offline pack
   generation and real-time enhancement and upscaling, but only on
   hardware that can hold the frame budget (Apple Neural Engine / Metal,
   discrete GPUs), detected at start and gated; every other machine gets
   the cheap shaders (xBRZ/ScaleFx/CRT) and cached replaced assets, and a
   fresh install still boots with the frame path AI-free (law 6). The
   original reason stands as the engineering bar, not the policy:
   ESRGAN-class models cost tens of ms/frame (`docs/research/prior-art.md`
   §7), so a real-time pass ships only behind a measured budget gate, a
   one-frame latency disclosure, and the honesty badge.
9. **No libretro core in v1.** The single-framebuffer API cannot carry the
   scene-graph output, multi-panel debugger, or authoring UX (§5 of
   prior-art). A flattened libretro export (wide geometry + core options) is
   plausible post-Phase 9; the core/frontend seam is designed to allow it.
10. **SNES enhancement chips deferred.** Super FX (~16) waits until
    plain LoROM/HiROM accuracy is gated (met 2026-09-17); **SA-1 (~34
    games) lifted 2026-09-18 (D-013, Brad), Wave 17;** DSP-2/3/4, Cx4, S-DD1,
    SPC7110, ST01x (1-3 games each) may never come. **DSP-1 (~13 games)
    lifted from this deferral 2026-09-17** (Brad's ruling): the LoROM/HiROM
    gate condition was met (Phase 14 census: 1001/1265 SNES archives
    render, 0 crashes) and DSP-1 is the largest chip bucket with the
    smallest surface (fixed command set, no second CPU on the bus) — see
    FR-CORE-038, D-010, SCOPE.md history.
11. **No generic "show the whole level" promise.** Unvisited areas require
    per-game ROM decoding via profiles. Generic tier shows *visited terrain
    only* (wideNES wall: off-screen actors are never simulated). UI language
    must never imply otherwise — this is the honesty contract.
12. **No generic widescreen gameplay.** SNES terrain widescreen is
    semi-generic (bsnes-hd precedent, with artifacts); NES widescreen and
    *gameplay* in extended areas are per-game (profiles/patches). bsnes-hd's
    top complaint was over-promising here; we won't repeat it.
13. **No silent game-behavior modification.** Anything that writes emulated
    memory or patches the ROM is mod-tier: per-game, per-user, explicit,
    ledgered, recorded in save states (`docs/design/PLUGINS.md` §2).
14. **No cheat database / achievement engine in v1.** The plugin API can
    host both later; we ship neither.
15. **No custom shader language.** WGSL passes with parameter UBOs; no
    slang/GLSL preset compatibility layer in v1 (import can come later).

## Process non-goals

16. **No hand-written per-game engines in the runtime.** Game knowledge
    enters as profile data or plugins. A decoder family is added to
    `rf-enhance` only when ≥2 games need the same shape
    (`docs/design/GAME_PROFILES.md` §2).
17. **No stable plugin ABI before Phase 9.** Native plugins stay in-tree;
    only profile TOML and Lua surface are public formats early
    (ARCHITECTURE §10 risk 5).
