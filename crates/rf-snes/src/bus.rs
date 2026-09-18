//! The SNES bus (ticket W6-02a; FR-CORE-035).
//!
//! Owns the machine's memory and registers, and implements
//! [`crate::cpu::CpuBus`] so the 65816 verified in W6-01a/b can run
//! against a real machine instead of a flat array.
//!
//! ## `read` vs `peek`
//!
//! Several registers here have read side effects — `$4211` acknowledges
//! the timer IRQ, and the WRAM port auto-increments. [`SnesBus::peek`]
//! therefore does **not** route to them: it is what a debugger, tracer or
//! save-state inspector uses, and it must be able to look at any address
//! without changing the machine. This is the contract `CpuBus::peek`
//! spells out, and the trap it warns about (a trace logger perturbing the
//! machine it is tracing) is live from this ticket onward, because before
//! it there were no side-effecting reads to get wrong.

use rf_cart::SnesMapMode;

use crate::apu::Apu;
use crate::cpu::CpuBus;
use crate::dma::{Dma, CYCLES_PER_BYTE, CYCLES_PER_CHANNEL};
use crate::mapping::{map, Target, WRAM_LEN};
use crate::ppu::Ppu;
use crate::regs::{IrqTimer, MathUnit, NmiTimen, WramPort};
use crate::timing::{Joypads, Timing};

/// Diagnostic-only DSP-1 DR-drain trace (ticket W16-11).
///
/// Counts DR (data register) accesses by how they were driven, so a probe
/// can report whether a title drains Raster (`0Ah`/`1Ah`) output by CPU
/// polling, general-purpose DMA, or HDMA — the question W16-11's
/// acceptance #1 asks. **Not part of save state**, the same "diagnostic,
/// unsaved" contract [`crate::dsp1::Dsp1`]'s `unknown_opcode_hist` field
/// documents: a save/load round trip losing these counts changes nothing
/// about emulated behaviour, only what a probe run across it would
/// report.
#[derive(Debug, Clone, Copy, Default)]
pub struct Dsp1DrTrace {
    /// DR reads made by the CPU (neither MDMA nor HDMA in progress) while
    /// a Raster stream was live.
    pub cpu_raster_reads: u32,
    /// DR reads made by general-purpose DMA (`$420B`-triggered).
    pub mdma_reads: u32,
    /// DR reads made by HDMA.
    pub hdma_reads: u32,
    /// General-DMA channel runs whose A-bus address fell inside the
    /// DSP-1 window at the moment `$420B` triggered them.
    pub mdma_channel_starts: u32,
    /// Of those, how many used fixed-address mode (`$43x0` bit 3) — the
    /// A-bus does not advance, so every byte re-reads the same nominal
    /// address (the DSP-1's own state machine still advances the DR).
    pub mdma_channel_starts_fixed: u32,
    /// HDMA transfer units (one per active channel per active line)
    /// whose source address fell inside the DSP-1 window.
    pub hdma_units: u32,
}

