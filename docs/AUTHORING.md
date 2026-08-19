# Authoring a game profile

Ticket W5-06 (R-A2). A worked, end-to-end walkthrough against this
repository's own fixture, **RF-Scroller** — annotate → export → decode →
validate → golden.

Every step below is executed by
`crates/retroforge/tests/authoring_walkthrough.rs`, so this document
cannot drift from the tool without a test failing. Re-run it with:

```
cargo test --release -p retroforge --test authoring_walkthrough -- --ignored --nocapture
```

## Time to first decoded level

**0.40 ms of machine time**, measured on the development machine
(Apple M-series, release build):

| step | time |
|---|---|
| annotate (build the annotation set) | 0.001 ms |
| export skeleton (1058 bytes of TOML) | 0.058 ms |
| decode the level | 0.222 ms |
| validate | 0.109 ms |
| golden (preview text) | 0.016 ms |
| **total** | **0.401 ms** |

**Read that number correctly.** It is the *mechanical* half of the loop
only. It excludes the two things that actually take a person time:

1. **Finding the addresses** in the debugger — watchpoints, memory
   viewer, a disassembly, or (for RF-Scroller) a symbol dump. Minutes to
   hours, depending on the game.
2. **Writing the `[decode]` section.** The exporter turns annotations into
   `memory_map`/`rom_map` rows, and it stops there: it cannot infer a
   level *format* from a list of addresses. Deciding that a game stores
   columns as run-length pairs is the authoring work, and no tool in this
   repository does it for you.

The number is still worth having, and it says something specific: **the
loop is not the bottleneck.** At sub-millisecond re-decode, an author can
save the file and see the result before their hand leaves the keyboard,
which is what makes iterating on offsets by trial and error viable at all.
That is the property the hot-reload was built for.

## 1. Annotate

Work in the debugger: watchpoints on values you can make change
on-screen (the player's X, the camera's scroll), the memory viewer for
what sits beside them, the pattern/nametable viewers for ROM-side tables.
Record each finding in the annotation store with a **`source`** — it is
required, and the exporter rejects an annotation without one
(FR-PROF-003, `rf_debugger::annotation::AnnotationError`).

RF-Scroller's set, as the walkthrough builds it:

| space | addr | type | label |
|---|---|---|---|
| RAM | `0x6029` | u16 | `player_x` |
| RAM | `0x602B` | u16 | `camera_x` |
| RAM | `0x602D` | u8 | `columns_streamed` |
| ROM | `0x11CD` | bytes ×4 | `metatile_table` |
| ROM | `0x11DD` | bytes | `collision_table` |
| ROM | `0x11E9` | bytes ×48 | `level_column_offset` |
| ROM | `0x1219` | bytes | `level_rle_data` |

**ROM offsets are into the NORMALIZED (header-stripped) image**, not the
file on disk. This is the single most common way to get a profile subtly
wrong: feed the raw iNES file and every table shifts by 16 bytes, which
does not error — it decodes a plausible-looking wrong level. See
`rf_enhance::decode`'s module doc.

**A cautionary tale from W5-01, worth ten minutes of your time.** That
ticket derived RF-Scroller's RAM map from a `.dbg` symbol dump left over
from an older local build. It put `frame_counter` at `0x602E` — its
address *before* ticket W2-10a re-pinned it, where `camera_y` now lives.
The profile would have read the wrong byte for two entries and looked
entirely plausible doing it. The fix was to re-link with `-g --dbgfile`
and **verify the ROM came out byte-identical to `rom.sha256`**, so the
symbols provably described the build being profiled, then cross-check
every address against a second source. Do the same: never trust a build
artefact you did not just produce.

## 2. Export a skeleton

`rf_debugger::profile_export::export_skeleton` turns the annotations into
a profile with `[meta]`, `[[memory_map]]` and `[[rom_map]]` filled in and
every `source` carried through. It does **not** write `[decode]`,
`[camera]` or `[entities]` — see step 3.

## 3. Write the parts a tool cannot infer

### `[decode]`

```toml
[decode]
kind = "metatile_screens"

[decode.metatile]
table = 0x11CD
size = 4

[decode.screens]
width = 48
height = 14
order = "column_rle"
```

The `metatile_screens` family additionally requires two `[[rom_map]]`
entries **found by label**: `level_column_offset` (one byte per column,
where that column's data starts) and `level_rle_data` (the packed runs).
That label contract is documented in
`rf_enhance::decode::metatile_screens`, and a profile missing either gets
a named error rather than a generic failure.

### `[entities]`

Point it at where the game actually keeps live entity positions. For
RF-Scroller that is the **OAM shadow**, not RAM: every gem's world X is a
compile-time constant and they share one Y, so the only per-entity RAM
state is which logical gem occupies which sprite slot. Pointing this
section at a RAM address that does not hold entity data produces a
profile that validates and reads garbage.

Declare an `active` field only if the game **has** one. RF-Scroller does
not — it writes all twelve gems unconditionally every frame — so the
profile omits it and the runtime falls back to the platform's own
convention (NES sprites parked at Y ≥ `0xEF`). A masked predicate invented
to fill the slot would validate and mean nothing.

## 4. Iterate with hot-reload

Open **Author…** in the overlay menu (it appears only once a profile
claims the open ROM). The panel watches the file and re-decodes on save,
showing:

- **errors first**, with the ROM offset you typed rendered in hex —
  `[ROM 0xF1219] level_rle_data at offset 0xF1219 length 246 runs past
  the end of a 24576-byte ROM`. A mistyped table address is the most
  common authoring mistake and the error tells you exactly which table
  and where it pointed.
- then loader warnings (unknown keys and the like),
- then the decoded level as a metatile-id grid.

The preview is **text, not pixels**, on purpose: the question you are
answering is "did my offsets land on real level data?", and an id grid
answers it more directly than a rendering. A wrong palette makes a
correct decode look broken; a right palette can make garbage look
plausible.

## 5. Validate

```
cargo run -p retroforge-tool -- profile validate profiles/nes/<title>/profile.toml
cargo run -p retroforge-tool -- profile hash <rom> profiles/nes/<title>/profile.toml
```

`profile hash` is the one that matters for identity. Profiles are keyed
on the **normalized** sha256 — `rom.sha256`-style raw file hashes are
diagnostics only (SRS FR-CORE-011). A profile whose `[[identity]]` does
not match reports `NO MATCH` and will silently never apply to the game it
was written for.

## 6. Golden it

Commit the profile under `profiles/<console>/<title>/`. Two gates then
cover it automatically:

- `crates/rf-profiles/tests/example_profiles.rs` loads every shipped
  profile and fails on any warning.
- `crates/rf-harness/tests/level_decode_vram.rs` asserts the shipped
  profile still identifies the fixture CI builds, and that the offline
  decode equals the nametable bytes the ROM itself streams into VRAM.

That second one is the gate worth understanding. It does not compare
screenshots — two renderings can agree while both misread the ROM. It
compares the decoder's output against the bytes the *game* wrote, which
is the only ground truth for "does my profile describe this game?"

## Known gap

The whole-level form of that VRAM comparison is currently `#[ignore]`d:
64 of 64 preloaded columns verify byte-exact, but 15 of the 96 streamed
columns do not, and nine of those are torn in the fixture's own VRAM.
Ticket **W5-02c** owns it. If you are authoring against RF-Scroller and
see a disagreement in metatile columns 43–47, that is the known issue and
not your profile.
