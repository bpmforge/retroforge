//! The NES system bus (ticket W1-02): memory map, NROM cartridge space,
//! OAM DMA, and controller strobe — the `CpuBus` a real console boots
//! against, as opposed to the CPU crate's own unit-test mocks
//! (`crate::cpu::bus`).
//!
//! ## The master-clock seam (`docs/design/EMULATION_CORES.md` §1)
//!
//! [`Cpu::step`](crate::Cpu::step) is instruction-granular: it issues every
//! bus cycle in hardware order but returns only after a whole instruction.
//! Per the architecture doc, "CPU is the master clock... each chip keeps
//! its own cycle counter", so [`NesBus`] — not `Cpu` — owns
//! [`NesBus::master_cycle`], and advances it *inside* [`CpuBus::read`]/
//! [`CpuBus::write`], one tick per real bus operation. This is what lets
//! OAM DMA's stolen cycles (see `run_oam_dma` below) land in the master
//! clock for free: the DMA triggered by a `$4014` write ticks the clock
//! hundreds of times from *inside* that one `write` call, entirely
//! invisible to `Cpu::step`, which only ever sees the single write cycle
//! its `STA $4014` instruction issued. Nothing here ever feeds cycles back
//! into `Cpu` — if it needed to, this seam would have failed.
//!
//! ## The PPU tick seam (ticket W1-04a)
//!
//! `docs/design/EMULATION_CORES.md` §1 calls for the PPU to run either
//! catch-up-scheduled or "in lock-step per CPU cycle" in Accuracy mode;
//! this crate builds lock-step only (see [`crate::ppu`]'s module doc for
//! why, and the debt that choice owes a later ticket). Lock-step means the
//! real [`crate::ppu::Ppu`] must advance exactly 3 dots for every one
//! master cycle the bus itself advances — so `tick_master` (below), the one
//! private helper every `master_cycle` increment in this file now goes
//! through, does both in one place: `self.master_cycle += cycles` and
//! `self.ppu.tick()` x3 per cycle. This preserves the master-clock seam
//! above unchanged (`master_cycle` still only ever changes inside this
//! file, at the same call sites as before) while guaranteeing the PPU can
//! never drift out of lock-step with it, including during OAM DMA's
//! stolen cycles (`run_oam_dma` ticks the PPU right along with them, which
//! is correct hardware behavior — DMA doesn't pause the PPU).
//!
//! ## Memory map (nesdev.org/wiki/CPU_memory_map)
//!
//! | Range | Contents |
//! |---|---|
//! | `$0000-$07FF` | 2 KiB internal RAM |
//! | `$0800-$1FFF` | mirrors of `$0000-$07FF`, every `$0800` |
//! | `$2000-$2007` | PPU registers (real 2C02 — see [`crate::ppu`]) |
//! | `$2008-$3FFF` | mirrors of `$2000-$2007`, every `$0008` |
//! | `$4000-$4013` | APU registers (stub — dropped/open-bus) |
//! | `$4014` | OAM DMA trigger |
//! | `$4015` | APU status (stub) |
//! | `$4016` | Controller 1 read / both controllers' strobe write |
//! | `$4017` | Controller 2 read / APU frame counter write (stub) |
//! | `$4018-$401F` | normally-disabled APU/IO test registers (stub) |
//! | `$4020-$5FFF` | unmapped cartridge expansion (open bus) |
//! | `$6000-$7FFF` | cartridge PRG RAM (always backed, 8 KiB) |
//! | `$8000-$FFFF` | cartridge PRG ROM (NROM: mirrored/mapped, see `read_prg`) |
//!
//! ## Mapper scope (`docs/design/EMULATION_CORES.md` §2.4)
//!
//! §2.4 sketches a general `Mapper` trait (`cpu_read`/`cpu_write`/
//! `ppu_read`/`ppu_a12`/`state_chunk`/...) for the *eventual* multi-mapper
//! system. This ticket implements exactly one mapper (NROM, mapper 0,
//! which has no bank registers, no PPU-side behavior, and no IRQ), and no
//! other mapper ticket exists on the board yet (`plan.json`'s W1-* set
//! stops at W1-07). Building out `MapperBus`/`BusValue`/`MapperState` —
//! none of which exist anywhere in this crate yet — for a single
//! zero-register mapper would be speculative scaffolding with no second
//! implementor to validate it against, so NROM's PRG read/mirroring logic
//! is inlined directly in this module (`read_prg`) instead. The next
//! mapper ticket is the right place to extract a trait, informed by an
//! actual second mapper's needs.
//!
//! ## Open bus
//!
//! Real NES open bus is the last byte value that was actually driven onto
//! the shared data bus, which decays over time and is not perfectly
//! deterministic on real hardware. This bus models the simple, common
//! subset: [`NesBus::open_bus`] is the last byte *this emulation* actually
//! drove (every real RAM/ROM/PRG-RAM access and every write updates it);
//! reads from undriven stub registers return it unchanged. We do **not**
//! model the address-high-byte quirk that makes real `LDA $4016` typically
//! read back `$40` specifically (nesdev.org/wiki/Controller_reading
//! mentions this as a commonly-observed value, not a guaranteed one) — our
//! controller reads combine the shift-register bit with whatever the
//! shared latch currently holds, which is simpler, fully deterministic,
//! and correct for the one bit every acceptance test actually checks (D0).
mod cartridge;
mod controller;

