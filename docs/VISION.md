# RetroForge — Vision

Status: Phase 0 · 2026-07-06

## 1. Problem

Every capability RetroForge needs already exists somewhere — and nowhere
together:

- **MesenCE** has the best debugger and the proven HD-pack format, but its
  enhancement ambition stops at tile replacement, and it inherited its
  architecture from a project that was archived in June 2026
  (`docs/research/prior-art.md` §1).
- **bsnes-hd** proved SNES widescreen and HD Mode 7, then went dormant in
  2021 — its ideas have had no maintained home for five years (§2).
- **ares** is the accuracy gold standard but explicitly has no enhancement
  ambition, and its cooperative-thread cores resist the hooking an
  enhancement layer needs (§3).
- **BizHawk** defined the Lua/determinism contract for scripting and TAS but
  renders exactly what the console rendered (§4).
- **wideNES** demonstrated generic map reconstruction as a 2018 blog-post
  prototype that was never productized (§6).

Nobody unifies accurate emulation, a modern enhancement pipeline, and an
authoring workflow across NES and SNES in one maintained project. Players who
want more than a CRT filter must hop between dormant forks, per-game ROM
hacks, and one-off tools.

## 2. Vision

**RetroForge is a retro game enhancement platform, not just an emulator.**

Accurate NES and SNES cores run user-provided ROMs exactly as the hardware
did. On top, an opt-in enhancement engine — observing, never perturbing —
composes what the PPU *meant*: de-flickered sprites, ultrawide terrain,
whole levels on one screen, replaced art, live overlays. Per-game knowledge
is data (profiles), authored in the built-in debugger and shared like mods.

The test of success: a player toggles between "exactly 1990" and "what this
game looks like unchained" — and can trust both.

## 3. Differentiators

| Axis | Incumbents | RetroForge |
|---|---|---|
| Render output | Fixed framebuffer (all of them; libretro enforces it) | Scene graph: BG layers, sprites, HUD, overlays composited independently (`docs/ARCHITECTURE.md` §5, §8) |
| Game-awareness | Ad-hoc per-emulator options | Data-driven per-game profiles with hash identity, decode rules, camera/HUD/entity knowledge (`docs/design/GAME_PROFILES.md`) |
| Authoring | Mesen HD Pack Builder (tiles only) | Debugger → annotation → profile export pipeline for maps, entities, cameras, packs |
| Consoles | NES *or* SNES enhancement, never both | One enhancement runtime over both cores |
| Enhancement honesty | "Remove sprite limit" checkboxes that break games silently | Honesty contract: every feature labeled generic/heuristic/profile-gated, with per-game overrides and side-by-side compare (`docs/ARCHITECTURE.md` §2) |
| Determinism | TAS emulators only | BizHawk-grade determinism *and* enhancement (enhancement provably cannot perturb simulation — CI invariant) |

## 4. Why now

- Mesen2's archival (2026) and bsnes-hd's dormancy (2021) left the
  enhancement niche without a maintained flagship.
- The Rust stack matured: TetaNES proves wgpu+egui+cpal ships a real
  emulator; WebGPU reached W3C CR (`docs/research/rust-stack.md`).
- The techniques are de-risked: wideNES/MappyLand (generic maps), Mesen HD
  packs (asset identity), bsnes-hd (widescreen policy) are published, proven
  designs waiting to be unified.
- AI-assisted development makes the long tail (per-game profiles, mapper
  breadth, viewer UIs) affordable for a small team.

## 5. Success criteria

**6 months — architecture proven (MVP, `docs/MVP.md`)**
- NES core passes the Phase-2 test gate (nestest golden log, blargg CPU +
  `ppu_vbl_nmi`; `docs/TESTING.md`).
- Enhancement runtime demonstrates: generic map stitching with ultrawide
  view on an unprofiled game, and a full-level profile demo on RF-Scroller
  (our in-repo fixture platformer — self-contained fixture doctrine D-001).
- Accuracy-vs-Enhanced state-hash invariant enforced in CI from day one.

**18 months — platform real**
- SNES core boots the plain-LoROM/HiROM commercial mainstream; Mode 7 games
  render with HD Mode 7-class internal-resolution scaling.
- Profile + Lua + pack formats stable at v1; ≥10 curated game profiles;
  Mesen HD-pack import path working.
- Debugger suite at "daily-drivable for ROM hackers" quality.

**Long-term — the enhancement commons**
- Community authors profiles/packs/scripts without touching Rust.
- RetroForge is the reference platform for "beyond original hardware" retro
  play — the maintained home wideNES and bsnes-hd never had.
- Project survives its founder (permissive license, docs, handoff-ready —
  the Mesen2 lesson, `docs/research/prior-art.md` implication 14).

## 6. Guardrails

The vision is bounded by `docs/NON_GOALS.md` (what we will not build),
`docs/CONSTRAINTS.md` (legal/technical lines we will not cross), and the
honesty contract (`docs/ARCHITECTURE.md` §2): we never promise generically
what only per-game work can deliver.