/// The machine's memory, cartridge and registers.
pub struct SnesBus {
    pub rom: Vec<u8>,
    pub sram: Vec<u8>,
    pub wram: Vec<u8>,
    /// `$2116`/`$2117` VMADD — a WORD address into the PPU's VRAM.
    ///
    /// The port lives here (W6-02b built it as a memory-mapped register)
    /// but the STORAGE belongs to the PPU. Keeping a second buffer on the
    /// bus would have been a silent disaster: writes would land here
    /// while rendering read `ppu.vram`, so every game would draw a black
    /// screen with nothing obviously wrong anywhere.
    pub vram_address: u16,
    /// `$2115` VMAIN — increment step and which port write advances it.
    pub vmain: u8,
    pub mode: SnesMapMode,
    pub math: MathUnit,
    pub nmitimen: NmiTimen,
    pub irq: IrqTimer,
    pub wram_port: WramPort,
    pub dma: Dma,
    /// `$420D` bit 0 — feeds [`crate::cpu::access_cycles`].
    pub fast_rom: bool,
    /// Last value driven on the bus, returned for unmapped reads. Open
    /// bus is not zero, and a game that reads an unmapped address sees
    /// this; returning 0 would be a different (wrong) answer that happens
    /// to look tidier.
    pub open_bus: u8,
    pub ppu: Ppu,
    pub apu: Apu,
    /// Master cycles the APU still owes.
    ///
    /// **The APU is never free-running** (§3.4). It is caught up
    /// immediately before any `$2140`-`$2143` access, so the CPU can only
    /// ever observe a port state the APU has actually reached. Letting it
    /// run ahead on its own clock and sampling whatever it had got to is
    /// how port handshakes become timing-dependent and games become
    /// flaky on some runs and not others.
    pub apu_debt: u64,
    /// SPC cycles the APU has already run AHEAD of its debt (ticket
    /// W14-09). An SPC700 instruction is atomic here and costs 2-12
    /// cycles, so a call owed one cycle can spend six; the surplus is
    /// paid off by the next calls before they run anything. This used to
    /// be folded back into `apu_debt` with a saturating subtraction —
    /// which, once the debt was settled every instruction, forgave the
    /// surplus every time and ran the SPC700 at one instruction per owed
    /// cycle, 4.4x too fast (measured by the test that guards this).
    pub apu_overspent: u64,
    /// `$2137`/`$213C`/`$213D`/`$213F`/`$4201` H/V counter latch (W14-10).
    pub hv: crate::regs::HvLatch,
    /// True while an HDMA unit is transferring (ticket W7-15).
    ///
    /// **HDMA runs in HBLANK, before the line it configures is drawn**, so
    /// its writes are line SETUP and must never be attributed as mid-line
    /// events — `Ppu::latch_line` is what carries them, and it runs right
    /// after. Without this flag every HDMA write splits the line it was
    /// meant to set up: StarWars, a Mode 7 perspective demo driven almost
    /// entirely by HDMA, turned its starfield into a regular grid of
    /// dashes.
    ///
    /// It has to be a flag rather than a check on the beam position
    /// because `hdma_run_line` is driven from the frame loop at whatever
    /// dot the CPU happened to reach, not at a real hblank dot.
    hdma_in_progress: bool,
    /// True while general-purpose DMA (`run_channel`) is transferring
    /// (ticket W16-11). Diagnostic classification only — unlike
    /// `hdma_in_progress` it feeds nothing but [`Dsp1DrTrace`], so it must
    /// never be read anywhere near `write_register`'s mid-line
    /// attribution.
    mdma_in_progress: bool,
    /// See [`Dsp1DrTrace`].
    pub dsp1_trace: Dsp1DrTrace,
    pub timing: Timing,
    pub joypads: Joypads,
    /// `$4016` strobe latch, and the serial shift position per port.
    pub(crate) manual_latch: bool,
    pub(crate) manual_shift: [u16; 2],
    /// `$420C` HDMAEN. HDMA itself is W7-07; this records what a ROM
    /// asked for so a mode-7 golden can say WHY it renders flat.
    pub hdmaen: u8,
    /// `$420B` write that is pending execution.
    pending_dma: u8,
    /// Ticket W13-02h: the events this core has produced and not yet
    /// handed to the sink.
    ///
    /// ## Why the queue lives on the bus, not the PPU
    ///
    /// `rf-nes` queues on its PPU because that is where its frame
    /// boundary, its event mask and its scanline counter all are. On the
    /// SNES they are not in one place: the frame clock is
    /// [`crate::timing::Timing`] (here on the bus), the watchpoint hits
    /// come from this type's own `read`/`write`, and the PPU knows about
    /// neither. The bus is the one owner that can see all of them, so it
    /// is where the queue goes.
    events: Vec<rf_core_api::CoreEvent>,
    /// What the consumer subscribed to. `EventMask::NONE` by default, so
    /// an unsubscribed session never builds a `CoreEvent` at all
    /// (ARCHITECTURE §5's pay-for-use, and the same discipline
    /// `Ppu::event_mask` follows in rf-nes).
    event_mask: rf_core_api::EventMask,
    /// Ticket W13-02e's watchpoints, evaluated here because only a bus
    /// can see an access.
    watches: rf_core_api::WatchTable,
    /// Ticket W13-02c: which HDMA channels transferred on each hardware
    /// line of the frame in progress, one bit per channel.
    ///
    /// Recorded unconditionally rather than behind the debug capture, and
    /// the measurement is the reason: 262 bytes and at most eight bit-ORs
    /// per line, against the ~245 KB frame the same loop already produces.
    /// Gating it would cost more in plumbing than it saves.
    hdma_lanes: Vec<u8>,
    /// The cartridge's DSP-1 bus window (D-010, ticket W14-19), present
    /// iff `dsp1` is. Kept separate from `dsp1` itself because the window
    /// is fixed cartridge shape (from `rf_cart::DspWindow`, never
    /// serialized — it is recomputed from the mapper on load) while
    /// `dsp1` is the chip's live, save-stated protocol state.
    dsp_window: Option<rf_cart::DspWindow>,
    /// The DSP-1 HLE (`None` for every cartridge that does not report
    /// [`rf_cart::Coprocessor::Dsp1`]) — see [`Self::install_dsp1`].
    pub dsp1: Option<crate::dsp1::Dsp1>,
}

impl SnesBus {
    /// Subscribe the consumer to a set of events (ticket W13-02h).
    ///
    /// Replaces the mask outright, the same contract
    /// `rf_nes::Ppu::set_event_mask` has.
    pub fn set_event_mask(&mut self, mask: rf_core_api::EventMask) {
        self.event_mask = mask;
    }

    #[must_use]
    pub fn event_mask(&self) -> rf_core_api::EventMask {
        self.event_mask
    }

    /// Install the debugger's watchpoints (ticket W13-02e).
    pub fn set_watches(&mut self, watches: rf_core_api::WatchTable) {
        self.watches = watches;
    }

    /// Queue an event. Callers check the mask first, so this never has to.
    pub(crate) fn queue_event(&mut self, ev: rf_core_api::CoreEvent) {
        self.events.push(ev);
    }

