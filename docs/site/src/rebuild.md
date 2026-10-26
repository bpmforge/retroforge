# Rebuild mode: how it works

Rebuild mode goes a step past emulation. RetroForge records which code your
game runs, maps it into functions, and then runs the game through that map,
so individual functions can be watched, named, hooked and enhanced. It is
off by default, it is per game, and Original mode is never affected.

It works for NES and SNES games. The CPU's code is what gets rebuilt; the
picture, sound and cartridge chips stay emulated exactly as before.

## What you need

Only your own game. RetroForge never ships game code, rebuilt or otherwise.
Everything below is built on your machine, from your ROM, and stays there:

| Ships with RetroForge | Built on your machine, never shared |
|---|---|
| The recorder, the code map builder, the rebuilt-path runner and the checker | The code map of your ROM |
| Recipes: facts about a game such as function names, hook points and what RAM addresses mean | Recordings of what your ROM ran |
| Our own enhancement code (for example the live-enemies mod) | Anything cached from your ROM |

Recipes match functions by a fingerprint of their shape, not only by
address, so a ROM hack that keeps the original engine gets the same named
functions and mods. Code a hack changed simply runs without the recipe's
extras.

## Turning it on

Rebuild is a switch per game. Open the game, press Esc, choose
**Enhancements** and turn on **Rebuild**. It works for any NES or SNES game,
needs no profile and no recording first, and is off until you turn it on. It
is part of Enhanced and Game-Aware mode; in Original mode the switch is not
offered and nothing runs through it. RetroForge remembers the choice for that
game.

The game keeps running on the plain emulator while RetroForge builds a first
code map from the game's start-up code; resume play and the map is installed a
moment later. As you play, RetroForge notes the code the game runs and grows
the map every few seconds, and keeps it for next time (built on your machine,
from your ROM).

The card under the switch says what is going on, and never says the game is
rebuilt without the rest of the story next to it:

* **Coverage**: how much of the game's code has run so far, and the size of
  the map.
* **Tier mix**: of the instructions run since the map was last installed, the
  share that came from code seen in play, the share worked out from the code
  itself, and the share that ran on the plain emulator instead, with the
  reason (code running from RAM, code the map does not hold yet, code in a
  coprocessor's memory).
* **Verification**: *not checked*, or *checked: N frames in lockstep* (the
  number of frames a rebuilt and a plain copy of this game were compared on,
  with no difference), or a warning if they ever differed.
* A **Proven** badge, only when the game has passed a long check: at least
  36,000 frames (ten minutes of play) compared over at least two separate
  runs, with no difference at all. One difference anywhere removes the claim.

The debugger's **Coverage** tab stays the detailed view: record coverage by
hand, the executor's counters, dispatch misses, hooks and naming. Its
**Rebuild on** button is the same switch while the game is in Enhanced or
Game-Aware mode, and a direct control in the debugger's own modes.

Code the map has never seen just runs on the emulator as usual, and so does
code a game copies into RAM. Nothing breaks when the map is incomplete; the
fallback share goes up instead, and the card says so.

## How you know it is right

Every instruction on the rebuilt path runs the emulator's own instruction
code, so the result is identical by construction. RetroForge still checks:
the **lockstep checker** runs a rebuilt and a plain copy of the game side by
side and compares the processor, all memories, the picture and the sound on
every frame. If they ever differ it stops and saves a repro bundle (a save
state from just before, the last inputs and the last functions run).

Measured so far: all 32 levels of Super Mario Bros. and Yoshi's Island 1 of
Super Mario World, plus five Super Mario World ROM hacks, ran with zero
differences.

## What it makes possible

* **Hooks.** Lua scripts and plugins can run when a game function starts or
  ends (`rf.on_enter`, `rf.on_exit`). A hook that only reads changes nothing.
  A hook that writes memory is a mod: it needs the `write_memory` permission,
  every write is listed, saved in save states, and shown with a badge.
* **Mods from recipes.** The first is *live enemies* for Super Mario Bros. at
  32:9: enemies stay alive in the widened margins and are drawn there in the
  game's own art. Turn it on under the game's settings, **Mods**. It shows a
  "Patched" badge and keeps its own save states.
* **Naming.** **Name functions** in the Coverage tab names functions from what
  they do (reading the controllers, uploading graphics, updating sound).
  Names can be saved into the game's recipe. Names never change how the game
  runs.

## Optional: naming with Claude

You can let a cloud model (Claude) suggest names for the functions the rules
could not name. It is off until you tick **Allow AI naming of functions** in
Settings and add your own API key; the Coverage tab then shows **Name
functions + ask** with the model's name. Only behaviour facts are sent: how often a function runs, which
hardware registers and RAM it touches, and which functions it calls. Game
code, ROM bytes and the code map never leave your machine, and RetroForge
checks every request for them before it is sent. Every suggested name goes
through the same checks as any other, and it only labels a function; it
never changes code.

## Limits

* Speed is not the point: the rebuilt path runs at the same speed as the
  emulator. A faster compiled tier (Cranelift) is being measured next.
* Coprocessor games (Super FX, SA-1 and similar) rebuild only the main CPU.
* Coverage depends on what was played. Unplayed paths run on the emulator.
* The checker cannot catch a bug that both copies share, because the picture
  and sound hardware are the same emulated chips on both sides. The test ROM
  suites still guard those.

## Where the ideas come from

The rules for this work are recorded in the project's decision ledger
(D-024 to D-028). In short: nothing derived from a game's code ships; facts
are found by watching the game run, never copied from disassemblies or
decompilations; permissively licensed recompilers were read for ideas only.
The design is in `docs/design/REBUILD_LAB.md`.
