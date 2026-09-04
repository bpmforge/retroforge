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

---

## 7. The "daily-drivable for ROM hackers" bar (W13-01, 2026-09-03)

`docs/VISION.md` §5 makes "debugger suite at *daily-drivable for ROM
hackers* quality" an 18-month success criterion. It was the only one of
those criteria with **no ticket, no test and no definition** — a phrase,
not a bar. This section replaces the phrase. §§1-6 above are the design and
are unchanged; this section grades what ships against them and states what
passing means.

### 7.1 Grading — §§1-6 against what ships

Every verdict cites the symbol or file that satisfies it. **A grade with no
citation is not a grade.**

| § | Promised | Ships | Verdict |
|---|---|---|---|
| 1 | run/pause/frame/scanline/instruction step | `EmuStepper` + `Step::{Instruction,Scanline,Frame}` (`rf-core-api/src/core.rs`) | **met** |
| 1 | step-over / step-out / run-to-cursor | `breakpoint::StepMode::{Into,Over,Out,ToCursor}`, `step_over_is_a_call` (JSR-only, tail-call safe) | **met** |
| 1 | breakpoints: PC exec | `Condition::Pc` | **met** |
| 1 | breakpoints: **memory read/write/access**, CPU *and* PPU spaces | `Condition::Watch` + `rf_core_api::WatchTable`, evaluated in the core's own buses (W13-02e) — **NES only**, see W13-02h | **met (NES)** |
| 1 | value-conditional `addr==X && val&mask` | `MemWatch::{value_mask,value_equals}` (W13-02e) | **met** |
| 1 | scanline/dot position (NES) | `Condition::Position` | **met** |
| 1 | H/V position (SNES) | nothing — and the SNES core emits no `CoreEvent` at all (W13-02h), so no core-reported break can arrive there yet | **GAP** |
| 1 | IRQ/NMI entry, mapper events | `EventKind::{Irq,Nmi,MapperIrq}` | **met** (bank-switch not distinguished) |
| 1 | zero cost when the table is empty | `BreakpointTable::check`, and `benches/debugger_idle.rs` measures it | **met** |
| 1 | watchpoint "promoted" to a `memory_map` annotation in one click | `annotation::promote_watch` + the Annotations panel's `promote` button (W13-02e) | **met** |
| 2 | per-chip rings, CPU/PPU/APU/DMA/mapper | `TraceKind::ALL` — all five | **met** |
| 2 | never blocks the core thread; overflow drops oldest + truncation flag | two-ring split (transport drops newest, `TraceScrollback` drops oldest), reasoned from rtrb 0.3's own API in the module doc | **met** |
| 2 | NES trace **nestest.log-compatible** | `rf_nes::trace::format_trace_line`, shared with the golden diff | **met** |
| 2 | SNES trace in **bsnes convention** (bank:addr, m/x-aware) | nothing | **GAP** |
| 2 | trace-to-file, lz4 framing, background writer | `lz4_flex` 0.14 + `trace_capture::write_loop` (`finish()` called explicitly) | **met** |
| 3 | Pattern/CHR — NES | `pattern::decode_pattern_table` + `tile_to_rgba` | **met** |
| 3 | Nametable — NES, scroll + mirroring + attributes | `nametable::decode_nametable` | **met** |
| 3 | Palette — NES 32-entry | `palette::decode_palette` | **met** |
| 3 | OAM — 64 entries, per-scanline occupancy, `dropped_by_limit` | `oam::{decode_oam,scanline_occupancy,dropped_by_limit,diff_oam}` | **met** |
| 3 | Event viewer — dot/scanline scatter | `event_timeline::build_timeline` | **met** |
| 3 | Trace viewer with filters | `trace::TraceScrollback` + `DebugTab::Trace` | **met** |
| 3 | Audio — channel scopes, mute/solo | `audio_scope::{MuteState,mix_host_side,trace}` | **met** |
| 3 | Memory hex — **live edit (pause-gated), goto/find, annotation coloring** | `memory_view::{build_rows,byte_at}` only. The panel has none of the three; the "find" box in `debug_dock.rs` is the *trace* filter. | **GAP** |
| 3 | **the entire SNES column** — 2/4/8bpp CHR, per-BG tilemaps, Mode 7 view, CGRAM 256 + color math, 128-entry OAM w/ 32-per-line, VRAM/CGRAM/ARAM spaces, HDMA lanes, DSP voice + BRR | **nothing.** No provider in `rf-debugger/src` mentions SNES; `DebugTab` has nine variants and none is console-aware. | **GAP (the largest)** |
| 4 | annotation store, typed, with `source_url` + confidence | `annotation::{Annotation,AnnotationStore}`, sourceless entries refused at `add` | **met** |
| 4 | DataCrystal TSV import | `datacrystal::parse_tsv` | **met** (library only — see below) |
| 4 | export to profile skeleton | `profile_export::export_skeleton` | **met** (library only — see below) |
| 4 | **the workflow reaches a user** | `DebugTab::Annotations` — create/edit/delete, TSV import, and an export that lands in the profile editor (W13-02f) | **met** |
| 4 | persisted per normalized ROM hash; JSON import/export | `crate::annotation_store` in the frontend — `<config>/retroforge/annotations/<hash>.rfannot`, and the stored file *is* the interchange file (W13-02f) | **met** |
| 4 | cross-links: labels inline in trace rows | `annotation::label_operands`, applied in the trace panel (W13-02f) | **met** |
| 4 | cross-links: annotation coloring in the memory view | nothing | **GAP** (W13-02d) |
| 5 | Lua console REPL (`rf.mem`, `rf.bp.add`, …) | `DebugTab::LuaConsole` (W4-04), reachable | **met** |
| 6 | pay-for-use; idle == compiled-out within noise, **measured** | `crates/retroforge/benches/debugger_idle.rs` | **met** |

