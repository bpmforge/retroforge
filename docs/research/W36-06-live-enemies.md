# W36-06: keeping Super Mario Bros. enemies alive in the 32:9 margins

Ticket W36-06 (simulation half). Written 2026-10-25 in our own words from
observation of a running game. **Clean room (D-021):** no disassembly,
decompilation or recompilation source was read; the only outside fact is the
RAM-map table the SMB profile already cites (Data Crystal), used as a list of
addresses to watch, each one confirmed by what it did in play. No ROM bytes and
no disassembly listing are in this note or in the repo; addresses below are
CPU addresses of the player's own cartridge, and the "constant" findings are
described by what they mean.

## The problem

At 32:9 the picture shows 300 px of margin each side of the NES's 256-px
screen (`Aspect::Wide32x9.output_width(240, 1.0)` = 856 dots, so
(856 - 256) / 2). SMB only brings an enemy to life about 48 px to the right of
the screen edge and removes it about 72 px outside either edge, so nearly all
of the margins are empty of enemies even when the game's level data has them.
To draw live enemies there (W36-06b) they must first exist in the simulation.

## Method

1. **Hypotheses from the RAM map.** Enemy slots are five bytes each: flag
   (`$0F-$13`: 0 free, 1 active), id (`$16-$1A`), state (`$1E-$22`), level x
   page and low byte (`$6E-$72`, `$87-$8B`); the camera is the screen's left
   edge (`$071A` page, `$071C` low byte) and right edge (`$071B`, `$071D`).
   Camera-relative x of an enemy = (page*256 + x) - camera. Watched in 1-1
   play every one behaved as described (flags toggled as enemies appeared and
   left, the relative x of a walker fell by its speed, Mario's own relative x
   matched `$0086`).
2. **Who changes the flag.** The harness steps the core one instruction at a
   time, peeks the five flags before and after each instruction, and logs the
   PC of any instruction that flips one, with the enemy's relative x (the
   `WatchTable` shape, done with peeks because the bytes sit at stable
   addresses and the PC is wanted directly). Over 1-1, 1-2, 2-1, 3-1, 4-1, 5-1
   with two input scripts the writers were few:
   * **Setters.** One PC inside the routine that looks at the next object in
     the level data and brings it to life: it fired with the enemy at relative
     x 301-303 (the screen is 256 wide), i.e. about 48 px past the right
     edge. A second setter places groups of two or three walkers at the screen's
     own right edge. Two more PCs (seen in 2-1, 3-1 and 4-1) set flags for
     generated objects at their own distances.
   * **Clearers.** One PC clears a flag at level start (everything cleared),
     and one PC is the small "empty this slot" routine. That routine has two
     callers, found by reading the return address off the stack when it
     entered: the squashed/dead-enemy cleanup (positive relative x) and the
     off-screen test, which fired with the enemy at relative x -74 to -75
     (just over 72 px left of the screen).
3. **PC to atlas function.** Each PC was located in the code atlas
   (W36-02) built from a coverage recording of the same play: the setter is in
   an atlas function entered by a call (named `enemy_activate` in the recipe),
   the off-screen test in an atlas function entered through the per-object
   dispatch (named `enemy_update`, the per-slot enemy routine whose tail is
   the off-screen check). Their shape and exact hashes are identical whether
   the atlas comes from 700 frames of 1-1, 2200 frames of 4-1 or either of the
   two input scripts, which is what makes a recipe row portable between
   players' recordings (a test asserts it).
4. **Reading the function.** The player's ROM, in memory, through our own
   6502 disassembler (`rf-nes` trace.rs) from the atlas function's
   instructions. Findings, as meanings:
   * **Activation lead (in `enemy_activate`).** The distance past the screen's
     right edge at which a level object comes alive is the screen's right
     edge plus one immediate (48 px), rounded down to a multiple of 16, as a
     16-bit page+low-byte add with carry. An object whose x is at or below
     that is brought to life; the immediate is the whole lead.
   * **Right cull (in `enemy_update`).** An enemy more than another immediate
     (72 px) past the right edge is removed, except a few kinds that are
     exempt. A 16-bit compare of level x against right edge plus the
     immediate.
   * **Left cull (same function).** An enemy more than an immediate (72-73 px,
     depending on a carry the previous compare leaves) left of the left edge
     is removed. Two enemy kinds get a different adjustment first.
   All three are one-byte immediates, found by comparing the values the game
   used in play (48, 72, 72-73) with the operands of the compares.
5. **Candidates under lockstep, keep what moves off-screen state.** Patching
   only the lead leaves enemies spawned in the margin but removed again at
   once or kept only to +72 (right margin frames drop to a handful); patching
   the cull alone changes nothing on the right (there are no enemies out
   there) and only keeps already-passed enemies alive on the left; all three
   together give sustained margin occupancy on both sides. The three-byte mod
   is the only one that did, so it is the one the recipe carries.

## What the mod is

