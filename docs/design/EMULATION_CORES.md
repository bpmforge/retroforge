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
- **Internal (no-bus-access) cycles are charged too (ticket W14-39).**
  `speed::AccessCost` prices bus accesses only, by design (its own doc's
  closing section); `cpu::cycles::internal_cycles` computes the rest —
  the cycles a 65C816 instruction spends touching no address at all —
  from a per-opcode/addressing-mode table built from a histogram of
  `cycles.len() - accesses` across all 5,080,000 SingleStepTests vector
  cases and cross-checked against the WDC W65C816S datasheet's penalty
  rules: DP low byte nonzero (+1, any direct-page-relative mode),
  direct-page indexing (+1 fixed for `dp,X`/`dp,Y`/`(dp,X)`),
  indexed-absolute or `(dp),Y` page-cross (+1, conditional on the actual
  carry for an 8-bit index, but **unconditional** for a 16-bit index —
  the CPU cannot add a full 16-bit index in one cycle regardless of
  whether it happens to cross), RMW's extra modify cycle (+1), branch
  taken (+1, +1 more crossing a page in emulation mode only), stack-
  relative forms (+1 fixed, `(sr,S),Y` +2), a one-byte implied
  instruction (never fewer than 2 cycles total), and fixed per-opcode
  costs for jumps/calls/returns/interrupts/`WAI`/`STP`/`PEA`/`PEI`/`PER`.
  Computed via side-effect-free `CpuBus::peek` reads before
  `ops::execute` runs, so the addressing/execution code itself — already
  verified against the same 5,080,000 vectors — is untouched.
  `SnesSystem::step` charges `internal_cycles * speed::FAST` master
  cycles alongside the access cost. This superseded two local
  compensations that had accumulated for the gap (W14-24's master-cycle
  re-bucketing of the math unit's clock, W14-28's single-access credit);
  both are gone now that the underlying undercount is fixed. Corrected
  pacing was ~47% too fast before this ticket, root-causing the
  W14-33/W14-38 APU handshake deadlock family for at least three titles
  (Rival Turf!, Super Turrican, Wario's Woods — confirmed rendering after
  the fix; ActRaiser 2/Illusion of Gaia/Robotrek's deadlock persists and
  is a distinct, driver-side defect per W14-33/38's own tracing).
- The `$42xx` math-unit ports are clocked by the CPU clock, not by master
  cycles (fullsnes "SNES Maths Multiply/Divide": "one needs the same
  amount of 'wait' opcodes no matter if the CPU Clock is 3.5MHz or
  2.6MHz"). `MathUnit::tick` takes a real CPU-cycle count — bus accesses
  plus internal cycles for the instruction, from `AccessCost::accesses`
  and `Cpu::internal_cycles` — directly, one unit step per cycle.
- 5A22 extras: hardware multiply/divide regs ($4202-$4206) with real
  latency (8/16 cycles, intermediate-value reads readable mid-operation —
  emulate the shift-register intermediates in Accuracy mode); NMITIMEN
  ($4200) NMI/IRQ enables; H/V IRQ ($4207-$420A, $4211 ack); auto-joypad
  read (starts ~line 225, 3-4 lines, $4218-$421F; reading $4016 during
  auto-read quirk); WRAM port $2180-$2183.
- Interrupt timing: NMI at V=225(240 overscan)/H≈0.5 with the $4210 read
  race; IRQ per H/V compare with the one-dot-late quirk.
- Test gates: **SingleStepTests `65816`** JSON vectors first (native unit
  test — as of W14-39, `accesses + internal == cycles.len()` is checked
  exactly, not just registers and memory), then **gilyon/snes-tests
  `cputest`** (golden `tests.txt` + framebuffer hash), PeterLemon CPU
  tests (golden frame).
- **What is still NOT modelled**: per-cycle bus timing inside an
  instruction — which of an instruction's several cycles touches which
  address, and in what order. W14-39 gives an exact total cycle *count*
  per instruction (pinned against the vectors) but not a replay of the
  cycle-by-cycle bus *sequence*; that needs a genuinely cycle-accurate
  executor and remains W6-02a's open scope. The `MVN`/`MVP` vector
  exclusions exist for exactly this reason — their SingleStepTests cases
  are captured mid-iteration, which only a resumable sub-instruction
  executor could replay.

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
  game-identity research). **W14-52 (2026-09-23):** real hardware never
  reads the header at all, so when neither location scores as plausible,
  `parse_snes_header` falls back to trying LoROM/HiROM/ExHiROM(>4 MiB) in
  turn and accepting the first whose RESET vector decodes to a plausible
  65816 reset prologue — the diagnostic lands on
  `SnesHeader::header_fallback`. See `docs/TESTING.md`'s W14-52 section
  for the cited rule, the population survey, and the one nibble-collision
  class (SA-1/S-DD1/ExHiROM) this ticket named but deliberately did not
  touch.
- FastROM ($420D) speed switch. ExHiROM + Super FX (~16 games) and the
  one-off chips (Cx4, S-DD1, SPC7110, ST01x) stay **explicitly deferred**;
  `rf-cart` detects and reports "unsupported chip: <name>" rather than
  half-booting. **SA-1 (~34 games, 11 in the local library) moved into
  scope 2026-09-18 (D-013, SRS FR-CORE-039)** — see below.
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

- **SA-1 (~34 games, 11 archives in the local library — D-013, SRS
  FR-CORE-039, Wave 17, `crates/rf-snes/src/sa1.rs`).** A second 65C816 at
  up to 10.74 MHz with its own 2 KiB I-RAM, an 8 KiB mappable BW-RAM
  window shared with the SNES side, its own bank registers, and a
  register-mapped DMA/character-conversion/arithmetic/variable-length-bit
  unit — clean-room from fullsnes "SNES Cart SA-1" and snes.nesdev.org
  (NFR-011, no emulator source). Shape (D-013): `rf-snes`'s CPU already
  takes its bus as a `CpuBus` trait object, so the SA-1 is a second
  `crate::cpu::Cpu` running over its own `Sa1Bus` (a borrowing wrapper
  over the shared I-RAM/BW-RAM/ROM/register store, built fresh every
  `Sa1State::step` call) — not a second CPU implementation, and not a
  second thread: `SnesSystem::step` runs the main CPU's one instruction,
  then lets the SA-1 catch up on the master cycles that instruction (plus
  its DMA/HDMA) just spent, the same catch-up-scheduling principle as
  every other chip on this machine (§1).
  - **Cost model (W17-04, superseding W17-02's flat approximation;
    `Sa1Bus::access_cost`).** The SA-1's own resources — I-RAM and its
    register window — always cost 2 master cycles per access: its full,
    uncontended 10.74MHz rate (master clock / 2), since nothing on the
    SNES side can reach them (fullsnes "Misc": "The SA-1 CPU can access
    memory at 10.74MHz rate (or less, if the SNES does simultaneously
    access cartridge memory)"). ROM and BW-RAM are shared with the SNES
    side, so they cost the same uncontended 2 **unless** the SNES side
    (the main CPU's instruction, its MDMA, or its HDMA — anything that
    ran earlier in the same `SnesSystem::step`) also touched that same
    device this step, in which case the access costs the doubled 4 —
    consistent with fullsnes's own DMA speed table ("SNES Cart SA-1 DMA
    Transfers": ROM->I-RAM at the full 10.74MHz, but ROM->BW-RAM,
    BW-RAM->I-RAM and I-RAM->BW-RAM all at the halved 5.37MHz whenever
    BW-RAM is involved). Contention is tracked per step, for the whole
    device, not per byte-range or per bus cycle
    (`SnesBus::sa1_rom_contended`/`sa1_bwram_contended`, set by `read`/
    `write` and read once after the main CPU's share of the step) — an
    explicit, deterministic approximation of "the documented wait
    states... when it touches BW-RAM/ROM while the SNES CPU holds the
    bus" that W17-02 deferred. A main CPU that runs from ROM (the common
    case) sees its SA-1 pay the doubled ROM rate almost every step, which
    is the expected, hardware-consistent outcome. Normal DMA
    (`execute_normal_dma`) charges its own per-byte rate straight from
    that same table (2 for ROM->I-RAM, 4 otherwise) to the SA-1's credit,
    never to the main CPU's — nothing in fullsnes's DMA section stalls
    the main CPU for a Normal DMA (the "SNES CPU is paused" sentence is
    about Character Conversion 1's `$43xx` SNES-side DMA, a different
    mechanism).
  - **Approximations, carried from all four slices, stated exactly as
    implemented rather than left implicit:**
    - The SNES CPU's own wait while the SA-1 holds BW-RAM mid-DMA is
      **not modelled** — DMA execution is charged atomically to the SA-1
      instruction that triggers it rather than spread across master
      cycles the main CPU could contend with, so fullsnes's "BW-RAM
      cannot be used during character conversion DMA" has no expression
      here yet.
    - DMA/VBR ROM addressing is linear (SDA/DDA treated as a plain index
      into each device's buffer, mod its length) rather than re-derived
      through the CXB/DXB/EXB/FXB bank registers — fullsnes's own
      "Unknown details" note says SDA/DDA increment behaviour isn't
      documented either.
    - Arithmetic unit timing is immediate (result available the same
      step MB's high byte is written); the DMA priority bit (`$2230` bit
      6) is read but has no scheduling effect (the SA-1 is stalled for
      the whole transfer either way — interleaving its instruction
      stream with an in-flight DMA is not modelled); the VBR window
      (`$230C`/`$230D`) increments only on `$230C`, not `$230D`; timer
      compare semantics were chosen where fullsnes states the polarity
      but calls the rounding/edge rule a guess.
    - BW-RAM/I-RAM write protection is one **shared** gate per side
      (`$2226`/`$2227` SBWE, `$2229`/`$222A` SIWP/CIWP) — Kirby Super
      Star and Kirby's Dream Land 3 both corrected an initial per-side
      model during W17-03 (see `sa1.rs`'s module doc and
      `docs/TESTING.md`'s named-cause list); `$2228` BWPA's protected-area
      floor is stored and cited but not enforced on top of the gate.
    - `Sa1Regs::unknown_write_offsets` (W17-04) counts writes into `$22xx`
      offsets fullsnes's own I/O map table leaves blank (`$2216-$221F`,
      `$222B-$222F`, `$223A-$223E`, `$2255-$2257`, `$225C` and up) —
      diagnostic only, printed by `title_probe`'s `PROBE_SA1REGS=1`, never
      part of save state and never gating a write.
  - **Determinism.** `crates/rf-snes/tests/sa1_determinism.rs` runs a
    hand-assembled SA-1 cart (both CPUs executing real 65816 code, not
    just register pokes) for a fixed instruction count from two
    independent `SnesSystem::load` calls and diffs a full
    `StateRegion::ALL` snapshot — nothing in the cost model above reads
    wall-clock, thread order, or hash-map iteration, so the two runs are
    bit-identical. A second test in the same file saves+reloads
    `StateRegion::Cart`/`Mapper` between arming a Normal DMA's parameters
    and writing the register that triggers it, and confirms the restored
    run copies the same bytes a never-interrupted run does.

### 3.6 Super FX (GSU) — slice 5 of 5 (census + named causes)

**Super FX / GSU-1/GSU-2 (~10 games, D-014, SRS FR-CORE-040, Wave 18,
`crates/rf-snes/src/gsu.rs`).** A cartridge-resident 10.74MHz (GSU1) or
21.4MHz-capable (GSU2) RISC CPU with its own R0-R15 register file, a
mappable ROM/RAM view, a code cache and a pixel/bitmap-plot unit for
Mode-7-style raster games — clean-room from fullsnes "SNES Cart GSU-n"
(NFR-011, no emulator source). Detection (`rf-cart`): chipset $13-$1A
(coprocessor nibble $1) accepted as `Coprocessor::SuperFx { version,
ram_kib }`; `version` is GSU1 vs GSU2, picked by fullsnes's own stated
heuristic ("Games with 2MByte ROM are typically using GSU2") since the
header carries no field that names it directly — this knowingly
mis-detects Star Fox 2 (1 MiB, GSU2) as GSU1, exactly as fullsnes's own
caveat predicts; `ram_kib` comes from the extended header's expansion-RAM
byte (`$FFBD`-canonical, 3 bytes before the LoROM/HiROM header base this
project already anchors on), `0` when that header is absent (a real,
observed shape in this project's own library dumps).

**Slice 1 (this ticket) models only the SNES-side memory map and register
window — the GSU does not execute.** Shape mirrors SA-1 slice 1 (W17-01):
`rf-cart` parses the board data, `SnesSystem::load` wires it into a live
`crate::gsu::GsuState` (a register file plus the cartridge's own RAM
buffer), and `crate::mapping::gsu_target` is checked by `SnesBus::target`
before the generic LoROM/HiROM `map` — a GSU cart keeps a plain LoROM
header (fullsnes: "the cartridge header declares the cartridge as
LoROM"), so this is a bus-window overlay, not a new `SnesMapMode` (unlike
SA-1's dedicated map mode $23).

- **Memory map** (fullsnes "SNES Cart GSU-n Memory Map", the GSU2 table
  used uniformly for both chip versions — GSU1 is the same shape at
  smaller sizes): register window `$3000-$3FFF` and RAM mirror
  `$6000-$7FFF`, both in banks `$00-$3F`**and**`$80-$BF`; primary ROM
  `$8000-$FFFF` in banks `$00-$3F` **only** — fullsnes lists `$80-BF` as a
  separate, unpopulated "Additional CPU ROM" chip select, so `gsu_target`
  reports `None` there and lets the ordinary LoROM mirror in the generic
  `map` answer it instead (the correct behaviour for hardware nothing
  populates); ROM again, HiROM-style and linear, in banks `$40-$5F`; RAM
  in banks `$70-$71`.
- **Register window** (fullsnes "...I/O Map"/"...General I/O
  Ports"/"...Bitmap I/O Ports"): R0-R15 with the documented even/odd
  LATCH write protocol (writing R15's MSB also sets GO and starts
  "execution" — which this slice never actually runs); SFR with bits 1-5
  (Z/CY/S/OV/GO) SNES-writable and bit 15 (IRQ) cleared on read; PBR
  (R/W), ROMBR/RAMBR/CBR/VCR (R, GSU-opcode-set only, so they never leave
  reset this slice), SCBR/SCMR/CLSR/CFGR/BRAMR (W); COLR/POR held for a
  future opcode slice with no SNES-side address, per fullsnes's own "N/A".
  Mirrors (`$3020-$302F`, `$3040-$30FF`, `$3300-$34FF`) fold onto the
  canonical `$3000-$303F` block exactly as fullsnes's own "Full I/O Map
  with Mirrors for GSU2" table lays out; cache RAM (`$3100-$32FF`) is
  plain, uninterpreted read/write storage this slice does not execute
  against.
- **SCMR RON/RAN ownership** (`$303Ah` bits 3-4): while the GSU owns the
  ROM or RAM bus, the SNES side's own read of that window returns open
  bus (`SnesBus::read`/`peek`) rather than the cartridge — fullsnes states
  the rule ("0=SNES, 1=GSU") but not literally what byte the SNES CPU
  reads while the GSU holds the bus, so this project's own existing
  open-bus convention is applied rather than invented a second time.
- **IRQ**: SFR bit 15 ORs into the 65C816's IRQ input alongside SA-1's
  `$2209` bit 7 (`SnesSystem::step`) — with no vector-override register
  (unlike SA-1's `$220E`/`$220F`), a GSU IRQ always dispatches through the
  ROM's own IRQ vector. Nothing sets the flag this slice (no STOP opcode
  runs yet); the plumbing is exercised directly by a test-only setter.
- **What is deliberately NOT modelled yet**: timing/cycle costs charged
  against the master clock (W18-04), the code/pixel/other caches' actual
  semantics (W18-04/W18-05), and the bitmap-plot pixel format (W18-05).
  `Gsu`'s fields for those (COLR, POR, the cache buffer) existed since
  slice 1 purely so a later slice does not have to touch the save-state
  format; slice 2 below gives COLR/POR their real write semantics.

**Slice 2 (this ticket, D-014) gives the GSU an instruction core** —
every opcode in fullsnes "SNES Cart GSU-n CPU MOV/ALU/JMP and
Prefix/Pseudo Opcodes" plus the register-side rules in "CPU Misc", run
from `GsuState::run`/`step_one`/`exec_opcode` in `gsu.rs`.

- **Fetch/memory model**: opcode and operand bytes are fetched at
  `PBR:R15` (`GsuState::fetch_byte`, advancing R15 exactly like real
  hardware's PC); `PBR` in `$70`/`$71` fetches from GSU RAM, everything
  else from ROM via the same LoROM (`$00-$3F`, offset-masked so
  `$0000-$7FFF` mirrors `$8000-$FFFF` — "for 'GETB R15' vectors") /
  linear-HiROM (`$40-$5F`) addressing fullsnes's "Memory Map (at GSU
  Side)" documents. GETB/GETBH/GETBL/GETBS/GETC read `[ROMBR:R14]` the
  same direct way. **No code-cache execution** (the 512-byte cache is
  still plain read/write storage — CACHE only updates CBR) **and no
  ROM/RAM-data-cache WAIT modelling** — ticket W18-02's brief accepts a
  direct, uncached read/write for this slice; both are W18-04/W18-05
  territory once cycle costs exist to make a WAIT meaningful.
- **Prefix state** (`Gsu::sreg`/`dreg`/`alt1`/`alt2`/`b_flag`, one
  instruction's lifetime): TO/WITH/FROM select Dreg/Sreg/both (WITH also
  sets B); ALT1/ALT2/ALT3 (`$3D`/`$3E`/`$3F`) select the opcode variant
  a `match` arm reads directly; "other" opcodes reset all five fields at
  the end of `exec_opcode` (`Gsu::reset_prefix_state`) — Bxx branches and
  the prefix opcodes themselves are the only opcodes that skip that call,
  per fullsnes's explicit exception. The B flag's realization of 1n/Bn as
  MOVE/MOVES (rather than TO/FROM) is a plain `if self.regs.b_flag` branch
  in those two opcode-range arms. SFR's ALT1/ALT2/B bits (`$3031`,
  read-only from the SNES) now reflect this live state instead of slice
  1's permanent zero.
- **Control-flow delay slot** (`Gsu::pending_jump`, fullsnes "Jump
  Notes": "the next BYTE after the jump opcode is fetched...and is
  executed before continuing at the jump-target address"): every branch
  taken, JMP/LJMP, a taken LOOP, and any MOV/ALU whose Dreg is R15 sets
  `pending_jump` instead of writing `R15` directly; `GsuState::step_one`
  applies it only after the ONE opcode that follows has finished — one
  `Option` field carries both the new PBR (for LJMP) and the new PC, so
  the same mechanism serves every one of these cases without duplicating
  the one-instruction delay in each opcode body.
- **STOP/IRQ**: STOP prefetches one dummy byte (never executed, landing
  R15 at `$+2`), clears GO, and sets SFR bit 15 unconditionally. The bit
  is always visible to a title polling `$3031` (and resets on that read,
  fullsnes); `Gsu::irq_pending` — the line ORed into the 65C816 — is
  additionally gated by `CFGR.Bit7` (the documented IRQ mask), so a title
  that disabled the mask still sees the SFR bit but the CPU never
  dispatches. The MC1/GSU1 "STOP after a RAM write hangs" erratum is not
  modelled (no cycle-accurate bus state exists yet to detect it).
- **PLOT/RPIX** are given the real pixel cache and RAM bitmap in slice 3
  below; `plot_calls`/`rpix_calls` remain simple call counters (not
  saved), now alongside real behaviour rather than in place of it.
  **COLOR/CMODE are real**, not stubbed — their documented effect is a
  plain register write (`COLR`/`POR`).

**Slice 3 (this ticket, D-014) gives PLOT/RPIX the real pixel cache and
RAM bitmap writeback**, per fullsnes "SNES Cart GSU-n Bitmap I/O Ports"
and "Pixel-Cache" (`Gsu::primary_cache`/`secondary_cache`,
`GsuState::exec_plot`/`exec_rpix`/`flush_primary`/`flush_line_to_ram`).

- **Pixel cache** (fullsnes "Pixel-Cache": "RAM-Pixel-Write-Cache (two
  8-pixel rows)"): `PixelCacheLine` holds a `valid` flag, the 8-aligned
  `x_base`/`y` the line was opened for, 8 colour bytes, and an 8-bit
  `pending` mask ("8 flags (indicating if (nontransparent) pixels were
  plotted)"). PLOT computes `seg_x = X AND F8h`; if the primary cache is
  valid and `(x_base, y)` differs from the new pixel's, the primary is
  flushed first (flush condition 1, "when plotting to different values");
  a fresh line is opened if none is active; the plotted pixel's bit is
  set in `pending` only when it isn't skipped by transparency. All 8
  pending bits set triggers flush condition 3 ("cache full") immediately
  after the same PLOT. RPIX is flush condition 2: it always flushes
  first, then reads RAM directly, never the cache ("RPIX isn't cached, it
  does always read data from RAM").
- **Second cache**: `Gsu::secondary_cache` models the documented hand-off
  stage ("Primary Pixel Cache... Secondary Pixel Cache (data copied from
  Primary Cache, this WAITs if Secondary cache wasn't yet forwarded to
  RAM)") structurally — `GsuState::flush_primary` moves the primary line
  into it, drains any stale secondary line to RAM first, then drains the
  new one — but performs both hand-offs **synchronously** within one
  call. No WAIT/stall state exists yet to make the overlap observable, so
  `secondary_cache` is always empty again by the time `flush_primary`
  returns; that stall timing is explicitly deferred to slice 4, which is
  also where `STEP_BUDGET`'s uncharged-cycle model as a whole gets fixed.
- **POR semantics** (fullsnes "Bitmap I/O Ports", POR bits 0-4): bit0
  Transparent=0 skips color 0 (PLOT still advances R1, never sets the
  pending bit); bit1 Dither uses `(R1 XOR R2) & 1` to pick COLR's high
  nibble (`COLOR/10h`) instead of the full byte, 4/16-color mode only;
  bit2 High-Nibble and bit3 Freeze-High transform COLOR/GETC's *incoming*
  byte before it lands in COLR (`GsuState::write_colr`: High-Nibble
  replaces the low nibble with the high nibble first, then Freeze-High
  writes only the low nibble of the stored register, "write-protect
  COLOR.MSB"); Freeze-High separately narrows PLOT's own transparency
  check to the low 2/4 bits even in 256-color mode ("ignores upper 4bit
  even when in 256-color mode"); bit4 OBJ Mode forces OBJ tile numbering
  regardless of SCMR.HT0/HT1.
- **RAM address formulas** (fullsnes "Bitmap I/O Ports", cited verbatim
  in `GsuState::tile_number`/`tile_row_addr`'s doc comments): Tile Number
  is `(X/8)*10h/14h/18h + (Y/8)` for 128/160/192-pixel height, or
  `(Y/80h)*200h + (X/80h)*100h + (Y/8 AND 0Fh)*10h + (X/8 AND 0Fh)` for
  OBJ mode; Tile-Row Address is `TileNo*10h/20h/40h (2/4/8bpp) +
  SCBR*400h + (Y AND 7)*2`, with plane pairs at `Addr+0/0x10/0x20/0x30`.
  This is always bank-`$70`-relative (`SCBR`'s own "Base =
  700000h+N*400h") — **not** RAMBR-relative, unlike LDB/STB/LDW/STW/SM/
  SMS/SBK — so `GsuState::bitmap_ram_index` folds the raw 17-bit offset
  onto `self.ram` directly (mod `ram_len`), the same flat-array shape
  `gsu_ram_index` already gives bank `$70`/`$71`. Each pixel's bits are
  packed MSB-first per plane byte (`plot_pixel_bits`/`read_pixel_bits`),
  matching the SNES-standard bitplane layout `crate::sa1::write_tile_pixel`
  already uses for SA-1's character conversion. Flush is a true
  read-modify-write: only `pending`-set pixels are written, so untouched
  RAM bytes (and untouched bit positions within a written byte) keep
  their prior value.
- **Cycle-accurate flush timing (slice 4, below)** replaces the "not yet
  modelled" note this bullet used to carry. The MC1/GSU1-specific
  pixel-cache erratum remains unmodelled — none is documented in the
  cited fullsnes chapter beyond the STOP-after-RAM-write note already
  covered in slice 2.

**Slice 4 (this ticket, D-014) gives the GSU real clocking against the
SNES master clock, plus the code cache, ROM buffer and RAM buffer**, per
fullsnes "SNES Cart GSU-n CPU Misc"/"Code-Cache"/"Other Caches"
(`crates/rf-snes/src/gsu.rs`, the module constants above `Gsu`'s
definition, `GsuState::run_credited`/`step_one`/`fetch_byte`).

- **Clock conversion** (fullsnes "3039h - CLSR": "0=10.7MHz, 1=21.4MHz"):
  the SNES NTSC master clock is 21.47727 MHz
  (`crate::timing::Region::Ntsc`), which divides evenly by both rates —
  `master_cycles_per_gsu_cycle` returns `2` (CLSR=0) or `1` (CLSR=1), an
  exact integer ratio in both modes, not an approximation.
- **Credit-based interleave** (`GsuState::run_credited`, called from
  `SnesSystem::step` the same place the provisional `GsuState::run` used
  to be, right after the SA-1 credit loop): replaces `STEP_BUDGET`
  entirely. Each `SnesSystem::step` deposits the master cycles the
  65C816 (plus its MDMA/HDMA) just spent into `GsuState::credit`; opcodes
  run one at a time, each one's cost (in GSU cycles, converted to master
  cycles) debited from the balance, until the balance cannot cover
  another opcode or GO clears — the exact shape `Sa1State::credit`
  already established for SA-1. Credit never accumulates while GO is
  clear, and is itself part of save state (`crates/rf-snes/src/state.rs`,
  the `Cart` region's GSU block) — a save/load round trip must not lose a
  fractional credit balance, since that would shift which master-clock
  cycle a subsequent GSU opcode completes on.
- **Per-opcode cost table**: every opcode's `exec_opcode` arm already
  returned its documented clock count as of W18-02 (kept as this
  project's canonical cache-hit cost table — the fullsnes chapter
  extracted for this project gives no full per-opcode cycle table of its
  own, only the aggregate facts below); this slice adds three
  surcharges on top of that base cost, each cited to "CPU Misc"/
  "Code-Cache"/"Other Caches":
  | source | cost | cited fact |
  |---|---|---|
  | code-cache hit (opcode/operand byte) | `+0` (already in the base cost) | "Cache-Code is 6/3 times faster than ROM/RAM" — this project reads the base cost as already assuming a 1-cycle cache-hit fetch |
  | code-cache miss (same byte) | `+2` | upgrades the assumed 1 cycle to the documented uncached "3 cycles at both 21MHz and 10MHz" |
  | ROM-buffer stall (GETxx/GETC right after R14/ROMBR changes) | `+3` (CLSR=0) / `+5` (CLSR=1) | "ROM Read: 5 cycles per byte at 21MHz, or 3 cycles per byte at 10MHz" |
  | RAM-buffer stall (a store right after another store) | `+10` per word / `+5` per byte | "RAM Write: 10 cycles per word at 21MHz" (10MHz-word and per-byte figures are undocumented — see the constants' own doc for the stated interpolation) |
  | pixel-cache flush (a PLOT/RPIX that drains the primary/secondary cache) | `+5` per bitplane byte written | same per-byte RAM-write figure as the RAM buffer, since fullsnes gives the flush no cost of its own beyond "it happens through the RAM-Write-Data Cache" |
- **Code cache** (fullsnes "Code-Cache": "512-byte cache... 32 lines of
  16-bytes"): `Gsu::cache_valid` is a 512-entry per-byte validity bitmap
  sharing `Gsu::cache`'s backing storage (real hardware's code-cache RAM
  *is* the same 512 bytes the SNES can pre-load through `$3100-$32FF` —
  "Code-Cache Loading Notes" describes GSU-side fill happening
  progressively, byte by byte, "loaded alongside while executing
  opcodes", which per-byte (not per-16-byte-line) tracking matches
  exactly). `GsuState::fetch_byte` is the single fetch/fill/cost path: a
  byte inside the current `[CBR, CBR+0x200)` window is a hit if already
  valid, or a miss that fills it and charges the surcharge above; a byte
  outside the window is never cached (always the surcharge, byte read
  straight from ROM/RAM). Lines empty on: an SNES `SFR` write with GO=0
  (`CBR=0` too, "the SNES can set CBR=0000h by writing GO=0"); the
  `CACHE` opcode (`CBR = R15 AND FFF0h`); `LJMP` (`CBR = R15 AND FFF0h`,
  R15 = the jump target — applied at dispatch against the already-known
  target rather than deferred to the delay slot, since either timing
  clears the cache well before the GSU's next fetch from the new
  address). Executing from Game Pak RAM ($70/$71 via PBR) is cached
  exactly like ROM — fullsnes's own caution that a title "must clear the
  cache by writing GO=0" if GamePak RAM code changed only makes sense if
  RAM-resident code is cached the same way ROM-resident code is.
- **ROM buffer** (fullsnes "Other Caches", "ROM-Read-Data Cache (1-byte
  read-ahead)"): modelled as a generation watermark, `Gsu::
  rom_buffer_ready_after`, bumped to `instructions_executed + 2` whenever
  R14 or ROMBR changes (`Gsu::commit_to`, the `ROMB` opcode); a
  GETB/GETBH/GETBL/GETBS/GETC that runs before that watermark (i.e. the
  instruction immediately after the change) pays the ROM-read stall from
  the cost table above, matching "GETxx executed shortly after changing
  R14".
- **RAM buffer** (fullsnes "Other Caches", "RAM-Write-Data Cache
  (1-byte/1-word write queue)"): the same watermark shape,
  `Gsu::ram_buffer_ready_after`, bumped by every STB/STW/SBK/SM/SMS store
  to `instructions_executed + 2`; a store that runs before that
  watermark (two stores back to back) pays the RAM-write stall, matching
  "executing two store opcodes shortly after each other".
- **SNES-side ownership while the GSU holds the bus** (SCMR RON/RAN):
  unchanged from slice 1 — the SNES still reads open bus for ROM/RAM the
  GSU owns (`SnesBus::read`/`peek`'s `Target::Rom`/`Gsu::ran` gates); this
  slice's caches/buffers only change what the *GSU itself* pays in
  cycles, not what the SNES CPU observes on its own reads.
- **Not yet modelled**: the RAM-Address-Cache's own WAIT rules beyond
  what `Gsu::last_ram_addr`'s SBK writeback already needs; the
  MC1/GSU1-only "STOP after a RAM write hangs" erratum (still, as slice
  2 left it, not modelled — no cycle-accurate bus state exists to detect
  the specific pattern fullsnes describes); the pixel cache's overlap
  between a *later* instruction and an in-flight flush (this project's
  flush is always synchronous within the triggering PLOT/RPIX, so a
  flush never spans multiple opcodes the way real hardware's WAIT would
  let it).
- **Undocumented-but-cited opcodes implemented as documented**: UMULT #n
  and XOR Rn/#n (fullsnes "GSU Undoc opcodes": present in the chip's
  opcode summary/index but not spelled out in the alphabetical body) are
  implemented per their Nocash-syntax one-liners, same as every
  documented opcode.
- **Known simplifications, cited where they live in code**: FMULT's
  real-hardware Dreg=R4 erratum ("this will reportedly leave R4
  unchanged") is not replicated — no title in this project's library is
  documented to depend on it, and special-casing it would make ordinary
  FMULT-into-R4 wrong for everyone else; FMULT/LMULT's CY flag has no
  documented definition, so this slice always clears it.

**Slice 5 (this ticket, W18-05) is census + named causes, not new GSU
mechanism.** One fix shipped, one diagnostic gap closed, everything else
named. Full per-title table and traces: docs/TESTING.md's own W18-05
section.

- **GSU RAM defaults to 32 KiB when the extended header is absent**
  shipped in the prior ticket (`W18-04`'s `fix(W18-04)` commit,
  `superfx_expansion_ram_kib`'s `raw == 0xFF` arm) is the one change
  this whole slice-5 arc rests on: it is what let Star Fox / Rev 1 /
  Rev 2 start rendering (fullsnes "Caution: Starfox/Star Wing,
  Powerslide, and Starfox 2 do not have extended headers... RAM Size
  for Starfox/Starwing is 32Kbytes"). This slice verified, rather than
  assumed, that the fix's own scope boundary is correct: Star Fox 2's
  own real cartridges (per this same fullsnes sentence) have their RAM
  size documented as **unknown**, not 32 KiB, so the fix's `raw == 0xFF`
  condition deliberately does not claim to cover it — and, checked
  against the actual `Star Fox 2 (USA, Europe) (Classic Mini, Switch
  Online).zip` header this ticket, that title's extended-header byte
  (`$7FBD`) is `$06` (64 KiB, the other size fullsnes's header chapter
  documents as existing), not `$FF` — this specific dump *does* carry a
  real extended header, unlike an original unreleased prototype, so the
  "RAM size unknown" caution does not even apply to it; GSU RAM parses
  non-zero (64 KiB) for this dump today, confirmed by `plot_calls=2`
  over an 1800-frame run (RAM-gated PLOT can only fire at all once
  `mapping::gsu_target`'s `board.ram_len > 0` gate passes) where it used
  to be `0` before any RAM existed.
- **`title_probe`'s `frames` mode gained the GSU core-state dump**
  (`PROBE_GSUREGS=1 PROBE_MODE=frames`, `crates/rf-harness/tests/
  title_probe.rs`): the register/PC/counter report `print_gsu_reg_report`
  already prints under the default (`PROBE_INSTR`-budget) mode and under
  `PROBE_MODE=gsuhist` had never been wired into `frames` mode, which is
  the one mode that answers "does this title ever render" directly — a
  stuck-GSU trace previously had to switch modes mid-investigation to
  see both facts at once. Now both print from one run.
- **Named, not fixed, for the still-non-rendering GSU archives** (full
  evidence in docs/TESTING.md): **Star Fox 2 (Classic Mini/Switch
  Online dump)** — RAM is not the blocker (see above); the GSU is
  started and cleanly stopped 18 times over an 1800-frame run and
  produces only 2 PLOTs before going idle, a shape this ticket could
  not distinguish between "a GSU2-only memory/bank detail this project's
  shared GSU1/GSU2 map does not yet model" and "a menu/boot wrapper this
  specific repackaged dump carries that genuinely needs more than 1800
  frames" — both named, neither verified, per Bug Fix Discipline (a
  theory not independently confirmed is not shipped as a fix).
  **Vortex** — still parked on a `JMP Rn`-family opcode with GO set,
  the same shape three prior tickets (W18-02/03/04) traced without
  finding an opcode, interleave, or cache defect; this session's own
  fresh counters (huge cache-hit and RAM-buffer-stall counts) confirm
  it is a genuinely active, not idle, loop, adding no new theory. **The
  three Star Fox 2 betas** — confirmed this ticket by calling
  `rf_cart::Cartridge::load` directly on their extracted bytes: all
  three return `InvalidHeader("no plausible SNES header at $7FC0 or
  $FFC0: neither location has a map mode matching it, an assigned
  country code and a plausible revision")` — non-canonical, incomplete
  prototype dumps (1,047,074 and 1,048,259 bytes, not the canonical
  1,048,576), refused by this project's ordinary, chip-agnostic header
  plausibility check, not a GSU-specific gap.
- **Super Star Fox Weekend is not stuck** — the one title this ticket's
  own `PROBE_FRAMES=1800` sweep resolves outright: `varied_at=Some(788)`,
  which is past `boot_census`'s fixed 600-frame budget but well inside a
  1800-frame window, the same "census-fixture, not a GSU defect" shape
  W18-04's own follow-up #3 named for Star Fox's boot-decompression
  pass. `boot_census`'s bucket for this title is therefore a fixture
  limitation, not a rendering defect — left unchanged rather than
  special-cased per-title (law 5).
- **Census** (RELEASE, same 15 real GSU archives + 5 canaries as every
  prior slice, `boot_census_child`): unchanged from the post-W18-04
  state committed at HEAD — **1101/44/120/0/0** — since nothing in this
  ticket's one production change (the RAM-default fix already landed
  last ticket) altered which title varies inside the fixed 600-frame
  window; only Super Star Fox Weekend's true, past-the-window varied
  frame changed, which `boot_census` cannot see by construction.
- **What is still not modelled, named honestly rather than implied
  complete**: the RAM-Address-Cache's own WAIT rules beyond
  `Gsu::last_ram_addr`'s SBK writeback; the MC1/GSU1-only "STOP after a
  RAM write hangs" erratum (no cycle-accurate bus state exists to detect
  the specific pattern fullsnes describes); the pixel cache's overlap
  between a later instruction and an in-flight flush (this project's
  flush is always synchronous, so it never spans multiple opcodes the
  way real hardware's WAIT could); FMULT's Dreg=R4 erratum and its
  undocumented CY-flag behaviour (both named in slice 2's own bullet
  above); any GSU2-only bank/mirror behaviour beyond the "GSU2 table
  used uniformly for both chip versions" choice slice 1 made — Star Fox
  2's still-unresolved cause (above) is the concrete case this
  simplification may or may not be responsible for, not yet
  distinguished from a boot-length explanation; and an SNES-side
  golden-frame test DMAing the GSU RAM bitmap to VRAM, named as a gap
  rather than claimed done back in slice 3 and still not written.

**W18-06 (this ticket) traced both remaining titles to their actual
inputs — a RAM cell and a bus-ownership register — rather than more
opcode auditing, per W18-04's own method lesson. Vortex's dropped-write
defect is FIXED and shipped, verified by two divergence traces (one per
title) that jointly pin the correct asymmetric rule. Star Fox 2 remains
BLOCKED with a named cell and both values. Full traces: docs/TESTING.md's
own W18-06 section.**

- **Vortex, root cause found and FIXED.** A traced boot writes `$303A`
  SCMR = `$39` (RON=1, RAN=1) while GO=0 (confirmed on every SCMR write
  sampled across the boot — the GSU program has not started yet), then
  stores its whole 8 KiB `$00:6000-$00:7FFF` GSU RAM setup block (8192
  consecutive SNES-side bytes, descending addresses, an `85/89/00`
  tile-data pattern) through the ordinary CPU write path. Every one of
  those 8192 writes was dropped: `SnesBus::write`'s `Target::GsuRam` arm
  gated on the raw RAN bit alone, which reads back `1` regardless of
  whether the GSU is actually running. The GSU's own program then read
  back `0` for cells the SNES had just written real bytes to, and spun
  forever inside a `JMP Rn`-family computed jump recomputed from that
  zeroed RAM (the same `PBR:R15` region three prior tickets traced
  without finding an opcode defect, because the defect was upstream of
  every opcode in that loop). **Fix, shipped after two rounds of
  verification**: `Gsu::owns_ram_bus` (`GO && RAN`) added, used ONLY in
  `SnesBus::write`'s `Target::GsuRam` arm — every READ path (both ROM and
  GSU RAM, in both `read` and `peek`) is UNCHANGED from W18-01's original
  raw-bit gate. The first attempt applied `GO`-gating to reads too and
  was reverted: a divergence trace against Star Fox (USA) found 2,361 ROM
  reads (zero RAM reads) where GO=0/RON=1 and the raw gate was load-
  bearing — Star Fox's own boot relies on seeing open bus at ordinary ROM
  addresses scattered across its code, not just the exception-vector
  region fullsnes's own GO-conditioned sentence describes — and
  `boot_census_child` confirmed the regression directly (Star Fox (USA)/
  (Rev 1)/(Rev 2) flipped from `rendered` to `uniform`, reproduced twice
  via `git stash`). The two traces (Vortex's write-side, Star Fox's
  read-side) do not conflict: they are about different bus operations on
  different address classes, and the narrow, write-only rule satisfies
  both. Verified: Vortex's GSU now stops cleanly (`go=false`, not a
  spin), fires 4 `PLOT` calls (was `0` in every measurement this whole
  investigation ever took), and — pushed to 80,000,000 instructions —
  clears `forced_blank` and populates VRAM (`vram_nonzero=7550`), both
  firsts. **Still does not render a frame**: the CPU parks permanently
  alternating between `$00:0000` (WRAM, an interrupt-vector trampoline
  cell) and `$00:FD21` (ROM, disassembled as a single `RTI` followed by
  unused `$FF`-filled space) — a DIFFERENT, downstream defect this ticket
  did not cause and did not fix: the real NMI/IRQ handler was apparently
  never installed at that trampoline cell, so every interrupt returns
  immediately. Named as the next cell for a follow-up ticket.
- **Star Fox 2, named not fixed.** 18 clean GSU start/stop cycles
  complete (`go_set=18/go_clear=18/go_clear_by_stop=18`), matching
  W18-05's own finding. Traced further this ticket: the SNES CPU reads
  `$3031` (SFR high byte, the IRQ flag) only 8 times in the entire run,
  all clustered in the earliest few GSU cycles (offsets `3031`/`3021`/
  `34B1`/`34A1` — SFR-high mirrors, consistent with one-time chip-version
  probing) — so whatever gates the 19th GSU launch is NOT SFR/IRQ
  polling; nothing reads it again for the rest of a 30,000,000-
  instruction run. A large `$70:xxxx` STZ-loop (RAM bank `$70` clear,
  `DBR=$70`, X climbing from ~`$5EF2` toward wraparound) found early in
  this trace is NOT the hang — confirmed by running past it: VRAM/OAM/
  CGRAM populate, NMI/IRQ fire repeatedly (13+ entries by frame 353),
  frame count climbs to 2624 by 30,000,000 instructions — genuine
  progress, just never with `forced_blank` cleared or brightness raised.
  The GSU itself never restarts after its 18th STOP for the entire
  30,000,000-instruction span (`go=false` unchanged). **Open**: which
  cell/condition the main loop actually waits on to trigger GSU run #19
  or enable the display — not yet found. The cart-fact mismatch already
  on record (`GSU2_ROM_THRESHOLD_BYTES`'s strict `>` misdetects this
  real 1 MiB GSU2 title as GSU1, acknowledged in `SuperFxVersion`'s own
  doc) was NOT independently confirmed to matter this session: `$303B`
  VCR is never read anywhere in the traced boot (the 8 SFR-mirror reads
  above are the only register-window reads observed at all), so there is
  no evidence yet that the GSU1/GSU2 misdetection is what blocks this
  title, and the experiment to force GSU2 and re-check was not run given
  that absence of evidence.
- **Census, unchanged** (RELEASE, `boot_census_child` run individually
  against all 15 of W18-05's own tracked GSU archives, direct against the
  `.zip` files, plus all five canaries (3 of which overlap the fifteen,
  17 distinct archives run): every bucket matches
  W18-05's own recorded `1101/44/120/0/0` table exactly — no regression,
  matching the pre-session state exactly (this ticket shipped no
  production change). Full per-title exit codes: docs/TESTING.md's own
  W18-06 section.

### 3.8 Capcom Cx4 (ticket W19-02): register window only, commands undocumented

**Cx4 (Mega Man X2, Mega Man X3 — 2 games, `crates/rf-snes/src/cx4.rs`).**
A Hitachi HG51B169 RISC CPU (fullsnes "SNES Cart Capcom CX4"), architecturally
the same class of chip as SA-1/GSU: it runs a *program from the cartridge's
own SNES ROM* (`$7F49-$7F4B` Program ROM Base, e.g. `$02:8000` in Mega
Man), not an undisclosed internal firmware — so, like SA-1/GSU, an
instruction-level core is ordinary clean-room hardware emulation, not
HLE against copyrighted firmware.

**Detection (`rf-cart`).** Chipset `$F3` (coprocessor nibble `$F`
"custom", `hw=$3`) alone is not enough — fullsnes reuses nibble `$F` for
other custom chips — so `parse_snes_header` also requires the extended
header's `$FFBF` sub-type byte `=$10` (fullsnes "CX4 Cartridge Header":
`"[FFD6]=F3h"`, `"[FFBF]=10h ;CustomChip=CX4"`), the same two-field
disambiguation SA-1/GSU already apply to their own nibbles. A chipset-$F3
cart whose `$FFBF` names something else (e.g. ST010/ST011's `$01`) is
refused, not guessed.

**What is modelled — fullsnes documents this well enough to implement
directly, cited by section ("...I/O Ports"):**

- The fixed `$6000-$7FFF` window, banks `$00-$3F`/`$80-$BF`
  (`crate::mapping::cx4_target`, checked by `SnesBus::target` before the
  generic LoROM `map`, same layering as `sa1_target`/`gsu_target`): 3 KiB
  CX4RAM at `$6000-$6BFF`; the documented register/DMA/status/vector-
  shadow ports (`$7F40-$7F52`, `$7F5E`, `$7F6A-$7F6B`, `$7F6E-$7F6F`,
  `$7F80-$7FAF`); every other offset in `$6C00-$7FFF` is fullsnes's own
  "Unknown/unused" and falls through to open bus, unclaimed.
- The DMA transfer: `$7F40-$7F42` (24-bit LoROM source), `$7F43-$7F44`
  (byte length), `$7F45-$7F46` (CX4RAM destination), `$7F47` write `$00`
  triggers the one documented direction (SNES-to-CX4) — any other write
  value is a documented no-op, since fullsnes names no other encoding.
- The sixteen 24-bit general registers R0-R15 (`$7F80+N*3`, little-endian,
  masked to 24 bits on write) and the NMI/IRQ vector shadows
  (`$7F6A-$7F6B`/`$7F6E-$7F6F`).
- The CX4ROM: 1024 24-bit values from six documented closed-form tables
  (Div/Sqrt/Sin/Asin/Tan/Cos), including the Div(0)/Cos(0)
  overflow-truncation rule fullsnes states explicitly.
- The busy flag (`$7F5E` bit 6): fullsnes says it is set by a write to
  `$7F47`/`$7F48`/`$7F4F` and clears "when the command has completed" —
  with no program execution (below), there is no real completion event,
  so this project models the transition as synchronous (set, then
  immediately cleared within the same write) rather than leaving it stuck
  set, which would hang any title's poll loop. Stated as a stub for
  undocumented timing, not a claim of real hardware behaviour.

**What is undocumented, and why this ticket stops at the register
window** (per the ticket's own acceptance: "if the chapter documents only
the interface and not each command's algorithm... stop there"):

| Documented piece | Documentation quality |
|---|---|
| Register/DMA/status window | Full — implemented above |
| CX4ROM math tables | Full closed-form formulas — implemented above |
| Opcode encodings (all ~40) | Full bit patterns given |
| Opcode flag effects (N/Z/C) | Mostly `???`/unstated — only a handful of `<op>`-vs-`<imm>` variants get concrete letters |
| ROM/vertex byte-read sequence (`612Eh`/`4000h`/`1C00h`) | Fullsnes states outright: "the exact meaning... is unknown (which one does what part?)" |
| Two of eight `skip<cond>` conditions | `?` |
| ~12 opcodes (`0400h`, `1800h`, `2000h`, `3800h`, `4400h`, `5C00h`, `7400h`, `A000h`, `A400h`, `E400h`, `F400h`, `F800h`) | Reserved, no stated effect |
| `$7F48`/`$7F4C`/`$7F50-51`/`$7F52` | "Unknown" — `$7F48`'s own entry says its documented guess doesn't match how the real games use it |
| All opcode/DMA timings | "100% unknown" |
| The 26 named game-facing functions (`build_oam`, `scale_tiles`, `hires_sqrt`, `sqrt`, `propulsion`, `get_sin`, `get_cos`, `set_vector_length`, `triangle1`, `triangle2`, `pythagorean`, `arc_tan`, `trapeziod`, `multiply`, `transform_coordinates`, `scale_rotate1`, `transform_lines`, `scale_rotate2`, `draw_wireframe_without_clearing_buffer`, `draw_wireframe_with_clearing_buffer`, `disintergrate`, `wave`, plus the 4 `test_*` functions) | Name + entry address only (`0000:00`-`000E:89`) — **no register-level input/output semantics or algorithm for any of them**. `test_square` (`R1:R2=R0*R0`) and `test_set_r0_to_0Xh` are the only two given any stated behaviour at all, and even `test_square`'s is too thin (no operand width/sign rule) to implement with confidence rather than guess. |

An opcode-level interpreter would need to guess the flag model,
`finish ext_dta`'s exact split of work, the two unknown skip conditions
and the reserved opcodes' effects — diverging from the real program on
its first affected branch or ROM read. That is not "clean-room HLE of a
documented command", it is guessing relocated to the opcode level, which
NFR-011/law 5 forbid the same as guessing a whole function's algorithm.
**Consequence: neither Mega Man X2 nor X3's Cx4-driven effects render**
(wireframe intro, rotating boss sprites) — the SNES CPU runs the cartridge's
own code as normal, but nothing the CX4 would have computed for it ever
appears, since the CX4 never executes.

**Tests** (`crates/rf-snes/src/cx4.rs`, `crates/rf-cart/src/snes.rs`,
`crates/rf-snes/src/tests/system.rs`): every CX4ROM table entry against
hand-computed values from fullsnes's own formulas (exact for Div/Sqrt/Sin/
Cos's documented endpoints, tolerance-bounded for Asin/Tan given fullsnes's
own approximate domain notes); register read/write semantics (write-only
ports read 0, 24-bit register masking, vector-shadow round-trip); the DMA
transfer (documented direction only, wrapping destination); save/load
round-trip preserving every field; chipset+extended-header detection
(accepts `$F3`+`$FFBF=$10`, refuses `$F3` with any other sub-type, refuses
other nibble-`$F` hw values); and an end-to-end `SnesSystem::load` test
that writes the DMA ports through the ordinary bus and reads the
transferred bytes back out of CX4RAM.

### 3.7 OBC1 (OBJ Controller)

**OBC1 (1 game: Metal Combat: Falcon's Revenge, W19-01,
`crates/rf-snes/src/obc1.rs`).** Not a firmware coprocessor and not a
second CPU — a pure address remapper in front of the cartridge's own
8 KiB battery-backed SRAM, clean-room from fullsnes "SNES Cart OBC1
(OBJ Controller)" (NFR-011, no emulator source). Detection (`rf-cart`):
chipset $25 (coprocessor nibble $2, hw $5 — the only assigned
combination) accepted as `Coprocessor::Obc1`, always battery-backed.

- **Window** (`crate::mapping::obc1_target`): the whole `$6000-$7FFF`
  system-area window (banks `$00-$3F`/`$80-$BF` — the same convention
  SA-1/GSU/DSP-1 use for their own windows, since fullsnes gives OBC1
  no bank list of its own) is either ordinary SRAM or one of the eight
  `$7FF0-$7FF7` "OBC1 I/O Ports".
- **Register model** (`crate::obc1::Obc1Regs`): `$7FF0-$7FF3` (OAM
  Xloc/Yloc/Tile/Attr) redirect straight to the SRAM byte at
  `[Base+Index*4+0..3]` rather than being registers of their own —
  fullsnes calls them "totally useless": the byte they expose has no
  existence independent of the table cell it aliases. `$7FF4` (OAM
  Bits) redirects to `[Base+Index/4+200h]` with asymmetric R/W a plain
  SRAM cell cannot express: write is a 2-bit read-modify-write at
  `(Index AND 3)*2..+1`; read returns the whole raw byte, unshifted.
  `$7FF5` selects the 220h-byte table's base address (bit0: 0=$7C00,
  1=$7800 — the inverse of the usual "clear = first option"
  convention). `$7FF6` is the Index (OBJ number), 0..127, not
  auto-incremented. `$7FF7` ("Unknown, set to 00h or 0Ah") is stored
  and read back verbatim; nothing branches on it.
- **What is deliberately NOT modelled**: two fullsnes hedges ("Setting
  Index bits7+5 does reportedly enable SRAM mapping at 6000h..77FFh?"
  and "ROM is reportedly mapped to bank 00h..3Fh, and also to bank
  70h..71h?") and the read/write timing restrictions the chapter says
  `$7FF4` "may involve" — this build's read-modify-write is
  instantaneous within one bus access, same as every other register in
  this crate.
- **Save/load**: `StateRegion::Cart` gains the same presence-flag
  pattern SA-1/GSU use — no bulk buffer to add, since the 220h-byte
  table lives in `SnesBus::sram`, already saved.

### 3.9 S-DD1 (Data Decompressor)

**S-DD1 (2 games: Street Fighter Alpha 2, Star Ocean; this project's
library only carries the former; ticket W19-03, `crates/rf-snes/src/sdd1.rs`).**
No CPU of its own and no firmware — clean-room from fullsnes "SNES Cart
S-DD1 (Data Decompressor)" and "SNES Cart S-DD1 Decompression Algorithm"
(`fullsnes.txt:10628-10757`; NFR-011, no emulator source). Detection
(`rf-cart`): map mode `$22` ("LoROM/32K Banks + S-DD1") paired with
chipset `$43`/`$45` (coprocessor nibble `$4`, hw `{3,5}` — fullsnes's
"in practice" list gives only these two combinations), the same "both
must agree" D-013 shape SA-1's map mode `$23` already has — map mode
`$22` alone, or the chipset byte under plain LoROM, still refuses.

- **Registers** (`crate::sdd1::Sdd1Regs`, `$4800-$4807`): `$4800`
  DMA Enable 1 and `$4801` DMA Enable 2, one bit per DMA channel each —
  a channel decompresses only while BOTH name it
  (`channel_decompresses`); `$4801`'s bit self-clears once its DMA
  completes ("automatically cleared after DMA"), `$4800` does not
  ("unchanged after DMA"). `$4802`/`$4803` are fullsnes-hedged
  "Unknown" ports, stored and read back verbatim, never branched on
  (same convention as OBC1's `$7FF7`). `$4804-$4807` are the four 1 MiB
  ROM bank selects for the `$C0-$CF`/`$D0-$DF`/`$E0-$EF`/`$F0-$FF`
  groups. No reset values are documented; every register starts
  zeroed.
- **Mapping** (`crate::mapping::sdd1_target`): banks `$C0-$FF` are a
  bank-register-selected, HiROM-fashion view of the ROM —
  `banks[group] * 1Mi + (bank_within_group) * 0x10000 + offset`,
  modulo the ROM length — checked before the generic `map`, the same
  "coprocessor window wins" pattern SA-1/GSU/CX4/OBC1 already use.
  Everywhere else (including bank `$00`'s exception-handler window
  fullsnes calls out by name) is the cartridge's ordinary LoROM map,
  which `SnesMapMode::Sdd1`'s arm in `map` reproduces directly.
- **Decompression** (`crate::sdd1::Sdd1Decompressor`): a Golomb-coded
  adaptive bitplane decoder, transcribed function for function from the
  chapter's pseudocode — `decompress_init`'s header byte (top 2 bits
  select 2/4/8bpp or "linear" raw mode via `num_planes`; the next 2
  select the context-mixing constants), `GetBit`/`ProbGetBit`'s 32-state
  context model with its `EvolutionCodeSize`/`EvolutionMpsNext`/
  `EvolutionLpsNext` tables, `GetCodeword`'s run-length tables
  (`RunTable`), and `decompress_byte`'s bitplane interleave (the
  even/odd-plane toggle for 2/4/8bpp tile format, the flat 8-bits-per-
  byte path for linear mode). One documented oddity is transcribed
  literally rather than "corrected": `decompress_init`'s own indexing
  reads the byte one past where the header's second byte would be,
  which as literally written never consumes the header's own second
  byte — see `Sdd1Decompressor::init`'s doc for the exact citation. This
  is the clean-room mandate (implement the description, not a guessed
  intent) at its most visible.
- **DMA trigger** (`SnesBus::run_channel`): fullsnes's `<DMA>` row
  ("DMA from ROM returns Decompressed Data, originated at DMA start
  addr") is read as: a general-purpose DMA channel armed in both
  `$4800`/`$4801`, whose A-bus start address resolves into the S-DD1
  ROM window, gets a decompressor seeded ONCE at that address; every
  byte of the transfer comes from `next_byte`, never a second ROM read,
  regardless of how the visible A-bus register itself steps. HDMA is
  not intercepted — fullsnes only ever writes "DMA", and every known
  title's use is a one-shot general DMA streaming tiles/tilemaps.
- **What is deliberately NOT modelled**: `$4802`/`$4803` (fullsnes
  hedges both as "Unknown"); HDMA-sourced decompression (undocumented);
  Star Ocean's LN3B board's extra SRAM (this project's library does not
  carry the title).
- **Save/load**: `StateRegion::Cart` gains the same presence-flag
  pattern SA-1/GSU/CX4/OBC1 use for `Sdd1Regs`. The live decompressor
  itself is NOT part of machine state — a general-purpose DMA always
  runs to completion inside one `run_channel` call in this build, so no
  decompression is ever mid-flight at a frame boundary; its own
  save/load (used directly by `crate::sdd1`'s determinism test, not by
  production code) proves the state IS resumable in isolation, per the
  ticket's determinism/save-load requirement.

### 3.10 ST010 (SETA D96050CW-012, extended NEC uPD77C25) — one command documented, eight named-only

**ST010 (1 game: F1 Race of Champions / Exhaust Heat II, 1993, SETA Corp.;
ticket W19-04, `crates/rf-snes/src/st010.rs`).** Architecturally the same
class of chip as DSP-1 — a NEC uPD77C25-family firmware coprocessor whose
program ROM is never shipped in this tree (law 5, NFR-011) — but fullsnes's
own text says the battery-backed RAM, not any command, is what matters:
"the only feature that is <really> used is the battery-backed on-chip
RAM... the powerful chip is a waste of resources."

**Detection (`rf-cart`).** Chipset `$F6` ("ROM+Custom+Battery", coprocessor
nibble `$F`, `hw=$6`) alone is not enough — nibble `$F` also covers CX4
(`$FFBF=$10`) and SPC7110 (`$FFBF=$00`) — so `parse_snes_header` also
requires the extended header's `$FFBF` sub-type byte `=$01` (fullsnes
"DSPn/ST010/ST011 Cartridge Header": `"[FFD6h]=F6h"`, `"[FFBFh]=01h
Chipset Sub Type = ST010/ST011"`), the same two-field disambiguation CX4
already uses for its own nibble. Always battery-backed: `$F6`'s "in
practice" entry is `"ROM+Custom+Battery"`, no bare-`$F6` variant is
documented. Both directions of the mismatch are pinned by test: `$F6`
with a non-`$01` sub-type refuses, and `$F3` (CX4's own chipset) with
sub-type `$01` also refuses rather than being accepted as either chip.

**What the chapter actually documents — verified by a full-text grep, not
assumed.** fullsnes's "BIOS Functions" gives an "ST010 Commands" table
(`fullsnes.txt:9265-9284`) that promises "See individual commands for
input and output parameter addresses" — a promise this chapter never
keeps. A grep across the whole vendored `fullsnes.txt` for `ST010`,
`Driver Placements`, `0010h]` and `Sort Driver` finds nothing between that
table (ending "the only feature that is <really> used is the
battery-backed on-chip RAM") and an unrelated oscillator part number at
line 22770; "ST010 Commands" is immediately followed by "ST011 Commands"
and then the unrelated ST018 chapter. So the only command whose *effect*
is stated is `00h` ("Set RAM[0010h]=0000h"); `01h`/`04h` are explicitly
"Unknown Command"; `02h`/`03h`/`05h`/`06h`/`07h`/`08h` are bare names
("Sort Driver Placements", "2D Coordinate Scale", "Simulated Driver
Coordinate Calculation", "Multiply", "Raster Data Calculation", "2D
Coordinate Rotation") with no documented input address, output address,
width or sign rule for any of them.

**What is modelled — the RAM, the protocol, and command `00h`:**

- **Memory** (`crate::st010::St010`, `crate::mapping::st010_target`): the
  chip's whole on-chip RAM — fullsnes: `"the RAM is contained in the
  ST01n chip, and is sized 2Kx16bit, whereas the SNES accesses it as
  4Kx8bit (even addresses accessing the LSB, odd ones the MSB of the
  16bit words)"` — as one flat 4096-byte buffer, mapped (mirrored) across
  banks `$68-$6F`/`$E8-$EF`, offsets `$0000-$0FFF` (fullsnes memory-map
  overview: `"680000h-6FFFFFh ST010/ST011 On-chip Battery-backed RAM"`;
  per-board table `"68-6F:0000-0FFF (SRAM)"`).
- **Command protocol**: fullsnes "ST010 Commands": `"Commands are
  executed on the ST-0010 by writing the command to 0x0020 and setting
  bit7 of 0x0021. Bit7 of 0x0021 will stay set until the Command has
  completed."` Byte `$0020`/`$0021` is word index `$0010` of the RAM —
  the same word `00h`'s own effect targets — so this build treats the
  "command register" as a documented *use* of one RAM word, not separate
  hardware. A write to `$0021` with bit 7 set dispatches the byte at
  `$0020` synchronously and stores the busy byte already cleared — the
  same completion model §3.8 already states for the CX4's `$7F5E` bit 6
  ("set, then immediately cleared within the same write... rather than
  leaving it stuck set, which would hang any title's poll loop"); this
  project has no real timing to model either chip's busy window against.
  `09h-0Fh`/`10h-FFh` fold onto `00h-08h` exactly as fullsnes's mirror
  table states.
- **Command `00h`**: "Set RAM[0010h]=0000h" — read as a *byte* offset
  (zeroing `$0010`-`$0011`), matching every other address this chapter
  states in byte terms; the word-index alternative (zeroing the command
  word itself) is documented as an equally plausible reading in
  `crate::st010`'s module doc and pinned by a dedicated test, since
  nothing in the one census title this ticket covers distinguishes the
  two.
- **Commands `01h`-`08h`**: busy-clear only, each counted in
  `St010::command_counts` for visibility. This is not a partial
  implementation of `06h` "Multiply" — no operand address, width or sign
  rule is documented for it, and guessing one would mean writing invented
  results into the very buffer fullsnes calls the chip's one real
  feature. The same call is already on record for the CX4's 26
  named-but-unaddressed functions (§3.8): "guessing relocated to the
  opcode level" is not clean-room HLE.
- **The generic per-board summary table's separate `$60-$67:0000/0001`
  DR/SR pair** is mapped (`Target::St010Register`) so it does not
  silently become open bus, but kept inert: no documented behaviour
  distinguishes it from the RAM-hosted protocol above, and no command is
  reachable through it. Reads report open bus (no ST010-specific
  idle sentinel is documented, unlike DSP-1's `$80` or DSP-4's `$FFFF`,
  both stated for those chips only); writes are counted and dropped.
- **Save/load**: `StateRegion::Cart` gains the same presence-flag pattern
  SA-1/GSU/CX4/OBC1/S-DD1 use. The whole 4096-byte RAM (command/busy word
  included) is the entire payload — this build has no separate `.sav`
  persistence path for any coprocessor's battery-backed memory (SRAM
  included; confirmed by grep across `rf-harness/src` and
  `rf-snes/src/system.rs` — nothing exists to hook), so "battery-backed"
  means exactly what OBC1/CX4/S-DD1's own battery already means here:
  it rides in the ordinary save-state. Diagnostic counters
  (`command_counts`, `inert_register_accesses`) are not saved, the same
  "diagnostic, not machine state" contract `Obc1Regs::unknown_reg_other_writes`
  and `Dsp1::unknown_opcode_hist` already use.

**DSP-4 (Top Gear 3000) is unaffected and stays refused.** DSP-4's own
"BIOS Functions" entry (fullsnes) gives every command as `"xxh Unknown"`
except two test/version functions (`13h`, `14h`) — no game-facing command
has stated semantics, so there is nothing to HLE. DSP-4's chipset byte
(`$03`, coprocessor nibble `$0`) and this ticket's detection branch
(nibble `$F`) never overlap, so this ticket makes no code change on that
path; the census confirms Top Gear 3000 stays refused with the same exit
code as before this ticket.

**Tests** (`crates/rf-snes/src/st010.rs`, `crates/rf-snes/src/tests/mapping.rs`,
`crates/rf-snes/src/tests/system.rs`, `crates/rf-cart/src/snes.rs`): every
command's busy-clear-only or `00h`'s documented effect against
hand-computed RAM contents (including the "poison the buffer, assert only
the command/busy word changed" style for the eight undocumented commands);
the `09h-0Fh`/`10h-FFh` mirror folding; the RAM window's mirroring across
`$68-$6F` and into `$E8-$EF`, and the separate inert `$60-$67:0000/0001`
pair; save/load round-tripping the RAM but not the diagnostic counters;
chipset+extended-header detection (accepts `$F6`+`$FFBF=$01`, refuses
`$F6` with any other sub-type, refuses `$F3`+`$FFBF=$01` as neither chip);
and an end-to-end `SnesSystem::load` test that writes a parameter byte,
issues command `00h` through the ordinary bus, and observes busy clear.

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
