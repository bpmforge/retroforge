# Design: UX Wave 15 — player-facing library and shell polish

Status: **plan-only, on hold (Brad, 2026-09-17).** Documented now so the
work is ready to claim; no ticket in this wave may be claimed until Brad
says go. See `plan.json` tickets W15-01..W15-08, each holding a
`"hold": true` flag and a HOLD note.

Source: `retroforge-ux-review.html` (2026-09-17 UX review of
`crates/retroforge/src` against this project's own spec and a survey of
Mesen 2, RetroArch, ES-DE, Playnite, Pegasus, DuckStation, Dolphin and
Steam). This document restates and expands that review's recommendations
into an implementable plan; it does not re-litigate the review's findings.

## 1. Purpose and verdict

The engine-facing parts of the shell are strong and unusually honest: the
mode badge, hold-to-peek, the three first-run states (G-21), always-visible
scrollbars, and controller navigation that rides egui's real focus so
screen readers see it. The player-facing part is not slick yet: the
library is a text list with one launch gesture, no memory of what was
played, no thumbnails, no context menu, and every secondary surface is a
plain `egui::Window`.

FRONTEND_UI.md §3.1 already specifies most of what's missing — a card
grid, recently-played, a selected-game action bar, a context menu. The
fastest path is to build the screen the spec already drew, in egui's own
idiom, before touching theme or fonts. That ordering is why thumbnails and
the grid (W15-05) are sequenced after launch/selection (W15-01),
recency/favourites (W15-02) and modals/menus (W15-03/04): each of those is
useful with plain rows, so the list stays the accessible, always-shippable
fallback while the grid is built alongside it.

## 2. Principles

Carried over from FRONTEND_UI.md §1:

1. Accuracy is the default face.
2. Honesty indicator — no silent enhancement, ever.
3. The UI never blocks emulation.
4. Two audiences, two densities (player surfaces sparse and
   controller-navigable; research surfaces dense, keyboard-first).
5. Everything keyed by normalized ROM hash.

New for this wave:

6. **One launch gesture per input device, at minimum.** Mouse
   double-click, Enter, and gamepad A must each launch the selected game;
   Play stays visible as the explicit, discoverable affordance for anyone
   who hasn't learned the gestures. Removing Play to make room for a
   gesture is not acceptable — the field survey (Playnite's issue tracker
   in particular) shows double-click semantics are the most contested part
   of library UX precisely because teams keep trying to replace the
   button instead of adding to it.
7. **Theme is a token set, not scattered literals.** Every color, radius,
   and spacing value a screen uses comes from the token table in §8, not
   a literal `Color32::from_rgb(...)` written at the call site. A screen
   that needs a color the table doesn't have gets a new token, not a
   one-off value — this is what keeps the high-contrast palette and any
   future theme swap mechanical instead of a grep-and-pray exercise.

## 3. Target library screen (text wireframe)

```
┌ RetroForge ──────────────────────────────────────────────────────┐
│ [Search…]  [All ▾][NES][SNES]  [Recently played][Favourites]      │
│                                   Sort: [Title ▾]      [▦][≡] (⚙) │
│ ┌─────────┐ ┌─────────┐ ┌─────────┐ ┌─────────┐                  │
│ │ thumb   │ │ thumb   │ │ thumb   │ │ thumb   │                  │
│ │ [NES]   │ │ [SNES]  │ │ [NES]   │ │ [SNES]  │  grid of cards   │
│ │ Title   │ │ Title   │ │ Title   │ │ Title   │  ⭑profile        │
│ │ ⭑ ✓ ◆   │ │   ✓     │ │ ⭑       │ │   ✓ ◆   │  ✓states         │
│ └─────────┘ └─────────┘ └─────────┘ └─────────┘  ◆enhanced-set   │
│                                                                    │
│ Selected: <title>            [▶ Play] [Play in mode ▾] [Settings] │
└────────────────────────────────────────────────────────────────────┘

List view (toggle, same toolbar):
┌ RetroForge ──────────────────────────────────────────────────────┐
│ [Search…]  [All ▾][NES][SNES]  [Recently played][Favourites]      │
│                                   Sort: [Title ▾]      [▦][≡] (⚙) │
│ thumb  Title A            NES   ⭑ ✓ ◆        [▶ Play]             │
│ thumb  Title B            SNES    ✓          [▶ Play]             │
│ thumb  Title C            NES   ⭑            [▶ Play]             │
│ Selected: <title>            [▶ Play] [Play in mode ▾] [Settings] │
└────────────────────────────────────────────────────────────────────┘
```