    /// Whether `bit` is subscribed — the check every emission site makes
    /// before it builds a `CoreEvent`.
    pub(crate) fn wants(&self, bit: rf_core_api::EventMask) -> bool {
        self.event_mask.is_subscribed(bit)
    }

    /// Report a watch hit, if this access trips one and anyone is
    /// listening. Observation only.
    fn note_watch_access(
        &mut self,
        space: rf_core_api::WatchSpace,
        access: rf_core_api::WatchAccess,
        addr: u32,
        value: u8,
    ) {
        if !self.watches.is_armed() || !self.wants(rf_core_api::EventMask::MEM_WATCH) {
            return;
        }
        if let Some(id) = self.watches.hit(space, access, addr, value) {
            self.events.push(rf_core_api::CoreEvent::MemWatch { id });
        }
    }

    /// Which HDMA channels transferred on each line of the frame in
    /// progress (ticket W13-02c).
    #[must_use]
    pub fn hdma_lanes(&self) -> &[u8] {
        &self.hdma_lanes
    }

    /// Clear the lane record — called at each frame start, so the view
    /// shows THIS frame rather than an accumulation over the session.
    pub fn clear_hdma_lanes(&mut self) {
        self.hdma_lanes.fill(0);
    }

    /// Take everything queued since the last call.
    pub fn drain_events(&mut self) -> Vec<rf_core_api::CoreEvent> {
        std::mem::take(&mut self.events)
    }

    #[must_use]
    pub fn new(rom: Vec<u8>, sram_len: usize, mode: SnesMapMode) -> Self {
        Self {
            events: Vec::new(),
            event_mask: rf_core_api::EventMask::NONE,
            watches: rf_core_api::WatchTable::new(),
            hdma_lanes: vec![0; 262],
            rom,
            sram: vec![0; sram_len],
            wram: vec![0; WRAM_LEN],
            vram_address: 0,
            vmain: 0,
            mode,
            math: MathUnit::default(),
            nmitimen: NmiTimen::default(),
            irq: IrqTimer::default(),
            wram_port: WramPort::default(),
            dma: Dma::default(),
            fast_rom: false,
            open_bus: 0,
            ppu: Ppu::new(),
            apu: Apu::new(),
            apu_debt: 0,
            apu_overspent: 0,
            hv: crate::regs::HvLatch::default(),
            hdma_in_progress: false,
            mdma_in_progress: false,
            dsp1_trace: Dsp1DrTrace::default(),
            timing: Timing::new(),
            joypads: Joypads::default(),
            manual_latch: false,
            manual_shift: [0; 2],
            hdmaen: 0,
            pending_dma: 0,
            dsp_window: None,
            dsp1: None,
        }
    }

    /// Wire up the cartridge's DSP-1 (D-010, ticket W14-19). Called by
    /// [`crate::system::SnesSystem::load`] when the header reports
    /// [`rf_cart::Coprocessor::Dsp1`]; every other cartridge never calls
    /// this, so `target` falls straight through to [`map`] for it.
    pub fn install_dsp1(&mut self, window: rf_cart::DspWindow) {
        self.dsp_window = Some(window);
        self.dsp1 = Some(crate::dsp1::Dsp1::new());
    }

    fn target(&self, addr: u32) -> Target {
        let bank = ((addr >> 16) & 0xFF) as u8;
        let offset = addr as u16;
        // Checked BEFORE the generic map: a DSP-1 window's `dr`/`sr`
        // ranges sit inside bank/offset space `map` would otherwise
        // resolve as ROM, SRAM or WRAM mirror (see `dsp1_target`'s doc).
        // `dsp_window` is `None` for every non-DSP-1 cartridge, so this
        // is a no-op there and every existing golden's mapping is
        // unchanged.
        if let Some(window) = &self.dsp_window {
            if let Some(target) = crate::mapping::dsp1_target(window, bank, offset) {
                return target;
            }
        }
        map(self.mode, bank, offset, self.rom.len(), self.sram.len())
    }

