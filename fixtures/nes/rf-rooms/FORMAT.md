# RF-Rooms — the `room_grid` fixture level

Ticket **W9-08**. This is the level data the second decoder family
(`rf_enhance::decode::room_grid`) is written against, and the evidence
`profiles/nes/rf-rooms/profile.toml`'s claims rest on.

## Provenance (FR-PROF-003)

**RetroForge authored every byte described here.** There is no
clean-room question and no commercial game involved: the layout below was
designed for this ticket, and `generate.md` states the rule that produces
it, so anyone can rebuild the bytes and check them.

That matters because W9-08's second criterion is "each ships with the
evidence its claims rest on". For a first-party fixture the evidence is
the construction rule itself — the strongest kind available, because it
is checkable rather than merely cited.

## Why this fixture exists at all

`profiles/snes/example-mode7` has declared `kind = "room_grid"` since
W4-02 while decoding **nothing**, because no such family existed. A
second profile of the *same* shape as RF-Scroller would have proven
nothing; W9-08's note is explicit that the point is breadth of format
coverage, not count. So this fixture is deliberately the shape
`metatile_screens` cannot express: **a grid of rooms, with reuse.**

## The level

A 4 × 3 grid of rooms. Each room is 4 × 4 tiles, so the level is
16 × 12 tiles.

Three **distinct** rooms are used twelve times — which is the property
that makes the family worth having, and the one an unindexed layout
cannot express:

| room | meaning | tile byte |
|---|---|---|
| 0 | solid floor | `0x10` |
| 1 | open space | `0x00` |
| 2 | wall | `0x20` |

### Grid (`room_grid_index`, 12 bytes, reading order)

```
row 0:  2 1 1 2
row 1:  2 0 0 2
row 2:  2 2 2 2
```

Room 2 appears seven times, room 1 twice, room 0 twice — a 12-cell level
stored as 3 rooms. An unindexed grid would need twelve rooms of data to
say the same thing.

### Data layout, in the NORMALIZED image

Offsets are into the header-stripped image, per FR-CORE-011. Handing a
decoder the raw iNES file instead shifts everything by 16 bytes and
produces a plausible-looking wrong level rather than an error.

| offset | length | contents |
|---|---|---|
| `0x0000` | 48 | `room_grid_data` — 3 rooms × 16 tiles, room 0 first |
| `0x0030` | 12 | `room_grid_index` — the grid above |

Room payloads are the tile byte repeated sixteen times, which makes a
wrong room immediately visible in a hex dump: a decode that reads room 1
where room 2 belongs shows `00` where `20` should be.

## What this fixture is NOT

It is **not a runnable ROM.** The `room_grid` family is a pure function
of ROM bytes — no core, no bus, no frame — so exercising it needs level
bytes and nothing else. Building a bootable NES image to test a pure
function would add a cc65 toolchain dependency to prove nothing extra.

RF-Scroller remains the runnable fixture; this one is level data, and
`generate.md` is how you regenerate it.