#[cfg(test)]
mod tests;

pub use cartridge::{NesLoadError, NesRom};
pub use controller::Controller;

use crate::cpu::CpuBus;
use crate::ppu::Ppu;
use rf_cart::NesHeader;
use rf_core_api::CoreSink;

const RAM_SIZE: usize = 0x0800;
const PRG_RAM_SIZE: usize = 0x2000;

/// The NES system bus: RAM, PPU/APU register stubs, controllers, and NROM
/// cartridge space, wired together as one [`CpuBus`] implementor. See the
/// module doc for the memory map and the master-clock seam this exists to
/// prove out.
pub struct NesBus {
    master_cycle: u64,
    ram: [u8; RAM_SIZE],
    open_bus: u8,
    ppu: Ppu,
    controllers: [Controller; 2],
    prg_ram: [u8; PRG_RAM_SIZE],
    rom: NesRom,
    /// Stall length (513 or 514) of the most recently completed OAM DMA,
    /// *not counting* the `$4014` write's own bus cycle (see
    /// `run_oam_dma`'s doc for the exact accounting). `None` until the
    /// first DMA runs.
    last_oam_dma_stall: Option<u32>,
}

impl NesBus {
    /// Build a bus from an already-loaded [`NesRom`] (mapper 0 / NROM
    /// only — see [`NesRom::from_ines_bytes`]).
    pub fn new(rom: NesRom) -> Self {
        // Built from `rom`'s own CHR bytes/mirroring before `rom` moves
        // into the struct below — no `Mapper` trait exists yet (mapper 0
        // has no CHR banking), so the PPU just owns a flat copy.
        let ppu = Ppu::new(
            rom.chr_rom().to_vec(),
            rom.chr_is_ram(),
            rom.header().mirroring,
        );
        NesBus {
            master_cycle: 0,
            ram: [0; RAM_SIZE],
            open_bus: 0,
            ppu,
            controllers: [Controller::new(), Controller::new()],
            prg_ram: [0; PRG_RAM_SIZE],
            rom,
            last_oam_dma_stall: None,
        }
    }

    /// Parse `raw` as an iNES/NES 2.0 image and build a bus from it in one
    /// step (acceptance criterion 2: "NROM loads via rf-cart").
    pub fn from_ines_bytes(raw: &[u8]) -> Result<Self, NesLoadError> {
        Ok(Self::new(NesRom::from_ines_bytes(raw)?))
    }

