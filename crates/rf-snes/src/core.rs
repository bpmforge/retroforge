//! `rf_core_api::EmulatorCore` for the SNES (ticket W11-11).
//!
//! ## The shape problem this file solves
//!
//! The two consoles produce a frame in opposite directions, and that —
//! not the CPU, not the PPU — is why one shell could never drive both.
//!
//! `rf-nes` **pushes**: the PPU queues completed scanlines as the CPU
//! runs, and `drain_video` hands them to a `CoreSink` mid-frame. That is
//! what lets layer extraction, the compare view and the debugger watch a
//! frame being built.
//!
//! `rf-snes` **pulls**: [`crate::SnesSystem::render_frame`] runs the CPU
//! to a settled vblank and *then* asks the PPU for each visible line via
//! `Ppu::render_scanline`. Composition on a settled frame is deliberate
//! (ticket W7-06) — mid-frame register writes are the norm on this
//! machine, and rendering as you go would compose half-written state.
//!
//! So this adapter runs the frame, then replays the settled picture into
//! the sink line by line. The sink sees the same sequence it would from a
//! pushing core; what it does not see is the *timing* of those lines,
//! which for a settled-composition renderer does not exist to report.
//! Recorded plainly because a future mid-frame-effects feature will care.
//!
//! ## Visible lines are 224 **or 239**
//!
//! `SETINI`'s overscan bit decides, and a core that always emitted 224
//! would silently crop the bottom fifteen rows of an overscan game rather
//! than letterbox it (ticket W7-06). W11-08 is what made this
//! expressible at all: the display path carried a hardcoded 256x240 until
//! then, so a 256x224 frame had nowhere to live.

use rf_core_api::{
    CartImage, CoreConfig, CoreError, CoreEvent, CoreSink, EmulatorCore, EventMask, InputFrame,
    ResetKind, StateError, StateReader, StateView, StateWriter, Step, StepResult,
};
use rf_core_api::{CpuRegs, Wdc65816Regs};

use crate::SnesSystem;

/// How many instructions a single frame may take before the adapter gives
/// up and reports that it did not complete one.
///
/// The same defence `rf_nes::core::CYCLE_BUDGET` provides, in the unit
/// this core counts in: a `STP` instruction, or a CPU spinning on a
/// register that will never change, must not be able to hang the caller.
/// In the app this runs inside the core thread's guarded closure, where
/// an unbounded loop would also block `CoreCommand::Shutdown` from ever
/// being drained — worse than the crash FM-01 exists to contain.
pub const FRAME_INSTRUCTION_BUDGET: u64 = 4_000_000;

/// The SNES as an [`EmulatorCore`].
/// Registers `$2100`-`$213F`, the range `StateView::ppu_regs` covers
/// (ticket W13-02a).
pub const PPU_REG_COUNT: usize = 0x40;

pub struct SnesCore {
    system: SnesSystem,
    config: CoreConfig,
    /// Ticket W13-02e: what was last pushed to the bus, so the sync at the
    /// top of `step` copies only when the caller changed something.
    pushed_watches: rf_core_api::WatchTable,
    /// Ticket W13-02a: CGRAM as bytes, refreshed each step.
    ///
    /// `StateView` lends `&[u8]` and the PPU stores CGRAM as `[u16; 256]`
    /// — a word array cannot be lent as bytes without either `unsafe`
    /// (which this workspace forbids outright) or a buffer. So: a buffer,
    /// 512 bytes, rebuilt at the end of each step. Little-endian, which is
    /// the order `$2122` writes them in, so a viewer reading a pair back
    /// sees exactly what the ROM wrote.
    cgram_bytes: Vec<u8>,
    /// Ticket W13-02a: the PPU register file, reconstructed. See
    /// [`SnesCore::refresh_state_snapshots`] for the layout and for why
    /// this is rebuilt rather than shadowed at write time.
    ppu_regs: Vec<u8>,
    budget: u64,
    /// The widescreen request: output width and which layers may use it.
    ///
    /// **`WIDTH` means off, and off is the default** — a fresh core
    /// renders 4:3 and never reaches the widening path at all (law 6:
    /// Accuracy Mode is the reference and enhancements are opt-in
    /// overlays). The mask travels with the width because a refusal is
    /// part of the request, not a separate setting the renderer could
    /// forget to consult.
    widescreen: (usize, crate::ppu::WidenMask),
}

