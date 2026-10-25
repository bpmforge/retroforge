# Rebuild lab (W36): rebuild a game's code from the player's ROM

Status: design R1, 2026-10-25 (ticket W36-00). Rulings are recorded as
D-024..D-027 in `docs/DECISIONS.md`; tickets W36-00..W36-11 are in
`plan.json`. Research summary and public sources:
`docs/research/W36-00-rebuild-sources.md`.

## 1. Goal

Go past emulation: rebuild a game's CPU code from the player's own ROM as
named, hookable functions inside RetroForge, verified frame by frame against
the interpreter, so every function can be hooked, replaced and enhanced. The
PPU, APU and coprocessors stay emulated. Anything unproven runs on the
interpreter, which stays the reference (law 6).

## 2. Brad's rulings (2026-10-25)

Asked in the RetroForge audit doc; answers quoted as given.

1. **Clean room, recipes not code.** "people to use their own rom they need
   their rom to play but the full decomp and and such to know how to play the
   superenhanced is part of our client ... so we are not giving the game
   away"; ROM hacks "like those custom super mario games" should get "the
   basics". Follow-up card, same day: **recipes, rebuilt on first launch**
   (not encrypted pre-built code). Nothing derived from a game's code ships;
   the client ships our own Rust plus facts, and rebuilds on the player's
   machine. Encrypting a pre-built copy would still distribute it, with the
   key in the client. → D-024.
2. **In the app, per game.** "rebuild in the retro per game": one Rebuild
   mode inside RetroForge, no standalone executables. → D-024.
3. **Cloud AI naming, opt-in.** "yes" to an optional cloud model (e.g.
   Claude) for the naming pass. NON_GOALS 6 and 8 amended. → D-026.
4. **Pilot:** "yes pilot then super mario world": Super Mario Bros. (NROM),
   then Super Mario World.
5. **Priority:** "parrlell": runs alongside the cheats and RetroAchievements
   wave.
6. **Reading exception:** "yes extend" for permissively licensed recompiler
   source (gb-recompiled: MIT; nes-to-sms: MIT OR Apache-2.0), read like
   D-022 (ideas in our words, nothing copied). snesrecomp, nesrecomp and the
   SMB/SMW/DKC recomps are PolyForm Noncommercial (DKC1 metadata also
   carries GPL-3 provenance): READMEs and design docs only. GPL source stays
   excluded (D-018). → D-025.

## 3. What ships vs what is built locally

| Ships in RetroForge (ours, public repo) | Built on the player's machine, never shared |
|---|---|
| Recorder, atlas builder, lifter, block executor, lockstep checker | Rebuilt code of their ROM (lifted IR, compiled blocks) |
| Per-game recipes: function addresses, byte-hashes, names, hook points, RAM meanings (facts, like profile rows) | Code atlas and coverage for their dump |
| Our own Rust replacement functions (wider despawn check, interpolation, renderer hooks), as in-tree plugins | Cached graphics or level data from their ROM |
| Public how-it-works guide | Optional Rust source export (developer tool output, gitignored) |

Recipes match functions by a hash of their bytes as well as address, so a
ROM hack that keeps the engine reuses them; a function the hack changed falls
back to the generic rebuild with the interpreter as backup. Recipe facts are
found by observation (recorder, finders, scout), never copied from a
decompilation, recomp metadata or disassembly (D-021 stands).

## 4. Change from R0: how rebuilt code runs on a player's machine

R0 said "emit Rust per function". With recipes rebuilt on first launch, that
would need a Rust toolchain on every player's machine, which they do not
have. Brad approved a JIT dependency the same day (D-027, answering the
Rebuild Player Guide). So R1 splits the roles into tiers that share one IR:

- **Tier 0, interpreter:** today's 6502/65816 cores. Unseen code, RAM code,
  failed guards, first block after a savestate.
- **Block executor (W36-03):** lifted IR compiled at load into pre-decoded
  blocks (each instruction a pre-resolved operation with its operands, flags
  made explicit, cycle cost attached), run inside the core. No compiler, no
  platform code; it is the reference for the JIT and the fallback for any
  platform the JIT does not support.
