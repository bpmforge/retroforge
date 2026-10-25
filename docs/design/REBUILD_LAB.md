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
   describes); 60+ fps by interpolating object and scroll positions between
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
| W36-01 | Coverage recorder (NES first, then SNES) + coverage view in debugger | SMB 1-1 recording lists every executed address; zero cost when off (bench) |
| W36-02 | `rf-rebuild` crate: code atlas + superset decode + per-ROM cache | SMB atlas covers 100% of recorded code; unseen bytes decoded without crash |
| W36-03 | 6502 IR + block executor + interpreter fallback (NROM) | SMB boots and plays through the rebuilt path; fallback rate reported |
| W36-04 | Lockstep checker + repro bundles | SMB 1-1 to 8-4 replay, zero divergence; fallback rate reported |
| W36-05 | Function hooks (Lua + Rust), recipe format with byte-hash matching | A hook mod changes one behaviour; recipe survives a relocated function |
| W36-06 | First Super Enhanced mod: SMB live enemies at 32:9 | Enemies stay active in margins; original mode byte-identical |
| W36-07 | Interpolated 60+ fps for objects and scroll (SMB) | Smooth at 120 Hz; simulation untouched (mode-invariant test) |
| W36-08 | 65816 lift with recorded M/X; SMW pilot | SMW Yoshi's Island 1 replay zero divergence |
| W36-09 | Understand pass + opt-in AI naming | Names exported as recipe facts; lockstep green |
| W36-10 | ROM-hack support: recipe matching on a Lunar Magic SMW hack | Hack boots rebuilt; matched functions get Super Enhanced features |
| W36-11 | Public guide: how Rebuild works | In docs/site |
| W36-12 | Cranelift JIT tier behind the block interface; spike vs block executor | SMB plays on the JIT in lockstep; compile time and frame cost measured for both tiers |

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