impl SnesCore {
    /// Build from a raw ROM image (LoROM or HiROM, copier header or not —
    /// `rf_cart` decides).
    ///
    /// # Errors
    /// Whatever [`SnesSystem::load`] rejects.
    pub fn load(raw: &[u8]) -> Result<Self, rf_cart::CartError> {
        Ok(Self {
            system: SnesSystem::load(raw)?,
            config: CoreConfig::default(),
            pushed_watches: rf_core_api::WatchTable::new(),
            cgram_bytes: Vec::new(),
            ppu_regs: Vec::new(),
            budget: FRAME_INSTRUCTION_BUDGET,
            widescreen: (crate::ppu::WIDTH, crate::ppu::WidenMask::ALL),
        })
    }

    /// Borrow the machine, for the shell's out-of-band reads.
    #[must_use]
    pub fn system(&self) -> &SnesSystem {
        &self.system
    }

    /// Mutable access to the machine.
    pub fn system_mut(&mut self) -> &mut SnesSystem {
        &mut self.system
    }

    /// Shrink the instruction budget, so a termination test can drive the
    /// bound instead of asserting a constant exists.
    /// Ask for a widened picture, with a per-layer mask (ticket W11-03).
    ///
    /// `width <= WIDTH` turns widescreen OFF and restores the accuracy
    /// path exactly. Clamped to the PPU's `MAX_WIDTH`.
    pub fn set_widescreen(&mut self, width: usize, mask: crate::ppu::WidenMask) {
        self.widescreen = (width.clamp(crate::ppu::WIDTH, crate::ppu::MAX_WIDTH), mask);
    }

    /// The width this core is currently emitting.
    #[must_use]
    pub fn frame_width(&self) -> usize {
        self.widescreen.0
    }

    /// Each background's geometry, for a widescreen policy decision.
    #[must_use]
    pub fn bg_geometry(&self) -> [crate::ppu::BgGeometry; 4] {
        self.system.bus.ppu.bg_geometry()
    }

    pub fn set_frame_budget(&mut self, budget: u64) {
        self.budget = budget;
    }

    /// Replay the settled frame into `sink`, line by line.
    ///
    /// Both the picture and the dropped-sprite overlay: `Scanline`
    /// carries `overlay` alongside `pixels`, and forwarding only the
    /// pixels would make the 32-per-line sprite suppression invisible to
    /// exactly the feature built to show it.
    fn emit_frame(&mut self, sink: &mut dyn CoreSink) {
        let visible = self.system.bus.ppu.setini.visible_lines();
        let (width, mask) = self.widescreen;
        for y in 0..visible {
            let line = if width > crate::ppu::WIDTH {
                self.system.bus.ppu.render_scanline_masked(y, width, mask)
            } else {
                // The accuracy path, untouched. Not `render_scanline_masked`
                // with a full mask — the point is that widescreen being off
                // means this code is not reached at all (law 6).
                self.system.bus.ppu.render_scanline(y)
            };
            sink.video_scanline(y, &line.pixels);
            sink.overlay_scanline(y, &line.overlay);
        }
        // Ticket W16-09: promote the Mode 7 matrix to the generic
        // CoreEvent path, pay-for-use like every other variant here --
        // constructed only when a subscriber asked AND the frame is
        // actually in BG mode 7 (a core in any other mode has nothing
        // meaningful to report, and must not manufacture a stale matrix
        // left over from a previous mode-7 frame).
        if self.config.event_mask.is_subscribed(EventMask::MODE7)
            && self.system.bus.ppu.bg_mode == 7
        {
            let frame = crate::debug::mode7_frame(&self.system.bus.ppu);
            sink.event(CoreEvent::Mode7(frame));
        }
        self.drain_events(sink);
    }