    /// Total bus cycles elapsed since this bus was created — the master
    /// clock (see module doc). Every [`CpuBus::read`]/[`CpuBus::write`]
    /// advances this by at least 1; OAM DMA advances it by hundreds more
    /// from inside a single `write` call.
    pub fn master_cycle(&self) -> u64 {
        self.master_cycle
    }

    /// The parsed header of the loaded cartridge.
    pub fn rom(&self) -> &NesHeader {
        self.rom.header()
    }

    /// The 256-byte OAM as last written (by CPU register writes or OAM
    /// DMA) — for tests and, eventually, the PPU's sprite evaluation.
    pub fn oam(&self) -> &[u8; 256] {
        self.ppu.oam()
    }

    /// Flush every completed-but-undrained scanline the PPU has produced
    /// through `sink` (acceptance criterion 3: "indexed pixels + metadata
    /// emitted via CoreSink") — see [`crate::ppu`]'s module doc for why
    /// this is a separate call rather than something `CpuBus::read`/`write`
    /// do inline. No `EmulatorCore` exists in this crate yet to call this
    /// once per frame automatically (out of this ticket's write scope);
    /// callers (today: tests) drive it directly.
    pub fn drain_video(&mut self, sink: &mut dyn CoreSink) {
        self.ppu.drain(sink);
    }

    /// Stall length (513 or 514) of the most recently completed OAM DMA,
    /// counted the way nesdev describes it: the number of cycles the CPU
    /// is halted for, *after* the `$4014` write's own bus cycle has
    /// already elapsed as an ordinary part of the `STA` instruction that
    /// performed it. `None` if no DMA has run yet.
    pub fn last_oam_dma_stall(&self) -> Option<u32> {
        self.last_oam_dma_stall
    }

    /// Host-side input hook: set port `index`'s (0 or 1) live button byte.
    /// Bit layout: 0=A, 1=B, 2=Select, 3=Start, 4=Up, 5=Down, 6=Left,
    /// 7=Right (see `controller` module doc).
    pub fn set_controller_buttons(&mut self, index: usize, buttons: u8) {
        self.controllers[index].set_buttons(buttons);
    }

    /// One CPU-visible read with no `master_cycle` side effect (it still
    /// updates `open_bus`, exactly like a real read would) — used
    /// internally by OAM DMA, which issues its own 256 "get" cycles by
    /// hand rather than recursing into [`CpuBus::read`] (module doc: OAM
    /// DMA's cycles must be counted exactly once). Callers other than
    /// `CpuBus::read`/`run_oam_dma` MUST tick `master_cycle` themselves —
    /// nothing enforces that mechanically, so a future ticket reusing this
    /// (e.g. DMC DMA) needs to preserve the same discipline.
    fn read_untimed(&mut self, addr: u16) -> u8 {
        let value = match addr {
            0x0000..=0x1FFF => self.ram[(addr as usize) & (RAM_SIZE - 1)],
            0x2000..=0x3FFF => self.ppu.read_register((addr & 0x0007) as u8, self.open_bus),
            0x4016 => self.controllers[0].read_bit() | (self.open_bus & !0x01),
            0x4017 => self.controllers[1].read_bit() | (self.open_bus & !0x01),
            0x4000..=0x4015 | 0x4018..=0x401F => self.open_bus,
            0x4020..=0x5FFF => self.open_bus,
            0x6000..=0x7FFF => self.prg_ram[(addr as usize) - 0x6000],
            0x8000..=0xFFFF => self.read_prg(addr),
        };
        self.open_bus = value;
        value
    }

