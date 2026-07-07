# Design: Debugger & Reverse-Engineering Tools (`rf-debugger`)

Scope: the inspection/tooling layer — execution control, tracing, viewers,
annotations, and the export path that turns debugging sessions into game
profiles. Runs against `StateView` + `CoreEvent`s + `step()`; shares the UI
shell (egui_dock panels) with the frontend. This layer is also the profile-
authoring workbench (GAME_PROFILES.md §3), which is why it's a first-class
subsystem and not an afterthought.

## 1. Execution control

- Modes: run / pause / frame-step / scanline-step / instruction-step /
  step-over / step-out (via stack-depth watch), run-to-cursor.
- **Breakpoints**: PC exec, memory read/write/access (CPU and PPU address
  spaces separately), value-conditional (`addr==X && val&mask`), scanline/
  dot position (NES) or H/V (SNES), IRQ/NMI entry, mapper events (bank
  switch, MMC3 IRQ). Implemented inside `step()` dispatch via a compiled
  breakpoint table — zero cost when the table is empty (checked by a single
  bool before the match).
- **Watchpoints** double as profile probes: a watchpoint can be "promoted"
  to a `memory_map` annotation with one click (address, size, label).

## 2. Tracing

- Per-chip trace ring buffers (CPU, PPU register writes, APU, DMA/HDMA,
  mapper), fixed-capacity SPSC (rtrb) drained by the UI/trace-dump worker —
  never blocks the core thread; overflow drops oldest and sets a truncation
  flag visible in the UI.
- CPU trace format is **nestest.log-compatible** for the NES
  (`PC  raw  disasm  A X Y P SP  PPU:sl,dot CYC`) so the golden-trace diff
  in CI and the on-screen trace viewer share one formatter. SNES format
  follows bsnes conventions (bank:addr, m/x-aware disasm) for easy diffing
  against reference emulators.
- Trace-to-file: background writer with lz4 framing; a 60s full-speed CPU
  trace is ~GBs raw — the UI warns and suggests filtered traces (PC ranges,
  event types).

## 3. Viewers (data providers + egui panels)

Each viewer is a pull-based data provider reading `StateView` at frame
boundaries (or at pause, any point), plus an egui panel. Providers live in
`rf-debugger` (UI-free, unit-testable); panels in the frontend crate.

| Viewer | NES | SNES specifics |
|---|---|---|
| Pattern/CHR | both pattern tables, palette selector, 8×16 mode | VRAM char data per BG char-base, 2/4/8bpp decode |
| Nametable/Tilemap | 4 nametables w/ scroll + mirroring overlay, attribute grid | per-BG tilemaps w/ tile flip/prio flags, offset-per-tile view, **Mode 7 view** (1024×1024 playfield + camera trapezoid) |
| Palette | 32-entry, emphasis variants | CGRAM 256, color-math preview |
| OAM/Sprite | 64 entries, per-scanline occupancy bar (8-limit visual), `dropped_by_limit` highlight | 128 entries, 32/line + 34-sliver occupancy, size/base decode |
| Memory hex | CPU/PPU/OAM spaces, live edit (pause-gated), goto/find, annotation coloring | +VRAM/CGRAM/ARAM/DMA regs spaces, 24-bit addressing |
| Event viewer | Mesen-style frame timeline: dot/scanline scatter of register writes, IRQ/NMI, DMA, sprite-0 hit | same + HDMA channel lanes per scanline |
| Trace viewer | scrollback of ring buffer w/ filters | same |
| Audio | channel scopes, mute/solo per channel | +DSP voice states, BRR source view |

Enhancement-side debug panels (de-flicker diff, stitcher canvas inspector,
profile decode preview) are provided by `rf-enhance` through the same panel
registry — one docking system, two providers.

## 4. Annotation store → profile export

The core RE workflow (GAME_PROFILES.md §3):

- Store: `addr-space:addr → { label, type (u8/u16/ptr/table/flags), notes,
  source_url, confidence }` + ROM-offset annotations; persisted per
  normalized ROM hash under the user data dir (SQLite or flat RON — decided
  at ticket time); import/export as JSON.
- Cross-links: breakpoint hits and trace rows render labels inline
  (`LDA $0086 {player_x_screen}`); the memory viewer colors annotated
  ranges; DataCrystal tables can be pasted via a small import dialog
  (address/len/label TSV).
- **Export to profile skeleton**: one command emits a valid profile TOML
  with `[meta]` (hashes filled), `[[memory_map]]` and `[[rom_map]]` rows
  from annotations (sources carried through), empty `[capabilities]`
  stubs — the authoring pipeline's step 2.

## 5. Lua console tie-in

The script tier (PLUGINS.md §1) doubles as the debugger's scripting console:
a REPL panel with `rf.mem`, `rf.trace.on`, `rf.bp.add`, `rf.gui.*` — the
BizHawk idiom for RE one-liners (e.g. log every write to an address with
stack context). Scripts saved from the console become plugin files.

## 6. Performance discipline

All debugger features must be pay-for-use: closed panels register no event
subscriptions (EventMask filtering in ARCHITECTURE §5); the breakpoint fast
path is one branch; viewers snapshot only visible regions. CI includes a
benchmark asserting Accuracy-mode frame time with debugger idle ==
debugger-compiled-out frame time within noise.
