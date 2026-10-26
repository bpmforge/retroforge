# W37-02: the generic tier across the library

Ticket W37-02, run 2026-10-26. Counts only: **no speed is measured or claimed (D-029).**

## What was run

`crates/rf-harness/tests/w37_02_census.rs` (`#[ignore]`, release; one child process per ROM, the
`boot_census` design, so a panic or hang costs one row). For each ROM: Rebuild **cold** (vectors-only
atlas from `build_seeded_atlas`, the functions `retroforge::rebuild::build_for` calls), recorder armed on
the rebuilt core, atlas rebuilt from the recording and swapped in at a frame boundary every 120 frames
(as the shell does, W37-01), 600 frames from power-on, the same input on a plain core (idle, a five-frame
Start press every 120 frames), the lockstep checker comparing registers, memory, video and audio
after every frame. Any divergence is a bug.

Sample (seed `0x3702`): 200 NES and 200 SNES archives, plus **every** SNES coprocessor title in the
library (39), plus up to three boards of each NES mapper the sample missed (24). 463 ROMs in all.
Budget allotted 120 min wall; the main run took 26.3 min, the mapper top-up 0.9 min, nothing skipped.

```text
RF_W37_OUT=out.tsv RF_W37_BUNDLES=bundles cargo test --release -p rf-harness --test w37_02_census \
  -- --ignored --nocapture census          # RF_W37_EXTRA_ONLY=1 for the mapper top-up alone
```

## Result

| | ROMs | booted | not a cartridge | panics | divergences | ROM-address audit failures |
|---|---|---|---|---|---|---|
| NES sample | 200 | 195 | 5 | 0 | 0 | n/a |
| NES mapper top-up | 24 | 24 | 0 | 0 | 0 | n/a |
| SNES sample + coprocessors | 239 | 237 | 2 | 0 | 0 | 0 |

"Booted" means both cores loaded and all 600 frames ran in lockstep (it is not "the game is correct").
"Not a cartridge": the archive holds no member `rf_cart` parses (5 NES, 2 SNES, e.g. a Super Game Boy
archive); not a mapper or chip refusal. **No ROM was refused for an unsupported mapper or chip.**
Divergences: none, so there are no repro bundles. Fallback rate is the share of executed instructions with
no atlas node, summed over every atlas of the run (including the cold first epoch, which is why it is not
small). Reasons are shares of those fallbacks.

### NES by mapper (sample, then top-up)

| mapper (board) | n | instructions | fallback % | no node | RAM |
|---|---|---|---|---|---|
| 0 NROM | 15 | 83.0M | 1.8 | 100% | 0 |
| 1 MMC1 | 48 | 272.3M | 7.4 | 99.7% | 0.3% |
| 2 UxROM | 23 | 129.7M | 6.3 | 99.6% | 0.4% |
| 3 CNROM | 16 | 91.0M | 2.5 | 100% | 0 |
| 4 MMC3 | 54 | 312.0M | 7.3 | 99.6% | 0.4% |
| 7 AxROM | 10 | 57.6M | 26.4 | 99.1% | 0.9% |
| 11 Color Dreams | 10 | 53.7M | 20.7 | 100% | 0 |
| 30 UNROM 512 | 1 | 5.8M | 18.4 | 100% | 0 |
| 64 RAMBO-1 | 1 | 5.8M | 0.2 | 100% | 0 |
| 69 FME-7 | 1 | 4.8M | 6.0 | 96.1% | 3.9% |
| 79 NINA | 7 | 35.5M | 7.8 | 99.9% | 0.1% |
| 87 Jaleco JF | 1 | 5.9M | 0.9 | 100% | 0 |
| 118 / 119 MMC3 variants | 1 + 1 | 5.1M / 6.1M | 4.1 / 24.0 | 100% | 0 |
| 144 Death Race | 1 | 5.5M | 0.2 | 93.3% | 6.7% |
| 148 Sachen | 2 | 8.9M | 0.8 | 100% | 0 |
| 206 DxROM | 3 | 16.9M | 3.7 | 100% | 0 |
| 5 MMC5 | 3 | 16.4M | 10.6 | 100% | 0 |
| 9 MMC2 | 3 | 16.4M | 16.8 | 100% | 0 |
| 13 CPROM | 2 | 11.5M | 0.7 | 70.9% | 29.1% |
| 18 Jaleco SS88006 | 3 | 16.4M | 2.8 | 100% | 0 |
| 34 BNROM | 3 | 16.0M | 21.1 | 99.3% | 0.7% |
| 47 (MMC3 multicart) | 1 | 6.3M | 17.4 | 99.6% | 0.4% |
| 66 GxROM | 3 | 16.1M | 17.3 | 100% | 0 |
| 71 Camerica | 3 | 18.0M | 29.1 | 100% | 0 |
| 232 Quattro | 3 | 17.2M | 21.0 | 100% | 0 |