- **Cranelift JIT (W36-12):** the same IR compiled to native arm64/x86-64 at
  launch and on each new function. The ticket adds the dependency (decision
  row in `docs/TECH_STACK.md` section 2) and measures it against the block
  executor on the SMB atlas before it becomes the default generic tier.
- **Developer tool:** Rust source emission from the IR, for reading a
  function and for writing our replacement functions against its shape.
  Output stays local and gitignored; it is never committed for a commercial
  game. Shipped Rust is only code we own (tier 3 replacement functions).

**W36-03 as built (2026-10-25):** the block executor is the interpreter with
a map. Per instruction it resolves `pc` to a PRG offset and an atlas node and
runs the core's own cycle-exact instruction code, so every bus cycle, dummy
read and interrupt poll is identical by construction; no node means plain
interpreter fallback. The semantic IR (explicit flags, ALU ops) and the Rust
emitter move to W36-12, whose JIT is their first executing consumer.

**Known tension, undecided:** a faster tier (W37-02's 3x, the guide's
"3 to 5 times") needs compiled code to run CPU-only while the PPU and APU
catch up at hardware accesses (sync points). The NES core ticks devices on
every bus cycle, so that is a scheduling change, not an executor feature. It
will be decided once the lockstep checker (W36-04) can prove it.

**W36-06 as built (2026-10-25):** the NES widescreen enemy mod is three
recipe patch rows (`rf_rebuild::PatchRow`; TOML `[[patch]]`; stream index plus
new byte), not a hook and not a replacement function. Resolved against the
unpatched atlas, applied in memory through the W32-01 session (badge, own
saves), loaded by the app from the Mods list in the game settings
(`GameSettings::mods_enabled`, atlas from the per-ROM cache). The Coverage tab
also gets a "Load hook script" entry for `CoreCommand::SetHooks` (read-only
manifest). Run-ahead's shadow machine is opened from the same `Vec` the real
core is, so a patched image is predicted correctly (test in `run_ahead`); hook
writes in savestates and the margin drawing stay W36-06b.

Speed is not the goal (the interpreter already runs far above real time);
named, hookable, replaceable functions and checking real compiled code in
lockstep are.

## 5. Pipeline

1. **Record** (in the emulator): every executed (bank, address, 65816 M/X,
   emulation flag), every indirect JMP/JSR/RTS-trick target, mapper bank
   state at fetch, RAM-executed code, interrupt entries. Sources: player
   sessions, scout, `.rfreplay` runs, random-input fuzz from savestates.
   Pay-for-use like `WatchTable` (rf-core-api `watch.rs`): one armed bool,
   zero cost when off.
2. **Code atlas**: functions and CFG from the recording, keyed by
   (bank, pc, entry M, entry X); superset decode (every offset) for bytes
   never seen. Cached per ROM hash next to the scout cache.
3. **Lift**: IR with explicit flags and first-class hardware ops (PPU, APU,
   mapper register access), per function, cycle costs per instruction.
   Guarded dispatch for indirect jumps: compare against recorded targets,
   else hand the PC to the interpreter. RAM-resident and self-modifying code
   always interpreted.
4. **Lockstep check**: rebuilt and interpreted cores side by side over
   `.rfreplay` runs; compare CPU state, WRAM, VRAM, OAM, CGRAM every frame;
   stop at first divergence; save a repro bundle (savestate, input tail,
   function trace). This cannot catch PPU/APU bugs (shared devices), so the
   census and test ROMs still guard those.
5. **Understand**: names for functions and RAM from the camera/item/tick
   finders and profiles; optional AI pass (opt-in, can be cloud per D-026).
   Every rename or rewrite must keep the lockstep check green.
6. **Rebuild**: Lua and Rust hooks at function entry/exit, replace-function
   overrides; live widescreen margins by overriding object culling,
   activation and OAM packing only (the approach DKC1Recomp's README
   describes); on NES a changed threshold is a recipe-relative ROM patch
   (function by shape hash, offset, new byte) through the soft-patch session,
   and margin objects are drawn from the object table because NES sprite X is
   8-bit; 60+ fps by interpolating object and scroll positions between
   game ticks; Diorama and a new renderer fed by real object tables.

## 6. Where it lives (layer law, ARCHITECTURE §3)

- **rf-core-api**: the shared recording shape (`CoverageTable`, armed-bool
  like `WatchTable`) and the hook event the cores emit. No rf-* deps, as now.
