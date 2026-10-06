# Design: UX Wave 20 — the play view tells the truth, then looks the part

Status: **planned and in progress on branch `ui/wave-17` (2026-10-06).**
Tickets W20-01..W20-21 in `plan.json`. Brad asked on 2026-10-06 to "write
all these down and build this out, also review the feature enhancements
we promised like full level view and all those super enhancements".

**Naming.** The request called this "Wave 17". Wave 17 on this board is
the SA-1 coprocessor (W17-01..04, done), and waves 18 and 19 are also
taken, so these tickets are **W20-xx / phase 20** — `validate-plan.mjs`
rule P3 requires the id prefix and the phase to agree, and ids are
unique. The working branch keeps the requested name `ui/wave-17`.

Source: the 2026-10-06 design review
(https://claude.ai/artifact/UAh9xB94EZjfyiY62qNpAb), whose findings were
read against `main` at `e80a883` and are restated below with file:line.
The companion audit of every enhancement this project has promised —
which ones a player can actually reach — is
`docs/design/ENHANCEMENT_AUDIT.md`.

## 1. Purpose and verdict

Wave 15 made the library and shell look like a product. The play view —
the screen a player spends 99% of their time on — still has controls
that **lie**: settings that are saved and never read, a shader setting
that is a text box waiting for a ticket that closed months ago, a pause
menu that doesn't pause. That is worse than missing features, because it
spends the trust the honesty badge exists to earn. So this wave runs in
three phases, and the order is the point:

1. **Fix what lies** (W20-01..W20-09). Every control does what it says,
   or is removed. Nothing new is added until this is true.
2. **Overlays** (W20-10..W20-16). The in-game experience other modern
   emulators have converged on (RetroArch's Quick Menu, DuckStation's
   OSD, Dolphin's performance overlay): a menu that pauses, feedback that
   appears on the picture rather than in a status string, rewind and
   recording a player can actually reach.
3. **Showcase** (W20-17..W20-21), only once 1 and 2 are green: wire the
   enhancements that are built but unreachable, explain the rest in
   player language, and make the library say what each game can do.

## 2. Principles

Carried over from FRONTEND_UI.md §1 and UX_WAVE_15.md §2 unchanged —
accuracy is the default face; honesty indicator, no silent enhancement;
the UI never blocks emulation; two audiences, two densities; everything
keyed by ROM hash; one launch gesture per device; theme is tokens.

New for this wave:

8. **A control either acts or is absent.** A setting that is persisted
   but read by nothing is a defect of the same class as a silent
   enhancement — it tells the user something untrue about the machine.
   Where wiring it is out of reach, the control goes, and the doc says
   why.
9. **Copy is for players.** Player-facing text never cites a ticket id,
   module path, or crate name. "Takes effect when you restart" is fine;
   "the window surface is created at startup (W3-02a)" is not. Research
   surfaces (Debug, Author) may stay technical.
10. **Symbols come from one icon font.** Every symbol the UI draws comes
    from the Phosphor icon font (`egui-phosphor`, MIT), not from whatever
    Unicode the body face happens to cover — the body face has shipped
    five tofu boxes so far (`◆`, `◇`, `●`, and found by this review `▦`,
    `⭐`). The glyph test covers **every** non-ASCII character in the
    shell's string literals, not a hand-maintained list.
11. **Feedback lands on the picture.** Save, load, screenshot, slot
    change, rewind and record are acknowledged by an OSD card over the
    game, with a thumbnail where one exists — not by a status-bar string
    the player is not looking at.

## 3. Findings (verified at `e80a883`)

| # | Pri | Finding | Where | Ticket |
|---|---|---|---|---|
| 1 | P0 | Video settings saved but never read: `ScaleMode` radio, shader free-text (its hint still says "arrive with W3-02a"), V-sync (`main.rs` `NativeOptions` never sets it). The play view always uses `Image::shrink_to_fit`. | app.rs:4735, 4754, 4767, 9051/9071/9090 | W20-01, W20-02 |
| 2 | P0 | `rf_renderer::ShaderChain` (Nearest, SharpBilinear, Scanlines, Crt, LcdGrid, Xbr + `ShaderParamDescriptor`) is constructed nowhere in `crates/retroforge`. | shader_chain.rs:89/112/504 | W20-02 |
| 3 | P0 | Esc toggles `overlay_menu` but never sends `CoreCommand::Pause`; no pad button opens it. | app.rs:5225 | W20-03 |
| 4 | P0 | No fullscreen anywhere. Rewind (`rf_state::rewind::RewindRing`, W8-02) and recording (`crate::recording::Recorder`, W8-03) not reachable; `AppAction::ALL` has 5 actions. | app_bindings.rs:63 | W20-04, W20-13, W20-14 |
| 5 | P0 | Tofu: grid/list toggle `▦`/`≡`, card badge `◆` (listed as verified-absent at app.rs:150), `⭐`, `⭑`, `⚠`, `⚡` (in the honesty badge itself), `✓`, `ℹ`, `•`, `→`. The glyph test only covers `PROPORTIONAL_GLYPHS`. | app.rs:5539/5550/6195/6211/5866/6175, enhance_ui.rs:250, toast.rs:69-71 | W20-05 |
| 6 | P1 | Save-state window prints "thumbnail"/"no thumbnail" instead of drawing it; mode/date use `selectable_label` as decoration. | app.rs:2241 | W20-06 |
| 7 | P1 | Run / Step Frame / Step Scanline + `f/sl` readout always in the status bar, even on the library with no ROM. | app.rs:4263 | W20-07 |
| 8 | P1 | Settings copy cites tickets/modules; audio device is free text; `Palette::LIGHT` exists with no toggle. | app.rs:4707, 4762 | W20-08 |
| 9 | P1 | Enhance features shown as jargon rows ("(generic, heuristic-gated) [shadow] — requires Enhanced mode"). | enhance_dock.rs | W20-18 |
| 10 | P1 | Menu bar + status bar never auto-hide in play (FRONTEND_UI §3.2 lists it unbuilt). | — | W20-11 |
| 11 | P2 | Library anonymous: 150 px cards, no continue-playing hero, no console identity, no enhancement chips. | app.rs:288 | W20-20 |
| 12 | P2 | Feedback is a status string: no OSD card for save/load/FF. | — | W20-12 |
| 13 | P0 | **New (audit):** "Full-level view" is offered (and counted by the badge) for any matched profile, but only profiles with a `[decode]` table decode a level — 2 of 11. On the other 9 the toggle does nothing. | enhance_ui.rs:113-136, level_view.rs:60 | W20-09 |
| 14 | P1 | **New (audit):** "Widescreen: decoded" is persisted per game but not re-applied when the game is reopened (`open_rom_path` restores sprite overlay, de-flicker, full-level — not widescreen), and the core path is SNES-only. | app.rs:2690-2705, stepper.rs:1028 | W20-09 |
| 15 | P0 | **New (audit):** "Atmosphere: fog" reads ON and counts toward the badge at ladder rung Active, but `rf_renderer::fog::FogPass` is never constructed by the app. | enhance_ui.rs:204, enhanced_view.rs:522 | W20-09, W20-17 |
| 16 | P0 | **New (audit):** MetalFX adds a badge suffix in a `metalfx` build, but `MetalFxScaler` is never constructed by the app. | enhance_ui.rs:270 | W20-09 |
| 17 | P1 | **New (audit):** Mesen HD packs have no UI route — `load_hd_pack` is called only by a test, so W11-05's "a user can import a pack from the app" is not true. | app.rs:7632 | W20-09 |

The review's claim that the widescreen toggle is wholly inert is
**wrong** and is corrected here: since W11-03 the toggle sends
`CoreCommand::SetWidescreen` (app.rs:9003, 8116). Finding 14 is the
narrower, true version.

## 4. Phase 1 — fix what lies

### W20-01 Scale mode and V-sync take effect
- The play view sizes the frame from `settings.video.scale_mode`:
  **Integer** (default; largest whole multiple of the 8:7-corrected frame
  that fits, letterboxed — RENDERER.md §2 rule (b): vertical integer,
  horizontal `y_scale * 8/7`), **Fit** (aspect-preserving fractional),
  **Stretch** (fill). Implemented as a pure `play_rect(available,
  frame_size, mode) -> Rect` with unit tests, used by all three
  `ActiveView` arms.
- V-sync applies **live** through `eframe::Frame::set_wgpu_surface_config`
  (eframe 0.35 `epi.rs:817`; egui-wgpu reconfigures the surface on the
  next paint, `winit.rs:525`) with `PresentMode::AutoVsync` /
  `AutoNoVsync`, and at startup through `NativeOptions::wgpu_options`.
  The "takes effect on restart" note goes.

### W20-02 Shader picker wired to ShaderChain
- The free-text field becomes a picker over `ShaderKind` (None + the six
  shaders), each with its manifest `display_name`; selecting one shows
  sliders generated from that shader's `ShaderParamDescriptor`s, values
  persisted per shader in `settings.toml`.
- The app builds one `ShaderChain` from its shared `GpuContext` and runs
  the selected stage over the displayed frame at an integer output scale
  (source × N, N chosen from the play rect, capped at 4), inside the same
  budget gate the diorama path uses. A failed pass falls back to the
  plain frame and says so once (FR-REND-007 shape) — never a black
  screen. Accuracy is untouched: the chain post-processes the presented
  picture only; capture/compare's `original` buffer stays pre-shader.
- The shader is a **presentation** choice, like MetalFX: it does not
  count as an enhancement and does not light the badge (RENDERER.md §4).

### W20-03 Pause means pause
- Opening the in-game menu (Esc, or pad **Home**, or **Select+Start**
  held together) sends `CoreCommand::Pause` if the core was running;
  closing it any way (Esc again, Resume, the window's ×) resumes only if
  the menu was what paused it. Space keeps its meaning (pause toggle,
  no menu). The pad chord is recognised from `rf_input::PadButton`
  state the app already polls.

### W20-04 Borderless fullscreen
- F11 and Cmd+Ctrl+F (macOS convention) toggle
  `ViewportCommand::Fullscreen`; also in the View menu and the Quick
  Menu. The windowed size and position are remembered in `settings.toml`
  and restored at startup (`ViewportBuilder::with_inner_size`). New
  `AppAction::Fullscreen`, remappable like the other app hotkeys.

### W20-05 One icon font
- Add `egui-phosphor = "=0.13.0"` (the newest release on egui 0.35;
  0.14 requires egui 0.36 and would break the lockstep pin), MIT OR
  Apache-2.0, font MIT. TECH_STACK §2 row; `cargo deny check licenses`
  before/after recorded.
- Register Phosphor Regular as a fallback in the proportional family
  (after the body face) in `theme::install_fonts`.
- Replace every symbol glyph: grid/list (`GRID_FOUR`/`LIST`), favourite
  (`STAR` / `STAR` fill-less), profile (`BOOKMARK_SIMPLE`), has-states
  (`FLOPPY_DISK`), enhanced-set (`SPARKLE`), warning (`WARNING`), info
  (`INFO`), success (`CHECK_CIRCLE`), bullet/arrow, and the honesty
  badge's `⚡` (`LIGHTNING`). The badge text format changes deliberately;
  its tests change with it.
- `glyphs_render.rs` gains a test that scans every `.rs` under
  `crates/retroforge/src`, extracts every non-ASCII character and
  `\u{…}` escape inside string literals (not comments), and asks the
  installed fonts for each. A self-test proves the scan finds a known
  character, so it cannot pass vacuously.

### W20-06 Save-state slot thumbnails
- The States window draws each slot's screenshot (already stored in the
  `.rfstate`) as a texture, 4:3-ish card with slot number, relative date,
  and mode-at-save as plain labels (not `selectable_label`). Empty slots
  show an empty card, not the word "no thumbnail".

### W20-07 Transport belongs to Debug
- Run / Step Frame / Step Scanline and the `f/sl` readout leave the
  player status bar and live in the Debug workspace's toolbar. The
  status bar on the library shows nothing that needs a ROM. Space still
  pauses; `hud_fits.rs` stays green.

### W20-08 Player copy, audio device list, Light theme
- Rewrite every player-facing string in Settings without ticket ids or
  module names (a test greps the rendered Settings tree's labels for
  `W\d+-\d+` and `crate::`).
- Audio output device is a dropdown of the devices `rf_audio` can
  enumerate (feature `audio`); without the feature, a disabled dropdown
  with "System default".
- Theme: System / Dark / Light / High contrast. `Palette::LIGHT` and the
  token table already exist; this exposes them.

### W20-09 Audit honesty fixes
- "Full-level view" is Available only when the matched profile actually
  decoded a level (`LevelSession` present); otherwise
  `Availability::NeedsProfile` with the reason "this game's profile has
  no level map". It never counts toward the badge while it does nothing.
- "Widescreen: decoded" is re-applied on ROM open when saved on, and is
  shown as unavailable ("SNES only") on NES, where no core path exists.
- "Atmosphere: fog" is never shown ON or counted by the badge while no
  fog pass runs in the live view; the row says "detected, not drawn yet"
  until W20-17 wires `FogPass`.
- The MetalFX badge suffix appears only when a `MetalFxScaler` actually
  processed the presented frame; until the scaler is wired, the Settings
  radio says "not used by the play view yet" and no suffix is added.
- File › "Load HD pack…" (and "Remove HD pack") call the existing
  `load_hd_pack`/`clear_hd_pack`, report the import summary as a toast,
  and are disabled on SNES (the pack format is NES-only).

## 5. Phase 2 — overlays

### W20-10 Quick Menu
Replaces the Esc window. Opens paused (W20-03), over a **dimmed frozen
frame** (the last presented texture, painted with a dark scrim — egui has
no blur, so dim it is, and the doc says so). Left rail: Resume · Save
state · Load state · Rewind · Display · Enhancements · Controls ·
Settings · Reset · Quit to library. Save/Load show a slot thumbnail grid
(W20-06's cards). A pad hint bar along the bottom (A select · B back ·
LB/RB tabs). The mode pill at the top keeps the honesty badge (principle
2): if anything is enhanced, the menu says so.

```
┌───────────────────────────────────────────────────────────────┐
│  NES · Enhanced (lightning)(2)                       ⏸ Paused │
│ ┌────────────┐ ┌───────────────────────────────────────────┐  │
│ │ ▶ Resume   │ │  Save state                                │  │
│ │   Save     │ │  ┌────┐ ┌────┐ ┌────┐ ┌────┐ ┌────┐        │  │
│ │   Load     │ │  │ 1  │ │ 2  │ │ 3  │ │ 4  │ │ 5  │  ...   │  │
│ │   Rewind   │ │  └────┘ └────┘ └────┘ └────┘ └────┘        │  │
│ │   Display  │ │                                            │  │
│ │   Enhance  │ │                                            │  │
│ │   Controls │ │                                            │  │
│ │   Settings │ │                                            │  │
│ │   Reset    │ │                                            │  │
│ │   Quit     │ │                                            │  │
│ └────────────┘ └───────────────────────────────────────────┘  │
│  (A) Select   (B) Back   (LB/RB) Section          dimmed game │
└───────────────────────────────────────────────────────────────┘
```

### W20-11 Auto-hiding chrome
In play (a core is running and no window has focus) the menu bar and
status bar slide away after 2.5 s without pointer movement and return
when the pointer moves into the top/bottom 48 px or any key/pad menu
action fires. Never hides while paused, on the library, or with the
honesty badge newly changed (a badge change re-shows the bar for 3 s —
principle 2 outranks tidiness).

### W20-12 OSD cards
A small card in the top-left of the play rect, 2 s, fading: "Saved to
slot 3" + thumbnail, "Loaded slot 3", "Screenshot saved", "Slot 4",
"Fast-forward ×N" while held, "Rewinding", "Recording". Replaces the
status-string feedback for those events; toasts stay for library-level
events.

### W20-13 Rewind
`RewindRing` (rf-state, W8-02) captured by the app at a fixed interval
from snapshots the core already produces for save states; hold
**Backspace** (new `AppAction::Rewind`, remappable) to step back through
it; while held a scrub bar shows the ring's extent and position. Off by
default (it costs memory), toggled in Settings › Video ▸ Rewind, with the
memory cost shown. Determinism: rewinding loads a state through the same
path as Load State — the simulation is never altered any other way.

### W20-14 Recording
`AppAction::Record` (default **F10**) toggles `crate::recording::Recorder`
on the presented frame; an OSD "● REC" indicator (Phosphor `RECORD`)
with elapsed time and estimated size; on stop the APNG is written to the
screenshots folder and an OSD card says where.

### W20-15 Performance overlay and input display
Optional (Settings › Video), off by default: a translucent corner panel
with FPS, frame time sparkline (last 120 frames) and the audio buffer
fill; an input display showing the pad state the core received this
frame.

### W20-16 Peek as a wipe
Hold-to-peek animates a vertical wipe between enhanced and original using
the existing compare-split composition (the same `compare_divider` the
Compare tab uses), 200 ms in/out, instead of a hard cut. Accuracy mode:
no-op.

## 6. Phase 3 — showcase

### W20-17 Wire what is built and safe
- **Loading fast-forward** (`rf_enhance::loading`, W8-07): for a profile
  with `[[loading.wait_loops]]`, the core thread unpaces while the PC is
  in a declared wait loop. Badge-counted, ledger-visible, off by default.
- **HUD separation** (`rf_enhance::hud`, W8-06): feeds the ultrawide /
  diorama HUD band from the profile's `[camera.hud]` (verdict surfaced
  in the Enhancements panel).
- Overlays (W8-12), smooth camera/room stitching (W8-08) and
  interpolation (W8-09, blending declined by design) stay unwired; the
  audit says why.

### W20-18 Player Enhancements panel
A card per feature: a before/after thumbnail pair (from the compare
buffers), one plain sentence of what it does, one plain sentence of what
it needs ("Needs a profile for this game", "Needs Enhanced mode"), and
the trust ladder in words — **Learning** (shadow) · **Suggesting**
(advisory) · **On** (active). The dense Enhance workspace stays as the
research surface.

### W20-19 Display panel
Live shader preview tiles (the current frame through each shader, small,
refreshed on open) and an ambient-glow letterbox option: the bars around
an integer-scaled picture take a blurred, darkened average of the frame
edge colours.

### W20-20 Library: continue, shelves, chips
A "Continue" hero for the most recently played game (large thumbnail,
Resume from last auto-state / Play), shelves (Recently played,
Favourites, NES, SNES), enhancement chips on cards (which profile
features a game has), and a console spine colour on each card.

### W20-21 Photographed tour
`capture_tour.rs` photographs every new surface: Quick Menu (each
section), OSD cards, rewind scrub bar, recording indicator, performance
overlay, shader picker + each shader, fullscreen-off/on chrome,
auto-hidden chrome, Enhancements panel, Continue hero. PNGs land in
`target/ui-tour/`.

## 7. Dependency graph

```
W20-01 ─> W20-02 ─> W20-19
W20-03 ─> W20-10 ─> W20-13, W20-18
W20-05 ─> W20-06 ─> W20-10, W20-12
W20-04, W20-07, W20-08, W20-09 — independent
W20-11, W20-14, W20-15, W20-16 — after Phase 1
W20-17 ─> W20-18
W20-20 after W20-05
W20-21 last
```

## 8. Testing

Every ticket: law-3 gate + `validate-plan` (+ `validate-traceability`,
which carries two pre-existing F3 failures on `main` at `e80a883` — a
decision id cited in plan.json and TESTING.md but not yet defined in DECISIONS.md — that this wave neither introduced nor touches).
UI behaviour is asserted headlessly with `egui_kittest` where the
harness can see it (pause sent on menu open, play rect sizes, glyph
presence, settings copy); pixels are photographed by `capture_tour`, and
anything only a human can judge is recorded as **UNVERIFIED (by eye:
Brad)** in the ticket notes, as this repo already does.

## 9. Out of scope

- No core changes (`crates/rf-snes/**` is being worked in another
  checkout; `crates/rf-nes/**` is not needed).
- No network features.
- No real-time AI pass (W16-07, held).
- No blur shader for the Quick Menu backdrop (egui has none; a GPU blur
  pass for one menu is not worth a new render path).

## 10. Note on stories[]

As with Wave 15 (UX_WAVE_15.md §15), no `USER_STORIES.md` entry is
specific enough to cite for this shell-only work, so the W20 tickets
carry `"stories": []` deliberately.
