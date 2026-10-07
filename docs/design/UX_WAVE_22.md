# Design: UX Wave 22 — enhancements the app explains itself

Status: **approved by Brad 2026-10-07** ("go"), branch `ui/wave-22`.
Design with mockups: https://claude.ai/artifact/Vr29wk2tuW76FTDhQCGqPr

Brad, 2026-10-07: "how do we do this with the UI and UX. this should be
designed and show examples" — after a written guide to turning
enhancements on. The app should teach it, not a document.

## Decisions (Brad, 2026-10-07: all three recommendations)

1. Player mode names: **Original / Enhanced / Game-Aware**. `Mode::Accuracy`
   keeps its name in code, docs and Debug; players see "Original" in the
   picker and the badge ("SNES · Original"). Law 6 is unchanged: a fresh
   install boots in it.
2. Plain feature names in player surfaces ("No sprite dropout", "Steady
   sprites"); the research names stay in the Enhance workspace.
3. A one-time "this game can be enhanced" tip per game.

## Tickets

- **W22-01** Mode picker: three cards at the top of Quick Menu ›
  Enhancements, each with a one-line meaning and how many features it
  unlocks for this game; one click changes and saves the mode.
- **W22-02** Profile status strip: matched (lists what it unlocks), or not
  matched, explained in plain words with "What's a profile?".
- **W22-03** Cards grouped "Works on any game" / "Needs a profile for this
  game"; an unavailable card carries the action that fixes it ("Switch to
  Game-Aware") when one exists, else a plain tag.
- **W22-04** Compare: a card's picture opens a drag-to-compare view of the
  paused frame, original against what you see.
- **W22-05** "What is this?": an explainer per feature with an
  illustration drawn from bundled sample art, never game art.
- **W22-06** The badge explains itself (hover lists what is on, click opens
  Enhancements) and the one-time tip.
- **W22-07** The tour photographs every state.

## Note on stories[]

As in `UX_WAVE_20.md` §11: `"stories": []`.
