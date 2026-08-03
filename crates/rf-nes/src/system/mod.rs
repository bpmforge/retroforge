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
//! ## Memory map (nesdev.org/wiki/CPU_memory_map)
//!
//! | Range | Contents |
//! |---|---|
//! | `$0000-$07FF` | 2 KiB internal RAM |
//! | `$0800-$1FFF` | mirrors of `$0000-$07FF`, every `$0800` |
//! | `$2000-$2007` | PPU registers (stub — see [`ppu_stub`]) |
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
mod ppu_stub;

#[cfg(test)]
mod tests;

pub use cartridge::{NesLoadError, NesRom};
pub use controller::Controller;
pub use ppu_stub::PpuStub;

use crate::cpu::CpuBus;
use rf_cart::NesHeader;

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
    ppu: PpuStub,
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
        NesBus {
            master_cycle: 0,
            ram: [0; RAM_SIZE],
            open_bus: 0,
            ppu: PpuStub::new(),
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
            0x2000..=0x3FFF => self.ppu.read((addr & 0x0007) as u8, self.open_bus),
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
            0x2000..=0x3FFF => self.ppu.write((addr & 0x0007) as u8, value),
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
    /// phase alignment) — an internal bookkeeping convention (there is no
    /// external reset-sequence anchor tying our `master_cycle` phase to
    /// real hardware's yet), not a claim about a specific absolute cycle
    /// number; what matters, and what the acceptance tests check, is that
    /// the two cases differ by exactly one cycle. Concretely,
    /// `master_cycle` starts at 0 in `NesBus::new` with no reset sequence
    /// run yet (out of this ticket's scope — see the `system` module doc
    /// and `crate::cpu::mod`'s `Default for Cpu` doc), so today the parity
    /// a real ROM sees depends on however many bus ops happened before its
    /// first `$4014` write; this is the one place a real ROM's DMA cost
    /// could disagree with this model by exactly one cycle until a reset
    /// sequence anchors the phase (W1-03+).
    ///
    /// Each of the 256 "get" reads below also updates `open_bus` (via
    /// `read_untimed`) to that source byte, same as a real CPU read would
    /// — so after a DMA, `open_bus` holds the *last PRG/RAM byte DMA
    /// fetched*, not the `$XX` page value that triggered it.
    fn run_oam_dma(&mut self, page: u8, start_cycle_odd: bool) -> u32 {
        let mut stall = 0u32;

        // 1 dummy read cycle while the DMA controller takes over the bus.
        self.master_cycle += 1;
        stall += 1;

        // +1 extra alignment cycle if the write that triggered this landed
        // on an odd cycle, so the first "get" cycle below lines up on an
        // even one (the "get/put alignment" EMULATION_CORES.md §2.5 cites).
        if start_cycle_odd {
            self.master_cycle += 1;
            stall += 1;
        }

        for i in 0..256u16 {
            let src = ((page as u16) << 8) | i;
            // "get": read one byte from the source page. Uses the
            // non-ticking internal read plus a manual tick, deliberately
            // *not* `CpuBus::read`, so this loop's 256 cycles are counted
            // exactly once each (see `read_untimed`'s doc).
            let byte = self.read_untimed(src);
            self.master_cycle += 1;
            stall += 1;

            // "put": write the byte into OAM via the OAMDATA path.
            self.ppu.write_oam_data(byte);
            self.master_cycle += 1;
            stall += 1;
        }

        stall
    }
}

impl CpuBus for NesBus {
    fn read(&mut self, addr: u16) -> u8 {
        let value = self.read_untimed(addr);
        self.master_cycle += 1;
        value
    }

    fn write(&mut self, addr: u16, value: u8) {
        if addr == 0x4014 {
            let start_cycle_odd = self.master_cycle % 2 == 1;
            self.open_bus = value;
            self.master_cycle += 1; // the $4014 write's own cycle
            let stall = self.run_oam_dma(value, start_cycle_odd);
            self.last_oam_dma_stall = Some(stall);
            return;
        }
        self.write_untimed(addr, value);
        self.master_cycle += 1;
    }

    // `nmi_line`/`irq_line` intentionally left at the `CpuBus` trait's
    // default (`false`): neither the PPU nor APU stub above has any
    // ability to assert an interrupt yet (both are pure register stubs).
    // A later ticket overrides these once the PPU has a real VBlank/NMI
    // output and the APU has a real frame-counter/DMC IRQ.
}
