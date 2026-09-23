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
  game-identity research).
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

### 3.6 Super FX (GSU) — slice 2 of 5

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
- **PLOT/RPIX** are recording no-ops (`Gsu::plot_calls`/`rpix_calls`,
  diagnostic, not saved) — PLOT still advances R1 per fullsnes ("Pixel=
  COLR, R1=R1+1") without touching RAM; RPIX returns a deterministic `0`.
  **COLOR/CMODE are real**, not stubbed — their documented effect is a
  plain register write (`COLR`/`POR`), not the pixel path, so they are
  implemented in full; only the pixel-cache/bitmap-RAM side (W18-03) is
  deferred.
- **Provisional scheduling**: `GsuState::run` executes up to
  `STEP_BUDGET` (`64`, not a hardware constant) opcodes per
  `SnesSystem::step` while GO is set, with **no** cycle cost charged
  against the master clock — `Gsu::last_cost` records each opcode's
  documented clock count (fullsnes "CPU Misc") for slice 4 to consume,
  but nothing reads it yet. Wired into `SnesSystem::step` right after the
  SA-1 credit loop, borrowing `&bus.rom` the same disjoint-field way.
  Law 8: the budget counter decrements unconditionally, first, every
  iteration, so a mis-encoded program cannot hang the loop.
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