    /// Register reads WITHOUT side effects, shared by `read` and `peek`.
    fn read_register_pure(&self, offset: u16) -> Option<u8> {
        Some(match offset {
            // The latch counters and STAT78 without their flip-flop and
            // flag side effects: a debugger peeking must not toggle them.
            0x213C => self.hv.h as u8,
            0x213D => self.hv.v as u8,
            0x213F => self.hv.peek_stat78(
                matches!(self.timing.region, crate::timing::Region::Pal),
                self.open_bus,
            ),
            0x4214 => self.math.rddiv as u8,
            0x4215 => (self.math.rddiv >> 8) as u8,
            0x4216 => self.math.rdmpy as u8,
            0x4217 => (self.math.rdmpy >> 8) as u8,
            0x4200 => self.nmitimen.0,
            0x420D => u8::from(self.fast_rom),
            0x2180 => self.wram[(self.wram_port.address as usize) % WRAM_LEN],
            0x4212 => self.timing.read_hvbjoy(),
            0x213E => self.ppu.read_stat77(),
            // $2134-$2136 MPYL/MPYM/MPYH — the M7A x M7B product.
            //
            // Wiring these is not optional even for a ROM that never uses
            // mode 7: it is the SNES's general-purpose signed multiplier,
            // and PeterLemon's RotZoom computes its rotation matrix with
            // it. Leaving them unmapped returned open bus — which is the
            // last value driven, i.e. $21 from the register address
            // itself — so every matrix element came out as $2121 and the
            // playfield rendered as diagonal stripes.
            0x2134..=0x2136 => {
                let p = self.ppu.mode7.product();
                ((p >> (8 * (offset - 0x2134))) & 0xFF) as u8
            }
            // Safe to peek: reading a port has no side effect, and peek
            // must never trigger the catch-up that `read` does.
            0x2140..=0x2143 => self.apu.cpu_read_port(usize::from(offset - 0x2140)),
            0x4218..=0x421F => {
                let port = ((offset - 0x4218) / 2) as usize;
                let word = self.joypads.latched[port];
                if offset & 1 == 0 {
                    word as u8
                } else {
                    (word >> 8) as u8
                }
            }
            _ => return None,
        })
    }

    fn read_register(&mut self, offset: u16) -> u8 {
        match offset {
            // $4211 TIMEUP: reading ACKNOWLEDGES and clears. This is why
            // `peek` cannot share this path.
            0x4211 => self.irq.read_timeup(),
            // $4210 RDNMI: reading CLEARS the vblank flag. gilyon
            // cputest's wait_for_vblank depends on it — see timing.rs.
            0x4210 => self.timing.read_rdnmi(),
            // $2137 SLHV: latch the beam; the read itself is open bus.
            0x2137 => {
                let (dot, line) = (self.timing.dot(), self.timing.line);
                self.hv.latch(dot, line);
                self.open_bus
            }
            // $213C/$213D: two-half reads through a flip-flop each.
            0x213C => self.hv.read_ophct(self.open_bus),
            0x213D => self.hv.read_opvct(self.open_bus),
            // $213F STAT78: reading resets the flip-flops and latch flag.
            0x213F => {
                let pal = matches!(self.timing.region, crate::timing::Region::Pal);
                self.hv.read_stat78(pal, self.open_bus)
            }
            // $4016/$4017 manual joypad read: each read shifts out one
            // bit, so this cannot be a pure read either.
            0x4016 | 0x4017 => {
                let port = usize::from(offset - 0x4016);
                let bit = (self.manual_shift[port] & 0x8000) >> 15;
                self.manual_shift[port] <<= 1;
                bit as u8
            }
            // $2180 WMDATA: reading auto-increments the port.
            0x2180 => {
                let v = self.wram[(self.wram_port.address as usize) % WRAM_LEN];
                self.wram_port.advance();
                v
            }
            // $2140-$2143: catch the APU up FIRST, so what the CPU reads
            // is a state the APU actually reached.
            0x2140..=0x2143 => {
                self.catch_up_apu();
                self.apu.cpu_read_port(usize::from(offset - 0x2140))
            }
            _ => self.read_register_pure(offset).unwrap_or(self.open_bus),
        }
    }

