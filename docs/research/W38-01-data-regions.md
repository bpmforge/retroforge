# W38-01: data regions from ROM access patterns (2026-10-26)

Finding a game's tables and level data **from how its code reads ROM**, with no
hints, no disassembly and no decompilation (D-021). Nothing from a ROM is
stored or quoted here: every number below is an offset, a size, a stride, an
index value or a count. ROM offsets are into the image the atlas indexes (the
PRG image on the NES, the header-stripped ROM on the SNES).

Code: `crates/rf-rebuild/src/dataregion.rs` (observer, inference, level
association, grading), `crates/rf-harness/src/data_regions.rs` (core drivers),
`crates/rf-harness/tests/w38_01_data_regions_{smb,smw}.rs` (ROM-gated,
`--ignored`), recipe section `[[data_region]]` in `rf-profiles`.

## Method

### 1. Observation (analysis runs only)

The finder is a `StepObserver` on the W36-09 per-instruction log: it sees the
CPU-bus accesses of each instruction (address, value, direction) and the atlas
function, and nothing else (no PC, no registers, no opcode table). Cores,
frame path and `rf-core-api` are untouched, so it costs nothing unless an
analysis run installs it; an observed frame is bit-identical to a plain one
(W36-09), so a run can switch between the two (the drivers observe only the
frames asked for).

A ROM *data* read is rebuilt from the access list:

1. the first access is the opcode fetch, so its address is the PC; the run of
   consecutive addresses after it is the operand (at most 3 bytes on the 6502,
   4 on the 65816), and the operand bytes are in the log;
2. the data read is the last ROM read after the operand that is not inside the
   instruction's own bytes. Earlier ROM reads are page-cross dummy reads, a
   `JSR`'s second operand byte arrives after its pushes, a taken branch reads a
   dummy byte, and a return reads the byte at the address it pulled. Those
   are excluded (branch opcode shape `xxy10000`, `BRA`/`BRL`, a stack read
   before the ROM read). Two ROM reads at consecutive addresses are one 16-bit
   read;
3. addressing: no RAM read before it means the operand is the base and
   `index = data address - base` (absolute: index 0); RAM reads before it mean
   the last two (65816 long pointer: three, when the third equals the data
   bank) are the pointer, and `index = data address - pointer`.