    /// Side-effect-free, cycle-free memory read (ticket W1-03): never
    /// advances `master_cycle`, never mutates controller shift-register
    /// state (`Controller::peek_bit`, not `read_bit`), never updates
    /// `open_bus`. Exists only for `crate::trace`'s disassembly-annotation
    /// column — nestest.log's `@ 80 = 0200 = 5A`-style operand annotations
    /// are computed by peeking memory *before* the traced instruction
    /// executes, and must never perturb the very state the trace is
    /// describing. This is not a general emulation primitive: every real
    /// CPU access must go through [`CpuBus::read`]/[`CpuBus::write`]
    /// instead.
    ///
    /// ## `$4000-$4015`/`$4018-$401F` disassemble as `$FF`, not tracked
    /// `open_bus` (ticket W1-03 finding)
    ///
    /// `read_untimed` (the *real* emulation path, used by
    /// [`CpuBus::read`]) still returns the tracked `open_bus` latch for
    /// this range, unchanged. This peek-only override is narrower, and its
    /// necessity is directly **verified** for exactly five addresses
    /// against the real, fetched `nestest.log`: its last few lines
    /// disassemble `STA` to `$4004`, `$4005`, `$4006`, `$4007` (write-only
    /// APU pulse2 registers — no circuit ever drives them back onto the
    /// bus) and `$4015` (APU status; nesdev.org/wiki/APU: *"This register
    /// is internal to the CPU, so the external CPU data bus is
    /// disconnected when reading it"* — i.e. a real `$4015` read cannot be
    /// `open_bus` passthrough at all) all as `= FF`, regardless of the
    /// actual preceding bus traffic (verified: the immediately-preceding
    /// instructions in that sequence are ordinary immediate-mode ROM
    /// fetches whose values do **not** match `FF`, so this is not our
    /// `open_bus` model coincidentally agreeing with real hardware). No
    /// combination of real emulated APU state or open-bus tracking
    /// produces `FF` from either of those two independent causes at that
    /// point — the parsimonious explanation, and the one that makes every
    /// one of nestest.log's 8991 lines byte-exact, is that nestest's own
    /// disassembler (like many generic 6502 disassemblers) prints a fixed
    /// `$FF` placeholder for any address it cannot resolve to real
    /// backing memory (RAM/ROM/PRG-RAM), rather than attempting to model
    /// MMIO/open-bus content at disassembly time. This crate has no APU
    /// (out of scope, a later ticket) to model `$4015`'s real status bits
    /// correctly anyway, so matching that same placeholder convention
    /// here — for the disassembly annotation only, never for real
    /// emulation — is the honest fix rather than a curve-fit.
    ///
    /// The rest of this arm's range — `$4000`-`$4013` (the other APU
    /// write-only registers), `$4014` (the OAM DMA trigger, also
    /// write-only), and `$4018`-`$401F` (disabled test registers) — is
    /// **extrapolated** by the same reasoning (stubbed, unreadable,
    /// nothing ever drives them back), not independently confirmed against
    /// the log: nestest.log never disassembles a read of any of those
    /// specific addresses. `$4016`/`$4017` (the controller ports) are
    /// deliberately *not* included here and stay on `open_bus` passthrough
    /// (combined with [`Controller::peek_bit`]) — nestest never reads a
    /// controller either, so there is no golden-log evidence either way
    /// for them, and passthrough is the conservative default absent a
    /// reason to change it.
    ///
    /// ## PPU registers (ticket W1-04a)
    ///
    /// [`Ppu::read_register`] has real side effects for `$2002`/`$2007`
    /// (clearing the vblank flag/`w` latch, advancing the delayed-read
    /// buffer/`v`) — using it here would violate this method's own
    /// no-side-effects contract. [`Ppu::peek_register`] is the
    /// side-effect-free counterpart (same convention as
    /// [`Controller::peek_bit`] vs. `read_bit` above). nestest never
    /// touches `$2000-$3FFF` at all (verified against the real fetched
    /// `nestest.log`: zero lines disassemble an operand in that range), so
    /// this arm's correctness has no golden-trace coverage either way —
    /// see `crate::ppu`'s own test suite for its behavior instead.
    pub fn peek(&self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x1FFF => self.ram[(addr as usize) & (RAM_SIZE - 1)],
            0x2000..=0x3FFF => self.ppu.peek_register((addr & 0x0007) as u8, self.open_bus),
            0x4016 => self.controllers[0].peek_bit() | (self.open_bus & !0x01),
            0x4017 => self.controllers[1].peek_bit() | (self.open_bus & !0x01),
            0x4000..=0x4015 | 0x4018..=0x401F => 0xFF,
            0x4020..=0x5FFF => self.open_bus,
            0x6000..=0x7FFF => self.prg_ram[(addr as usize) - 0x6000],
            0x8000..=0xFFFF => self.read_prg(addr),
        }
    }

    /// NROM PRG read: a 16 KiB image is mirrored into both `$8000-$BFFF`
    /// and `$C000-$FFFF`; a 32 KiB image is mapped straight through.
    /// `% prg_rom.len()` implements both in one line since 16 KiB and
    /// 32 KiB both evenly divide the 32 KiB `$8000-$FFFF` window.
    fn read_prg(&self, addr: u16) -> u8 {
        let prg = self.rom.prg_rom();
        debug_assert!(!prg.is_empty(), "NROM image with empty PRG ROM");
        let offset = (addr as usize - 0x8000) % prg.len();
        prg[offset]
    }

    /// One CPU-visible write with no `master_cycle` side effect (see
    /// `read_untimed`'s doc — the same reasoning, and the same
    /// caller-must-tick obligation, applies to OAM DMA's "put" cycles).
    fn write_untimed(&mut self, addr: u16, value: u8) {
        self.open_bus = value;
        match addr {
            0x0000..=0x1FFF => self.ram[(addr as usize) & (RAM_SIZE - 1)] = value,
            0x2000..=0x3FFF => self.ppu.write_register((addr & 0x0007) as u8, value),
            0x4016 => {
                // Both controllers share the one strobe line (nesdev.org/
                // wiki/Standard_controller): a $4016 write reaches both.
                self.controllers[0].write_strobe(value);
                self.controllers[1].write_strobe(value);
            }
            0x4014 => unreachable!("intercepted in CpuBus::write before reaching here"),
            // $4017 write is the APU frame counter register, not a
            // controller register — stub, dropped. $4000-4013/4015 are
            // the other APU registers, and $4018-401F the disabled test
            // registers; all stub/dropped.
            0x4000..=0x4013 | 0x4015 | 0x4017..=0x401F => {}
            0x4020..=0x5FFF => {}
            0x6000..=0x7FFF => self.prg_ram[(addr as usize) - 0x6000] = value,
            // NROM has no mapper registers; PRG ROM writes have no effect.
            0x8000..=0xFFFF => {}
        }
    }

    /// OAM DMA (`$4014`): copies 256 bytes from `$XX00-$XXFF` (`page =
    /// $XX`) into OAM through the same path as 256 `OAMDATA` writes
    /// (nesdev.org/wiki/DMA), honoring and advancing whatever `OAMADDR`
    /// already holds.
    ///
    /// Cost, exactly as nesdev.org/wiki/DMA describes it and exactly as
    /// this method's own tick-counting proves: **513 or 514 cycles**,
    /// counted as `1 dummy alignment read + (0 or 1) extra get/put
    /// alignment cycle + 256 get/put pairs (512)`. This total does *not*
    /// include the `$4014` write's own bus cycle — that already elapsed
    /// as an ordinary cycle of the `STA $4014` instruction before the CPU
    /// is halted (see `CpuBus::write`'s call site: it ticks once for the
    /// write, *then* calls this). `start_cycle_odd` is whether the
    /// `$4014` write itself landed on an odd `master_cycle` (get/put
    /// phase alignment) — an internal bookkeeping convention when no reset
    /// has run, not a claim about a specific absolute cycle number; what
    /// matters, and what the acceptance tests check, is that the two cases
    /// differ by exactly one cycle. Concretely, `master_cycle` starts at 0
    /// in `NesBus::new`, so a caller that skips `Cpu::power_on` (ticket
    /// W1-03) and sets `cpu.pc` directly — as this crate's own bus-level
    /// unit tests still do — sees a parity that depends on however many
    /// bus ops happened before the first `$4014` write; this is the one
    /// place such a caller's DMA cost could disagree with a real console
    /// by exactly one cycle. A caller that runs `Cpu::power_on` first
    /// (which burns exactly 7 cycles, an odd count) gets the phase
    /// anchored to hardware from then on.
    ///
    /// Each of the 256 "get" reads below also updates `open_bus` (via
    /// `read_untimed`) to that source byte, same as a real CPU read would
    /// — so after a DMA, `open_bus` holds the *last PRG/RAM byte DMA
    /// fetched*, not the `$XX` page value that triggered it.
    fn run_oam_dma(&mut self, page: u8, start_cycle_odd: bool) -> u32 {
        let mut stall = 0u32;

        // 1 dummy read cycle while the DMA controller takes over the bus.
        self.tick_master(1);
        stall += 1;

        // +1 extra alignment cycle if the write that triggered this landed
        // on an odd cycle, so the first "get" cycle below lines up on an
        // even one (the "get/put alignment" EMULATION_CORES.md §2.5 cites).
        if start_cycle_odd {
            self.tick_master(1);
            stall += 1;
        }

        for i in 0..256u16 {
            let src = ((page as u16) << 8) | i;
            // "get": read one byte from the source page. Uses the
            // non-ticking internal read plus a manual tick, deliberately
            // *not* `CpuBus::read`, so this loop's 256 cycles are counted
            // exactly once each (see `read_untimed`'s doc).
            let byte = self.read_untimed(src);
            self.tick_master(1);
            stall += 1;

            // "put": write the byte into OAM via the OAMDATA path.
            self.ppu.write_oam_data(byte);
            self.tick_master(1);
            stall += 1;
        }

        stall
    }

    /// The single place `master_cycle` is ever mutated (module doc's "PPU
    /// tick seam"): advances the master clock by `cycles` and ticks the PPU
    /// exactly `3 * cycles` dots, keeping it in permanent lock-step with
    /// the bus regardless of which call site (a plain read/write or one of
    /// `run_oam_dma`'s stolen cycles) is advancing time.
    fn tick_master(&mut self, cycles: u32) {
        self.master_cycle += cycles as u64;
        for _ in 0..cycles * 3 {
            self.ppu.tick();
        }
    }
}