    fn write_register(&mut self, offset: u16, value: u8) {
        match offset {
            // $2100-$213F belong to the PPU, EXCEPT the VRAM port below,
            // which W6-02b built and which stores into `self.vram`.
            0x2100..=0x2114 | 0x211A..=0x213F => {
                // Per-dot (W7-15): the beam's position decides whether
                // this is a MID-LINE write that splits the line being
                // composed, or ordinary setup. `mid_line_position`
                // returns None during vblank and hblank, which is the
                // common case and behaves exactly as before.
                let at = if self.hdma_in_progress {
                    None
                } else {
                    self.timing.mid_line_position()
                };
                self.ppu.write_register_at(offset, value, at);
            }
            0x2140..=0x2143 => {
                self.catch_up_apu();
                self.apu.cpu_write_port(usize::from(offset - 0x2140), value);
            }
            0x2115 => self.vmain = value,
            0x2116 => self.vram_address = (self.vram_address & 0xFF00) | u16::from(value),
            0x2117 => self.vram_address = (self.vram_address & 0x00FF) | (u16::from(value) << 8),
            0x2118 => {
                let at = (self.vram_address as usize * 2) % self.ppu.vram.len();
                self.ppu.vram[at] = value;
                if self.vmain & 0x80 == 0 {
                    self.step_vram_address();
                }
            }
            0x2119 => {
                let at = (self.vram_address as usize * 2 + 1) % self.ppu.vram.len();
                self.ppu.vram[at] = value;
                if self.vmain & 0x80 != 0 {
                    self.step_vram_address();
                }
            }
            0x2180 => {
                let at = (self.wram_port.address as usize) % WRAM_LEN;
                self.wram[at] = value;
                self.wram_port.advance();
            }
            0x2181 => self.wram_port.set_low(value),
            0x2182 => self.wram_port.set_mid(value),
            0x2183 => self.wram_port.set_high(value),
            0x4200 => {
                self.nmitimen = NmiTimen(value);
                // Disabling the H/V IRQ deasserts the line (ticket
                // W14-10). fullsnes, $4211 TIMEUP: the flag "is reset
                // ... on disabling IRQs via 4200h"; bsnes clears its
                // irqLine on a $4200 write that leaves neither H nor V
                // enabled. Without this a flag latched by an earlier
                // H/V IRQ survives the disable and fires the moment I is
                // cleared -- Final Fantasy Mystic Quest takes exactly
                // that IRQ into a BRK whose vector is a STP trap.
                if self.nmitimen.irq_mode() == crate::regs::IrqMode::Off {
                    self.irq.fired = false;
                }
            }
            // $4201 WRIO: a 1-to-0 transition of bit 7 latches the
            // counters, the hardware's other route to what $2137 does.
            0x4201 => {
                if self.hv.write_wrio(value) {
                    let (dot, line) = (self.timing.dot(), self.timing.line);
                    self.hv.latch(dot, line);
                }
            }
            0x4202 => self.math.wrmpya = value,
            0x4203 => self.math.start_multiply(value),
            0x4204 => self.math.wrdiv = (self.math.wrdiv & 0xFF00) | u16::from(value),
            0x4205 => self.math.wrdiv = (self.math.wrdiv & 0x00FF) | (u16::from(value) << 8),
            0x4206 => self.math.start_divide(value),
            0x4207 => self.irq.htime = (self.irq.htime & 0x100) | u16::from(value),
            0x4208 => self.irq.htime = (self.irq.htime & 0x0FF) | (u16::from(value & 1) << 8),
            0x4209 => self.irq.vtime = (self.irq.vtime & 0x100) | u16::from(value),
            0x420A => self.irq.vtime = (self.irq.vtime & 0x0FF) | (u16::from(value & 1) << 8),
            0x420B => self.pending_dma = value,
            0x420C => self.hdmaen = value,
            0x420D => self.fast_rom = value & 1 != 0,
            0x4016 => {
                // Strobe: while high, the shift registers reload.
                let strobe = value & 1 != 0;
                if self.manual_latch && !strobe {
                    self.manual_shift = [self.joypads.ports[0], self.joypads.ports[1]];
                }
                self.manual_latch = strobe;
                if strobe {
                    self.manual_shift = [self.joypads.ports[0], self.joypads.ports[1]];
                }
            }
            0x4300..=0x437F => self.write_dma_register(offset, value),
            _ => {}
        }
    }

    fn write_dma_register(&mut self, offset: u16, value: u8) {
        let ch = ((offset >> 4) & 0x07) as usize;
        let c = &mut self.dma.channels[ch];
        match offset & 0x000F {
            0x0 => c.control = value,
            0x1 => c.b_address = value,
            0x2 => c.a_address = (c.a_address & 0x00FF_FF00) | u32::from(value),
            0x3 => c.a_address = (c.a_address & 0x00FF_00FF) | (u32::from(value) << 8),
            0x4 => c.a_address = (c.a_address & 0x0000_FFFF) | (u32::from(value) << 16),
            0x5 => c.count = (c.count & 0xFF00) | u16::from(value),
            0x6 => c.count = (c.count & 0x00FF) | (u16::from(value) << 8),
            0x7 => c.indirect_bank = value,
            0x8 => c.table_addr = (c.table_addr & 0xFF00) | u16::from(value),
            0x9 => c.table_addr = (c.table_addr & 0x00FF) | (u16::from(value) << 8),
            0xA => c.line_counter = value,
            _ => {}
        }
    }

    /// Run any DMA armed by a `$420B` write, returning its master-cycle
    /// cost. Called by the system after each instruction — DMA on
    /// hardware halts the CPU, so running it between instructions is the
    /// right granularity for a core that is not yet cycle-stepped.
    pub fn service_dma(&mut self) -> u64 {
        let enabled = std::mem::take(&mut self.pending_dma);
        if enabled == 0 {
            return 0;
        }
        let mut cycles = 0;
        for ch in 0..8 {
            if enabled & (1 << ch) == 0 {
                continue;
            }
            cycles += CYCLES_PER_CHANNEL + self.run_channel(ch) * CYCLES_PER_BYTE;
        }
        cycles
    }

