# Design: Frontend / UI (`retroforge` bin)

Scope: the application shell — information architecture, screens, panel
specs, and UX workflows. Framework: egui/eframe 0.35 + egui_dock 0.20
(docking) over winit; game viewport is a wgpu texture inside the egui frame.
Requirements source: SRS FR-FE; tickets W1-06, W2-06, W2-07, W4-05, W4-06,
W5-03 (+ Phase 8/9 additions).

## 1. UX principles

1. **Accuracy is the default face.** Fresh install boots to Accuracy Mode;
   the window shows the game and nothing else. Enhancement is discoverable,
   never presumed.
2. **Honesty indicator.** Whenever ANY enhancement is active, a persistent
   "ENHANCED" badge appears in the status bar with a hover breakdown of
   active features + one-click "view original" (hold-to-peek). No silent
   enhancement, ever (FR-MODE).
3. **The UI never blocks emulation.** Panels read from FrameBundle /
   debugger ring buffers; a stalled panel drops frames of *telemetry*, not
   frames of *game*.
4. **Two audiences, two densities.** Player surfaces (library, settings) are
   sparse and controller-navigable (Phase 8); research surfaces (debugger,
   profile author) are dense, keyboard-first, dockable.
5. **Everything keyed by normalized ROM hash** — settings, states, layouts,
   profiles — so files can be renamed/moved freely.

## 2. Information architecture

```
App
├── Library (home)                    W2-07
├── Play view (per game)              W1-06
│   ├── status bar: mode badge · FPS · A/V sync · profile chip
│   └── overlay menu (Esc): resume · states · settings · switch mode · quit
├── Workspaces (dockable, per-game layout persisted)
│   ├── Debug        (Research/Debug Mode)        W4-06
│   ├── Enhance      (compare + feature toggles)  W4-05, W3-04
│   └── Author       (profile editor)             W4-06 → W9
├── Settings (app-wide)
│   ├── Input (remap, devices)        W2-06
│   ├── Video (scaling, shaders, vsync, display mode)
│   ├── Audio (device, latency, volume)
│   ├── Paths (library folders, cache location & size cap)
│   └── Plugins (manager)             W4-04 → W9
└── Modals: ROM open · save-state manager · screenshot/recording
```

## 3. Screen specs (text wireframes)

### 3.1 Library (home)

```
┌ RetroForge ──────────────────────────────────────────────┐
│ [Search…]  [NES ▾] [SNES ▾] [Recently played ▾]    (⚙)   │
│ ┌─────┐ ┌─────┐ ┌─────┐ ┌─────┐                          │
│ │boxsh│ │     │ │     │ │     │   grid of games;         │
│ │ ot/ │ │     │ │     │ │     │   badges: ⭑profile       │
│ │thumb│ │     │ │     │ │     │   ✓states  ◆enhanced-set │
│ └─────┘ └─────┘ └─────┘ └─────┘                          │
│ Selected: <title>  [▶ Play] [Play in mode ▾] [Settings]  │
└──────────────────────────────────────────────────────────┘
```

- Scans configured folders; identifies by normalized hash (rf-cart);
  unidentified ROMs still playable (shown with generic card).
