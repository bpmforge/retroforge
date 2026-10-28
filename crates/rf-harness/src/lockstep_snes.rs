//! The lockstep checker for the SNES (ticket W36-08b): the engine, report and
//! repro bundle of [`crate::lockstep`], instantiated for [`SnesCore`].
//!
//! Two cores from power-on, the same pad input, one with the block executor
//! on, compared after every frame:
//!
//! - the 65816 register file (`A X Y S D DBR PBR PC P E`) and the master
//!   cycle counter,
//! - work RAM (128 KiB), VRAM, OAM, CGRAM, and cartridge SRAM,
//! - the serialised **CPU-side bus state** (raster timing, IRQ/NMI latches,
//!   the maths unit, joypad latches, all DMA/HDMA channels), the whole
//!   **PPU register file** and the whole **APU** (SPC700 registers, its
//!   64 KiB RAM, the timers, the port latches, every S-DSP voice with its
//!   BRR cursor, the echo ring and the noise LFSR), and the mapper and
//!   cartridge chunks. These come from `SnesSystem::save_region`, the same
//!   non-perturbing path a savestate uses, so nothing is read through a
//!   register with side effects,
//! - hashes of the video (palette indices per line) and the audio the frame
//!   produced.
//!
//! **What it cannot see.** Both cores run the same PPU, APU and bus code, so
//! a bug those share with the interpreter is invisible: the checker proves
//! the *executor* (and, later, a JIT) did not change the machine, not that
//! the machine is right. The coprocessors' own CPUs (SA-1, Super FX) are
//! covered only through their serialised state in the `cart` chunk, except
//! the SA-1's 65816, whose registers, I-RAM and BW-RAM are also compared by
//! name (`sa1.cpu.*`, `sa1.iram`, `sa1.bwram`; ticket W40-03). The fine
//! pass (the diverging frame re-run an instruction at a time) compares the
//! registers, the cycle counter and work RAM after every instruction; a
//! difference that appears only in VRAM/OAM/CGRAM/APU/PPU state or in the
//! output hashes is reported at frame level with no instruction named.
//!
//! The repro bundle's `.rfstate` is the shipped SNES container
//! (`console = 1`); the region-to-tag table is a copy of
//! `retroforge::snes_save_state::REGION_TAGS` (the app crate cannot be a
//! dependency of the harness), and the per-console chunk versions come from
//! `rf_state::current_version_for`.

use rf_core_api::{CpuRegs, EmulatorCore, InputFrame, ResetKind};
use rf_rebuild::Atlas;
use rf_snes::core::SnesCore;
use rf_snes::state::StateRegion;
use rf_state::Container;

use crate::lockstep::{
    bytes, rom_hash_bytes, run_machine, scalar, FieldDiff, Hashes, LockstepError, LockstepMachine,
    Options, PayloadBuf, Report, TraceLine,
};

/// Console byte of a SNES container (`retroforge::snes_save_state::CONSOLE_SNES`).
const CONSOLE_SNES: u8 = 1;

/// Mirror of `retroforge::snes_save_state::REGION_TAGS`.
const REGION_TAGS: [(StateRegion, [u8; 4]); 9] = [
    (StateRegion::Cpu, *b"CPU_"),
    (StateRegion::Ppu, *b"PPU_"),
    (StateRegion::Apu, *b"APU_"),
    (StateRegion::Wram, *b"WRAM"),
    (StateRegion::Vram, *b"VRAM"),
    (StateRegion::Oam, *b"OAM_"),
    (StateRegion::Cgram, *b"CGRM"),
    (StateRegion::Mapper, *b"MAPR"),
    (StateRegion::Cart, *b"CART"),
];

/// The regions compared as serialised bytes, with the field name a
/// difference is reported under. Work RAM, VRAM, OAM and CGRAM are compared
/// directly (same bytes, better offsets).
const SERIALISED: [(StateRegion, &str); 5] = [
    (StateRegion::Cpu, "bus_state"),
    (StateRegion::Ppu, "ppu_state"),
    (StateRegion::Apu, "apu_state"),
    (StateRegion::Mapper, "mapper_state"),
    (StateRegion::Cart, "cart_state"),
];

