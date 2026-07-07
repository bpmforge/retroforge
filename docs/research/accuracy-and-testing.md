# NES/SNES Accuracy & Testing Research (2026-07)

Research for RetroForge's automated test strategy. All URLs checked 2026-07-06 unless noted.
Confidence flags: [verified] = fetched/confirmed this session; [thin] = plausible but not fully verified.

## 1. NES documentation

- **NESdev Wiki** — https://www.nesdev.org/wiki/ — canonical, actively maintained. Migrated from
  wiki.nesdev.com to nesdev.org on 2021-09-28 after domain trouble; the .org wiki is authoritative. [verified]
  Key pages:
  - CPU: https://www.nesdev.org/wiki/CPU — 2A03 = 6502 minus decimal mode; cycle-by-cycle behavior at
    https://www.nesdev.org/wiki/CPU_cycle_reference and interrupt hijacking at https://www.nesdev.org/wiki/CPU_interrupts
  - PPU frame timing (odd/even frame skipped dot, pre-render line): https://www.nesdev.org/wiki/PPU_frame_timing
    and https://www.nesdev.org/wiki/PPU_rendering (the famous frame-timing diagram)
  - Sprite evaluation state machine: https://www.nesdev.org/wiki/PPU_sprite_evaluation
  - APU: https://www.nesdev.org/wiki/APU ; non-linear mixer formulas/LUTs: https://www.nesdev.org/wiki/APU_Mixer
  - Mappers: https://www.nesdev.org/wiki/Mapper and https://www.nesdev.org/wiki/List_of_mappers
  - Emulator test index: https://www.nesdev.org/wiki/Emulator_tests — the single best "what to run" page. [verified]