    /// Rebuild the byte views `state_view` lends (ticket W13-02a).
    ///
    /// Called at the end of every step, because a debugger reads state
    /// between steps and a snapshot older than the last instruction would
    /// be a viewer showing the wrong frame. ~570 bytes of copying against
    /// a frame that already carries ~245 KB of pixels.
    ///
    /// ## Why the registers are rebuilt rather than shadowed at write time
    ///
    /// A shadow array updated inside `write_register` would be a second
    /// copy of state that can silently disagree with the decoded fields
    /// the renderer actually uses — the exact failure `SnesBus`'s own doc
    /// warns about for VRAM ("writes would land here while rendering read
    /// `ppu.vram`, so every game would draw a black screen with nothing
    /// obviously wrong anywhere"). Deriving them from the live fields
    /// cannot drift, and pays only when someone is looking.
    ///
    /// ## Layout
    ///
    /// Indexed so `ppu_regs[n]` is register `$21nn` — a viewer asking for
    /// `$2105` reads index 5, rather than having to learn a private field
    /// order. Registers this core does not model read back as zero;
    /// [`PPU_REG_COUNT`] covers `$2100-$213F`.
    fn refresh_state_snapshots(&mut self) {
        let ppu = &self.system.bus.ppu;

        self.cgram_bytes.clear();
        self.cgram_bytes.reserve(ppu.cgram.len() * 2);
        for word in &ppu.cgram {
            self.cgram_bytes.extend_from_slice(&word.to_le_bytes());
        }

        self.ppu_regs.clear();
        self.ppu_regs.resize(PPU_REG_COUNT, 0);
        let mut set = |offset: usize, value: u8| {
            if let Some(slot) = self.ppu_regs.get_mut(offset) {
                *slot = value;
            }
        };
        // $2100 INIDISP: forced blank in bit 7, brightness in bits 0-3.
        set(
            0x00,
            (u8::from(ppu.forced_blank) << 7) | (ppu.brightness & 0x0F),
        );
        // $2101 OBSEL: size in bits 5-7, name select in 3-4, base in 0-2.
        set(
            0x01,
            ((ppu.obj_size & 0x07) << 5)
                | (((ppu.obj_name_select >> 13) as u8 & 0x03) << 3)
                | ((ppu.obj_name_base >> 14) as u8 & 0x07),
        );
        // $2102/$2103 OAMADDL/H — the word address, plus the rotation bit.
        set(0x02, (ppu.oam_addr >> 1) as u8);
        set(
            0x03,
            ((ppu.oam_addr >> 9) as u8 & 0x01) | (u8::from(ppu.oam_priority_rotation) << 7),
        );
        // $2105 BGMODE: tile sizes in bits 4-7, BG3 priority in 3, mode in 0-2.
        let mut bgmode = (ppu.bg_mode & 0x07) | (u8::from(ppu.bg3_priority) << 3);
        for (i, bg) in ppu.bgs.iter().enumerate() {
            bgmode |= u8::from(bg.tile_size_16) << (4 + i);
        }
        set(0x05, bgmode);
        // $2107-$210A BGnSC: tilemap base in bits 2-7, size in 0-1.
        for (i, bg) in ppu.bgs.iter().enumerate() {
            set(
                0x07 + i,
                (((bg.tilemap_base >> 10) as u8 & 0x3F) << 2) | (bg.tilemap_size & 0x03),
            );
        }
        // $210B/$210C BGnNBA: two 4-bit char bases per register.
        for pair in 0..2usize {
            let lo = (ppu.bgs[pair * 2].char_base >> 12) as u8 & 0x0F;
            let hi = (ppu.bgs[pair * 2 + 1].char_base >> 12) as u8 & 0x0F;
            set(0x0B + pair, lo | (hi << 4));
        }
        // $212C TM / $212D TS: which layers are on each screen.
        let mut tm = u8::from(ppu.obj_enabled) << 4;
        for (i, bg) in ppu.bgs.iter().enumerate() {
            tm |= u8::from(bg.enabled) << i;
        }
        set(0x2C, tm);
        set(0x2D, ppu.ts);
        // $213E STAT77: range-over in bit 6, time-over in bit 7.
        set(
            0x3E,
            (u8::from(ppu.range_over) << 6) | (u8::from(ppu.time_over) << 7),
        );
    }