/// Run `inputs` (one entry per frame; port 0 and 1 are the `$4218` layout
/// words) through a plain and a rebuilt SNES core from power-on, comparing
/// after every frame. See the module doc for what is compared and the
/// [`crate::lockstep`] doc for the divergence report and the bundle.
///
/// `raw` is the cartridge image (copier header allowed); `atlas` must be for
/// its ROM.
///
/// # Errors
/// The image does not load or the atlas is for another cartridge.
pub fn run(
    raw: &[u8],
    atlas: &Atlas,
    inputs: impl IntoIterator<Item = InputFrame>,
    opts: Options<'_, SnesCore>,
) -> Result<Report, LockstepError> {
    run_machine::<SnesCore>(raw, atlas, inputs, opts)
}

fn regs(core: &SnesCore) -> rf_core_api::Wdc65816Regs {
    match core.state_view().cpu_regs {
        CpuRegs::Wdc65816(r) => r,
        _ => rf_core_api::Wdc65816Regs::default(),
    }
}

fn region_bytes(core: &SnesCore, region: StateRegion) -> Vec<u8> {
    let mut buf = PayloadBuf::default();
    // Writing to a Vec cannot fail; a refusal would surface as a difference.
    let _ = core.system().save_region(region, &mut buf);
    buf.0
}

impl LockstepMachine for SnesCore {
    const CONSOLE: &'static str = "SNES";
    const BANKED: bool = true;
    const FINE_STEP_LIMIT: u32 = 2_000_000;

    fn power_on(raw: &[u8]) -> Result<Self, LockstepError> {
        let mut core = SnesCore::load(raw).map_err(|e| LockstepError::SnesRom(format!("{e}")))?;
        core.reset(ResetKind::Hard);
        Ok(core)
    }

    fn install(
        &mut self,
        atlas: &Atlas,
        trace_len: usize,
        hook_ids: &[u32],
    ) -> Result<(), LockstepError> {
        self.set_rebuild(Some(atlas.clone()))
            .map_err(|e| LockstepError::Atlas(format!("{e:?}")))?;
        self.set_rebuild_trace(trace_len);
        self.set_function_hooks(hook_ids);
        Ok(())
    }

    /// `SnesCore::run_frame` ignores its input argument (the pad is wired
    /// through the bus), so the words are latched here, the way the app does.
    fn feed(&mut self, input: &InputFrame) {
        let ports = &mut self.system_mut().bus.joypads.ports;
        for (slot, word) in ports.iter_mut().zip(input.ports) {
            *slot = word;
        }
    }

    fn pc(&self) -> u32 {
        self.system().cpu.pc24()
    }

    fn compare_cpu(plain: &Self, rebuilt: &Self) -> Vec<FieldDiff> {
        let (a, b) = (regs(plain), regs(rebuilt));
        let mut out = Vec::new();
        scalar(&mut out, "cpu.a", a.a, b.a);
        scalar(&mut out, "cpu.x", a.x, b.x);
        scalar(&mut out, "cpu.y", a.y, b.y);
        scalar(&mut out, "cpu.s", a.sp, b.sp);
        scalar(&mut out, "cpu.d", a.d, b.d);
        scalar(&mut out, "cpu.dbr", a.dbr, b.dbr);
        scalar(&mut out, "cpu.pbr", a.pbr, b.pbr);
        scalar(&mut out, "cpu.pc", a.pc, b.pc);
        scalar(&mut out, "cpu.p", a.p, b.p);
        scalar(&mut out, "cpu.e", u8::from(a.e), u8::from(b.e));
        scalar(
            &mut out,
            "cycles",
            plain.system().master_cycles,
            rebuilt.system().master_cycles,
        );
        bytes(
            &mut out,
            "wram",
            &plain.system().bus.wram,
            &rebuilt.system().bus.wram,
        );
        // Ticket W40-03: the SA-1's 65816, by name (it is also inside the
        // `cart_state` bytes, but a register difference should say which).
        if let (Some(p), Some(r)) = (&plain.system().bus.sa1, &rebuilt.system().bus.sa1) {
            let (a, b) = (&p.cpu, &r.cpu);
            scalar(&mut out, "sa1.cpu.a", a.a, b.a);
            scalar(&mut out, "sa1.cpu.x", a.x, b.x);
            scalar(&mut out, "sa1.cpu.y", a.y, b.y);
            scalar(&mut out, "sa1.cpu.s", a.sp, b.sp);
            scalar(&mut out, "sa1.cpu.d", a.d, b.d);
            scalar(&mut out, "sa1.cpu.dbr", a.dbr, b.dbr);
            scalar(&mut out, "sa1.cpu.pbr", a.pbr, b.pbr);
            scalar(&mut out, "sa1.cpu.pc", a.pc, b.pc);
            scalar(&mut out, "sa1.cpu.p", a.p, b.p);
            scalar(&mut out, "sa1.cpu.e", u8::from(a.e), u8::from(b.e));
            scalar(&mut out, "sa1.credit", p.credit, r.credit);
            scalar(
                &mut out,
                "sa1.instructions",
                p.instructions_executed,
                r.instructions_executed,
            );
        }
        out
    }

