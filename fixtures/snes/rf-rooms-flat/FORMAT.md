# RF-Rooms-Flat — the UNINDEXED `room_grid` fixture

Ticket **W9-08**. The companion to `fixtures/nes/rf-rooms`, and the
reason there are two: `room_grid` has two halves, and a single fixture
would leave one of them unexercised by any profile.

| fixture | grid | proves |
|---|---|---|
| `nes/rf-rooms` | indexed | room REUSE — three rooms fill twelve cells |
| `snes/rf-rooms-flat` (this) | unindexed | cell N is room N, no indirection table |

The unindexed half is not a degenerate case of the indexed one: it is a
different on-ROM layout, it has its own failure mode (a grid larger than
256 cells cannot be numbered in one byte, and the decoder refuses with
the remedy named), and a profile that gets `indexed` wrong decodes
garbage rather than erroring. Both halves therefore need a profile
standing on them.

## Provenance (FR-PROF-003)

RetroForge authored these bytes. `generate.md` states the rule, so the
evidence is a construction rule rather than a citation — checkable rather
than merely attributed.

## The level

A 3 × 2 grid of 2 × 2-tile rooms — a 6 × 4-tile level — with **six
distinct rooms** and no reuse, which is exactly what "unindexed" means:
cell N is room N, laid out in reading order.

Room *n* is the byte `n + 1` repeated four times, so a misread room is
obvious in a hex dump: reading room 3 where room 4 belongs shows `04`
where `05` should be.

| offset | length | contents |
|---|---|---|
| `0x0000` | 24 | `room_grid_data` — 6 rooms × 4 tiles, in reading order |

There is deliberately **no `room_grid_index`**. Adding one without
setting `indexed = true` would leave it silently unread; setting
`indexed = true` without the table is refused by the decoder, which is
the behaviour that keeps a half-configured profile from looking decoded.

Offsets are into the NORMALIZED, header-stripped image (FR-CORE-011).

## Not a runnable ROM

Same as `nes/rf-rooms`: the family is a pure function of ROM bytes, so
exercising it needs bytes and nothing else. RF-Scroller and RF-Scroller-S
remain the runnable fixtures.