    /// Hand everything the bus queued to the sink (ticket W13-02h).
    ///
    /// Drained at the frame boundary and at the end of every step, so a
    /// single-stepping debugger sees a watch hit at the instruction that
    /// caused it rather than at the next frame.
    fn drain_events(&mut self, sink: &mut dyn CoreSink) {
        for ev in self.system.bus.drain_events() {
            sink.event(ev);
        }
    }
}

impl EmulatorCore for SnesCore {
    fn load(&mut self, cart: CartImage<'_>) -> Result<(), CoreError> {
        self.system =
            SnesSystem::load(cart.rom).map_err(|e| CoreError::InvalidImage(format!("{e:?}")))?;
        Ok(())
    }

    fn reset(&mut self, _kind: ResetKind) {
        self.system.reset();
    }

    fn run_frame(&mut self, input: &InputFrame, sink: &mut dyn CoreSink) {
        let _ = input; // Controller wiring is W11-12's; see the notes there.
        let _ = self.step(Step::Frame, sink);
    }

    fn step(&mut self, granularity: Step, sink: &mut dyn CoreSink) -> StepResult {
        // Ticket W13-02h/W13-02e: `CoreConfig` is the documented path a
        // consumer configures a core through, and this is what makes that
        // true here. Pushed on entry rather than on a setter so there is
        // exactly one place the bus can disagree with the config.
        if self.system.bus.event_mask() != self.config.event_mask {
            self.system.bus.set_event_mask(self.config.event_mask);
        }
        if self.config.watches != self.pushed_watches {
            self.system.bus.set_watches(self.config.watches);
            self.pushed_watches = self.config.watches;
        }
        let start_frame = self.system.bus.timing.frame;
        let mut instructions = 0u64;
        let frame_complete;

        match granularity {
            Step::Instruction => {
                // A halted CPU is not an error here: `SnesSystem::step`
                // reports the halt opcode, and the debugger wants to see
                // that state rather than have it swallowed.
                let _ = self.system.step();
                instructions = 1;
                frame_complete = self.system.bus.timing.frame != start_frame;
            }
            Step::Scanline | Step::Frame => {
                // Run to a settled vblank and out the other side, which
                // is what makes composition safe on this machine.
                while !self.system.bus.timing.in_vblank() && instructions < self.budget {
                    if self.system.step().is_err() {
                        break;
                    }
                    instructions += 1;
                }
                while (self.system.bus.timing.in_vblank()
                    || self.system.bus.timing.frame == start_frame)
                    && instructions < self.budget
                {
                    if self.system.step().is_err() {
                        break;
                    }
                    instructions += 1;
                }
                frame_complete = self.system.bus.timing.frame != start_frame;
                if frame_complete {
                    self.emit_frame(sink);
                }
            }
        }

        // Ticket W13-02a: refresh the byte views a debugger reads between
        // steps, before reporting the step as finished.
        self.refresh_state_snapshots();

        // Every step drains, not only a completed frame: an Instruction or
        // Scanline step that trips a watchpoint must report it at the step
        // that caused it, which is the whole point of single-stepping with
        // one armed. `emit_frame` has already drained on a frame boundary,
        // so this is empty in that case rather than a double delivery.
        self.drain_events(sink);

        StepResult {
            // Instructions, not master cycles: this core counts its
            // budget in instructions, and reporting a number in a unit it
            // does not measure would be worse than reporting this one.
            cycles: instructions,
            frame_complete,
        }
    }

    fn save_state(&self, _w: &mut dyn StateWriter) -> Result<(), StateError> {
        Err(StateError::Io(
            "SNES state is carried by rf_state's container (W8-02), not this trait method"
                .to_string(),
        ))
    }

    fn load_state(&mut self, _r: &mut dyn StateReader) -> Result<(), StateError> {
        Err(StateError::Io(
            "SNES state is carried by rf_state's container (W8-02), not this trait method"
                .to_string(),
        ))
    }