Three `[[patch]]` rows in `profiles/nes/super-mario-bros/recipe.toml`
(`mod = "live-enemies"`), each a function by recipe name, an index into that
function's instruction-byte stream, and the new byte only:

| row | function | meaning | game's own | mod |
|---|---|---|---|---|
| `activation-lead` | `enemy_activate` | px past the right edge at which objects come alive | 48 | 240 |
| `cull-right` | `enemy_update` | px past the right edge before an enemy is removed | 72 | about 255 |
| `cull-left` | `enemy_update` | px left of the left edge before an enemy is removed | 72-73 | about 255 |

Because an immediate is not masked out of the shape hash, rows are resolved
against the atlas of the **unpatched** image and only if the function's exact
hash matches (`rf_rebuild::resolve_patches`); the old byte is never stored.

**Why not a hook poke or a replacement function.** The constants are
mid-function immediates the CPU reads on every call; a hook at the entry
cannot change them without writing the ROM, and a Rust replacement would not
produce the same bus cycles on this per-cycle core (REBUILD_LAB section 4).

## Margin width and the byte limit

300 px each side at 32:9 over 240 lines (272 over 224 lines). A byte holds at
most 255, so the right cull and the left cull are set to the largest correct
value (about 255), and the activation lead to 240 (it must stay below the
right cull or an object would be brought to life and removed in the same
frame; it rounds down to a multiple of 16). The outermost ~45 px of each
margin therefore cannot hold a kept-alive enemy. Nothing is lost that the
vanilla game had.

## Measured (ROM-gated `rf-harness` test `w36_06_live_enemies`)

Mod off, enemies never get farther than about 72 px outside the screen, so
the **deep bands** (right relative x 336 and up, left below -80) must be
empty. Counts are frames in play with at least one active enemy there.

| 1-1 | frames in play | right margin | left margin | deep right | deep left | longest deep run |
|---|---|---|---|---|---|---|
| unassisted, mod off | 1700 | 172 | 38 | 0 | 0 | 0 |
| unassisted, mod on | 1700 | 849 | 315 | 600 | 275 | 275 |
| Mario unhurt, mod off | 2700 | 180 | 484 | 0 | 0 | 0 |
| Mario unhurt, mod on | 2700 | 459 | 1063 | 242 | 913 | 302 |

"Unhurt" pokes the injury timer every frame in play on both variants (test
side only) so Mario meets many enemies. Kept-alive enemies move: of the
8-frame windows that ended in the deep band, 174 of 191 saw the enemy's level
x change in the unhurt run (31 of 32 on the right alone). Unassisted, Mario
dies in both variants and a death freezes every enemy, so that run's movement
count is not representative.

## Slot pressure

SMB has five enemy slots. The same 3200-frame unhurt run on each level, mod
off against the shipped mod (lead 240) and against shorter leads; "late"
counts spawns that came alive already inside the 256-px screen (a slot was
busy when the object's turn came); "all five" is frames with every slot
taken.

| level | variant | spawns | late | most active | all five |
|---|---|---|---|---|---|
| 1-1 | game's own | 21 | 0 | 5 | 49 |
| 1-1 | lead 128 | 21 | 0 | 5 | 275 |
| 1-1 | lead 176 | 22 | 0 | 5 | 152 |
| 1-1 | lead 240 | 22 | 0 | 5 | 175 |
| 1-2 | game's own | 16 | 0 | 5 | 125 |
| 1-2 | lead 128 | 15 | 0 | 5 | 246 |
| 1-2 | lead 176 | 18 | 0 | 5 | 367 |
| 1-2 | lead 240 | 18 | 0 | 5 | 430 |
| 4-1 | game's own | 20 | 10 | 4 | 0 |
| 4-1 | lead 128 | 23 | 11 | 5 | 26 |
| 4-1 | lead 176 | 24 | 12 | 5 | 32 |
| 4-1 | lead 240 | 21 | 9 | 4 | 0 |

Reading: kept-alive enemies hold their slots longer (frames with all five
taken go up three to four times in 1-1 and 1-2), but spawn counts did not
fall and no spawn came late in 1-1 or 1-2 (the 4-1 "late" ones are the
generated objects, unchanged by the mod, and equal in the game's own run).
So no bound on the margin was needed in these three levels and the full 240
is shipped. Dense levels were not measured; a recipe author who sees starved
spawns can ship a smaller lead as a different value of the same row.

## Things the mod does not do

* Group enemies (groups of two or three walkers) are placed at the 256-px screen's
  own right edge when their object comes alive. With the longer lead that
  happens earlier, so in the wide picture they appear mid-picture (at the
  old screen edge) rather than at the far margin. Not changed.
* Generated objects (seen in 2-1, 3-1 and 4-1) use their own spawn distances and
  culls; unchanged.
* Mario's own game is changed: enemies arrive from farther away, so the same
  input can end differently. It is a mod (NON_GOALS 13), off by default,
  with its own save states.
* Drawing the enemies in the margins, and the NES's 8-bit sprite x, is
  W36-06b.