    /// Transfer one channel to completion; returns the byte count.
    fn run_channel(&mut self, ch: usize) -> u64 {
        let c = self.dma.channels[ch];
        // A count of 0 means 65536 bytes — the one place where "no bytes"
        // and "all the bytes" share an encoding.
        let total = if c.count == 0 {
            0x1_0000u32
        } else {
            u32::from(c.count)
        };
        let pattern = c.pattern();
        let step = c.a_step();
        let mut a = c.a_address;
        let mut moved = 0u64;

        // Ticket W16-11: classify this channel's A-bus start for the
        // probe, BEFORE any byte moves (a channel with `count == 0` still
        // "starts" for this purpose even though it moves 65536 bytes, not
        // zero — see the comment above).
        if matches!(self.target(a), Target::Dsp1Dr | Target::Dsp1Sr) {
            self.dsp1_trace.mdma_channel_starts += 1;
            if step == 0 {
                self.dsp1_trace.mdma_channel_starts_fixed += 1;
            }
        }
        self.mdma_in_progress = true;

        for i in 0..total {
            let b = 0x2100u32
                + u32::from(
                    c.b_address
                        .wrapping_add(pattern[i as usize % pattern.len()]),
                );
            if c.reverse() {
                let v = self.read(b);
                self.write(a, v);
            } else {
                let v = self.read(a);
                self.write(b, v);
            }
            // Only the low 16 bits step; DMA does not cross banks.
            let low = ((a as u16) as i32 + step) as u16;
            a = (a & 0x00FF_0000) | u32::from(low);
            moved += 1;
        }

        self.mdma_in_progress = false;
        self.dma.channels[ch].a_address = a;
        self.dma.channels[ch].count = 0;
        moved
    }

    /// Run the APU forward by everything it is owed.
    ///
    /// The SPC700 runs at ~1.024 MHz against a 21.477 MHz master clock,
    /// so one SPC cycle is about 21 master cycles.
    ///
    /// **Called after every CPU instruction, not only on a port access**
    /// (ticket W14-09). Until then this ran ONLY from the `$2140-$2143`
    /// arms, and it discarded every cycle past 64 — so a game waiting on
    /// `$4210` or a RAM flag left its sound driver frozen, and one that
    /// polled a port fed it 64 cycles per read. ActRaiser's stub was
    /// still inside its first zero-page clear loop two vblanks after
    /// being started, and the `$FF` the CPU then sent was wiped by the
    /// stub's own `$F1` init write. The port arms still call this first,
    /// so what the CPU reads is a state the APU actually reached.
    pub fn catch_up_apu(&mut self) {
        const MASTER_PER_SPC_CYCLE: u64 = 21;
        // A sanity bound on one call, NOT a budget: the remainder is
        // carried to the next call, never dropped. With the debt settled
        // every instruction a call owes one to a few cycles; the largest
        // honest lump is a full-bank DMA at ~25k, well under this.
        const MAX_CYCLES_PER_CALL: u64 = 1 << 16;
        let owed = self.apu_debt / MASTER_PER_SPC_CYCLE;
        let owed = owed.min(MAX_CYCLES_PER_CALL);
        self.apu_debt -= owed * MASTER_PER_SPC_CYCLE;
        // Cycles already run ahead come out of this call's budget first.
        if self.apu_overspent >= owed {
            self.apu_overspent -= owed;
            return;
        }
        let spc_cycles = owed - self.apu_overspent;
        self.apu_overspent = 0;
        // SPEND THE BUDGET AS CYCLES, NOT INSTRUCTIONS (ticket W7-08).
        //
        // This loop used to run `spc_cycles` ITERATIONS OF ONE
        // INSTRUCTION EACH. Instructions are not one cycle — they are 2
        // to 12 — so the APU ran 2-5x too fast, and the symptom was
        // precise: on the first `$2140` read after the boot hand-over the
        // SPC had already executed a 5-cycle instruction on 1 cycle of
        // debt, clobbering the echo the 65816 was still waiting for.
        let mut spent = 0u64;
        while spent < spc_cycles {
            if self.apu.cpu.stopped || !self.apu.boot.is_running() {
                // Nothing to execute: either halted, or the HLE boot
                // handshake still owns the machine. Still costs a cycle —
                // charging zero is what froze the clock in W7-15's
                // investigation, since a halted CPU waits for an
                // interrupt that only arrives when cycles are spent.
                // The DSP is clocked by the same signal, so it advances
                // here too. It previously did not: a halted or still-booting
                // SPC700 froze the DSP with it, which is not what sharing a
                // clock means.
                self.apu.tick_clock(1);
                // The HLE boot handshake is a POLLING program and this is
                // the only place it gets to look: `step_counted` is not
                // reached while it still owns the machine, so polling only
                // there would leave the handshake frozen forever.
                self.apu.poll_boot();
                spent += 1;
                continue;
            }
            let cycles = self.apu.step_counted().unwrap_or(1);
            spent += u64::from(cycles.max(1));
        }
        // Anything overspent comes out of the next catch-up, so the APU
        // cannot drift ahead one instruction at a time.
        self.apu_overspent = spent - spc_cycles;
    }

    /// VMAIN bits 0-1 select the address increment: 1, 32, 128, 128
    /// words. Bit 7 selects which of the two data ports triggers it,
    /// which is why the callers above differ.
    fn step_vram_address(&mut self) {
        let step = match self.vmain & 0x03 {
            0 => 1u16,
            1 => 32,
            _ => 128,
        };
        self.vram_address = self.vram_address.wrapping_add(step);
    }

    /// Read the low bytes of `len` VRAM words starting at `word_addr`.
    ///
    /// Tile-map entries keep the character in the low byte, so this is
    /// what a ROM that "wrote text" actually wrote.
    #[must_use]
    pub fn vram_low_bytes(&self, word_addr: u16, len: usize) -> Vec<u8> {
        (0..len)
            .map(|i| self.ppu.vram[((word_addr as usize + i) * 2) % self.ppu.vram.len()])
            .collect()
    }