    fn state_view(&self) -> StateView<'_> {
        StateView {
            // Typed, per ruling D-6 (ticket W13-02i): the contract names
            // the 65C816 register file rather than a byte layout every
            // consumer would decode per console. `e` travels alongside
            // `p` because it is not a bit of `p` and changes what two of
            // `p`'s bits mean.
            cpu_regs: CpuRegs::Wdc65816(Wdc65816Regs {
                a: self.system.cpu.a,
                x: self.system.cpu.x,
                y: self.system.cpu.y,
                sp: self.system.cpu.sp,
                d: self.system.cpu.d,
                dbr: self.system.cpu.dbr,
                pbr: self.system.cpu.pbr,
                pc: self.system.cpu.pc,
                p: self.system.cpu.p,
                e: self.system.cpu.e,
            }),
            wram: &self.system.bus.wram,
            vram: &self.system.bus.ppu.vram,
            cgram: &self.cgram_bytes,
            oam: &self.system.bus.ppu.oam,
            ppu_regs: &self.ppu_regs,
            // **Empty is the correct report here, not a stub.** A plain
            // LoROM/HiROM cartridge has no bank registers and no IRQ
            // counter — there is nothing to serialize, which is exactly
            // what this field's own doc calls for ("empty slice for
            // mappers with no persistent state"). It becomes non-empty
            // when a core supports a cartridge chip that has some (SA-1,
            // SuperFX), and `rf_cart` reports none of those yet.
            mapper_state: &[],
        }
    }

    fn config(&mut self) -> &mut CoreConfig {
        &mut self.config
    }

    fn peek(&self, addr: u32) -> u8 {
        // **Memory targets only, and that is the point.** A peek must not
        // perturb the machine, and a SNES hardware register read very
        // much can — `$2139`/`$213A` advance the VRAM read address,
        // `$4218`-style ports latch. So this resolves the address through
        // the same mapping the CPU uses and answers for ROM, WRAM and
        // SRAM; a register or an unmapped address reports the quiescent
        // 0 rather than being read for real.
        //
        // The alternative — routing through the CPU's read path — would
        // make the debugger's memory viewer a participant in the
        // simulation, which is exactly what ARCHITECTURE §2's honesty
        // contract forbids. A viewer that changes what it observes is
        // worse than one with blanks in it.
        let bank = ((addr >> 16) & 0xFF) as u8;
        let offset = (addr & 0xFFFF) as u16;
        let target = crate::mapping::map(
            self.system.bus.mode,
            bank,
            offset,
            self.system.bus.rom.len(),
            self.system.bus.sram.len(),
        );
        match target {
            crate::mapping::Target::Rom(i) => self.system.bus.rom.get(i).copied().unwrap_or(0),
            crate::mapping::Target::Wram(i) => self.system.bus.wram.get(i).copied().unwrap_or(0),
            crate::mapping::Target::Sram(i) => self.system.bus.sram.get(i).copied().unwrap_or(0),
            // This viewer resolves through the plain `map` above, never
            // `dsp1_target`/`sa1_target` — a DSP-1 window's DR/SR, or an
            // SA-1 cart's I-RAM/BW-RAM/register window, occupy the same
            // bank/offset space `map` alone would call ROM/SRAM/open bus
            // here, so these arms are unreachable in practice but must
            // still type-check (tickets W14-19, W17-01 added the
            // variants).
            crate::mapping::Target::Register(_)
            | crate::mapping::Target::Dsp1Dr
            | crate::mapping::Target::Dsp1Sr
            | crate::mapping::Target::Sa1IRam(_)
            | crate::mapping::Target::Sa1BwRam(_)
            | crate::mapping::Target::Sa1Register(_)
            | crate::mapping::Target::Sa1Bitmap(_)
            | crate::mapping::Target::GsuRam(_)
            | crate::mapping::Target::GsuRegister(_)
            | crate::mapping::Target::Obc1Register(_)
            | crate::mapping::Target::Obc1Bits(_)
            | crate::mapping::Target::Open => 0,
        }
    }
}