**Toolbar:** search box; console filter chips (All / NES / SNES, exactly
as today); two new filter chips, Recently played and Favourites, which
combine with the console filter (AND) but not with each other (radio-like:
selecting one clears the other, since "recent favourites" is a sort
question, not a second filter, at this wave's scope); a sort control
(Title, Last played, Console); a Grid/List toggle; the existing Settings
gear.

**Card:** thumbnail (or a generic placeholder tinted by console when none
exists yet — never a picture of a game that isn't there), title, a
console badge (NES/SNES), and up to three status badges from §3.1:
⭑ profile present, ✓ has save states, ◆ enhancement set active. Clicking
a card selects it (focus ring); double-click launches.

**Selected-game action bar:** appears once a game is selected (row or
card), pinned above/below the grid depending on space: title, Play,
"Play in mode ▾" (a dropdown of the five mode presets from FR-MODE-001),
and Settings (opens the single Game Settings window, §5).

**Context menu** (right-click on desktop, gamepad long-press on a
focused card/row — see W15-08): Play, Play in mode ▸, Favourite /
Unfavourite, Game settings…, Show in Finder/Explorer, Hash info (RA/
No-Intro cross-ref, per §3.1).

## 4. Thumbnail and art pipeline

Three sources, in priority order, all keyed by the game's normalized ROM
hash (principle 5):

1. **Save-state screenshot.** The most recent save-state's screenshot
   (already captured per `SAVE_STATES.md` — 10 slots + auto-slots each
   carry one) is the freshest, most representative image of a game
   actually played.
2. **First-frame capture.** On a game's first successful boot, capture
   the first rendered frame once (not every launch) and cache it. This is
   what covers a game with no save state yet.
3. **User-supplied art folder.** A folder configured in Settings › Paths,
   matched by normalized title (the DuckStation pattern) — stays entirely
   local, no network.
4. **libretro-thumbnails fetch (opt-in, off by default).** Per **D-011**
   (`docs/DECISIONS.md`), fetching box art / metadata from
   libretro-thumbnails or similar community sources (ScreenScraper, IGDB,
   SteamGridDB) is permitted as an explicitly opt-in network feature,
   never required, no accounts. A Settings toggle ("Fetch box art from
   the internet") gates it; when it is off, sources 1-3 are all that run.
   A **cache cap** (size-bounded LRU, alongside the existing `rf-cache`
   size cap in Settings › Paths) bounds what the fetch can store, an
   **attribution line** ("Art: libretro-thumbnails") is shown wherever
   fetched art appears (Settings and, on hover, the card itself), and a
   small **network indicator** (a dot or icon distinct from the local-art
   case) marks any thumbnail that came from a fetch, so the honesty
   principle (§2.2) extends to art the way it already does to
   enhancement: nothing on screen should imply "local" when it wasn't.
   NON_GOALS #5 (no ROM distribution, no "get games" UX) is untouched —
   this fetches pictures and text, never ROM data.

**Storage:** all three local/cached forms live in the cache directory
Settings › Paths already manages, keyed by ROM hash — see §7's crate-scope
note on `rf-cache` vs. `retroforge`. Cache eviction follows the existing
size-capped LRU (`rf-cache`, W4-08).

## 5. Modals and toasts

- **`egui::Modal`** (built into egui since 0.31) for anything that must
  block a decision: confirm overwrite of a save slot, quit with unsaved
  state, and the crash report. Its dimmed backdrop and outside-click
  dismissal replace the ad hoc plain-`egui::Window` treatment these three
  have today.
- **`egui-notify`** (MIT-licensed, maintained) for non-blocking status:
  ROM folder added/rescanned, state saved, script error. This replaces
  the plain status string the script-error path uses today (the gap the
  review calls out against user story E7-S1).
- **Existing dialogs and their destination:**

  | Dialog today | Becomes |
  |---|---|
  | Save-state overwrite (plain window) | `egui::Modal` |
  | Quit-with-unsaved-state (plain window) | `egui::Modal` |
  | Crash report (plain window) | `egui::Modal` |
  | Script error (status string) | `egui-notify` toast |
  | ROM added / folder rescanned (no UI today) | `egui-notify` toast |
  | Save-states manager, Settings, Controls remap, Enhance workspace, Layers debug, Debug viewers, Author | unchanged — these are workspaces/browsers, not blocking decisions or transient status, so they stay plain windows/docks |

- **One Game Settings window** (W15-03) consolidates what the Enhance
  menu and Enhance workspace scatter today: Mode, De-flicker, and
  Heuristics on one window, opened from the context menu and from the
  overlay menu (both routes lead to the same window instance).

## 6. Hotkeys

A new **App** section in the Controls (remap) window, namespaced apart
from game bindings so the two can never collide (per FRONTEND_UI §4):

| Action | Default (keyboard) | Default (gamepad) |
|---|---|---|
| Save state (active slot) | F5 | — (remappable) |
| Load state (active slot) | F9 | — (remappable) |
| Fast-forward (hold) | Tab (hold) | — (remappable) |
| Screenshot | F12 | — (remappable) |
| Hold-to-peek (view original) | Backtick (hold) | — (remappable) |

All five are remappable in the Controls window like any other binding.
The overlay menu (Esc) shows the current binding next to each action it
lists, so bindings are learned by seeing them rather than by reading a
manual — directly addressing the "hotkeys the spec already lists" gap.

## 7. One window-opening idiom

**Decision: commands with visible open state.** Every secondary window
(Settings, Controls, Save states, Author, Enhance workspace, Debug
viewers, Layers debug, the new Game Settings window) opens from a
command — a menu item or a button — and that command shows whether the
window is currently open (a checkmark in the View menu, or a pressed/
highlighted state on a toolbar button), consistently, everywhere. This
retires the current split where some windows open only from checkboxes
in View and others only from buttons in the overlay.

## 8. Theme and typography tokens

A token table, so every screen pulls from the same set (principle 7)
rather than literals at the call site:

| Token | Role | Light | Dark | High-contrast |
|---|---|---|---|---|
| `bg` | App background | — | — | — |
| `surface` | Card/window background | — | — | — |
| `ink` | Primary text | — | — | — |
| `muted` | Secondary text | — | — | — |
| `line` | Borders/dividers | — | — | — |
| `accent` | Interactive/selection | — | — | — |
| `accent-soft` | Selection background | — | — | — |
| `ok` / `ok-soft` | Success semantic | — | — | — |
| `warn` / `warn-soft` | Warning semantic | — | — | — |
| `error` / `error-soft` | Error/hard semantic | — | — | — |
| `radius-sm` / `radius-md` | Corner radii | — | — | n/a |
| `space-1..5` | Spacing scale | — | — | n/a |

Exact color values are an implementation detail of W15-07 (they should be
derived from the existing five-colour accessibility set already measured
for WCAG ratios, not invented fresh — see FRONTEND_UI §6), but the table
shape above is fixed: any new UI surface written under this wave names
its colors from this table, and W15-07 is what actually wires the table
into `apply_theme` in `app.rs` and re-derives high-contrast from the same
tokens rather than hand-tuning a second palette.

**Fonts:** one distinctive display face for headings/badges and a clean
body face, both embedded (no network fetch for the UI's own fonts —
distinct from the opt-in art/metadata network feature in §4). Licence
must be OFL or Apache, per the project's licence policy.

**Motion:** the existing "one moving element" rule (today: hover
elevation on the Run button) is right and stays the ceiling — this wave
adds hover elevation on cards and a short fade on modal open/close, and
nothing else.

## 9. Controller-first library

Once the grid exists (after W15-05): four-direction navigation between
cards/rows using the existing pad-to-focus bridge (`ui_nav.rs`), a large
and clearly visible focus ring (larger than the mouse-hover focus ring),
larger type when a gamepad is the most-recently-active input device, and
Start opens the context menu (§3) on the focused item. AccessKit inherits
the same focus state egui already tracks, so no separate accessibility
path is needed here.

## 10. Accessibility requirements

- Every interactive element (card, row, chip, toolbar button) has a
  visible focus ring meeting WCAG 2.2 non-text contrast (3:1 against
  adjacent colors) in both the default and high-contrast palettes.
- AccessKit labels on all new player-facing controls (search box, filter
  chips, sort control, grid/list toggle, cards/rows, action bar buttons,
  context menu items) — matching the existing coverage on player-facing
  surfaces per FRONTEND_UI §6.
- Text and badge contrast meets WCAG 2.2 AA at minimum against both
  `surface` and any thumbnail-overlay background; badges over a thumbnail
  get a scrim rather than relying on the image's own contrast.
- The list view stays the fully accessible fallback: it must remain
  functionally complete (search, filter, sort, launch, context menu) on
  its own, not merely as a lesser view of the grid.

## 11. Acceptance criteria per ticket

See `plan.json` W15-01..W15-08 for the authoritative, checkable acceptance
lists. Summary:

- **W15-01** — double-click, Enter, and gamepad A each launch the
  selected item; a visible focus/selection ring exists for keyboard and
  pad; Play remains on-screen.
- **W15-02** — last-played timestamp and play count recorded per ROM hash
  in the existing per-game settings file; Recently played and Favourites
  filter chips; sort control (Title/Last played/Console); empty search
  defaults to recent-first.
- **W15-03** — right-click and gamepad long-press context menu with the
  items in §3; one Game Settings window reachable from both the menu and
  the overlay.
- **W15-04** — save-state overwrite, quit-with-unsaved-state, and crash
  report become `egui::Modal`; `egui-notify` toasts for ROM
  added/rescanned and script error.
- **W15-05** — thumbnails from save-state screenshot / first-frame
  capture / user art folder, keyed by ROM hash, in the cache dir; card
  grid with the badges in §3; Grid/List toolbar toggle, list kept fully
  functional.
- **W15-06** — App hotkey section in Controls with the five defaults in
  §6, all remappable, bindings shown in the overlay menu.
- **W15-07** — token table in §8 wired into `apply_theme`; embedded
  OFL/Apache display + body fonts; high-contrast palette re-derived from
  the same tokens; hover elevation on cards; modal fade.
- **W15-08** — four-direction grid navigation, large focus ring, larger
  type on pad-active, Start opens context menu.

## 12. Dependency graph

```
W15-01 (launch gestures + selection)
  ├─> W15-02 (recently played / favourites / sort)
  └─> W15-03 (context menu + Game Settings)

W15-04 (modals + toasts)                 — independent

W15-01, W15-04 ─┬─> W15-05 (thumbnails + card grid)
                │      ├─> W15-07 (theme + typography)
                │      └─> W15-08 (controller-first library)

W15-06 (app hotkeys)                     — independent
```

Rationale: the grid (W15-05) needs a selectable item (W15-01) and the
modal/toast plumbing (W15-04) is used by its save-triggered toast paths;
theme (W15-07) and controller-first (W15-08) both need real cards to
theme and navigate, so both wait on W15-05. W15-02, W15-03 and W15-06 can
run in parallel with each other and with W15-04 once W15-01 lands.

## 13. Explicitly out of scope

- No accounts, no telemetry (NON_GOALS #6).
- No ROM store, no "get games" UX, no ROM distribution (NON_GOALS #5,
  untouched by D-011).
- No generic box-art scraping *by default* — the opt-in toggle in §4 is
  the only network path, and it fetches art/metadata only, never ROM
  data.
- No new enhancement heuristics, no core changes — this wave is shell-only
  (`crates/retroforge/**`, plus `crates/rf-cache/**` where the art cache
  belongs, per W15-05).
- No wasmtime/plugin-marketplace work (Phase 9 territory, NON_GOALS #17
  context).

## 14. Open questions for Brad

1. Which art source ships first when the opt-in fetch is built — plain
   libretro-thumbnails only, or libretro-thumbnails plus a
   ScreenScraper/IGDB option from day one? (Affects W15-05's scope and
   whether it needs an API-key/settings field for a scraper account.)
2. Does the list view stay the *default* view until real art exists in a
   user's library, switching the default to Grid only once thumbnails are
   present for a majority of scanned games — or does Grid become default
   immediately (with placeholder cards) once W15-05 ships?
3. Should Recently played/Favourites be exposed as smart "collections" in
   the sense Playnite and ES-DE use the term (user-creatable groups), or
   stay the two fixed filter chips this wave scopes? (Fixed chips are
   what W15-02 currently plans; collections would be a larger, later
   ticket.)
4. Is a play-count/last-played reset (per game, and "clear all history")
   needed for privacy/testing, and if so does it belong in W15-02 or a
   follow-up?

## 15. Note on stories[]

No `USER_STORIES.md` entry names library recency, thumbnails, context
menus, modals/toasts, hotkey display, theming, or controller-first
navigation specifically enough to cite; the nearest candidates (E1-S3
"save states with hotkeys", E2-S3 "a ROM library view with hash-verified
identity") are Phase 1/2 stories already covered by earlier tickets, not
this wave's work. All eight W15 tickets therefore carry `"stories": []`,
recorded here so a future reader does not mistake the empty array for an
oversight.