Per reading instruction (a *site*, keyed by its ROM offset) it keeps: hits,
the ROM bytes read, distinct index values, per-base `[lo, hi)` extents, how
often a read followed the previous one by 1 to 4 bytes (walking), the pointer's
RAM address, the RAM bytes read a few steps earlier whose value was the index
(the index's likely source) and how many reads fed a picture-unit data port.

CPU address to ROM offset comes from the harness: NES PRG window pages are read
from the mapper once per frame (a bank switch in the middle of a frame is seen
with the frame's mapping; neither game here switches banks while playing), the
SNES map is the fixed LoROM/HiROM one.

### 2. Value flow and links

A byte read from ROM and stored to RAM a few steps later (equal value, at most
8 steps old, most recent match wins) taints that RAM byte with the read
(`site, offset, index`). When an indirect read finds its pointer bytes tainted
with the right values, the table read and the pointer use form a **link**:
(low-byte site, high-byte site[, bank-byte site]), the entry's low-byte offset
and index, and the base the pointer led to. The value is used for the check and
dropped.

### 3. Inference (`infer`, pure and deterministic)

| Region | Rule |
|---|---|
| pointer table | a link group with at least 2 entries whose high-byte read keeps one fixed distance from the low-byte read in 80% or more of uses (coincidental value matches do not). Distance 1 (bank 2) is one interleaved table (stride = distance between entries); otherwise one row per byte (`part` lo / hi / bank). Start is the base the reading instruction names when it names one, else the first entry seen. |
| stream | indirect reads grouped by pointer RAM address (a level parser reads one object stream from many sites, each touching one field): the bytes read fill 75% or more of their span, or reads walk forward. Reads from bases within 8 bytes of each other are one stream (the code steps the pointer past a header). |
| graphics | a site where at least a quarter of reads (4 or more) landed in a picture-unit data port (NES `$2007`; SNES `$2118/$2119/$2122`) |
| element table | the rest: direct indexed reads with 2+ distinct indices (stride = gcd of index gaps) or sparse reads through a pointer |

Extents are what the game was seen reading: a lower bound. Confidence is a
heuristic 0-95 (entries, distance share, index source seen, fill, `via`).

### 4. Level association and the experiment (`associate`, `grade_rows`)

One facts set per level (the harness forces or reaches the level and cuts the
observation), then for each pointer feed seen in 2+ runs: entries used by every
run are set aside (shared lookups), each run's own entry is taken, and its
target is followed to the stream the run actually read from it (reads from a
base up to 8 bytes above the target). A feed **tracks the level** when at least
90% of runs used an entry of their own, at least 75% as many distinct entries as
runs, and every such run read a stream from the entry's target. Only rows the
experiment proves are `confirmed` (the feed's pointer-table halves and the
streams some run read from one of its targets); everything else stays
`candidate`. The recipe row's source says what was varied.

Runs that share an entry, or entries that alias one target, are reported, never
merged away.

## Super Mario Bros. (NES, multicart image, PRG offsets)

Experiment: the W36-04 level entry, test side only: write the observed
world/level bytes (`$075F/$0760`) into the title screen's Start frames for each
of the 32 levels; observe frames 380 to 1700 of each run (the load and about
1,200 frames of play).

- 32 levels observed in 110 s (release): 5,173,560 ROM data reads, 233 reading
  sites, 598,446 pointer uses; every ROM data read classified (0 unknown) and no
  direct index above 255 (the classification check).
- 5 pointer feeds seen in 2+ levels; **2 follow the level**:

| Feed (low-byte site) | Pointer table (lo / hi rows) | Entries seen | Levels with an own entry | Distinct entries / targets | Streams read | Grade |
|---|---|---|---|---|---|---|
| `0x1c39` | `ptrtab_001ce4` lo `0x1ce4` size 33, hi `0x1d06` size 33, stride 1 | 26 | 32 of 32 | 26 / 26 | 32 of 32 | confirmed |
| `0x1c4e` | `ptrtab_001d2c` lo `0x1d2c` size 34, hi `0x1d4e` size 34, stride 1 | 26 | 32 of 32 | 26 / 26 | 32 of 32 | confirmed |

  The other three feeds (3, 2 and 4 distinct entries over 32 levels) are
  coarse classes, not the level: not confirmed (a table of 7 entries at `0x5a`/`0x6d`, one of 4 at `0xb08`/`0xb0c`
  and an interleaved word table at `0x790e` size 167, stride 2, 9 entries stay
  candidates).
- Switching the level moves the pointer to a different table entry: the entry
  index (the register value at the table read) runs, by level 1-1 to 8-4, from
  the 26 values 0 to 33 in both tables (for example entry 11 for 1-1, 15 for
  1-2, 28 for 1-3 in the first; 8, 12, 25 in the second). 6 levels share an
  entry with another level (first table: entry 15 for 1-2, 2-2, 4-2 and 7-2;
  12 for 1-4 and 5-3; 13 for 2-4 and 7-4; 32 for 2-3 and 7-3), and for those the
  target and stream are the same, as they must be. Every pair of levels with
  different entries has different targets and different stream starts (asserted
  by the test).
- Streams: 50 confirmed stream rows for 52 distinct targets (rows that overlap
  are merged): 4 to 29 bytes for the first table's targets and 8 to 44 for the
  second's, as far as the 1,200 observed frames of play read (a stream is read
  as the screen scrolls, so extents are lower bounds). Two tables, two stream
  families, 26 targets each; the finder does not say which is the object
  stream of which kind and this note does not guess.
- 120 rows (9 pointer-table rows, 50 confirmed streams, and the 61 other
  candidates of confidence 60 or more; the cap of 150 was not reached) are in
  `profiles/nes/super-mario-bros/recipe.toml` between the finder markers.
  Counts by kind and grade over all 205 regions found: pointer table 4
  confirmed + 5 candidate; stream 50 confirmed + 42 candidate; graphics 21
  candidates; element table 83 candidates.

## Super Mario World (SNES, LoROM, ROM offsets)

Experiment: no level-select RAM write worked (see "tried and not used"), so
the levels are the ones the W36-08 menu-and-play script and the two W37-00 movies
reach: the script (5,069 frames, observed in 50 s) enters the opening level
(translevel `0x00`), `0x28` and `0x29`; the lsnes and Snes9x movies (8,000 and
14,000 frames, idle offsets 3 and 4 from W37-00's sweep, observed in 80 and
140 s) enter three translevels each and add `0x2a`. **4 levels in all.** One
facts set per level (from the frame the game leaves a level to the frame it
leaves the next), repeat visits merged.

- 23 pointer feeds seen in 2+ levels; **3 follow the level** (every run used an
  entry of its own, 4 distinct entries over 4 levels, a stream read in each
  run):

| Table (rows) | ROM start | Observed extent | Stride | Pointer | Entry index per level `0x00 / 0x28 / 0x29 / 0x2a` | Targets | Streams read in each run (size) | Grade |
|---|---|---|---|---|---|---|---|---|
| `ptrtab_02e000` | `0x2e000` | 789 | 3 | 3 bytes (RAM `$7e0065`) | 591 / 780 / 783 / 786 | 4 distinct | 24, 33, 283, 388 | confirmed |
| `ptrtab_02e600` | `0x2e600` | 788 | 3 | low+high read from the table (the bank byte is not) | 591 / 780 / 783 / 786 | 2 distinct: levels `0x00`, `0x28` and `0x29` reach the same target | 441 (x3), 766 | confirmed |
| `ptrtab_02ec00` | `0x2ec00` | 526 | 2 | 2 bytes | 394 / 520 / 522 / 524 | 4 distinct | 5, 20, 21, 45 | confirmed |

  Index 591, 780, 783, 786 are 3 x 197, 260, 261, 262: the first table is
  indexed by three times a level number, the second the same, the third by
  twice it (394 = 2 x 197, and so on). The finder reports the indices and
  nothing about what the numbers mean. The three tables start where the
  reading instruction's operand puts them (`0x2e000`, `0x2e600`, `0x2ec00`,
  `0x600` apart, the first two 3 bytes per entry), so the start is exact while
  the end is only the highest entry the four levels used.
