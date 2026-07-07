# RetroForge — Competitive Analysis

Status: Phase 0 · 2026-07-06
Source detail and URLs: `docs/research/prior-art.md`. Statuses verified 2026-07-06.

## 1. Landscape table

| Project | Status (2026) | Accuracy | Enhancement | Authoring/tooling | Scripting | Consoles | Key limitation |
|---|---|---|---|---|---|---|---|
| **MesenCE** (ex-Mesen2) | Active fork; Mesen2 archived 2026-06-04 | High (test-gated) | HD packs, sprite-limit removal, overclock | **Best-in-class debugger**, HD Pack Builder | Lua | NES, SNES, GB/GBA, PCE, SMS, WS | Enhancement stops at tile replacement; fixed-framebuffer output; no game-aware layer |
| **bsnes-hd** | Dormant (beta 10.6, 2021-06); lives as libretro core | High (bsnes base) | **Widescreen, HD Mode 7**, true color | Per-game manual settings, no pipeline | — | SNES | Unmaintained 5 years; sprite pop-in needs ROM hacks; CPU-rendered cost |
| **ares** | Very active (v148, 2026-05) | **Gold standard** | None (explicit non-goal) | Minimal debugger | — | 40+ systems | Cooperative-thread cores resist hooks/cheap savestates; zero enhancement ambition |
| **BizHawk** | Very active (2.10/2.11, 2026) | High (core-dependent) | Filters only | TAS tools, RAM watch/search | **Lua contract (de-facto standard)** | Many (ports) | Renders what the console rendered; C#/ported-core architecture |
| **RetroArch/libretro** | Very active | Core-dependent | Shader ecosystem (slang) | — | — | Everything | Single fixed framebuffer per core; frontend owns UI — no scene graph, no core-owned panels (fatal for our feature set) |
| **wideNES / MappyLand** | Blog prototype (2018) / paper (AAAI 2021) | n/a (technique) | **Generic map stitching** | — | — | NES | Never productized; terrain-only (off-screen actors don't exist) |
| **TetaNES** | Active (v0.14, 2026) | Good NES | Filters | Basic debugger | — | NES | Proof our Rust stack ships; not an enhancement platform |

## 2. What each proves for us

- **MesenCE** proves the tile-identity + conditions HD-pack contract and
  that the debugger is the enhancement substrate (Pack Builder = productized
  debugger view). We adopt its pack semantics as an import target and its
  "auto-re-enable sprite limit" lesson: every enhancement needs an escape
  hatch. Its predecessor's archival proves single-maintainer risk (RISKS R-14).
- **bsnes-hd** proves SNES widescreen terrain is semi-generic (tilemap data
  beyond 256px usually exists) *and* that per-BG-layer policy + sprite
  clip/safe/unsafe modes are the right knobs — we adopt that policy model
  into profiles. Its dormancy means the best SNES enhancement ideas have had
  no maintained home since 2021: that is our opening.
- **ares** proves where the accuracy bar sits and that cooperative-thread
  purity conflicts with an enhancement platform — we choose Mesen-style
  state-machine cores (hook-friendly, cheap savestates) and use ares as a
  behavioral oracle in disputes, never as the architecture template.
- **BizHawk** defines the Lua API and determinism contract the modding/TAS
  community expects; matching its `memory`/`event`/`gui` shape
  (`docs/design/PLUGINS.md` §3) converts an existing script-author community.
- **libretro** proves why we build native: one framebuffer cannot carry
  layered composition, side-by-side compare, map explorer, or authoring UI.
  bsnes-hd's core shows a flattened wide-geometry export is feasible later
  (NON_GOALS #9).
- **wideNES/MappyLand** contribute the generic stitching algorithm (scroll
  telemetry + IRQ split masking + perceptual-hash scene identity) that we
  productize with profile-overridable quirks (ENHANCEMENT_RUNTIME §3).

## 3. The gap RetroForge occupies

Accuracy people don't do enhancement (ares). Enhancement people are
unmaintained (bsnes-hd) or capped at asset swaps (MesenCE). Tooling people
don't render beyond the console (BizHawk). Nobody:

1. outputs a **scene graph** (layers/sprites/HUD as compositable items),
2. treats **per-game knowledge as shareable data** with an authoring
   pipeline instead of emulator settings,
3. runs **one enhancement runtime across NES and SNES**,
4. guarantees **enhancement cannot perturb simulation** (CI-tested), and
5. is **maintained** while doing any of the above.

That intersection is the product. It is defensible because it is *work* —
the honest per-game column of ARCHITECTURE §2 — made cheap by tooling,
not waved away.

## 4. Threats

- **MesenCE grows game-aware features** — most capable incumbent; but its
  C++/fixed-framebuffer architecture and preservation-focused community make
  a scene-graph pivot unlikely. Mitigation: pack-format compatibility so its
  ecosystem is additive to ours, not rival.
- **A funded team forks ares/bsnes for enhancement** — possible; our moat is
  the profile/authoring ecosystem and two-console runtime, which compound
  with community content over time.
- **We under-deliver accuracy and lose trust** — enhancement claims mean
  nothing on a broken core; hence accuracy gates precede every enhancement
  phase (ROADMAP ordering is non-negotiable).
