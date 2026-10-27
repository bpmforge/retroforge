# Design: Cheats and RetroAchievements (wave W41)

Status: approved for build 2026-10-27 (D-030, Brad asked for the wave).
Revisits NON_GOALS 14, which D-000 says needs a design doc: this is it.

## 1. What ships

| Feature | NES | SNES |
|---|---|---|
| Game Genie codes | 6-letter and 8-letter (with compare) | `XXXX-XXXX` |
| Pro Action Replay codes | `AAAA:VV` RAM form | `AAAAAADD` (RAM or ROM address) |
| Raw codes | `AAAA:VV`, `AAAA?CC:VV` | `AAAAAA:VV`, `AAAAAA?CC:VV` |
| RetroAchievements | softcore first, hardcore gate built | same |

No cheat **database** ships. Codes come from the user (typed or pasted, or
a plain-text file they open). Engine code never names a title (law 5).

## 2. Two kinds of code, two mechanisms

**ROM-read overrides** (Game Genie on both consoles, SNES PAR at a ROM
address). The real Game Genie sits between cartridge and console and
answers CPU reads at one address with its own byte, optionally only when
the cartridge's byte equals a compare value (nesdev wiki "Game Genie").
Patching the ROM image instead would be wrong for bank-switched games: one
CPU address maps to many ROM offsets, and a compare code exists precisely
to hit only the bank the game has mapped. So the core does what the
hardware does:

* `rf-core-api` gains `RomOverride { addr: u32, value: u8, compare: Option<u8> }`
  and `EmulatorCore::set_rom_overrides(&mut self, &[RomOverride])`
  (default: unsupported, returns `false`).
* rf-nes checks the list in the `$8000-$FFFF` arm of the CPU read, after the
  mapper has produced the cartridge byte; rf-snes checks it on `Target::Rom`.
  The empty case is one length test, so Accuracy Mode pays nothing
  measurable (bench before/after, PERFORMANCE.md).
* Overrides apply to CPU reads only, never to DMA or PPU fetches, matching
  the device. `peek` shows the overridden byte, so the debugger shows what
  the CPU sees.
* At most 16 overrides per session (more than any real device took). This
  is a sanity bound, not hardware: the NES Game Genie took 3 codes, and the
  SNES one took 5.

**RAM writes** (PAR RAM codes, raw codes). Applied by the app on the core
thread at the start of every frame through the existing mod-tier write
door, the same one hook scripts and the memory editor use, so every write
is ledgered (NON_GOALS 13).

**Rebuild tiers.** Pre-decoded and compiled code does not re-read ROM, so
a ROM override would be silently ignored there. While any ROM override is
active the core thread runs the interpreter tier, and the Rebuild status
says why.

## 3. Mod tier, state and identity

Cheats are mods: off by default per game, persisted per game (by ROM
SHA-256) in the game settings store, and listed in the `PROF` save-state
chunk so a state taken with cheats is refused by a session without them
(mod_chunk.rs rules apply unchanged). Any active cheat clears Accuracy
Mode's badge and shows "Cheats: N".

## 4. RetroAchievements

* **Identity**: rf-cart's RA-compatible MD5 (already in `hash.rs`) is the
  game hash the RA server looks up.
* **Approval**: RA accepts hardcore unlocks only from emulators it has
  approved; any other client is softcore-only
  (https://retroachievements.org/forums/topic/29231). Approval is requested
  from RAdmin by the project owner, never by the build. So W41 ships
  softcore working end to end and a hardcore mode that is correct but off
  until approval.
* **Hardcore gate**: in hardcore the app refuses cheats, rewind, loading a
  state, run-ahead, frame advance, slow motion and the Rebuild lab's
  recipe mods, and says which one it refused. Turning any of them on asks
  to leave hardcore first.
* **Credentials**: the user signs in themselves in Settings; only the
  returned token is stored (OS keychain where available). Off by default,
  no network until the user signs in (NON_GOALS 6).
* **Evaluator**: open question, see §6. Runs on the core thread after each
  frame through `peek`, never mutates the machine, so determinism holds.

## 5. Tickets

W41-00 this doc, D-030, NON_GOALS 14 amendment, plan rows.
W41-01 `rf-cheats` crate: code parsers/encoders for every format in §1.
W41-02 core ROM-read override (rf-core-api, rf-nes, rf-snes).
W41-03 app: cheats panel, per-game store, per-frame RAM writes, PROF/ledger,
interpreter-tier fallback.
W41-04 RA condition evaluator. W41-05 RA client (sign-in, game lookup,
unlocks, hardcore gate). W41-06 RA UI (toasts, list, progress).

## 6. Open: how the RA evaluator is built

Asked of Brad 2026-10-27. (1) vendor rcheevos, RA's MIT-licensed C client
library, built with `cc`; this is the path every approved emulator takes.
(2) a first-party Rust evaluator from RA's published logic docs; clean,
but likely softcore-only for good. W41-04..06 wait on the answer.
