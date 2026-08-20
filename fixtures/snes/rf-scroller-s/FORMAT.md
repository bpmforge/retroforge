# RF-Scroller-S

The project's own in-repo **SNES** fixture (ticket W6-05, doctrine D-001,
FR-CORE-037): game-shaped content we control end to end, built from
source in CI, never a fetched binary.

Its sibling is `fixtures/nes/rf-scroller`, and this document is written
to the standard that one sets — including its section on what went
wrong, because that is the part a later reader actually needs.

## What it does

A horizontally scrolling playfield in BG mode 0. The D-pad moves a player
marker, the camera follows it half a screen behind, and new tile columns
are streamed into the tilemap as the camera advances — so the 32-column
tilemap is **reused indefinitely** rather than the level being one screen
wide.

Build with `./build.sh`. The hash lives in `rom.sha256`; the script
refuses a mismatch rather than re-pinning silently.

## Toolchain provenance: cc65, not libSFX

This ticket's title and D-001 both say "libSFX". They are not followed
here, and the reason is evidence rather than convenience.

W6-06 established — by checking rather than assuming — that **cc65's
`ca65` already assembles 65816** (`--cpu 65816`, `.p816`, explicit
`.a8`/`.a16` width directives) and that `ld65` links a flat LoROM image
from a hand-written config. cc65 is **already installed in CI** for the
NES fixture. W6-06's own FORMAT.md predicted libSFX would still be right
*here*, on the grounds that a scroller wants "a framework's init code,
macros and asset pipeline".

In the event it wants none of those. This fixture's init is about 40
lines of register writes; its "asset pipeline" is four solid-colour
characters generated in-source, deliberately, because a fixture whose
graphics live in a binary blob is a fixture whose build is not
reproducible from text. Against that, libSFX would be a new dependency
needing a `docs/TECH_STACK.md` row, a licence review and a CI install
step.

**This is a deviation from the ticket title and from D-001's wording, and
it is flagged as one** rather than quietly taken — see the W6-05 close
note. If a later SNES fixture genuinely needs a framework, nothing here
argues against adding libSFX then.

## RAM map

Zero page. **Declaration order decides this layout**, so anything new
goes at the END — or every address below, and every consumer of them,
silently goes stale. RF-Scroller (NES) learned this in W5-02c and it cost
a ticket.

| addr | width | name | meaning |
|---|---|---|---|
| `$00` | 2 | `frame_counter` | frames since reset |
| `$02` | 2 | `player_x` | player position, pixels; spawns at 16 |
| `$04` | 2 | `camera_x` | camera scroll, pixels |
| `$06` | 2 | `columns_streamed` | tile columns written since reset |
| `$08` | 2 | `next_column` | next world column to stream |
| `$0A` | 2 | `pad1` | joypad 1, latched from `$4218` |
| `$0C` | 2 | `scratch` | address arithmetic |
| `$0E` | 2 | `ground_top` | first ground row of the column being written |

`crates/rf-harness/tests/rf_scroller_s_replay.rs` reads the first four.

## Level format

There is no stored level data. A column's content is a pure function of
its **world index**:

* ground occupies rows `24 - (column mod 4)` through 27, so the surface
  steps up and down on a four-column cycle and horizontal motion is
  visible;
* everything above is character 0, which is transparent and shows the
  backdrop.

That makes the level deterministic and endless without an asset file, and
it means a scrolling bug shows up as a broken surface rather than as
"nothing happened".

## Streaming, and why `columns_streamed` is the anti-vacuity hook

The tilemap is 32 columns wide. `stream_if_needed` writes one new column
whenever the camera has advanced past the last one written, one screen
ahead of itself. So:

* `columns_streamed == 33` means the 32-column preload plus the first
  frame's column — i.e. **nothing has scrolled**;
* `columns_streamed > 33` is only reachable once a *physical* tilemap
  column has been overwritten with new content.

A fixture that only ever renders its opening screen would pass a replay
hash forever while proving nothing. The harness therefore asserts a
`columns_streamed` above 33, and has a counter-test asserting that with
no input it stays at exactly 33.

## The replay gate

**Delivered.** The scripted five-minute log runs without faults and is
gated as a real `.rfreplay`: recorded, serialised, parsed, and replayed on
a fresh machine that the recording run never touched, with bit-identical
reachable state at all 30 ten-second checkpoints.

It did not work the first time, and the reason is worth keeping. `.rfreplay`
was **NES-only**: `PortLogKey::buttons` was a `Vec<NesButton>`, so a SNES
button outside the NES 8-bit set — Right is bit 8 — had no log-key entry
and was *silently dropped* on serialisation. The recorded "hold Right" log
played back as an idle pad, and the run diverged at the very first
checkpoint. The emulator was deterministic throughout; the log was lossy.

W7-02 added `SnesButton` and made the log key console-directed. The replay
test now also asserts the log carries the SNES button table and that each
replayed frame's buttons equal what was recorded — so a future format
regression fails as a format error rather than masquerading as an
emulator divergence, which is how this one first presented.

## One thing that went wrong, worth not repeating

`stream_one_column` originally saved only the flags (`PHP`/`PLP`) and then
used `X` as its row counter. `preload_columns` also uses `X`, as its loop
counter, so the first build streamed **30,103 columns** and never left
forced blank: a black screen, a frame counter stuck at zero, and no error
anywhere.

`PHP` saves the processor status, not the registers. Any routine here that
touches `X` or `Y` must `PHX`/`PHY` — the fix is two instructions, and the
symptom looks nothing like the cause.