- **New crate `rf-rebuild`** (core layer; depends on rf-core-api only; added
  to `CORE_CRATES` and the determinism lint in `scripts/validate-arch.sh` and
  to the `validate-plan.mjs` crate vocabulary when W36-02 creates it): the IR,
  atlas/CFG builder, recording format, recipe format with byte-hash matching,
  guarded dispatch, and a block executor generic over a `RebuildBus` trait
  and an `Isa` trait.
- **rf-nes / rf-snes** (`rebuild` module each): the 6502 and 65816 lifters
  (they own the decode tables already) and the `RebuildBus` impl over their
  real bus. Cores never import upper layers.
- **rf-harness**: the lockstep checker and repro bundles (it may already
  depend on both cores).
- **retroforge**: the Rebuild mode toggle, coverage view, rebuild-on-first-
  launch progress, honest fallback-rate display.
- **Replacement functions** (Super Enhanced): in-tree plugins keyed by
  recipe, not engine code (NON_GOALS 16, law 5: no titles in engine crates).
  Recipes live as data under `profiles/`.

## 7. Laws this must keep

- **Law 4, determinism and the core thread.** The block executor runs on the
  core thread inside the core's own step, against the same bus with the same
  per-instruction cycle accounting. Interrupt and DMA deadlines are checked
  at every instruction boundary, never only at block ends, or NMI/IRQ timing
  would drift. Savestates stay interchangeable: CPU state lives in the same
  registers, and the executor only yields at instruction boundaries.
- **Law 6, Accuracy Mode is the reference.** Rebuild mode is opt-in per game.
  With no hooks or mods armed, Rebuild mode must be state-identical to the
  interpreter (the lockstep check is the proof). A fresh install boots in
  Accuracy Mode with Rebuild off.
- **NON_GOALS 13.** Hooks that write memory and replace-function overrides
  are mod-tier: explicit, per game, ledgered, recorded in savestates.

## 8. Tickets

| Id | Ticket | Acceptance |
|---|---|---|
| W36-00 | This design doc + D-024..D-026 + NON_GOALS amendment | Merged; rulings cited |
| W36-01 | Coverage recorder (NES) + coverage view in debugger (SNES recording moves to W36-08) | SMB 1-1 recording lists every executed address; zero cost when off (bench) |
| W36-02 | `rf-rebuild` crate: code atlas + superset decode + per-ROM cache | SMB atlas covers 100% of recorded code; unseen bytes decoded without crash |
| W36-03 | Block executor over the atlas + interpreter fallback (SMB cart, mapper 66: no standalone NROM dump on hand) | SMB boots and plays through the rebuilt path; fallback rate reported |
| W36-04 | Lockstep checker + repro bundles | SMB 1-1 to 8-4 replay, zero divergence; fallback rate reported |
| W36-05 | Function hooks (Lua + Rust), recipe format with byte-hash matching | A hook mod changes one behaviour; recipe survives a relocated function |
| W36-06 | First Super Enhanced mod, simulation: SMB enemies stay active in the 32:9 margins (recipe-relative ROM patch) | Activity measured in the margins; mod off byte-identical; lockstep green on the patched image |
| W36-06b | Same mod, presentation: draw margin enemies in the NES pad; mod writes in savestates; run-ahead parity | Screenshot; original mode byte-identical |
| W36-07 | Interpolated 60+ fps for objects and scroll (SMB) | Smooth at 120 Hz; simulation untouched (mode-invariant test) |
| W36-08 | SNES recorder (bank, M, X, E), 65816 ISA, SMW atlas | Every executed SMW opcode in the atlas |
| W36-08b | SNES executor + lockstep; SMW pilot | SMW Yoshi's Island 1 replay zero divergence |
| W36-09 | Understand pass + opt-in AI naming | Names exported as recipe facts; lockstep green |
| W36-10 | ROM-hack support: recipe matching on a Lunar Magic SMW hack | Hack boots rebuilt; matched functions get Super Enhanced features |
| W36-11 | Public guide: how Rebuild works | In docs/site |
| W36-12 | Cranelift JIT tier behind the block interface; spike vs block executor | SMB plays on the JIT in lockstep; compile time and frame cost measured for both tiers |

### W36-06b: how a margin object is drawn (built 2026-10-25)