    /// **HDMA init**, at the start of every frame.
    ///
    /// Each enabled channel reloads its table pointer from `$43x2`-`$43x4`
    /// and reads its first line counter. This happens once per frame, not
    /// once per enable — a game that sets `$420C` mid-frame does not get
    /// a transfer until the next frame's init, which is what
    /// `hdmaen_latch_test` checks.
    pub fn hdma_init(&mut self) {
        for ch in 0..8 {
            let enabled = self.hdmaen & (1 << ch) != 0;
            let c = &mut self.dma.channels[ch];
            c.hdma_done = !enabled;
            c.do_transfer = false;
            if !enabled {
                continue;
            }
            c.table_addr = c.a_address as u16;
            self.hdma_reload(ch);
        }
    }

    /// Read the next line counter (and, in indirect mode, the next
    /// pointer) from a channel's table.
    ///
    /// A counter of `$00` **terminates the channel for the frame** — it
    /// is the table's end marker, not a zero-length entry. Treating it as
    /// "transfer nothing this line and carry on" walks off the end of the
    /// table into whatever follows it.
    fn hdma_reload(&mut self, ch: usize) {
        let bank = (self.dma.channels[ch].a_address >> 16) & 0xFF;
        let addr = |offset: u16, table: u16| (bank << 16) | u32::from(table.wrapping_add(offset));

        let table = self.dma.channels[ch].table_addr;
        let counter = self.read(addr(0, table));
        self.dma.channels[ch].table_addr = table.wrapping_add(1);
        self.dma.channels[ch].line_counter = counter;

        if counter == 0 {
            self.dma.channels[ch].hdma_done = true;
            return;
        }
        if self.dma.channels[ch].indirect() {
            let t = self.dma.channels[ch].table_addr;
            let lo = self.read(addr(0, t));
            let hi = self.read(addr(1, t));
            self.dma.channels[ch].table_addr = t.wrapping_add(2);
            self.dma.channels[ch].count = u16::from(lo) | (u16::from(hi) << 8);
        }
        self.dma.channels[ch].do_transfer = true;
    }

    /// Run one scanline of HDMA for every enabled channel.
    ///
    /// Returns the master-cycle cost. Called once per visible scanline —
    /// HDMA is what makes gradients, wavy effects and most split-screen
    /// HUDs work, and it is the reason a mode-7 perspective demo looks
    /// like perspective rather than a flat rotated plane.
    pub fn hdma_run_line(&mut self) -> u64 {
        // See `hdma_in_progress`: these writes are hblank line SETUP, not
        // mid-line events, and attributing them mid-line splits the very
        // line they configure.
        self.hdma_in_progress = true;
        let cycles = self.hdma_run_line_inner();
        self.hdma_in_progress = false;
        cycles
    }

    fn hdma_run_line_inner(&mut self) -> u64 {
        let mut cycles = 0u64;
        for ch in 0..8 {
            if self.hdmaen & (1 << ch) == 0 || self.dma.channels[ch].hdma_done {
                continue;
            }
            if self.dma.channels[ch].do_transfer {
                // Ticket W13-02c: this is the moment a channel actually
                // moves bytes on this line, which is what the lane view
                // shows — not merely being enabled in $420C.
                let line = usize::from(self.timing.line);
                if let Some(slot) = self.hdma_lanes.get_mut(line) {
                    *slot |= 1 << ch;
                }
                cycles += CYCLES_PER_CHANNEL + self.hdma_transfer_unit(ch);
            }

            // Decrement the low seven bits; the repeat flag is bit 7 and
            // is NOT part of the count.
            let c = &mut self.dma.channels[ch];
            let repeat = c.line_counter & 0x80 != 0;
            let remaining = (c.line_counter & 0x7F).wrapping_sub(1);
            c.line_counter = (c.line_counter & 0x80) | remaining;

            if remaining == 0 {
                self.hdma_reload(ch);
            } else {
                // With the repeat flag set the unit transfers on EVERY
                // line; without it, only on the line the counter reloads.
                self.dma.channels[ch].do_transfer = repeat;
            }
        }
        cycles
    }

    /// Transfer one HDMA unit (1-4 bytes, per the channel's pattern).
    fn hdma_transfer_unit(&mut self, ch: usize) -> u64 {
        let c = self.dma.channels[ch];
        let pattern = c.pattern();
        let mut moved = 0u64;
        for (i, step) in pattern.iter().enumerate() {
            let b = 0x2100u32 + u32::from(c.b_address.wrapping_add(*step));
            let source = if c.indirect() {
                (u32::from(c.indirect_bank) << 16) | u32::from(c.count.wrapping_add(i as u16))
            } else {
                let bank = c.a_address & 0x00FF_0000;
                bank | u32::from(c.table_addr.wrapping_add(i as u16))
            };
            // Ticket W16-11: count units sourced from the DSP-1 window,
            // BEFORE the read (which is what advances the DR).
            if matches!(self.target(source), Target::Dsp1Dr | Target::Dsp1Sr) {
                self.dsp1_trace.hdma_units += 1;
            }
            let v = self.read(source);
            self.write(b, v);
            moved += 1;
        }
        // Direct mode consumes the bytes it just read from the table;
        // indirect mode advances the pointer it dereferenced.
        let len = pattern.len() as u16;
        if self.dma.channels[ch].indirect() {
            self.dma.channels[ch].count = self.dma.channels[ch].count.wrapping_add(len);
        } else {
            self.dma.channels[ch].table_addr = self.dma.channels[ch].table_addr.wrapping_add(len);
        }
        moved * CYCLES_PER_BYTE
    }

