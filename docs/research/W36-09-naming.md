# W36-09: naming a game's functions by what they do

Ticket W36-09 (understand pass and opt-in AI naming). Written 2026-10-25 in our
own words from observation of running games. **Clean room (D-021, D-024):** no
disassembly, decompilation or recompilation source was read. Nothing here is a
ROM byte or a listing; addresses are CPU addresses of the player's own
cartridge, as in the other W36 notes, and every "evidence" sentence is a
measurement (a count, a register, a RAM range), not code.

## Method

1. **Observe.** Record a drive script and build the atlas (as W36-04 and
   W36-08b do), then run the same script again under the block executor with
   every atlas function hooked and a per-instruction observer
   (`EmulatorCore::run_frame_observed`). The observer is told, after each
   instruction, which atlas function it ran in and every CPU-bus access it
   made (address, value, read or write). Nothing is paid on the shipped frame
   path: the access log rides on the cores' existing "is a watchpoint armed"
   test (`WatchTable::set_observe`), and a test per core checks that an
   observed frame leaves the machine bit-identical to `run_frame`.
2. **Profile.** Each atlas function gets a behaviour profile: calls and
   arrivals, frames it was active in, instructions in the main loop versus NMI
   versus IRQ context, callers and callees, hardware registers touched by class
   and address with read and write counts, RAM ranges read and written, and a
   few value-derived facts (a write of `$3F` to the PPU address port; a status
   read that saw the vblank flag; a DMA channel pointed at VRAM, OAM or CGRAM).
   Registers are classified with the console's own map (nesdev "CPU memory map",
   "PPU registers", "APU registers", "OAM DMA", "Controller reading"; fullsnes
   "SNES Memory Map", "PPU Control", "DMA and HDMA", "Maths", "Timers and I/O",
   "Interrupt Vectors"). ROM reads and the stack page are not classified: they
   say nothing about what a function does to the machine.
3. **Name.** Generic rules turn a profile into a name, a confidence and the
   evidence in words. Several rules can fire on one function; the most
   confident wins (ties go to the rule listed first) and the others are kept as
   "also fired". Names are unique within an atlas: a name earned by one
   function stays bare; duplicates take their entry address (and decode width
   on the 65816); the busiest of several functions that are the same code at
   different widths keeps the bare `nmi` or `irq`.
4. **Export.** Accepted names become recipe rows: `source = "rule:<rule>"` or
   `"ai:<model>"`, a `confidence`, the exact hash and the entry address as a
   hint. They round-trip through the recipe TOML and are written as a marked
   block, so re-running replaces them without touching people's rows. Rows
   people wrote win, and a recipe cannot tell apart two functions with the same
   bytes at the same address (one code body entered at two 65816 widths), so
   those share one row named for the busiest.
5. **Prove it changes nothing.** The ROM-gated tests export every proposal,
   resolve the rows against the atlas, hook every resolved function and run the
   lockstep checker with a hook sink that does nothing: rebuilt and plain agree
   over the whole run on both games.

## The rules

