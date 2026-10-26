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

1. Open the game and the debugger's **Coverage** tab.
2. Press **Record coverage** and play for a while. RetroForge notes every
   instruction the game runs. Recording costs nothing when it is off.
3. Press **Rebuild on**. RetroForge builds the code map (a second or two) and
   switches the game to the rebuilt path. The tab shows how much of the code
   runs rebuilt, how much falls back to the plain emulator, and why.

Code the map has never seen just runs on the emulator as usual, and so does
code a game copies into RAM. Nothing breaks when the map is incomplete; the
fallback share goes up instead, and the tab says so.

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
