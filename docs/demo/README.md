# Demo captures

Automated, reproducible captures of the enhancement features. Recorded by
a harness rather than by screen recording (Brad's ruling, 2026-08-19,
ticket W5-04): a capture regenerated from a checked-in replay and a
deterministically-built ROM can be reproduced by anyone, and a change that
breaks the feature changes the image — whereas a stale video keeps looking
right forever.

## `rf-scroller-full-level.png`

The RF-Scroller full-level demo (`docs/MVP.md` §3's RF-Scroller line).
Four sampled moments from
`fixtures/replays/unprofiled-scroller.rfreplay`, one per row:

| panel | what it is |
|---|---|
| left | the **unmodified console output**, 256×240 — what the player sees, and the control the right panel must be consistent with |
| right | the **full level decoded from ROM**, 768×224, drawn through the game's own CHR tiles and `area_palette`, with the original-viewport outline (white) and the live player marker (red) on it |

Read it top to bottom: the white outline marks exactly the region the left
panel is showing, and it travels across a reconstruction of geometry the
player has not yet visited. The red marker sits at `player_x` — the same
profile-published address `plugins/examples/player-overlay/main.lua` reads.

Regenerate with:

```
cargo test -p retroforge --test demo_capture -- --ignored --nocapture
```

It is `#[ignore]`d because it writes a checked-in artefact, and a test that
rewrote one on every `cargo test` would dirty the working tree every run.

### Two things about this image that are deliberate

**It is downscaled 3×.** `rf_renderer::png` writes *stored* (uncompressed)
deflate blocks — it was built for W3-04's screenshot path without taking a
compression dependency — so a PNG costs exactly `width × height × 4` bytes.
At full size this sheet is 4 MB, which is not something to check into git
and re-commit on every regeneration. At 3× it is ~450 KB and the level
panel is still 256 px wide, enough to watch the outline travel. Teaching
that encoder to deflate is a real follow-up and would let this go back to
full resolution.

**The sky is composited, not copied.**
`rf_debugger::pattern::tile_to_rgba` writes alpha 0 for palette index 0 —
"transparent where nothing drew", so a pattern-viewer image composites
over any background. The first version of this capture copied those pixels
straight through, leaving the whole sky transparent, which a PNG viewer
renders as **white** — a level with a white sky sitting next to a console
panel with a black one. The capture now alpha-tests and composites onto
the game's own `area_palette[0]`.
