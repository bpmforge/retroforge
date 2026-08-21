# Action 53 (iNES mapper 28) fixture

Built by `build.sh` from `src/main.s`; **the ROM is not committed** (law 5)
— `rom.sha256` pins the bytes that build produces.

## Why this exists

W7-11's third criterion asks for "test ROMs for both mappers". Mapper 7
has published test ROMs. **Mapper 28 has none**: Action 53 is a homebrew
multicart collection rather than a mapper with a conformance suite, and
there is no mapper-28 ROM in `tests/rom-manifest.toml`, in
`docs/TESTING.md`, or in anything fetched into `roms/`. That absence is
what left the criterion blocked; this fixture closes it using the same
cc65 toolchain `fixtures/nes/rf-scroller` already uses.

## Layout

128 KiB, eight 16 KiB banks. Banks 0-6 are stamped with their own index at
their first byte. **Bank 7 holds the code and the vectors**, which is
forced rather than chosen: nesdev specifies mapper 28 powers up with the
PRG mode bits set and `outer = $3F`, so `$C000` shows `(outer << 1) | 1 =
127`, i.e. bank 7 of eight. The reset vector must live wherever that
lands.

The test then sets `outer = 3` so the fixed half stays on bank 7 —
`(3 << 1) | 1 = 7` — while `$8000` moves. With four inner bits,
`$8000 = (3 << 4) | inner = 48 + inner`, and 48 is a multiple of the
eight-bank count, so `$8000` shows exactly `inner`.

## What it proves

* The `$5000` select latch, which is the register outside `$8000-$FFFF`
  that makes mapper 28 unlike UNROM.
* The inner-bank field, swept across all eight banks.
* **Mode 3's fixed half staying put**: `$C000` is re-checked on every
  iteration, because "a fixed bank temporarily forces the outer size to
  32 KiB" is the rule most easily got wrong and it is invisible unless
  something reads the fixed half while the switchable half moves.

Each of those is mutation-checked: breaking the select latch, the inner
register, or the forced-32-KiB rule each fails this fixture.

## What it caught

Mapper 28 was implemented, unit-tested twelve ways, added to `rf-cart`'s
`SUPPORTED_MAPPERS` and wired into `NesBus::new`'s factory — and
`EMULATED_MAPPERS` in `system/cartridge.rs` did not list it, so the gate
rejected every mapper-28 ROM before the factory was reached and the
`28 =>` arm was dead code. All twelve unit tests passed throughout,
because each built `Action53` directly. Only a real ROM entering through
`from_ines_bytes` crosses that line.
