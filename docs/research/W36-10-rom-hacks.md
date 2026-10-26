# W36-10: the Super Mario World recipe on five ROM hacks

Ticket W36-10. Written 2026-10-26 in our own words from measurements of
running ROMs. **Clean room (D-021):** no disassembly or decompilation was read;
nothing here is a ROM byte, a patch byte or a listing. The patches are the
player's own downloads (`~/Games/Roms/snes/hacks`), applied in memory to his
own copy of the game and never committed.

## What was built

- **Entry mode on recipe rows.** The 65816's instruction widths depend on
  `E`/`M`/`X`, and a function's shape hash ignores them, so the same bytes
  entered at two widths are two atlas functions with one shape hash (W36-08).
  `RecipeFunction` and the TOML gained an optional `mode` (0 emulation, 1..=4
  native, `1 + m8 + 2 * x8`). A row with a mode matches only functions entered
  in it; a row without one (every 6502 row) matches any, so NES recipes are
  unchanged. The understand pass writes the mode on rows whenever the atlas has
  more than one mode.
- **Twin rows match only at their own address.** The SMW recipe holds `nmi`
  and `irq`, which are the same short stub: identical shape and exact hash. On
  a hack that changed one of them the old rule bound the name to the other
  (see Findings). Two rows of one recipe that share a shape (and could share a
  mode) now only match the function sitting at their own address hint; a
  hack that moved it leaves the row unmatched and its hooks inactive. (Superseded by W38-00:
  entry kinds first, address hint only among same-kind twins; the RDL and A
  Simple Story NMI stubs were edited by those hacks, so `nmi` correctly stays
  unmatched there.)
- **`profiles/snes/super-mario-world/recipe.toml`:** 29 function rows (the
  W36-09 rule names on the Yoshi's Island 1 run, each with shape hash, exact
  hash, address hint and mode) and 11 RAM rows (game mode, tick counter, camera,
  player position and state, level id, lives, coins, timer) taken from the
  profile's observed facts. Regenerating it reproduces the file exactly
  (`w36_10_smw_recipe`, ROM-gated), and `rf-profiles` has a no-ROM test that
  every row carries a mode and every RAM address is in the profile.

## Method (per hack, `w36_10_rom_hacks`, release, ROM-gated)