    /// Advance the math unit by `cycles` CPU cycles.
    pub fn tick_math(&mut self, cycles: u32) {
        for _ in 0..cycles {
            if !self.math.busy() {
                break;
            }
            self.math.step();
        }
    }
}

impl CpuBus for SnesBus {
    fn read(&mut self, addr: u32) -> u8 {
        let value = match self.target(addr) {
            Target::Rom(i) => self.rom[i],
            Target::Wram(i) => self.wram[i],
            Target::Sram(i) => self.sram[i],
            Target::Register(offset) => self.read_register(offset),
            // DR reads advance the chip's output cursor (ticket W14-19);
            // `dsp1` is `Some` whenever `target` can return these
            // variants, since both come from the same `dsp_window`.
            //
            // Ticket W16-11: classify the read BEFORE it mutates
            // `dsp1` — `hdma_in_progress`/`mdma_in_progress` say who is
            // asking, which is exactly what a probe wants to know about a
            // Raster stream's drain.
            Target::Dsp1Dr => {
                let in_raster = self
                    .dsp1
                    .as_ref()
                    .is_some_and(crate::dsp1::Dsp1::raster_active);
                if self.hdma_in_progress {
                    self.dsp1_trace.hdma_reads += 1;
                } else if self.mdma_in_progress {
                    self.dsp1_trace.mdma_reads += 1;
                } else if in_raster {
                    self.dsp1_trace.cpu_raster_reads += 1;
                }
                self.dsp1
                    .as_mut()
                    .map_or(self.open_bus, crate::dsp1::Dsp1::read_dr)
            }
            Target::Dsp1Sr => self.dsp1.as_ref().map_or(self.open_bus, |d| d.read_sr()),
            Target::Open => self.open_bus,
        };
        self.open_bus = value;
        // Ticket W13-02h: reported AFTER the read, so a watch's value
        // condition tests what the CPU actually got — including a
        // register read's side effect, which is the point of watching a
        // register at all.
        self.note_watch_access(
            rf_core_api::WatchSpace::Cpu,
            rf_core_api::WatchAccess::Read,
            addr,
            value,
        );
        value
    }

    fn write(&mut self, addr: u32, value: u8) {
        // Before the write lands, so the condition tests the value
        // arriving (`MemWatch::value_mask`'s own doc).
        self.note_watch_access(
            rf_core_api::WatchSpace::Cpu,
            rf_core_api::WatchAccess::Write,
            addr,
            value,
        );
        self.open_bus = value;
        match self.target(addr) {
            Target::Wram(i) => self.wram[i] = value,
            Target::Sram(i) => self.sram[i] = value,
            Target::Register(offset) => self.write_register(offset, value),
            // DR writes feed the command/parameter protocol (ticket
            // W14-19). SR is documented read-only (snesdev/fullsnes name
            // no write behaviour for it), so a write there is dropped —
            // the same "ignore rather than guess" the ROM arm below uses.
            Target::Dsp1Dr => {
                if let Some(d) = self.dsp1.as_mut() {
                    d.write_dr(value);
                }
            }
            Target::Dsp1Sr => {}
            // ROM is read-only; a write is dropped rather than panicking,
            // because real cartridges ignore it and a game doing it by
            // accident must not take the emulator down (FR-CORE-013's
            // "never a crash" applies to the whole core, not just
            // headers).
            Target::Rom(_) | Target::Open => {}
        }
    }

    fn peek(&self, addr: u32) -> u8 {
        match self.target(addr) {
            Target::Rom(i) => self.rom[i],
            Target::Wram(i) => self.wram[i],
            Target::Sram(i) => self.sram[i],
            // Only the side-effect-free subset. An address whose read has
            // consequences reports open bus rather than firing them.
            Target::Register(offset) => self.read_register_pure(offset).unwrap_or(self.open_bus),
            // Non-perturbing by construction: `peek_dr` never advances
            // the output cursor, and `read_sr` has no side effect either
            // way (contract this impl block's own doc states).
            Target::Dsp1Dr => self
                .dsp1
                .as_ref()
                .map_or(self.open_bus, crate::dsp1::Dsp1::peek_dr),
            Target::Dsp1Sr => self.dsp1.as_ref().map_or(self.open_bus, |d| d.read_sr()),
            Target::Open => self.open_bus,
        }
    }
}