- **Empty/first-run state** (design review G-21): no folders configured ⇒
  the grid area shows a single call-to-action card ("Add a ROM folder…" →
  Paths settings) plus a drag-and-drop target; a folder with zero
  recognized ROMs says so explicitly ("0 ROMs found in <path>") rather
  than rendering an empty grid. No network fallback, ever (NON_GOALS #5).
- Thumbnail = last save-state screenshot or first-frame capture. No
  box art scraping v1 (network-free principle).
- Per-game context menu: settings, open file location, hash info (RA/No-Intro
  cross-ref), "author profile…".

### 3.2 Play view + overlay menu

Game viewport centered, integer-scaled with configurable aspect handling;
everything else hidden until Esc/Start-long-press. Status bar (thin, can
auto-hide): `NES · Accuracy` or `NES · Enhanced ⚡(3)` badge · FPS ·
audio-buffer health dot · profile chip (name@rev, click → inspector).

Save-state manager modal: 10 slots + auto-slots, each with screenshot,
timestamp, mode-at-save, "contains mods" warning flag (PROF chunk); load
warns on version-migrated states.

### 3.3 Enhance workspace (the platform's showpiece)

```
┌ tabs: [Compare] [Features] [Map] ────────────────────────┐
│ Compare: ┌ Original ────────┐┌ Enhanced ────────┐        │
│          │ 256×240 (PPU)    ││ ultrawide/decoded │       │
│          └──────────────────┘└───────────────────┘       │
│          linked pause/step · difference-highlight toggle │
│ Features: per-feature rows w/ toggle · scope · provenance│
│   [x] Sprite-limit bypass      (generic)      ⓘ         │
│   [x] De-flicker: temporal     (generic, tuned by profile)│
│   [ ] Widescreen: decoded      (requires profile ✓ present)│
│   [ ] Full-level view          (requires profile ✓ present)│
│   rows disabled+explained when no profile capability      │
│ Map: stitched/decoded canvas · fog for unvisited ·        │
│   live player marker · original-viewport outline · export │
└──────────────────────────────────────────────────────────┘
```

Feature rows are generated from capability flags (profile `[capabilities]` +
generic feature registry) — the UI cannot offer what the honesty contract
(ARCHITECTURE §2) says is unavailable.

### 3.4 Debug workspace

egui_dock tree, default layout (persisted per game, resettable):

```
┌ CPU ─────────┐┌ Game view (embedded, step controls) ┐┌ PPU state ┐
│ regs/disasm  ││  ⏸ ▶ ⭢frame ⭢scanline ⭢instr       ││ v/t/x/w…  │
├ Breakpoints ─┤├ Pattern │ Nametable │ OAM │ Palette ┤├ Event view┤
│ +watchpoints ││  viewers (hover = address + tooltip │ │ frame     │
├ Memory hex ──┤│  click = annotate)                  │ │ timeline  │
└ Trace ───────┘└─────────────────────────────────────┘└───────────┘
```

Every viewer cell click-through: hover shows address/tile id/palette entry;
click opens "annotate" (label/type/notes/source) feeding the annotation
store (DEBUGGER.md) — this is the profile-authoring on-ramp. Lua console
docks here too (W4-04).

### 3.5 Author workspace (Phase 4 minimal → Phase 9 full editor)

Left: annotation table (filter/search, DataCrystal-shaped columns). Center:
live decode preview — profile TOML (external editor or embedded text editor)
hot-reloads on save; decoded level/tilemap renders immediately with error
list inline. Right: profile inspector (identity match status, capability
checklist, validation output). Actions: "export skeleton from annotations",
"validate", "run golden screenshot test".

### 3.6 Plugin manager

List: name@version · tier (script/native) · capability chips (read-mem,
overlay, replace-layers, WRITE-MEM in red) · state (enabled/paused/error w/
last error text) · per-plugin budget usage meter. Enabling a `write_memory`
plugin requires typed confirmation and shows the mod ledger. Install =
drop-in folder scan v1; no marketplace.

## 4. Input & remapping UX

- Remap screen shows a virtual controller (NES/SNES per core), click-a-
  button → press-a-key/pad flow; per-game overrides; conflicts surfaced
  inline. Hotkeys (save/load state, fast-forward, screenshot, mode peek) in
  the same system, namespaced so game bindings and UI bindings can't clash.
- Gamepad navigation of Library/Play overlay is Phase 8 (FR-FE priority P2);
  keyboard+mouse is fully sufficient through MVP.

## 5. Capture & media

- Screenshot (hotkey): pre-shader 1× indexed capture + post-shader window
  capture, both saved; compare view captures both panes (W3-04).
- Video recording: Phase 8 — encode from the render-thread texture queue
  (ffmpeg sidecar or gstreamer TBD at W8 planning), never on the core thread.
- Rewind: Phase 8 (SAVE_STATES.md §4); UI = hold-key scrub with ghost
  timeline.

## 6. Accessibility & platform

- UI scale slider (egui pixels_per_point), high-contrast theme, colorblind-
  safe palettes for debug/heat overlays (no red/green-only encodings),
  screen-reader labels on player-facing surfaces where egui supports it;
  translation/accessibility *game* overlays are the AI-pipeline's job
  (ENHANCEMENT_RUNTIME.md §6), not the shell's.
- Targets: macOS (primary dev), Windows, Linux — all via winit/wgpu; no
  platform-specific UI code outside file dialogs (rfd crate).

## 7. State & persistence

`~/.retroforge/` (platform-appropriate config dir): `settings.toml` (app),
`games/<sha256>/` (per-game settings, states, dock layouts, thumbnails),
`profiles.d/` (user profile overrides), `cache/` (rf-cache, size-capped LRU).
All hash-keyed, all human-inspectable.

## 8. Phasing (maps to plan.json)

| Phase | Frontend deliverable | Ticket |
|---|---|---|
| 1 | window + viewport + step controls + ROM open | W1-06 |
| 2 | library, per-game settings, remapping, save-state modal | W2-06/07, W2-04 |
| 3 | shader/scaling settings, compare view, screenshot | W3-02/04 |
| 4 | mode toggles + honesty badge, Enhance workspace, Debug workspace, Lua console | W4-04/05/06 |
| 5 | Map tab (full-level demo surface) | W5-03 |
| 8 | rewind UI, video recording, gamepad nav, accessibility pass | W8-* (planning ticket) |
| 9 | full profile editor, plugin manager polish | W9-* (planning ticket) |