- **Aliasing**: in the second table 3 pairs of levels use different entries
  that lead to one target; the test reports that and still holds "same entry,
  same target". Only the first and third tables give a distinct target per level
  (`distinct_targets == runs`, the assertion the test makes).
- Confirmed rows: the three tables and 9 streams: 33, 283 and 388 bytes (first
  table, levels `0x28`, `0x29`, `0x2a`), 441 and 766 (second), 5, 21, 45 and 20
  (third). The opening level's first-table stream (24 bytes at `0x30603`) lies
  inside a larger merged candidate region and stays a candidate.
- Candidates that the experiment did not confirm (their entries did not differ
  per level, or led to no stream read in the run): a split 3-byte table
  (`ptrtab_003992` lo/hi/bank, 44 bytes each, 20 entries), `ptrtab_0004d0` (21
  bytes, stride 3, 4 entries) and `ptrtab_028000` (16 bytes, stride 14, 2
  entries).
- 254 regions over the four levels: pointer table 3 confirmed + 5 candidate,
  stream 9 confirmed + 83 candidate, graphics 138 candidates, element table 16
  candidates. 167 rows (8 pointer-table rows, the 9 confirmed streams and the
  150 most confident other candidates of confidence 60 or more) are in
  `profiles/snes/super-mario-world/recipe.toml` between the finder markers. The
  graphics rows (136 of 138 over confidence 60) are code that stores ROM bytes to
  the VRAM/CGRAM ports one instruction at a time; DMA from ROM is not followed.

## Experiments tried and not used

- **SMW level entry by RAM.** The SMB trick (write the level number where the
  game reads it) did not carry over. Writing `$7E13BF` (translevel) while the
  game was in modes `0x11`/`0x12` (for 11 values, 89 frames each) left the
  level that loaded and the VRAM contents identical in every run
  (`vram 8f56...` for all 11), so the byte is an output of the level load, not
  its input. The level number the pointer lookups used is computed from the
  overworld position, and forcing it without reading a disassembly would mean
  guessing; the levels SMW reached are the ones the script and the movies
  reach.
- **Whole-movie observation.** The W37-00 movies stall after three levels each
  (6,000 frames without progress) and the finder's observed frames run at about
  90 frames/s on the 65816, so the runs are capped (8,000 and 14,000 frames).

## Limits and what is not done

- Table extents are observed extents. The base is known when the reading
  instruction names one (SMW `LDA.L abs,X`: `0x2e000`), so tables start there;
  their end is only what the run touched.
- The NES window map is per frame; a mapper that switches PRG banks mid-frame is
  resolved with the frame's start mapping.
- Value matching is heuristic (equal value within 8 steps). The 80% fixed
  distance rule, the target check (pointer value equals what the stream read
  began at) and the level experiment are what keep false pointer tables out of
  the confirmed rows; candidate rows can include coincidences.
- `index_ram` is recorded only when a RAM byte with the index's value was read
  within 6 steps in 60% or more of reads. In both games the level index is
  computed (shifted, multiplied by 3, added to a base), so the confirmed level
  pointer tables carry no `index_ram`; the experiment ties them to the forced
  world/level RAM instead, in the row's `source`.
- Jump tables (`JMP (abs)`, return-address dispatch) are not classified as code
  pointer tables.
- SNES DMA from ROM is not followed (graphics are found only through CPU stores
  to the picture-unit data ports).