| Name | Console | Fires when | Confidence |
|---|---|---|---|
| `reset` | both | function the reset vector points at, and it ran | 99 |
| `nmi` | both | function entered by an interrupt sequence at an NMI vector | 99 |
| `irq` | both | function entered by an interrupt sequence at an IRQ/BRK vector | 99 |
| `oam_dma` | NES | writes $4014 | 97 |
| `oam_dma` | SNES | DMA channel set to OAM data ($43x1 = $04) and $420B written; or 16+ writes to $2104 | 85 / 70 |
| `read_controllers` | NES | 8+ reads of $4016/$4017 and a strobe write to $4016 (80 without the strobe) | 92 / 80 |
| `read_controllers` | SNES | 2+ reads of $4218-$421F (auto-read), or 8+ reads of $4016/$4017 | 85 / 80 |
| `palette_upload` | NES | $2007 writes after $2006 was set to $3Fxx, no nametable address, at most 48 data writes per call | 85 |
| `palette_upload` | SNES | 2+ writes to $2122, or DMA channel set to CGRAM data ($43x1 = $22) with $420B written | 85 |
| `vram_upload` | NES | 16+ writes to $2007 with $2006 written, unless only $3Fxx was addressed | 88 |
| `vram_upload` | SNES | 16+ writes to $2118/$2119, or DMA channel set to VRAM data ($43x1 = $18/$19) with $420B written | 85 |
| `sound_update` | NES | writes 3+ distinct APU registers ($4000-$4013, $4015, $4017); one or two: 65 | 90 / 65 |
| `sound_update` | SNES | 2+ writes to the sound ports $2140-$2143 (one: 60) | 85 / 60 |
| `mapper_bank_switch` | NES | writes to cartridge space ($8000-$FFFF or $4020-$5FFF) | 85 |
| `wait_for_vblank` | NES | 8+ reads of $2002 per call and a read with bit 7 set | 85 |
| `wait_for_vblank` | SNES | 8+ reads of $4210/$4212 per call and a read with bit 7 set | 85 |
| `wait_for_sprite_zero` | NES | 8+ reads of $2002 per call, bit 6 seen, bit 7 never | 75 |
| `hw_multiply_divide` | SNES | 2+ writes to $4202-$4206 | 80 |
| `hdma_setup` | SNES | writes $420C | 80 |
| `scroll_update` | both | NES: 2+ writes to $2005 and none to $2007; SNES: 4+ writes to the BG scroll registers $210D-$2114 | 70 |
| `frame_main_loop` | both | runs in the main context in 90%+ of frames, at most twice per frame, calls 3+ distinct functions; the busiest such function | 55 |

The rules never mention a title. They name only what the hardware makes
nameable; game logic (physics, enemy AI, level loading) has no hardware
signature, which is what the opt-in model pass is for.

## Results

Drive scripts and atlases are the ones the W36-04 (SMB) and W36-08b (SMW)
lockstep tests use. Release build, Apple M2 Max.

