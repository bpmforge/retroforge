# RetroForge — User Stories

Status: Phase 2 baseline · 2026-07-06
Points: Fibonacci (1/2/3/5/8/13); 13 means "split before scheduling".
Traces: SRS requirement IDs; roadmap phases per `docs/ROADMAP.md`.

## Personas

| Persona | Who | Cares about |
|---|---|---|
| **Priya — Player** | Plays her own ROM dumps casually | It just works, looks great on her ultrawide, never breaks the game |
| **Tomás — Enhancement Tinkerer** | Power user, toggles everything | Per-game settings, compare views, visible honest limits |
| **Ada — Profile Author / Reverse Engineer** | DataCrystal contributor | Debugger ergonomics, annotation → profile export, fast decode iteration |
| **Kenji — Plugin Developer** | Writes Lua scripts / plugins | Stable capability-gated API, BizHawk-familiar idioms, good failure messages |
| **Sam — Speedrunner / TASer** | Frame-precise practice + verification | Determinism, replays, frame stepping, rewind later |

## E1 — Baseline NES play (Phase 1) — SRS: FR-CORE-020..026, FR-FE-006

- **E1-S1** (5) As Priya, I want to open a `.nes` file and play it with my keyboard so that the emulator is immediately useful.
  - AC: NROM game boots to gameplay; 60 fps; audio without crackle (underrun counter = 0 over 5 min); default keys documented.
- **E1-S2** (3) As Sam, I want frame stepping and pause so that I can inspect exact game behavior.
  - AC: pause/resume, step 1 frame, step N frames; deterministic across identical steps (FR-CORE-002).
- **E1-S3** (5) As Priya, I want save states with hotkeys so that I can retry hard sections.
  - AC: save/load slots; roundtrip hash-identical (FR-STATE-002); refuses wrong-ROM state with clear message.
- **E1-S4** (3) As Sam, I want my inputs recorded to a replay I can play back so that runs are verifiable.
  - AC: `.rfreplay` records from power-on; playback reproduces final state hash (FR-STATE-006).

## E2 — NES compatibility (Phase 2) — SRS: FR-CORE-025, FR-CORE-012

- **E2-S1** (8) As Priya, I want MMC1/UxROM/CNROM/MMC3 games to run so that ~91% of the licensed library works.
  - AC: mapper test ROMs pass per TESTING gates; battery saves persist across restarts.
- **E2-S2** (3) As Priya, I want my gamepad to work with remapping so that I can play comfortably.
  - AC: gilrs pad detected hot-plug; remap UI; per-game overrides (FR-FE-003).
- **E2-S3** (2) As Tomás, I want a ROM library view with hash-verified identity so that files are recognized regardless of headers.
  - AC: normalized-hash identification (FR-CORE-011); duplicate dumps deduplicate.

## E3 — Research & debug tooling (Phase 2) — SRS: FR-DBG-001..007

- **E3-S1** (5) As Ada, I want pattern/nametable/palette/OAM viewers updating live so that I can see what the PPU sees.
  - AC: all four viewers dockable; update every frame at 60 fps; hover shows indices/addresses.
- **E3-S2** (5) As Ada, I want breakpoints, watchpoints, and a CPU trace in nestest format so that I can reverse engineer game logic.
  - AC: exec/read/write watchpoints halt core via `step`; trace ring exports to file; format diffs cleanly against nestest.log tooling.
- **E3-S3** (3) As Ada, I want to label addresses while debugging and export the labels so that my findings become a profile skeleton.
  - AC: annotation store with label/type/notes/source; export produces valid `memory_map`/`rom_map` TOML (FR-DBG-005).

## E4 — Modern renderer (Phase 3) — SRS: FR-REND-001..007

- **E4-S1** (5) As Priya, I want integer scaling and CRT/clean shaders so that the picture suits my display and taste.
  - AC: shader chain selectable at runtime; screenshots capture post-shader output.
- **E4-S2** (5) As Tomás, I want a side-by-side original/enhanced view so that I can verify enhancements change only what they claim.
  - AC: synced frame pairing; toggle ≤1 frame latency; works with all enhancement features.
- **E4-S3** (3) As Priya, I want renderer problems to fall back to the original pipeline so that emulation never dies from GPU issues.
  - AC: simulated device-loss test recovers within 1 s (FR-REND-007).

## E5 — Generic enhancements (Phases 3-4) — SRS: FR-ENH-001..004, FR-MODE-002/003

- **E5-S1** (5) As Priya, I want sprite flicker reduced in busy scenes so that games look cleaner, without changing gameplay.
  - AC: limit-bypass + temporal modes independently toggleable; sprite-0 games unaffected (test ROM gate); mode invariant holds (FR-MODE-002).
- **E5-S2** (8) As Tomás, I want the emulator to build a map of where I've been (stitching) and show an ultrawide view so that I see beyond the original viewport.
  - AC: stitched canvas persists across sessions; unvisited area fogged (FR-ENH-004); HUD band excluded on a scroll-split game.
- **E5-S3** (2) As Priya, I want enhancements off by default so that first run is authentic.
  - AC: fresh config boots Accuracy-equivalent (FR-MODE-003).

