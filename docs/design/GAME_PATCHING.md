# Game patching: the future state (design, 2026-10-16)

**Goal (Brad, 2026-10-16):** the emulator understands how games are built
and patched: it can apply the patches people make (widescreen, fixes,
translations), knows what a patch changes, can help a user build one, and
can build homebrew from source.

**Status:** design only. Nothing here is implemented except level 0. The
tickets at the end are proposals, not claims.

## The rules that do not move

- **Accuracy Mode is the reference (law 6).** Any patch makes the run "not
  the cartridge". A patched session says so in the HUD/readout, is never the
  default, and keeps its own saves and save states (keyed by the patched
  image's hash) so a patch can never corrupt an unpatched save.
- **No ROM bytes in git (law 5).** RetroForge never ships patches for
  commercial games, disassembly or ROM-derived data. A profile may name a
  patch only by its hash, title and where its author publishes it. The user
  supplies the patch file.
- **Clean-room.** Formats and conventions come from published docs (format
  specs, sneslab, libretro docs). No emulator or game source.
- **Never write to the user's ROM.** Patching is in memory at load time
  (soft-patching), always reversible by reloading.

## Level 0: profile mods (exists, W7-12)

`rf_enhance::mods` applies `[mods]` byte patches declared in a profile: off
by default, refuse on overlap, reversible (`revert` restores exact bytes),
every on/off ledgered with its frame, `is_accuracy_mode()` false the moment
one is on. Everything below reuses this engine's guarantees.

## Level 1: soft-patching standard patch files

- **Formats:** IPS (offset/length records, RLE, `EOF` marker, 24-bit offsets),
  BPS (Near/byuu: relative source/target copies, CRC32 of source, target and
  patch in the footer, so a wrong ROM is refused before anything runs), UPS
  (XOR with CRC32s). BPS is preferred: it knows its source ROM.
- **Discovery,** as RetroArch does it: `Game.sfc` loads `Game.bps` /
  `Game.ips` / `Game.ups` beside it, plus numbered stacks (`Game.ips1`,
  `.ips2` ...) applied in order; also a per-game `patches/` folder in
  RetroForge's library dir. The user turns each one on in the game's
  settings; nothing applies silently.
- **Header handling:** SNES copier headers (512 bytes) are stripped before
  patching and the patch is checked against both forms; a mismatch is a
  clear message ("this patch is for the headered dump"), never a corrupt
  boot.
- **Identity:** the patched image gets its own hash; profiles match the
  base ROM's hash plus the patch hash, so a profile can say "with patch X,
  use these settings" (for example widescreen width).
- **Tests:** round-trip fixtures built from RetroForge's own homebrew
  (RF-Scroller), never commercial bytes; malformed patches (truncated, bad
  CRC, overflow offsets) are refused without panics (law 8 on every record
  walk).

## Level 2: knowing what a patch does

- **Inspector:** diff the base and patched images and map every changed
  range through the profile's `[[rom_map]]` (code / data / graphics / text
  regions), so the UI can say "changes 1.2 KB of code in bank 0, 40 KB of
  graphics".
- **Live effect:** run the same input movie on the base and patched images
  and report where they diverge (first frame, RAM ranges), using the
  determinism the cores already guarantee.
- **Recognising conventions:** a profile entry (by patch hash) or a sidecar
  config declares what the patch expects from the emulator, starting with
  the bsnes-hd widescreen convention (level 3).

## Level 3: emulating the bsnes-hd widescreen convention

Widescreen patches such as Vitor Vilela's Super Mario World patch are
written for bsnes-hd. Its convention (sneslab "Widescreen"):

- Output 352x224 (16:9, +48 a side) or 448x224 (21:9, +96 a side), 8:7
  pixel aspect kept.
- Sprite X read as -128..+384 (>= 384 wraps by -512), so sprites live in the
  extra area.
- Window positions map by `2x - 128` when widescreen is on.
- The extra area is invisible on real hardware, so the patched game still
  runs on everything.

RetroForge's PPU already renders widened lines (W30-03,
`render_sub_scanline_width`, per-layer widen masks). Adding the sprite range
and window mapping, switched on only for a patch that declares the
convention, gives real gameplay in the margins: the strongest widescreen
possible, because the game itself streams the wider level. Presentation
only: what the game computes is unchanged; only what is drawn in the
invisible area is.

## Level 4: helping a user build a patch

The emulator already watches games closely (GSU polygon observer, camera
finder, profile census, widescreen layer policies). Pointed at the user's
own ROM on their own machine, the same tools can assist an author:

- **Find the screen-width assumptions:** watch which code compares sprite
  X against 256 / culls off-screen objects, and which code streams tilemap
  columns at the screen edge; list the call sites with frame evidence.
- **Try a change safely:** apply a candidate byte change through the level-0
  engine in a sandbox session, replay the same movie, and show the visual
  and RAM differences.
- **Export:** write the result as a BPS patch against the user's ROM. The
  patch belongs to the user; RetroForge does not publish it.

## Level 5: building games

RetroForge already builds its own test games from source (cc65 fixtures:
RF-Scroller, RF-Scroller-S, mirror maps). A "developer loop" would expose
that: build a homebrew project (cc65/ca65 for NES first), load it straight
into the core, keep the debugger's symbol map in sync, and rebuild +
hot-reload on save with the state preserved where layout allows.

## Proposed tickets

| Id | What | Depends on |
|---|---|---|
| W32-01 | Soft-patching: IPS + BPS + UPS parsers (fuzz-safe), discovery beside the ROM and in `patches/`, per-game opt-in, header handling, patched-image identity, separate saves/states, "Patched" readout | - |
| W32-02 | Patch inspector: changed ranges mapped through `[[rom_map]]`, movie A/B divergence report | W32-01 |
| W32-03 | bsnes-hd widescreen convention: sprite X -128..384, window `2x - 128`, 352/448 output, enabled by a profile/patch declaration | W32-01, W31-07 |
| W32-04 | Research spike: screen-width assumption finder (sprite cull and column-streaming call sites) on homebrew first | W32-02 |
| W32-05 | Patch authoring sandbox + BPS export | W32-04 |
| W32-06 | Homebrew developer loop (cc65 build, load, symbols, hot reload) | - |

## Sources

- RetroArch soft-patching: https://docs.libretro.com/guides/softpatching/
- BPS format (spec at romhacking.net documents/746, via nesdev): https://forums.nesdev.org/viewtopic.php?p=209682
- sneslab, Widescreen: https://sneslab.net/wiki/Widescreen
- Survey of widescreen approaches: docs/research/W31-07-widescreen-survey.md