| | Super Mario Bros. (the mapper-66 multicart) | Super Mario World |
|---|---|---|
| Frames observed | 1,700 (menus, then 1-1) | 5,069 (menus to Yoshi's Island 1, then 1,500 level frames) |
| Atlas functions | 304 | 770 |
| Functions that ran | 243 | 451 |
| Named by rules | 25 | 29 |
| Observation time | 3.6 s | 47.7 s |
| Recipe rows after export | 25 | 27 (two proposals share a row with another function: the same code entered at a different width) |
| Lockstep with every name hooked and inert | 1,700 frames, no divergence | 5,069 frames, no divergence |

Named by rule (function count): SMB `sound_update` 10, `vram_upload` 5, `nmi` 2
(the busier is the game's, the other the multicart menu's), `read_controllers` 2, `mapper_bank_switch`
2, `reset`, `wait_for_vblank`, `scroll_update`, `palette_upload` 1 each. SMW
`vram_upload` 11, `nmi` 3, `irq` 3, `palette_upload` 3, `sound_update` 2,
`read_controllers` 2, `hw_multiply_divide` 2, `reset`, `oam_dma`, `hdma_setup` 1
each. (The three `nmi` and three `irq` in SMW are the same two routines entered
at different 65816 register widths: three functions each in the atlas.)

### A sample, in our words

| Name | Rule | Evidence summary |
|---|---|---|
| `nmi` (SMB, the busier of two) | `nmi` 99 | entered by the interrupt sequence 1,487 times in 1,700 frames |
| `read_controllers_8e5c` (SMB) | `read_controllers` 92 | 23,792 reads of the pad ports and it strobes the strobe port |
| `vram_upload_8e19` (SMB) | `vram_upload` 88 | 2,048 PPU data writes per call after setting the PPU address |
| `palette_upload` (SMB) | `palette_upload` 85 | sets the PPU address into the palette range, writes 32 bytes per call |
| `wait_for_vblank` (SMB) | `wait_for_vblank` 85 | polls the PPU status port thousands of times per call and sees the vblank flag |
| `sound_update_f667` (SMB) | `sound_update` 90 | writes nine different APU registers |
| `scroll_update` (SMB) | `scroll_update` 70 | 398 writes to the scroll port and no PPU data writes |
| `mapper_bank_switch_84f4` (SMB) | `mapper_bank_switch` 85 | 195 writes to cartridge space (bank select on this multicart) |
| `nmi` (SMW) | `nmi` 99 | entered 4,312 times in 5,069 frames |
| `irq` (SMW) | `irq` 99 | entered 2,600 times by the interrupt sequence |
| `oam_dma` (SMW) | `oam_dma` 85 | points a DMA channel at OAM data and starts it, 4,326 times |
| `vram_upload_0085fa` (SMW) | `vram_upload` 85 | DMA channel pointed at VRAM, over a hundred thousand data-port writes |
| `palette_upload_00922f` (SMW) | `palette_upload` 85 | DMA channel pointed at CGRAM |
| `read_controllers_008650` (SMW) | `read_controllers` 85 | reads the auto-joypad result registers |
| `hw_multiply_divide_00cc14` (SMW) | `hw_multiply_divide` 80 | tens of thousands of writes to the multiply/divide unit |
| `hdma_setup` (SMW) | `hdma_setup` 80 | writes the HDMA enable port |

### What the rules did not name, and why

- **No `oam_dma` in SMB.** SMB triggers OAM DMA inside its NMI handler, so the
  handler is `nmi` (99) and the DMA is evidence on it, not a separate function.
- **No `frame_main_loop` in either game.** No function met the rule's
  thresholds in these runs (a main loop that lives inside the reset function,
  which the vector rule names first, would not). It is there for games that
  keep one in its own function.
- **`sound_update_008079` in SMW (261,568 writes to the sound ports)** writes
  far more than a per-frame sound routine would; it is more likely a bulk
  transfer through the same four ports (the sound program's upload). The rule
  names the hardware use; a person or the model pass would refine it. Not
  checked further here.
- **Rules only name hardware users.** 61 of SMB's and 319 of SMW's atlas
  functions never ran in these scripts, and most of the ones that did are game
  logic with no register signature.

## The AI pass, and what it may see

The pass is off by default and needs the player to opt in and supply a key. It
asks about the observed functions the rules left unnamed, in batches of 40, at
most six requests per run. Each function goes as a closed record of behaviour
facts: calls per 100 frames, share of frames active, instructions per call,
context shares, whether an interrupt entered it, caller and callee ids with
counts, registers by class and address with read/write counts, the value-derived
facts above as a closed word list, RAM ranges read and written with the
recipe's own RAM names, the accepted names of neighbours and the rules' name as
a hint. It deliberately has **no static property of the code** at all (no size,
entry address, block count, hash).

`rf_ai::naming::audit_request` checks every request: every object key is on a
fixed list, every string value is in a closed vocabulary plus the names the
caller vouches for, no function hash appears, and no 4-byte window of the
cartridge appears as hex or as a decimal list. The unit tests prove it fails
on a stray key, an unknown string, a hash and a ROM window. The ROM-gated tests
run it on the real requests: SMB, 5 requests for 195 functions against the
81,936-byte image; SMW, 10 requests for 390 functions against the 524,288-byte
image. Both clean.

The request and the parser follow the brief for this ticket (Messages API,
`output_config` with a JSON schema, `stop_reason` read before content, 429/5xx
retried with backoff, other 4xx not) and are tested on hand-written captures
and a scripted transport. **No real call to any AI service was made in
building or testing this ticket**, so the live round trip is untested; the
first real run is the player's.

## Costs and limits

- Observation steps every instruction through the cores' normal code, with a
  hook per function and an access log: SMB 1,700 frames take 3.6 s, SMW 5,069
  frames 47.7 s (plain `run_frame` speed was not measured here). Analysis
  only; the app runs it on a private copy of the machine on a worker thread,
  300 frames (five seconds of game time) by default.
- The profile is only as broad as the play that produced it. A function the
  script never reached has no profile and no name.
- Value-derived facts are a short fixed list. Rules that need more (what a
  routine wrote to a RAM table) are for a later pass.
- Context (main / NMI / IRQ) comes from a mirror of the executor's hooked call
  stack; it is bounded at 64 frames, deeper nesting forgets the oldest.