**Observed first.** With the live-enemies mod on, the game never draws a margin enemy where it is: in the
1-1 unhurt script, of 1,570 slot-frames deep in a margin (right 283, left 1,287), 0 had OAM entries
wrapped onto the real picture, 1,566 had the enemy's own tiles in OAM with Y pushed off the visible
range (hidden) and 4 had none (absent). (An earlier pass of the classifier counted 81 left frames as
wrapped; they were a real enemy standing on the picture at the wrapped X, now excluded.) So the
design is the shifted-camera redraw, not relocation of wrapped entries.

**Redraw.** `retroforge::margin_draw` keeps the machine state from before each of the last three frames
(`MachineSnapshot`). When a recipe object-table slot is outside the picture, a private `EmuStepper` of
the same (patched) image is restored from the state two frames back, both camera pages (recipe RAM
meanings) are moved by one page toward the object, and it runs three frames with the real inputs. The
picture of frame N is drawn from the OAM copied in the vblank before it, built by logic two frames
earlier (`w36_06b_oam_observation::which_frames_oam_does_the_picture_come_from`: 899 of 899), which is why
the camera is moved two frames before. The PPU's own drawn-sprite list of the third frame
(`DrawnSprite`, now carrying the OAM index) is filtered to tiles within tolerance of the objects' expected
spots, the player excluded, and rasterised from the copy's pattern bytes and palette into a 256 x 240
indexed strip per side. With the camera moved by exactly one page, an 8-bit sprite X in the copy is the
same number the real machine would hold, which is what lets a wrapped copy on the real picture be told
by its OAM bytes (`ghost_indices`) and its dots by the per-dot sprite id; the compositor gives those
dots back to the background colour the PPU computed under them (`Ppu::bg_under`, a presentation-only
capture, off by default).

**Recipe vocabulary** read by the redraw (names, not titles): `enemy_flag` (width = slot count),
`enemy_x_page`, `enemy_x`, `enemy_y`, `screen_left_page`, `screen_left_x`, `screen_right_page`, optional `player_screen_x`, `player_y`.

**Cost.** Release build, M2 Max, 1-1 unhurt 3,200 frames: the real frame 1.8 ms, the redraw 3.4 ms on
average and 12.3 ms at worst (three replayed frames per side).

**Mod writes in states.** The live save path stamps `PROF`: mod ids, the patch rows (recipe function,
its exact hash, offset, value) and the hook host's write ledger, as a tail after the v1 fields. A load
restores the ledger into the host and warns when the session's mods differ from the state's.

**Copies and hooks.** Run-ahead's shadow and the redraw machine are built from the session's image but
carry no hook host, so a host holding `write_memory` turns both off, with a badge.

### W36-08: the SNES recorder and the 65816 atlas (built 2026-10-25)

**Width model.** The 65816's instruction length depends on `M` (accumulator
immediates), `X` (index immediates) and `E` (which forces both to 8 bits), so
the identity of an instruction is `(ROM offset, mode)`. A *mode* is an index
below `Isa::mode_count`: 0 is emulation, 1..=4 native with `1 + m8 + 2 * x8`;
those are the only five states the chip can be in (fullsnes, "SNES CPU 65XX
Microprocessor"). The 6502 has one mode. The recorder stores the modes an
opcode ran in as five bits of its `prg_flags` byte (`MODE_SHIFT`), the atlas
keys every node, block and function by `(offset, mode)` (format **v3**, cache
name `code-atlas-v3`: the NES atlas moved to v3 too, so cached v2 entries are a
clean miss), and the same bytes entered in two widths are two functions.

**Static width tracking.** `Decoded::after` says what an instruction does to
the mode of the code after it: `Same`, `Set(mode)` for `REP`/`SEP` with their
immediate (native mode only; emulation ignores both), or `Unknown` for `XCE`
(carry) and `PLP` (stack). After `Unknown` the builder follows the successor
only in modes the recording saw it run in, and an edge with no observed mode is
`Why::WidthUnknown`, never a guess. Calls enter the callee in the caller's
mode; the instruction after a call is also entered in every mode it was seen
in, since a callee may change widths before returning. A decode that did not
execute is accepted only if it cannot contradict what ran: when the offset ran
in other modes it must decode to the same length as one of them, otherwise
none of its bytes may have executed (`overlap_rejected` counts the refusals).

**Addresses.** The LoROM/HiROM map is fixed, so the builder asks the ISA
(`fixed_mapping`, `rom_offset`, `cpu_for_rom`) instead of using the recorded
bank windows. 16-bit targets keep the site's bank (branches and fall-through
wrap inside it); `JSL`/`JML` carry 24 bits. `rf_snes::mapping::rom_offset`
and `SnesBus::rom_offset` are tested against the byte the bus returns across
both layouts. Boards that switch ROM banks (SA-1, S-DD1) are not covered by
the static map.