    fn compare_frame(plain: &Self, rebuilt: &Self, hp: Hashes, hr: Hashes) -> Vec<FieldDiff> {
        let mut out = Self::compare_cpu(plain, rebuilt);
        let (p, r) = (plain.system(), rebuilt.system());
        scalar(
            &mut out,
            "frame_count",
            p.bus.timing.frame,
            r.bus.timing.frame,
        );
        bytes(&mut out, "vram", &p.bus.ppu.vram, &r.bus.ppu.vram);
        bytes(&mut out, "oam", &p.bus.ppu.oam, &r.bus.ppu.oam);
        let cgram = |s: &rf_snes::SnesSystem| -> Vec<u8> {
            s.bus
                .ppu
                .cgram
                .iter()
                .flat_map(|w| w.to_le_bytes())
                .collect()
        };
        bytes(&mut out, "cgram", &cgram(p), &cgram(r));
        bytes(&mut out, "sram", &p.bus.sram, &r.bus.sram);
        if let (Some(ps), Some(rs)) = (&p.bus.sa1, &r.bus.sa1) {
            bytes(&mut out, "sa1.iram", &ps.iram, &rs.iram);
            bytes(&mut out, "sa1.bwram", &ps.bwram, &rs.bwram);
        }
        for (region, name) in SERIALISED {
            bytes(
                &mut out,
                name,
                &region_bytes(plain, region),
                &region_bytes(rebuilt, region),
            );
        }
        scalar(&mut out, "video_hash", hp.video, hr.video);
        scalar(&mut out, "audio_hash", hp.audio, hr.audio);
        out
    }

    fn snapshot(&self, raw: &[u8]) -> Result<Vec<u8>, String> {
        let hash = rom_hash_bytes(&rf_cart::hash::identity_snes(raw).normalized.sha256)?;
        let mut container = Container::new(CONSOLE_SNES, hash, env!("CARGO_PKG_VERSION"), 0);
        for (region, tag) in REGION_TAGS {
            let mut payload = PayloadBuf::default();
            self.system()
                .save_region(region, &mut payload)
                .map_err(|e| format!("{e:?}"))?;
            let version = rf_state::current_version_for(CONSOLE_SNES, tag).ok_or_else(|| {
                format!("tag {} not in the registry", String::from_utf8_lossy(&tag))
            })?;
            container
                .add_chunk(tag, version, payload.0)
                .map_err(|e| format!("{e:?}"))?;
        }
        container.encode().map_err(|e| format!("{e:?}"))
    }

    fn wram(&self) -> Vec<u8> {
        self.system().bus.wram.clone()
    }

    fn trace(&self) -> Vec<TraceLine> {
        self.rebuild_trace()
    }

    fn rom_sha256(raw: &[u8]) -> String {
        rf_cart::hash::identity_snes(raw).normalized.sha256
    }
}