**One root cause sits under most of the SNES rows, and it is worth naming
once rather than nine times.** `SnesCore::state_view()`
(`crates/rf-snes/src/core.rs`) returns **empty slices** for `cpu_regs`,
`vram`, `cgram`, `oam`, `ppu_regs` and `mapper_state` — only `wram` is
real. The shell then answers `None` for `bus()`, `bus_mut()` and `cpu()`
on `Machine::Snes` (`crates/retroforge/src/stepper.rs`). So a SNES debug
panel has no data source to render *even if it were written*. This is the
half-console shape W11-10's close note predicted, arriving on the debugger
side: the trait was adopted, and one implementation of it is a stub.

### 7.2 The bar

**B-0 — the walkthrough, which is what "daily-drivable" means.** A ROM
hacker, using only the running application and no source edits, can: load a
game, find an unknown quantity in RAM by watching it change, label it,
promote the label to an annotation with a source citation, export a profile
skeleton, and load that profile back — **on either console**. B-1 to B-9
are that sentence decomposed into pass/fail parts; B-0 is passed by an
end-to-end test that performs it, not by inspection.

1. **B-1 — one debugger, two consoles.** Every viewer with a SNES column in
   §3 renders real data with a SNES ROM loaded. *Test: an app-level test
   opens the RF-Scroller-S fixture and asserts each provider returns
   non-empty output.*
2. **B-2 — no stub `StateView`.** No core returns an empty slice for a
   memory the console physically has. *Test: a per-core assertion over
   every `StateView` field.*
3. **B-3 — memory is an editor, not a dump.** Goto, find, live edit gated
   on pause, and annotated ranges visibly coloured.
4. **B-4 — real watchpoints.** Break on memory read / write / access in the
   CPU *and* PPU address spaces, with a value+mask condition.
5. **B-5 — one action promotes a watchpoint to an annotation** (address,
   size, label), per §1's last bullet.
6. **B-6 — the export path is reachable from the UI**, not only from
   tests: annotations → profile skeleton without leaving the window.
7. **B-7 — annotations survive a restart**, keyed by normalized ROM hash,
   with JSON import/export.
8. **B-8 — labels appear where the work happens**: inline in trace rows
   (`LDA $0086 {player_x_screen}`) and as colour in the memory view.
9. **B-9 — the SNES trace is diffable against a reference emulator**
   (bsnes convention: bank:addr, m/x-aware disassembly).
10. **B-10 — pay-for-use is not lost while doing the above.**
    `debugger_idle` stays within noise of debugger-compiled-out. *Already
    met; carried as a regression criterion, not new work.*

**Not in the bar, deliberately.** Nothing here asks for features §§1-6 do
not already promise. This is a conformance bar against the project's own
design doc plus the reachability standard the W10/W11 arc established — a
capability that only tests can reach is not a capability a ROM hacker has.

### 7.3 What closes it

`W13-02a` … `W13-02g` in `plan.json` (**32 points**, filed by W13-01).
`W13-02a` is the unblocker for the SNES half: until `SnesCore::state_view`
is real, every SNES viewer has nothing to draw.

**Closed so far: `W13-02e`, `W13-02f`.**

`W13-02e` (2026-09-04) — bars **B-4** and **B-5**. Watchpoints are
evaluated **inside the core** and reported as
`CoreEvent::MemWatch`, a variant that had been in the contract since
W4-00 with no producer. It also found that the SNES core emits **no
`CoreEvent` of any kind** (`grep` returns zero lines), so every
event-borne debugger facility is NES-only whatever its own code says —
filed as `W13-02h`.

`W13-02f` (2026-09-04) — bars **B-6**, **B-7** and the
trace half of **B-8**, which is most of §4. It also found and fixed a
second reachability wall on the way: **a tab that is in `DebugTab` but in
no layout could not be opened at all**, because `restore_layout` only
shows what the persisted file names and nothing could add one. `LuaConsole`
(§5, shipped W4-04) and `OamDiff` (FR-DBG-006, shipped W4-06c) were both in
that state — working UI nobody could reach. The "Add panel" picker
(`DebugTab::ALL`) opens any of them.

**Worth knowing before trusting a layout claim:** the dock persists to
`std::env::temp_dir()/retroforge-debug-dock-layout.toml` — a
machine-global path, *not* the config root — so it is shared across
sessions and is not isolated by `RETROFORGE_CONFIG_DIR`. Putting a new tab
in `default_layout` therefore reaches a fresh install and nobody else,
which is why the picker is the load-bearing half.