- **6502 references**: https://www.masswerk.at/6502/6502_instruction_set.html (opcode matrix incl. illegal ops);
  Visual6502 (http://visual6502.org) for transistor-level ground truth.
- **Mapper popularity (actual numbers)** — NESdev forum thread "Most common mappers"
  https://forums.nesdev.org/viewtopic.php?t=20019 , stats posted by tepples (2020-05-07) for the
  **695 licensed North American games**: [verified]
  - Discrete/"Action 53 subset" mappers: 242 games (NROM 55, CNROM 60, UNROM 94, BNROM 1, AOROM 32)
  - MMC1: 225 — MMC3 family: 208 (MMC3 202, MMC6 2, MIMIC-1 4) — Other: 20
  - So **NROM+MMC1+UxROM+CNROM+MMC3 ≈ 636/695 ≈ 91.5%** of the licensed NA library; add AxROM ⇒ ~96%.
    tepples: implementing Action 53 (#28, superset of the discrete mappers) + MMC1 + MMC3 leaves
    "fewer than two dozen" licensed US games uncovered.
  - Older worldwide counts (see https://forums.nesdev.org/viewtopic.php?t=13128): MMC1 681, MMC3 600,
    UxROM 270, NROM 248, CNROM 155, AxROM 76 titles. [thin — forum-sourced, includes JP/PAL]

## 2. SNES documentation

- **Fullsnes (nocash)** — https://problemkaputt.de/fullsnes.htm — single-file spec of the whole console,
  add-ons, and coprocessors; built partly on anomie's docs. Best register-level reference. [verified]
  Mirror: https://patrickjohnston.org/ASM/ROM%20data/snestek.htm
- **SNESdev Wiki** — https://snes.nesdev.org/wiki/Main_Page — the community wiki (same infra as nesdev.org);
  includes its own test-ROM index at https://snes.nesdev.org/wiki/Emulator_tests. [verified]
- **anomie's docs** (timing, PPU regs, memory map, S-DSP) — historically the deepest prose docs; archived at
  Romhacking.net ("Anomie's SNES Documents") and folded into fullsnes/wiki. Use fullsnes first; anomie for rationale.
- **65C816**: WDC datasheet + "Programming the 65816" (Eyes & Lichty, official PDF from WDC);
  https://wiki.superfamicom.org/ opcode guides ("Guide to 65C816 Opcodes").
- **SPC700 / S-DSP**: fullsnes APU chapters; SPC700 reference at https://wiki.superfamicom.org/spc700-reference ;
  SPC boot ROM behavior: https://snes.nesdev.org/wiki/Booting_the_SPC700
- **Source-as-documentation**: higan/bsnes (https://github.com/bsnes-emu/bsnes), ares
  (https://github.com/ares-emu/ares), and Mesen2 Core/ (https://github.com/SourMesen/Mesen2) are the de facto
  behavioral references, esp. for PPU dot timing and DMA/HDMA edge cases.
- **LoROM/HiROM coverage**: no authoritative percentage table found this session. The commercial library is
  dominated by plain LoROM, with HiROM a sizable minority and ExHiROM a handful (Tales of Phantasia,
  Star Ocean-class). [thin — implement LoROM+HiROM first; that plus no-chip carts covers the large majority.]
- **Enhancement chips** (Wikipedia list: https://en.wikipedia.org/wiki/List_of_Super_NES_enhancement_chips):
  matter eventually: **DSP-1** (~16 games incl. Super Mario Kart, Pilotwings), **SA-1** (~34 games incl.
  Super Mario RPG, Kirby Super Star), **Super FX/GSU** (~16 games incl. Star Fox, Yoshi's Island).
  One-offs safely deferred indefinitely: DSP-2/3/4 (1 game each), Cx4 (2), S-DD1 (2), SPC7110 (3), ST01x.
  **All chips can be deferred for v1**; gate on plain LoROM/HiROM accuracy first.

## 3. NES test ROMs

Primary mirror: **https://github.com/christopherpow/nes-test-roms** (collects blargg suites + community tests;
contents verified: instr_test-v5, cpu_interrupts_v2, ppu_vbl_nmi, sprite_hit/overflow, oam_read/stress,
apu_test, apu_mixer, dmc_dma_during_read4, mmc3_test/mmc3_irq_tests, mmc5test, and more). [verified]
Index with descriptions: https://www.nesdev.org/wiki/Emulator_tests and TASVideos accuracy table:
https://tasvideos.org/EmulatorResources/NESAccuracyTests

| Suite | What it tests | Pass criteria |
|---|---|---|
| nestest (kevtris) | All official + many illegal 6502 ops, no PPU needed (start at $C000) | Diff CPU log vs golden trace `nestest.log` (http://www.qmtpro.com/~nes/misc/nestest.log); byte-exact PC/A/X/Y/P/SP/CYC per instruction. Ideal first CI gate. |
| blargg instr_test-v5 | Every official+unofficial instruction, far deeper than nestest | Each ROM writes status to $6000 + text at $6004; $6000=0 on pass ("All tests passed" on screen). Automatable headless. |
| blargg cpu_timing_test6 / instr_timing / branch_timing_tests | Instruction cycle counts, page-cross penalties, branch timing | Same $6000/screen protocol |
| blargg cpu_interrupts_v2 | NMI/IRQ timing, interrupt hijacking, CLI latency | $6000 protocol |
| cpu_dummy_reads / cpu_dummy_writes / cpu_exec_space | Dummy bus cycles, open-bus execution | $6000 protocol |
| blargg ppu_vbl_nmi | VBL flag timing, NMI suppression/timing to the PPU-cycle | 10 sub-ROMs; $6000 protocol. The canonical PPU/CPU alignment gate. |
| blargg sprite_hit_tests / sprite_overflow_tests | Sprite-0 hit corner cases; buggy overflow-flag evaluation | $6000/screen protocol |
| oam_read / oam_stress | OAM ($2004) read behavior, decay-adjacent behavior | $6000 protocol |
| blargg full_palette / ppu_open_bus / ppu_read_buffer | Palette rendering, PPU open bus, $2007 buffer | Screen match / $6000 protocol |
| blargg apu_test + apu_reset + dmc_dma_during_read4 | Frame counter timing, length counters, IRQ, DMC DMA conflicts | $6000 protocol |
| blargg apu_mixer | Non-linear mixer levels per channel | **Listen/scope test** — compare output waveform; automate by capturing audio and comparing RMS envelope vs known-good recording |
| mmc3_test_2 / mmc3_irq_tests | MMC3 A12 scanline IRQ counter variants (rev A/B) | $6000/screen protocol |
| Holy Diver Batman (rainwarrior) | 28-ROM mapper acid test: NROM/MMC1/UxROM/CNROM/MMC3/MMC5/AxROM/VRC etc. | https://github.com/rainwarrior/hdbat (in nes-test-roms as `nes15`? — fetch from rainwarrior's repo); visual per-ROM: title screen renders + correct banking [thin on exact repo path] |
| SingleStepTests `nes6502` | 10,000 JSON vectors per opcode incl. cycle-by-cycle bus activity (RP2A03 profile) | https://github.com/SingleStepTests/ProcessorTests (`nes6502/`) [verified]; run as a native unit test against the CPU core — no ROM loader needed. Best possible CI signal for the CPU. |

CI pattern: run ROM headless N frames → poll $6000 (0x80=running, 0x00=pass, else fail code) → also hash the
framebuffer against a golden PNG for the screen-only tests.

## 4. SNES test ROMs

| Suite | What it tests | Pass criteria |
|---|---|---|
| SingleStepTests 65816 + SPC700 | JSON per-instruction vectors with bus activity | https://github.com/SingleStepTests/65816 , https://github.com/SingleStepTests/spc700 [verified] — native unit tests, run before any ROM boots |
| gilyon/snes-tests (cputest/spctest) | Comprehensive 65C816 + SPC-700 op behavior on-console | https://github.com/gilyon/snes-tests (MIT) [verified] — on-screen "Test N failed"; releases ship `tests.txt` golden expected-value lists; automate via framebuffer hash or RAM inspection |
| undisbeliever/snes-test-roms | Grab-bag: PPU (windows, HDMA gradients), DMA/HDMA edge cases, interrupts, controller | https://github.com/undisbeliever/snes-test-roms (zlib, built with bass-untech) [verified] — mostly visual; golden-frame compare |
| PeterLemon (krom) SNES tests | Huge per-feature demo/test set: CPUTest (ADC/SBC/etc per-op with expected checksums on screen), PPU modes incl. Mode 7, DMA, SPC700 | https://github.com/PeterLemon/SNES (active 2026) [verified] — screen shows computed vs expected values; golden-frame compare works well |
| blargg SPC-700 tests (spc_timing etc.) | S-SMP/S-DSP timing | historically from blargg's site (dead); mirrored in collections like https://gitlab.com/higan/snes-test-roms [thin on canonical home] |
| higan/snes-test-roms (GitLab) | byuu-era collection used to validate higan | https://gitlab.com/higan/snes-test-roms (GH mirrors exist) |
| SourMesen/SnesTests | Mesen2 author's own SNES test ROMs | https://github.com/SourMesen/SnesTests [verified exists] |

**How bsnes/Mesen2 actually test**: neither ships a public golden-frame CI pipeline. [verified negative — no test
target in Mesen2 makefile; TASVideos accuracy tables are hand-run]. Mesen(1) had an automated-test mode
(recorded input + hash). Practical takeaway: build our own harness — headless core, scripted input, compare
framebuffer hash (and audio ring-buffer hash) after N frames against checked-in goldens. That's the pattern
the SNESdev/TASVideos accuracy tables assume (https://tasvideos.org/EmulatorResources/SNESAccuracyTests).

## 5. Determinism & replay

- **TASVideos requirements** (https://tasvideos.org/EmulatorResources/Requirements): emulation must be
  deterministic; movies must declare power-on vs savestate start and be tamper-proof; resets/lag handling defined. [verified]
- **BK2 format** (BizHawk, https://tasvideos.org/Bizhawk/BK2Format): a zip containing `Input Log.txt`
  (UTF-8, one `|`-delimited line per frame, first line is a "log key" naming buttons), `Header.txt`
  (sync settings, core, ROM hash), `Comments/Subtitles`. Excellent model for our input-log format. [verified]
- **bsnes/higan BSM movies & RetroArch .bsv**: input-stream + savestate-anchored replay; less documented than BK2.
- **Design rules for us**: (1) no wall-clock/RNG in core — all entropy from seeded init + input log;
  (2) input sampled at a fixed point in the frame; (3) ROM hash + core version + sync settings embedded in the
  movie header; (4) savestates = versioned serialization (magic + schema version per chip block, refuse or migrate
  on mismatch — bsnes uses a serializer signature, Mesen versions its state format) [thin on exact peer details];
  (5) CI replays a BK2-style log and asserts final framebuffer/RAM hash — this doubles as the regression suite.

## 6. Legally safe demo ROMs

| Name | Console | Genre | License | Source | Suitability |
|---|---|---|---|---|---|
| **Nova the Squirrel** | NES | Side-scrolling platformer | **GPLv3** [verified] | https://github.com/NovaSquirrel/NovaTheSquirrel (active 2026) | Top pick: full level format/engine in source; redistribution OK under GPL |
| **Nova the Squirrel 2** | SNES | Side-scrolling platformer | **GPLv3** [verified] | https://github.com/NovaSquirrel/NovaTheSquirrel2 (active 2026) | Top SNES pick — same author/level tooling, LoROM |
| Alter Ego (Shiru) | NES | Puzzle-platformer | Public domain (source included, cc65) [verified via PDRoms/OpenEmu bundle] | Source ships with ROM; also in OpenEmu homebrew set | Good second NES title; PD = ship-in-repo safe |
| Lan Master (Shiru) | NES | Puzzle | Freeware; Shiru releases code PD-style [thin — confirm per-title] | pdroms.de listing | Test-suite filler, not a side-scroller |
| Micro Mages (Morphcat) | NES | Platformer | **Commercial**; free demo exists (itch.io) but proprietary — do NOT redistribute | Closed | Manual smoke-testing only, user-supplied ROM |
| Super Boss Gaiden | SNES | Action/boss-rush | Freeware, distribution "approved on specific sites"; source on GitHub but **no clear license** | https://github.com/snesdev0815/SNES-SuperBossGaiden | Demo candidate w/ author permission; don't vendor |
| libSFX example ROMs | SNES | Tech demos | MIT [verified] | https://github.com/Optiroc/libSFX | Ideal for shipping tiny SNES fixtures we build ourselves |
| cc65/NESdev example ROMs (nesdoug tutorials, neslib samples) | NES | Demos | PD/permissive | https://github.com/clbr/neslib , nesdoug.com | Build-from-source CI fixtures |

**Open-source side-scrollers for the game-aware enhancement demo**: Nova the Squirrel (NES) and
Nova the Squirrel 2 (SNES) — both GPLv3 with level data formats fully documented by their build pipelines.
Third option: Alter Ego (PD, source included) if a non-GPL NES title is needed, though it's flip-based rather
than scrolling.

## 7. Toolchains for CI-built test ROMs

- **cc65/ca65** — https://github.com/cc65/cc65 — zlib license, active (pushed 2026-07) [verified]. C compiler +
  macro assembler + ld65 linker for 6502; the standard NES homebrew chain (Alter Ego, neslib). `brew install cc65`;
  deterministic builds → check in ROM hashes.
- **asar** — https://github.com/RPGHacker/asar — active 2026 [verified]; SNES 65816 patch-oriented assembler
  (Super Boss Gaiden ecosystem, romhacks). Good for one-off SNES test patches.
- **libSFX** — https://github.com/Optiroc/libSFX — MIT, active 2026 [verified]; full SNES framework (CA65-based,
  SPC700 support, Makefile-driven) — best base for our own SNES test ROMs in CI.
- **bass-untech** — undisbeliever's fork of bass used by snes-test-roms; needed only to rebuild that suite.
- CI recipe: pin toolchain versions in a container; `make` test ROMs from source; run emulator headless;
  assert $6000-protocol result (NES) / framebuffer hash (both).

## Sources (primary)
nesdev.org wiki & forums (t=20019, t=13128) · problemkaputt.de/fullsnes.htm · snes.nesdev.org ·
tasvideos.org (Requirements, BK2Format, NES/SNESAccuracyTests) · github.com: christopherpow/nes-test-roms,
SingleStepTests/ProcessorTests + /65816 + /spc700, gilyon/snes-tests, undisbeliever/snes-test-roms,
PeterLemon/SNES, SourMesen/Mesen2, NovaSquirrel/NovaTheSquirrel(+2), cc65/cc65, RPGHacker/asar, Optiroc/libSFX ·
en.wikipedia.org List_of_Super_NES_enhancement_chips