## E6 — Game-aware full level (Phase 5) — SRS: FR-ENH-005..007, FR-PROF-*

- **E6-S1** (8) As Tomás, I want the fixture platformer's (RF-Scroller) current level rendered in full with the live game inside it so that I can see the whole level while playing.
  - AC: level decoded from ROM (not stitched); player + active sprites drawn at correct positions; original-viewport outline; camera modes ultrawide/zoom/full-map.
- **E6-S2** (5) As Ada, I want to iterate decode rules with live re-decode on file save so that authoring a profile takes hours, not days.
  - AC: profile file watch; decode errors shown inline with offsets; level preview panel.
- **E6-S3** (3) As Kenji, I want a documented example profile and demo so that I can copy a working pattern.
  - AC: `/profiles/nes/rf-scroller/` ships with README, sources cited, golden screenshot test.

## E7 — Scripting & plugins (Phases 4-5) — SRS: FR-PLUG-001..005

- **E7-S1** (5) As Kenji, I want to write a Lua script that reads memory and draws an overlay so that I can build a HUD (e.g. hitbox viewer) without recompiling.
  - AC: `rf.mem.read_u8`, `rf.on_frame`, `rf.gui.*` work; script error pauses script only, toast shown.
- **E7-S2** (3) As Priya, I want plugins to declare what they can do and ask before writing memory so that nothing alters my game silently.
  - AC: capability list at enable time; write ledger visible (FR-PLUG-003).

## E8 — SNES baseline (Phase 6) — SRS: FR-CORE-030..032, FR-CORE-035

- **E8-S1** (13→split) As Priya, I want to play a LoROM SNES game so that the platform covers my second console.
  - AC: boots gilyon cputest + a homebrew LoROM title; audio via S-SMP path; input works.
- **E8-S2** (5) As Sam, I want SNES save states and replays with the same guarantees as NES so that my workflow is console-agnostic.
  - AC: FR-STATE suite green for rf-snes.

## E9 — SNES depth (Phase 7) — SRS: FR-CORE-033/034/036/037

- **E9-S1** (8) As Priya, I want Mode 7 games rendered correctly so that flagship SNES titles work.
  - AC: PeterLemon Mode 7 ROMs pass golden-frame; a Mode 7 homebrew plays correctly.
- **E9-S2** (8) As Tomás, I want windowing/mosaic/color-math games to look right so that visual effects aren't broken.
  - AC: undisbeliever + PeterLemon PPU suites pass per TESTING gates.

## E10 — Advanced enhancement (Phase 8) — SRS: FR-ENH-006..008

- **E10-S1** (8) As Tomás, I want widescreen on a supported SNES game (extra tilemap columns per bsnes-hd precedent) so that 16:9/21:9 displays are filled honestly.
  - AC: per-layer profile policy; artifacts documented; compare view demonstrates parity in center crop.
- **E10-S2** (5) As Tomás, I want profile-declared loading waits fast-forwarded so that transitions feel instant without logic changes.
  - AC: wait-loop trigger + release verified on a homebrew case; simulation hash unchanged vs normal-speed run.
- **E10-S3** (5) As Sam, I want rewind so that practice loops are fast.
  - AC: N-second rewind ring, user-enabled, memory budget documented (FR-STATE-008).

## E11 — AI packs (Future) — SRS: FR-AI-001..004

- **E11-S1** (8) As Tomás, I want to generate an upscaled sprite pack locally overnight and review it before use so that AI art is my choice, not a surprise.
  - AC: job queue over extracted assets; review UI diffs original/enhanced; pack activates only after approval; fully offline.

## E12 — Ecosystem (Phase 9) — SRS: FR-PLUG-006, FR-PROF-005/006

- **E12-S1** (5) As Kenji, I want a plugin SDK with docs and a sandboxed WASM host so that third-party plugins are safe to install.
- **E12-S2** (3) As Ada, I want to publish profiles in a community-safe format (facts only, sources required) so that sharing never distributes copyrighted data.
  - AC: CI provenance/asset checks (FR-PROF-003/006) run on contributed profiles.

## Story map summary

| Epic | Phase | Points | MVP? |
|---|---|---|---|
| E1 Baseline NES play | 1 | 16 | ✅ (E1-S1..S4) |
| E2 NES compatibility | 2 | 13 | — |
| E3 Debug tooling | 2 | 13 | E3-S1 partial (2 viewers) |
| E4 Modern renderer | 3 | 13 | E4-S1 partial (scaling) |
| E5 Generic enhancements | 3-4 | 15 | E5-S3 + overlay demo |
| E6 Game-aware full level | 5 | 16 | — |
| E7 Scripting & plugins | 4-5 | 8 | — |
| E8 SNES baseline | 6 | 18 | — |
| E9 SNES depth | 7 | 16 | — |
| E10 Advanced enhancement | 8 | 18 | — |
| E11 AI packs | future | 8 | — |
| E12 Ecosystem | 9 | 8 | — |

MVP composition is defined in `docs/MVP.md`; it draws E1 fully plus slices of
E3/E4/E5 to prove every architectural layer end-to-end.
