# Design: UX Wave 42 — the October review, fixed

Status: **approved by Brad 2026-10-30** ("fix items"), branch `ui/wave-42`
from `main` at `968b8cf0`.

Source: the 2026-10-30 design review
(https://claude.ai/artifact/X5MfaMrfvW19oSBwfaMfLm), 67 screenshots of
`main` at `968b8cf0` taken with `capture_tour.rs` plus two scratch captures
of the surfaces the tour skips (cheats, RetroAchievements, Rebuild, HD-2D,
Esc). Findings are restated below with where they live.

Principles: `UX_WAVE_20.md` §2 (8–11) and `UX_WAVE_21.md` §3 (12–13)
unchanged. New:

14. **Esc closes the top layer, and only that.** Every overlay — Quick
    Menu, a Quick Menu page, the Settings sheet, a player window — is one
    layer on one stack. Esc pops one layer. Opening a layer from another
    pushes it; it never closes the layer below.

## Findings (verified at `968b8cf0`)

| # | Pri | Finding | Where | Ticket |
|---|---|---|---|---|
| 1 | P0 | Opening Settings from the Quick Menu closes the menu; one Esc then leaves a paused game with no menu; Esc on the Game settings window opens the Quick Menu over the still-open window. | app.rs Section::Settings (10523), Esc handling | W42-01 |
| 2 | P0 | Cheats reachable only inside the research "Game settings" window, under "Trust ladder (D-004)" and Shadow/Advisory/Active rows. | app.rs:9728 | W42-02 |
| 3 | P0 | Achievements page with RA off is a sentence with no action; the RA setting lives in Settings › Paths; `SETTINGS_INDEX` has no entry for it. | ra_panel.rs:33, app.rs:9194, app.rs:348 | W42-03 |
| 4 | P1 | Save states and Controls exist as framed windows duplicating Quick Menu pages (principle 13). | app.rs states window, controls window | W42-04 |
| 5 | P1 | Player copy: "Accuracy" on save slots, `$7E0004` in the camera card, "RA: softcore" in the badge, `a/v high`, "Using default bindings (no config file yet)", a "Ready." chip on every card, Rebuild card's research table, profile builder "It went up/down" for horizontal camera. | slot_cards.rs, app.rs, rebuild_card.rs, profile_builder.rs | W42-05 |
| 6 | P1 | Quick Menu Resume page is one sentence and a text button; unlock toast looks like a file toast; HD-2D card in Original mode has no fix action. | app.rs Section::Resume, ra.rs toast, hd2d_preset_card | W42-06 |
| 7 | P2 | Ambient glow paints flat bars; library placeholder art is a flat square; library status bar shows play readouts with no game; mode card repeats "Nothing added". | presentation / play_view, library.rs, app.rs status bar | W42-07 |

## Tickets

- **W42-01 One overlay stack.** Esc pops exactly one layer: a framed player
  window (Settings sheet, Game settings, Controls, States, profile builder)
  if one is open, else the Quick Menu page/menu. The Quick Menu's Settings
  page opens the sheet *over* the menu and closing the sheet returns to the
  menu with the Settings rail item focused. The Quick Menu never draws over
  an open player window. Test: for each layer, open it, press Esc, assert
  the layer below is what is shown and the game stays paused while the menu
  is up.
- **W42-02 Cheats page.** New `Section::Cheats` in the Quick Menu rail
  between Enhancements and Achievements, drawing the existing
  `cheats_section` (paste, add, switch, remove) as a page. The Game
  settings window keeps no Cheats panel. Errors are one short line per
  rejected line ("Line 2 isn't a code (6 or 8 letters, or 6 hex digits)").
- **W42-03 Achievements you can turn on.** With RA off or signed out, the
  Achievements page carries the switch and sign-in form itself (the same
  controls Settings has). Settings gains an **Online** tab holding
  RetroAchievements, box-art fetching and AI naming; `SETTINGS_INDEX`
  indexes RetroAchievements and Hardcore.
- **W42-04 Retire duplicate windows.** File › Save states opens the Quick
  Menu on Save; the States window goes. The standalone Controls window's
  entry points open Settings › Controls; the window goes.
- **W42-05 Player words.** Slot meta uses the player mode name
  (Original/Enhanced/Game-Aware). Camera card drops the address. Badge says
  "Achievements" / "Achievements · Hardcore", not "RA: softcore". Status
  bar a/v readout shows only when there is a problem, in words. "Ready."
  chip removed (show a chip only for a state that needs attention). The
  Rebuild card shows one summary line and keeps the rest behind a
  "Details" collapsing header. Profile builder: horizontal camera step
  says "It moved right / It moved left / It didn't move". The settings
  copy test is extended to every player surface: no `W\d+-\d+`, `D-\d{3}`,
  `$` hex addresses, `crate::`, or the word "Accuracy".
- **W42-06 Resume and celebration.** Resume page: thumbnail of the paused
  frame, mode badge, last save time, active enhancements and cheats count,
  a primary Resume button. Achievement unlocks get their own OSD card (top
  centre, trophy icon, title, points). HD-2D card in Original mode offers
  "Switch to Enhanced and turn on".
- **W42-07 Polish.** Ambient glow blurred and faded (stays off by default —
  Brad's open question, answered by the review). Library placeholder art is
  a console-coloured tile with the title. Library status bar hides play
  readouts with no game. Original mode card drops the duplicate caption.
- **W42-08 Tour.** `capture_tour.rs` photographs the cheats page, the
  achievements sign-in page, the Settings-over-menu stack and Esc back.

## Note on stories[]

As in `UX_WAVE_20.md` §11: `"stories": []`.