Over the 195 sampled boards: median fallback 2.9%, p90 22.6%, max 32.3%, none over 50%; 31 over 20%.
Width mismatch is 0 by construction on the 6502. Mean 3.6 atlas swaps per run. Indirect transfers
checked 18.2M, 2.46M not in the atlas's recorded targets (counted, never acted on). **No NES fallback
is a bank-switch mis-resolution**: almost every one is code the atlas has not seen yet (no node), which the
growing atlas shrinks, and a little is RAM code. Mapper 28 (Action53) is the one implemented board that
met no real cartridge (none in the library); it has only its unit test.

### SNES by chip / map

| chip / map | n | instructions | fallback % (median) | width | no node | RAM | other |
|---|---|---|---|---|---|---|---|
| none / LoROM | 157 | 1331.4M | 22.4 (20.8) | 0.6% | 95.6% | 3.8% | 0 |
| none / HiROM | 41 | 363.7M | 27.4 (22.4) | 0.5% | 87.5% | 12.0% | 0 |
| DSP-n (read as DSP-1) / HiROM | 8 | 72.1M | 24.0 | 1.8% | 97.9% | 0.3% | 0 |
| DSP-n / LoROM | 3 | 26.8M | 23.3 | 0 | 100% | 0 | 0 |
| Cx4 / LoROM | 2 | 16.0M | 21.7 | 0 | 90.6% | 9.4% | 0 |
| OBC1 / LoROM | 1 | 9.4M | 25.4 | 0 | 100% | 0 | 0 |
| ST010 / LoROM | 1 | 9.7M | 32.9 | 0 | 100% | 0 | 0 |
| S-DD1 | 1 | 10.4M | 42.2 | 2.0% | 98.0% | 0 | 0 |
| Super FX / LoROM | 15 | 178.0M | 42.4 (38.6) | 0.1% | 35.0% | 64.9% | 0 |
| SA-1 | 8 | 58.2M | 49.1 (46.6) | 0 | 55.1% | 44.9% | 0 |

SNES overall: median 21.7%, p90 40.5%, 13 of 237 over 50% (SA-1, Super FX, and one HiROM title that runs
entirely from work RAM: 100% RAM). SPC7110: **none present** (header sniff `$FFD6 = F5/F9`, `$FFBF = 0`;
this build has no SPC7110 variant, so one would load as a plain cart). All DSP-n carts are read as DSP-1
(D-010). "other" is 0 everywhere: the executor's only non-ROM, non-RAM fetches (coprocessor memory) did
not occur on the main CPU in 600 frames.

## Coprocessor carts: bank switching never mis-resolves a node

The executor keys every lookup by the ROM offset **the bus resolves at run time** (`SnesBus::target`), and
it never executes a node: it is the interpreter with a map (module doc, `rf-snes/src/executor.rs`). So a
switched bank cannot make it run the wrong code. What can go wrong is the *atlas*: it places static
targets with a fixed LoROM/HiROM map, and a board whose banks differ puts nodes at offsets the CPU never
reads there. Three things were found and handled:

