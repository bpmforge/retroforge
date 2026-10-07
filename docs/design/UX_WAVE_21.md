# Design: UX Wave 21 — the play view looks like the design

Status: **built 2026-10-07** on branch `ui/wave-21`, each ticket merged to
`main` as it closed. Tickets W21-01..W21-07 in `plan.json`; §7 lists the
deviations. Everything visual is UNVERIFIED (by eye: Brad) until he looks.

Brad, 2026-10-07: "the UI does not look updated to the style you designed,
can you look at fresh design ui and make sure all the settings and such we
are there".

## 1. Why there is a Wave 21

The 2026-10-06 design review
(https://claude.ai/artifact/UAh9xB94EZjfyiY62qNpAb) had two halves: a
findings ledger with a functional plan, and five mockups plus a **"Visual
language"** table and a **Phase 4 · polish** column. `UX_WAVE_20.md` turned
the first half into tickets W20-01..W20-22, and all of them work — the
2026-10-07 tour (`target/ui-tour/`, 26 photos at `3e77778`) shows every
W20 surface functioning. The second half was never ticketed, so every new
surface is drawn with egui defaults: framed windows, one body size, plain
radio buttons, no scrim, no motion. That is what Brad is looking at.

Nothing was stale: `main` (3e77778) and `~/Code/retroforge-ui`
(`ui/wave-17`, 4a7ee48) both carry Wave 20, and the binaries in `main`'s
`target/` were built on 2026-10-07.

## 2. Present vs missing (tour of 2026-10-07 against the artifact)

| Artifact item | State at 3e77778 | Ticket |
|---|---|---|
| Quick Menu rail, slot grid, pad hint bar, honesty pill | present (12/13-quick-menu*.png) | — |
| Quick Menu over a full-screen dimmed/blurred frame, no window frame | **missing**: a framed panel; menu bar and status bar stay lit | W21-02 |
| Keyboard binding beside each Quick Menu item | **missing** | W21-02 |
| Condensed SemiBold section titles, 12/14/16/22/32 type scale, tabular numerals | **missing**: Plex Sans Regular/SemiBold only, egui default sizes | W21-01 |
| Elevation (base / raised / overlay, soft shadow, 1 px highlight), scrim | **missing** | W21-01 |
| Motion: overlay 120 ms fade + 8 px rise, card focus scale 1.03, OSD slide | **missing** | W21-01, W21-02, W21-05 |
| Slot cards: rename, delete, overwrite with a 10 s undo | **missing** (save/load only) | W21-03 |
| Shader tiles with plain names ("CRT TV", technical name in small caps) | tiles present, technical names only ("CRT-class") | W21-04 |
| Shader parameter sliders | present (Settings and Display) | — |
| Scaling as a segmented control | present as radio buttons | W21-04 |
| Aspect: 4:3 TV / 8:7 pixel / Widescreen | partly: "Pixel shape" TV/Square; widescreen lives only in Enhance | W21-04 |
| Around the picture: Black / Ambient glow / Bezel | partly: ambient glow checkbox; no bezel | W21-04 |
| Apply to: This game / All NES / Everything | **missing** for display settings | W21-04 |
| Fullscreen toggle | present (View menu, Display, F11) | — |
| Theme System / Dark / Light / High contrast | present (Settings › Accessibility) | — |
| Library: Continue hero | present, small (22-library-continue.png) | W21-05 |
| Library: shelves, 3:4 cards, console spine colours, focus scale | partly: one card row, 150 px cards, spine present | W21-05 |
| Enhancements panel: card per feature, toggle, plain tags | present as rows, not cards with tags | W21-05 |
| Settings as a full sheet with search and live preview | **missing**: a floating window over the game | W21-06 |
| Player copy never shows internal paths | **violated**: status bar prints `Loaded /var/folders/…` | W21-06 |
| OSD cards / perf overlay / input display / rewind scrub | present, default styling | W21-01 tokens, W21-02 |

## 3. Principles

Carried from `UX_WAVE_20.md` §2 unchanged (8–11). New:

12. **Tokens before pixels.** Every size, radius, shadow, duration and
    colour added by this wave lives in `theme.rs` as a named token and is
    zeroed by the reduced-motion setting where it is motion.
13. **Overlays have no frame.** Quick Menu, Settings sheet and OSD cards
    sit on a scrim, never inside an `egui::Window` title bar.

## 4. Tickets

- **W21-01 Visual tokens.** Embed IBM Plex Sans Condensed SemiBold (OFL);
  `TypeScale` 12/14/16/22/32; tabular numerals in readouts (Plex Mono for
  numbers); `Elevation::{Base, Raised, Overlay}` with shadow + 1 px inner
  highlight; `SCRIM`; `Motion` durations honouring `animation_time = 0`.
- **W21-02 Quick Menu to mockup.** Full-viewport scrim over the paused
  frame (the artifact's own trick: downscale the frozen texture twice and
  stretch it behind a 70 % scrim, built once on open — not a per-frame
  blur), no window frame, chrome hidden while open, binding beside each
  rail item, Condensed titles, 120 ms open animation.
- **W21-03 Slot management.** Rename, delete, overwrite with undo toast;
  X/Y pad hints.
- **W21-04 Display controls.** Plain shader names; segmented controls;
  Aspect selector; Around the picture (Black / Ambient glow / Bezel —
  bezel as a procedural frame per console); Apply-to scope keyed by ROM
  hash / console / global.
- **W21-05 Library and Enhancements look.** Larger 3:4 cards, shelves,
  hero at mockup size, focus scale; Enhancements as cards with tags.
- **W21-06 Settings sheet.** Full sheet, search, live preview; status bar
  shows the game title, never a filesystem path.
- **W21-07 Tour.** `capture_tour.rs` photographs every W21 surface.

## 5. Open decision for Brad

The artifact recommended a fresh install show **ambient glow on**; W20-19
shipped it off. It is presentation, not an enhancement (law 6 does not
apply). Until Brad answers, the default stays off.

## 6. Note on stories[]

As in `UX_WAVE_20.md` §11, no story is specific enough to cite; the W21
tickets carry `"stories": []`.

## 7. Deviations (as built)

- **W21-01:** no monospaced numeric face — IBM Plex Sans's default figures
  are already tabular (measured in `tests/theme_fonts.rs`), so
  `theme::numeric` names the body face.
- **W21-02:** the "blur" is the frozen frame box-downscaled 8x once on open
  and stretched back with linear filtering (the review's own recipe).
- **W21-03:** no X/Y pad hints for rename/delete: nothing binds X/Y in the
  shell, and a hint for a button that does nothing would lie (principle 8).
  The States window keeps W15-04's overwrite confirmation; only the Quick
  Menu saves at once with an undo. Landed in one commit with W21-04.
- **W21-04:** Aspect offers TV (8:7) / Square pixels; Widescreen is an
  enhancement (it draws level the original never showed), so the Aspect
  field says where to turn it on instead of offering an inert segment.
  Shader parameter values stay global (only the choice is scoped).
- **W21-05:** cards keep their W15 width (tests derive grid columns from
  it), so no 3:4 box-art proportion and no focus scale; the hero uses the
  library thumbnail. Grid is now the default library view.
- **W21-06:** search jumps to the tab holding a match rather than
  filtering widgets in place. The sheet sits between the menu bar and the
  status bar, so View › Settings still toggles it.