**Recorder.** `SnesSystem::step` peeks before `Cpu::step` (bank, PC, mode,
operand bytes by the table length for the widths in force; nothing is recorded
for a CPU halted in `WAI`/`STP`), notes the indirect edge right after it
(`JMP (abs)`/`(abs,X)`/`[abs]`/`JML [abs]`, `JSR (abs,X)`, `RTS`, `RTL`, `RTI`,
`BRK`, `COP`) and notes NMI/IRQ entries at the dispatch and the Reset entry at
reset. Work-RAM execution goes to the volatile set (`ram_exec` is now sized by
the core: 128 KiB of WRAM offsets on the SNES). Fetches from coprocessor memory
(SA-1 I-RAM/BW-RAM, Super FX RAM, ...) are counted in `coproc_exec`, other
non-ROM, non-WRAM fetches in `other_exec`; the coprocessors' own CPUs are never
recorded. One `Option::is_some` per instruction when off.

## 9. After W36: phases B to E (approved 2026-10-25, ticketed W37..W40)

Brad approved the follow-on path ("ok do this") from the Rebuild Player
Guide. Answers folded in: engine recipes after SMB and SMW follow his
showcase order, Zelda, Super Metroid, Castlevania, Mega Man; SA-1 lifting is
in scope; a community recipe hub is approved once a moderation rule exists;
shadow-core lockstep (a second interpreted core checking rebuilt play) is on
by default while a game is unproven and switches off once it is proven.

| Phase | Wave | What | Gate |
|---|---|---|---|
| B, generic runtime | W37 | Input-movie coverage import (.fm2, .lsmv, .bk2; a desync is a compatibility report), Cranelift as the default generic tier, portable fallback tier, Rebuild for any ROM with a coverage/tier/verification panel, shadow-core lockstep, savestate fuzz, savestates across tiers. W37-06 ships generic interpolated 60/120 fps from OAM deltas early, latency labelled | Census reaches first-screen hashes through the generic tier with zero regressions |
| C, recipes and finders | W38 | Recipe format v1 with grades; data-region, object-table and collision-table finders (Diorama heights derive per engine); widescreen checklist finders so widescreen fixes come from recipes, not per-game code; engine recipes Zelda 1, Super Metroid, Castlevania, Mega Man 2 and X; recipe RAM maps to and from cheats and RetroAchievements | 20 random SMW hacks boot rebuilt with live margins, no per-hack work |
| D, Super Enhanced rendering | W39 | Plugin surface (logic, fact, render), no-latency interpolation, object-aware Diorama, baked level geometry, HD-2D render plugin (Triangle Strategy-style tilt, shadows, depth of field on one engine), MetalFX temporal upscaling, local AI HD asset pass over the tile cache, shader presets | Original mode byte-identical |
| E, community | W40 | Recipe hub with the checker as referee, local on-device naming model, SA-1 lifting (the public guide is W36-11) | Moderation rule approved by Brad |

Plugin kinds over the recorded "living decomp": logic plugins (hooks,
overrides) run under the lockstep checker; fact plugins (names, RAM maps,
tile semantics) change nothing at run time; render plugins only read and
draw. Out of scope throughout: standalone executables, lifting Super FX,
DSP, Cx4 or SPC700, and anything that ships game code.

## 10. Risks

- Coverage gaps leave code interpreted; report the rate honestly in the UI.
- Mid-frame raster timing; keep per-instruction cycle accounting.
- MMC3 IRQ timing, mid-frame CHR switching, undocumented opcodes (NES).
- Community backlash against AI ports: be open about AI use; everything
  verified by the lockstep check; the AI pass only names, it never writes
  code that skips the check.