impl CpuBus for NesBus {
    fn read(&mut self, addr: u16) -> u8 {
        let value = self.read_untimed(addr);
        self.tick_master(1);
        value
    }

    fn write(&mut self, addr: u16, value: u8) {
        if addr == 0x4014 {
            let start_cycle_odd = self.master_cycle % 2 == 1;
            self.open_bus = value;
            self.tick_master(1); // the $4014 write's own cycle
            let stall = self.run_oam_dma(value, start_cycle_odd);
            self.last_oam_dma_stall = Some(stall);
            return;
        }
        self.write_untimed(addr, value);
        self.tick_master(1);
    }

    // `nmi_line`/`irq_line` intentionally left at the `CpuBus` trait's
    // default (`false`): the PPU now has real vblank/NMI-enable state
    // (`$2000` bit 7, `$2002` bit 7), but nothing connects it to this trait
    // method yet — the exact VBlank/NMI edge-suppression timing
    // (`ppu_vbl_nmi`) is a later ticket's acceptance criterion, not this
    // one's, and a half-modeled NMI with no test-ROM oracle to check it
    // against would risk silently perturbing every ROM this crate runs
    // (see `crate::ppu`'s module doc scope fence). The APU stub still has
    // no interrupt source either. A later ticket overrides this once both
    // are ready.
}
