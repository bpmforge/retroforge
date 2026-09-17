# Design: Emulation Cores (`rf-nes`, `rf-snes`)

Scope: internal design of both console cores — chip models, scheduling,
mapper/cartridge integration, accuracy-vs-compatibility switches, and the
test ROMs that gate each subsystem. The cross-core contract (`EmulatorCore`,
`CoreSink`, `StateView`, indexed-pixel output) is defined in
`docs/ARCHITECTURE.md` §5 and is not restated here.

## 1. Shared core principles

- **Single-threaded, catch-up scheduled.** CPU is the master clock. Each
  chip keeps its own cycle counter; when the CPU touches a register another
  chip owns, that chip runs `catch_up(to_master_cycle)` first (Mesen-style).
  In Accuracy mode the PPU may instead be ticked in lock-step per CPU cycle
  (simpler to reason about, ~10-20% slower) — both paths must produce
  identical state, and CI diffs them on the test-ROM suite.
- **Frame-boundary states.** `save_state`/`load_state` are only legal at
  frame boundaries (after the last visible scanline's end-of-frame event).
  No mid-instruction or mid-scanline serialization; this keeps chunk formats
  small and removes an entire class of determinism bugs.
- **Indexed pixels + metadata.** Cores emit `PpuPixel { palette_index: u8,
  layer: PixelLayer, sprite_id: Option<u8>, priority: u8,
  dropped_by_limit: bool }` — where `PixelLayer` is
  `Backdrop | Background(u8) | Sprite` — a **scanline at a time** via
  `CoreSink::video_scanline(y: u16, pixels: &[PpuPixel])`. RGB conversion
  happens in the renderer (palette LUT), never in the core.
  *(Corrected 2026-08-03: this bullet previously described a `color_index`
  and `palette_group` that the shipped `rf-core-api` struct does not have,
  omitted `sprite_id`, and said "per dot" where the sink is per scanline.
  `crates/rf-core-api/src/video.rs` is the authority — W0-04 shipped it and
  W1-04a consumes it.)*
- **No wall clock, no RNG, no floats** in core state or timing paths.
  (Mode 7 matrix math is fixed-point, matching hardware — see §3.4.)

## 2. NES core (`rf-nes`)

### 2.1 CPU — Ricoh 2A03 (6502 minus BCD)

- Cycle-stepped: every instruction is executed as its exact sequence of bus
  cycles (read/write/dummy), not "execute then add N cycles". This is
  mandatory for DMC DMA conflicts, dummy-read side effects ($2007, $4016),
  and OAM DMA alignment.
- Implement all 256 opcodes including unofficial/illegal ops (games use
  them: `LAX`, `SAX`, `DCP`, `ISC`, `SLO`, `RLA`, `SRE`, `RRA`, NOP
  variants). Unstable ops (`XAA/ANE`, `LXA`, `AHX/SHA`, `TAS`, `LAS`) get
  the commonly-observed constants; flag them in the trace log.
- Interrupts: NMI edge-detected, IRQ level-sensitive, both polled on the
  penultimate cycle of each instruction; implement interrupt hijacking
  (NMI overtaking BRK/IRQ vector fetch) per nesdev "CPU interrupts".
- Test gates, in order:
  1. **SingleStepTests `nes6502`** JSON vectors — native unit test, per
     opcode, cycle-by-cycle bus activity. Runs with no ROM loader; this is
     the first thing the CPU passes.
  2. **nestest** at $C000 vs golden `nestest.log` — byte-exact
     `PC A X Y P SP CYC` trace diff.
  3. blargg `instr_test-v5`, `cpu_timing_test6`, `branch_timing_tests`,
     `cpu_interrupts_v2`, `cpu_dummy_reads`, `cpu_dummy_writes`,
     `cpu_exec_space` (all $6000-protocol, headless).

### 2.2 PPU — 2C02

Per-dot pipeline, 341 dots × 262 scanlines (NTSC):

- Loopy registers `v`, `t`, `x`, `w` drive scrolling exactly as documented;
  $2005/$2006 share the `w` toggle; $2000 writes bits 10-11 of `t`.
- Background: 8-dot fetch cadence (NT, AT, pattern lo, pattern hi), shift
  registers reloaded at dots 9,17,…; fetches at dots 321-336 prefetch the
  next line's first two tiles.
- Sprite evaluation (dots 65-256): the real 2-phase OAM scan into secondary
  OAM, 8-sprite limit, and the *buggy* overflow-flag diagonal scan —
  emulated as hardware does, bugs included. Sprite fetches at dots 257-320.
  **The scanline emit is accuracy-exact in BOTH modes** — it always carries
  the true hardware framebuffer, so a limit-dropped sprite is never emitted
  and `dropped_by_limit` is always `false` on the NES path. The sprite-limit
  bypass (W3-05) reconstructs dropped sprites from OAM via `StateView` /
  its SpriteHistorian, which its own acceptance criteria already assume.
  Evaluation is unchanged either way — sprite-0 hit and overflow flags
  always reflect hardware behavior; see ENHANCEMENT_RUNTIME.md §2.
  *(Corrected 2026-08-03 by Brad's ruling during W1-05a: this previously
  said "in Enhanced mode the scanline emit includes dropped sprites", which
  is unachievable — `CoreSink::video_scanline` carries exactly one
  `PpuPixel` per x, so emitting a dropped sprite would displace the pixel
  the CRT actually showed, violating law 6 and FR-MODE-002's mode
  invariant. `crates/rf-core-api/src/video.rs` carries the full ruling.)*
- Sprite-0 hit: opaque BG ∩ opaque sprite-0 pixel, not at x=255, not in the
  left-8-pixel column when masked by $2001; set at the exact dot.
- VBlank/NMI: flag set at dot 1 of scanline 241; reading $2002 near that dot
  suppresses per `ppu_vbl_nmi` cases; NMI can retrigger by toggling $2000
  bit 7 during VBlank.
- Odd/even frames: with rendering enabled, dot 339 of the pre-render line is
  skipped on odd frames.
- $2007 read buffer (delayed read, palette bypass quirk), PPU open bus decay
  on the shared latch, greyscale/emphasis bits of $2001 carried in pixel
  metadata (renderer applies them via LUT variant).
- Test gates: `ppu_vbl_nmi` (all 10), `sprite_hit_tests`,
  `sprite_overflow_tests`, `oam_read`/`oam_stress`, `ppu_open_bus`,
  `ppu_read_buffer`, `full_palette` (golden frame).

### 2.3 APU

- Five channels (2 pulse, triangle, noise, DMC) + frame counter (4/5-step;
  $4017 write takes effect 3-4 CPU cycles later; IRQ on 4-step).
- DMC DMA: steals CPU cycles with correct alignment; the RDY-line double-read
  glitch on $2007/$4016/$4017 during DMC fetch is emulated
  (`dmc_dma_during_read4` gates this).
- Mixer: non-linear LUT formulas from nesdev APU_Mixer, output as i16 mono
  at ~1.789 MHz effective, downsampled by the core to a fixed internal rate
  (blip-buffer band-limited steps), delivered via `CoreSink::audio`. Host
  resampling/rate control lives in `rf-audio`, not here.
- Test gates: `apu_test`, `apu_reset`, `dmc_dma_during_read4`; `apu_mixer`
  via captured-audio RMS-envelope comparison (see harness).

### 2.4 Mappers

Trait (in `rf-nes`, constructed from `rf-cart` detection):

```rust
pub trait Mapper {
    fn cpu_read(&mut self, addr: u16, bus: &mut MapperBus) -> BusValue; // open-bus aware
    fn cpu_write(&mut self, addr: u16, val: u8, bus: &mut MapperBus);
    fn ppu_read(&mut self, addr: u16) -> u8;   // CHR / nametable routing
    fn ppu_write(&mut self, addr: u16, val: u8);
    fn ppu_a12(&mut self, rising_cycle: u64);   // MMC3 IRQ clocking hook
    fn mirroring(&self) -> Mirroring;
    fn irq_pending(&self) -> bool;
    fn state_chunk(&self) -> MapperState;       // for MAPR save-state chunk
}
```

Launch set and library coverage (695 licensed NA games; nesdev forum
t=20019):

| Mapper | Games | Notes / edge cases |
|---|---|---|
| NROM (0) | 55 | none — first boot target |
| MMC1 (1) | 225 | serial 5-write shift register; reset bit; **consecutive-cycle writes ignored** (Bill & Ted bug-bait); PRG/CHR modes; SOROM/SUROM 512K variants later |
| UxROM (2) | 94 | bus conflicts on write (AND with ROM byte) on most boards |
| CNROM (3) | 60 | CHR bank + bus conflicts |
| MMC3 (4) | 208 (family) | bank registers R0-R7; **A12 rising-edge IRQ counter with ~3-cycle low filter**; rev A vs rev B reload quirk (both selectable, default rev B); scanline IRQ gates split-screen games |

Total ≈ 91.5% of the licensed NA library. Next wave (Phase 2+): AxROM (32),
MMC2/4 (Punch-Out/Fire Emblem latches), MMC5 explicitly deferred.
Test gates: `mmc3_test_2`, `mmc3_irq_tests`, Holy Diver Batman (visual,
golden frame per sub-ROM).

### 2.5 I/O

- $4016/$4017 controller strobe/shift, open-bus upper bits; standard pad
  first, Four Score later. Input arrives pre-latched per frame
  (`InputFrame`) — the core never talks to host input APIs.
- OAM DMA ($4014): 513/514 cycles with get/put alignment.

## 3. SNES core (`rf-snes`)

The main original work of the project — there is no mature Rust SNES core to
lean on (see rust-stack research §7). Behavioral references: fullsnes,
snes.nesdev.org, and ares/bsnes/Mesen2 source for tie-breaking. Everything
below is NTSC-first; PAL is a Phase 7 config.

### 3.1 CPU — 65C816 on the 5A22

- Emulation vs native mode (`e` flag), variable-width A/X/Y via `m`/`x`
  flags, direct-page indexing quirks (DL≠0 penalty cycle), 24-bit banked
  addressing with wrapping rules per addressing mode (data bank vs program
  bank vs bank-0 wraps).
- Memory-speed model: master clock 21.477 MHz; bus access costs 6/8/12
  master cycles by region (FastROM $80+ 6 vs SlowROM 8; $4000-41FF joypad
  12). CPU timing is expressed in master cycles from day one — retrofitting
  is not viable.
- 5A22 extras: hardware multiply/divide regs ($4202-$4206) with real
  latency (8/16 cycles, intermediate-value reads readable mid-operation —
  emulate the shift-register intermediates in Accuracy mode); NMITIMEN
  ($4200) NMI/IRQ enables; H/V IRQ ($4207-$420A, $4211 ack); auto-joypad
  read (starts ~line 225, 3-4 lines, $4218-$421F; reading $4016 during
  auto-read quirk); WRAM port $2180-$2183.
- Interrupt timing: NMI at V=225(240 overscan)/H≈0.5 with the $4210 read
  race; IRQ per H/V compare with the one-dot-late quirk.
- Test gates: **SingleStepTests `65816`** JSON vectors first (native unit
  test), then **gilyon/snes-tests `cputest`** (golden `tests.txt` +
  framebuffer hash), PeterLemon CPU tests (golden frame).

### 3.2 DMA / HDMA

- 8 channels, MDMA ($420B) byte-by-byte at 8 master cycles/byte + setup
  overhead; transfer patterns 0-7 ($43x0 DMAP); A-bus↔B-bus direction;
  DMA-to-$2180 and $4300-region open-bus edge cases.
- HDMA ($420C): per-scanline table walker — line counter reload semantics
  (repeat bit, 0x00 terminator), indirect mode bank register, **HDMA
  init at start-of-frame and per-line timing windows**, HDMA/MDMA collision
  (HDMA steals mid-MDMA). HDMA is what makes gradients/wavy effects and most
  split-screen HUDs work — it gates HUD-separation heuristics too
  (`ScrollWrite`+HDMA events feed rf-enhance).
- Test gates: undisbeliever `snes-test-roms` DMA/HDMA set (golden frames),
  PeterLemon DMA demos.

### 3.3 PPU — PPU1/PPU2 (S-PPU)

Scanline-based renderer in v1 (per-dot deferred; see §5 accuracy switches):
each scanline is composed at once from register state latched at line start
+ mid-line writes recorded with H-position (sufficient for the large
majority of games; per-dot upgrade path documented in code).

- BG modes 0-6 per BGMODE ($2105) with per-BG char sizes, tile sizes
  (8×8/16×16), BG3 priority bit in mode 1; direct-color mode 3/4/7 via
  CGWSEL; offset-per-tile in modes 2/4/6.
- **Mode 7**: 8.8 fixed-point matrix ($211B-$211E signed multiply through
  M7A×M7B product readable at $2134-36), 1024×1024 playfield, screen-over
  repeat/clamp per M7SEL, EXTBG (mode 7 BG2 priority-bit) in Phase 7.
- Sprites (OBJ): 128 entries, OBSEL size/base, **32 sprites/line and 34
  tile-slivers/line limits** (both emitted with `dropped_by_limit` metadata
  for the enhancement layer), priority rotation via OAMADDR, time-over/
  range-over flags in $213E.
- Windows: WH0-WH3, per-layer window logic (OR/AND/XOR/XNOR), clip-to-black
  vs color-math masking ($2130).
- Color math: CGADSUB add/sub, half, per-layer enable, fixed color $2132,
  sub-screen composition; pseudo-hires and hires modes 5/6 emit 512-wide
  scanlines (the `PpuPixel` stream carries a width tag per line).
- Mosaic ($2106) including mode-7 mosaic quirks; interlace + overscan via
  SETINI (frame emits 224 or 239 visible lines; renderer letterboxes).
- Latches: $2137 software latch, H/V counters $213C/D, PPU1/PPU2 open bus
  distinctions ($2134-$213F return values).
- Test gates: PeterLemon PPU mode demos + Mode 7 tests, undisbeliever
  window/HDMA-gradient ROMs — all golden-frame; krom CPUTest-style screens
  double as PPU smoke tests.

### 3.4 APU — S-SMP (SPC700) + S-DSP

- SPC700 @ 1.024 MHz nominal: full instruction set via **SingleStepTests
  `spc700`** vectors; 64 KB ARAM; IPL boot ROM handshake ($FFC0-FFFF,
  readable/bankable via $F1); timers T0/T1 (8 kHz) T2 (64 kHz).
- CPU↔APU ports $2140-$2143 ↔ $F4-$F7: the famous handshake protocols are
  timing-sensitive — run the SPC700 in catch-up sync with main CPU port
  accesses (never a free-running thread).
- S-DSP @ 32 kHz: 8 voices, BRR decode (4-bit ADPCM, 9-byte blocks, filter
  modes 0-3), pitch modulation, ADSR/GAIN envelopes, noise, **echo buffer in
  ARAM with 8-tap FIR** (writes ARAM — games depend on this corruption
  behavior), main/echo volume, mute/reset flags. Sample-exact DSP is a
  Phase 7 accuracy item; v1 targets handshake correctness + clean audio.
- Clock skew: real S-SMP runs ~1.0024 MHz (32040 Hz sample rate); config
  carries exact vs nominal ratio (affects music tempo determinism).
- Test gates: SingleStepTests spc700 vectors, gilyon `spctest`, blargg SPC
  timing set (mirrored in higan snes-test-roms) in Phase 7.

### 3.5 Cartridge mapping

- **LoROM** (mode $20): 32 KB banks at $8000-$FFFF, banks $00-$7D/$80-$FF,
  SRAM at $70-$7D:0000-7FFF (size-masked mirrors).
- **HiROM** (mode $21): 64 KB banks $C0-$FF (+ $40-$7D), SRAM at
  $20-$3F:6000-7FFF.
- Header detection in `rf-cart`: score candidate headers at $7FC0/$FFC0
  (checksum/complement, mapper byte, reset vector sanity) — never trust the
  extension; 512-byte copier header stripped before hashing (see
  game-identity research).
- FastROM ($420D) speed switch. ExHiROM + coprocessors (SA-1 ~34 games,
  SuperFX ~16, one-offs) **explicitly deferred to Phase 9+**; `rf-cart`
  detects and reports "unsupported chip: SA-1" rather than half-booting.
- **DSP-1 (~13 games incl. Super Mario Kart, Pilotwings) — in scope via HLE
  since 2026-09-17 (D-010, SRS FR-CORE-038, W14-18/W14-19).** The uPD7725's
  own program ROM is copyrighted firmware and no dump ships in the
  library, so this is command-level HLE (snes9x-style), not LLE of the
  DSP chip — the documented ~30-command set with its fixed-point math, run
  over the real DR/SR handshake. The coprocessor nibble in the header
  (`data[base+0x16]`) cannot distinguish DSP-1 from DSP-2/3/4; `rf-cart`
  accepts every nibble-0 DSP cart as DSP-1, and the DSP-2/3/4 titles
  (Dungeon Master, SD Gundam GX Rasetsu no Sho, Top Gear 3000) are named
  only in the profile/rom-manifest layer as known-wrong, never by title in
  engine code (law 5). DR/SR bus window per cartridge variant (verified
  against snes9x's `memmap.cpp`, the reference HLE implementation this
  approach follows — see W14-18/19 notes for the exact bank/offset ranges
  and the one open cross-check against fullsnes/snesdev prose): LoROM
  ≤1 MiB, LoROM >1 MiB (DSP-1B), and HiROM each map DR and SR to a
  different bank/offset window, split at a per-variant boundary offset
  inside a shared 32 KB (LoROM) or 8 KB (HiROM) window.

## 4. Cartridge layer boundary (`rf-cart`)

`rf-cart` owns file parsing (iNES/NES 2.0 incl. submapper/PRG-RAM fields,
SNES header heuristics), normalization + hashing (CRC32/MD5/SHA-1/SHA-256,
RA-compatible), battery-SRAM persistence (`.sav` beside ROM or per-user
dir), and mapper/chip *detection*. Mapper *behavior* lives in the core
crates. `Cartridge` handed to a core is already validated and hashed.

## 5. Accuracy vs Compatibility switches (`CoreConfig`)

| Switch | Accuracy | Compatibility | Risk of compat setting |
|---|---|---|---|
| NES PPU stepping | lock-step per CPU cycle | catch-up on register access | none if catch-up correct (CI diffs both) |
| NES OAM decay | emulated | off | a handful of tests |
| SNES PPU | scanline w/ mid-line latch (v1) | same | per-dot effects (rare) render off by a few pixels |
| SNES mul/div latency | intermediate values | instant results | very few games peek mid-op |
| S-DSP | sample-exact (Phase 7) | clean fast mixer | audio fidelity only |
| Open-bus modeling | full | simplified | rare unlicensed/test ROMs |

Both configs are deterministic. Compatibility never changes *observable
game-facing* behavior for the supported library — divergences must be test-
suite-visible, or the switch doesn't exist.

## 6. What gates "done" per phase

- NES core exits Phase 1/2 when: SingleStepTests 100%, nestest exact,
  blargg CPU+PPU+APU suites pass headless, MMC3 IRQ suite passes, Holy
  Diver Batman golden frames match, determinism replay green.
- SNES core exits Phase 6/7 when: 65816+spc700 vectors 100%, gilyon suites
  pass, PeterLemon/undisbeliever golden-frame set matches, DMA/HDMA suite
  green, LoROM+HiROM commercial-shaped fixture boots (RF-Scroller-S), determinism
  replay green.