1. **Super FX held the ROM bus and the executor said "ROM".** While the GSU runs (GO and RON) the SNES
   reads a fixed vector table or open bus where ROM is; `bus.rom_offset` still named a ROM offset, so a
   fetch there would have counted as an atlas hit on bytes the CPU did not read. Fixed
   (`SnesBus::rom_offset`, executor and recorder): such a fetch is coprocessor-owned (`fallback_other`,
   `coproc_exec`). Test `rom_held_by_a_running_gsu_is_not_rom_code`.
2. **Static map wrong on Super FX and S-DD1.** The first pilot (before the fix) put **2,179 inferred
   instructions at the wrong offset** on Super FX carts that run code in `$40-$5F` (its linear window), and
   plain LoROM would place the GSU RAM banks and S-DD1's `$C0-$FF` as ROM mirrors. `Wdc65816::for_board`
   now places `$40-$5F` as the bus does, and leaves `$70-$71` (GSU RAM), S-DD1 `$C0-$FF` and LoROM
   `$70-$7D:0000-7FFF` (SRAM or mirror: unknowable statically) unresolved, so those targets stay
   unresolved and only recorded code gets nodes there. After the fix the full run has **0 inferred
   instructions placed at another offset** on every chip (tests
   `the_super_fx_isa_places_addresses_where_the_bus_does`,
   `switched_boards_place_nothing_statically_in_their_switched_banks`).
3. **SA-1 maps nothing statically** (`map` answers open bus for map mode `$23`), so its atlas holds only
   recorded code, one function per instruction; its "placed elsewhere" count (9,077 of 18,292) is
   observed code recorded at the offsets the bank registers had then, compared with the registers' state
   at frame 599. It is expected, not a fault. Known limitation: an SA-1 atlas is not useful for naming
   or recipes (every instruction its own function) until SA-1 banks are modelled; the fallback is correct
   and counted (49% median here).

Evidence the invariant holds: at frames 120, 360 and 599 of each of the 39 coprocessor runs, **every
24-bit address the bus calls ROM was read back through `peek` and compared with `rom[offset]`: 1.26 billion
addresses, 0 differences** (Super FX 481M, SA-1 201M, DSP-1 392M, S-DD1 37M, Cx4 75M, OBC1 36M, ST010
37M); plus lockstep, 0 divergences on all 39 (Doom, Star Fox 2 and the SA-1 RPG included).

Cache: the atlas shape did not change, so `ATLAS_FORMAT_VERSION` (and the `code-atlas-v4` cache key, and
the recipe tests that pin it) stay. A cached atlas built before this ticket for a Super FX, S-DD1 or
LoROM cart may hold wrongly placed inferred nodes; that can only misattribute provenance, never execution.
Clearing the atlas cache regenerates it.

## NES mappers: `prg_rom_offset`

Every mapper module now implements `Mapper::prg_rom_offset`, so none stays interpreted: new this ticket
are MMC2, MMC5 (`None` where a window selects PRG RAM), FME-7, Action 53, Camerica, Color Dreams
(and Death Race), DxROM, Jaleco JF, Jaleco SS88006, NINA, Quattro, RAMBO-1, Sachen, UNROM 512 (W36-01 had
NROM, CNROM, CPROM, GxROM, UxROM, AxROM, BNROM, MMC1, MMC3). Unit tests in
`crates/rf-nes/src/mappers/prg_offset_tests.rs` assert the offset indexes the byte `cpu_read` returns
at all 32 KiB after every bank change (12 tests, 6 new). Ticket reading note: before this, a mapper without
the method reported its ROM fetches as RAM (`Fetch::Ram`); with every board implemented, an NES
`fallback_ram` is real RAM or register code.

## Not done / limits

- 600 frames per ROM with idle-plus-Start input: most games are still on a title or attract screen, so
  these are the fallback rates of the first ten seconds, not of play. Play-length lockstep stays in
  `w37_01_generic_tier.rs` (SMB, SMW; SMW re-run after this ticket: 5,188 frames, 0 divergences).
- The census compares the executor with a plain core; both share one PPU/APU/bus, so a bug common to both
  is invisible (the lockstep module docs say the same).
- The GSU, SA-1 and Cx4 own CPUs are checked only through their serialised state.
- Action 53 met no real board; SPC7110 has none in the library.