1. Apply the `.bps` in memory to the base ROM; the applier checks the patch's
   source size and CRC32 (all five matched the profile's dump).
2. Drive a plain core, recording coverage: Start and A alternating every 120
   frames through the title and file select; on the map one direction (cycling
   left, right, up, down) per visit, then A to enter the level; in a level a
   jump-and-run pattern. Stop at 1,500 in-level frames with the camera having
   moved 200 px, or at 5,500 frames. If the camera never scrolled, the W36-08b
   session script (the same route as the control) is tried too and the one that
   scrolled more is used. "In gameplay" is the profile fact `$7E0100 == $14`.
3. Build that run's atlas; match the recipe: exact (shape and exact hash),
   shape only, or unmatched.
4. Lockstep the rebuilt core against the plain core over the drive script.
5. Hook `nmi` read-only (a counter on the entry) and compare its count with
   the interrupt dispatches a plain core made into that function's entry in its
   mode, counted an instruction at a time.

## Results

| hack | patch bytes | ROM bytes | gameplay (`$7E0100 == $14`) | atlas functions | exact | shape | none | lockstep | fallback | `nmi` hook |
|---|---|---|---|---|---|---|---|---|---|---|
| SMW, unpatched (control) | - | 524,288 | yes, from frame 786; 2,616 level frames, camera span 589 (session script) | 770 | 29 | 0 | 0 | zero divergence, 5,069 frames | 0.455 % | 4,312 = 4,312 |
| Super Skyler World | 9,772 | 1,048,576 | yes, frame 727; 2,411 level frames, span 68 (generic) | 738 | 25 | 0 | 4 | zero, 5,500 | 0.425 % | 4,382 = 4,382 |
| Return to Dinosaur Land | 177,549 | 2,097,152 | yes, frame 778; 1,500 level frames, span 288 (generic) | 773 | 21 | 1 | 7 | zero, 4,579 | 0.272 % | row unmatched, inactive, 0 events |
| Kaizo Mario World | 108,411 | 2,097,152 | yes, frame 766; 1,533 level frames, span 0 (generic) | 753 | 27 | 0 | 2 | zero, 5,500 | 0.326 % | 3,996 = 3,996 |
| A Simple Story | 724,432 | 4,194,304 | yes, frame 816; 2,188 level frames, span 0 (generic) | 695 | 15 | 2 | 12 | zero, 5,500 | 0.216 % | row unmatched, inactive, 0 events |
| Brutal Mario Demo 7 | 1,243,080 | 4,194,304 | yes, frame 826; 2,841 level frames, span 40 (session script) | 790 | 22 | 0 | 7 | zero, 5,069 | 0.495 % | 4,329 = 4,329 |

The control matches all 29 rows exactly, so the recipe is sound on the game it
came from.  The exact, shape and none columns add up to the 29 recipe rows. Every fallback in every
run was work-RAM code (the game runs routines it copies to RAM); no width
mismatch, no missing node. The "ROM bytes" figure is the patched image (the
cartridge, with no copier header).

**Which rows are lost.** Return to Dinosaur Land and A Simple Story change the
most code: `reset` and the `nmi` stub are not found, and A Simple Story also
loses several upload and sound routines. Super Skyler World, Kaizo and Brutal
keep nearly everything. `hw_multiply_divide_04945d`, `vram_upload_00aa6b` and
the width variants `nmi_*_m1/m2`, `irq_*_m1/m3` often show as unmatched; a
function the drive never executed is not in that hack's atlas, and a width
variant exists only if the game ran in that width, so for those rows we cannot
tell "removed by the hack" from "not reached by this drive" (not checked per
row). The control, driven by the longer session script, matches all of them. The shape-only
matches (one in Return to Dinosaur Land, two in A Simple Story) are functions a
patch touched without moving.

**Gameplay.** All five reach the in-level game mode. Whether the camera moves
depends on where the generic drive lands: on a map it presses A on whatever tile
Mario stands, which is often the first small room, so Kaizo and A Simple Story
sat in a level for over a thousand frames with the camera not scrolling. That
still exercises the level code (sprites, controls, the NMI) but it is less
coverage than a scrolling run; the report states the span so the number is not
read as more than it is. The generic drive reached a scrolling level in two of
the five (Skyler, 68 px; Return to Dinosaur Land, 288 px); for Brutal Mario the
SMW session script scrolled 40 px. No hack needed other input to leave its title
screen.

## Findings

1. **Twin rows mis-bound on two hacks.** With the first matcher, Return to
   Dinosaur Land and A Simple Story bound the `nmi` row to the function at the
   IRQ stub's address (its shape was the only one left that equalled the row),
   so the "NMI" hook fired on every IRQ and call: 1,757 events against 1,576
   interrupt dispatches into that function. Nothing diverged (a read-only hook
   changes nothing), but the name was wrong. Fixed in the matcher (twin rows
   match only at their own hint); both hacks now report the row unmatched and
   the hook inactive. This is the kind of silent mis-binding the exact/shape
   split alone cannot see, because the two stubs have the same bytes.
2. **Zero divergence everywhere.** Rebuilt equals plain on all six runs
   (about 78 M instructions each). No repro bundle was needed.
3. **The independent NMI count has a subtlety.** A hook fires when a call or
   an interrupt enters the function. The plain-core count also saw 3 to 6
   other arrivals at the NMI stub per run (jumps, not calls); the hook did not
   count those, and the hook count equalled the dispatch count on all four runs
   where the row matched (the control and three hacks). The test counts dispatches (the stack grows by the
   interrupt's push size) landing in the function's own mode and asserts
   equality.
4. **Mode matters in the recipe.** Atlas functions for the same NMI handler at
   different widths are separate functions with the same shape; with `mode` the
   rows `nmi_00816a_m1` and `_m2` are distinct from `nmi`.

## Not done

- A row that is unmatched because the drive never reached that code is not
  distinguishable from one the hack removed; a longer or branching drive (or a
  per-hack route) would sort them.
- Only the five patches named in the ticket; the Kaizo Japanese patch was not
  run.
