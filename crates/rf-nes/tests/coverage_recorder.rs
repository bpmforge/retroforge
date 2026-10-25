//! **The coverage recorder sees what runs, and perturbs nothing** (ticket
//! W36-01; `docs/design/REBUILD_LAB.md` §5-7).
//!
//! Synthetic cartridges whose every executed byte is known in advance, so
//! each assertion names the exact flag a byte must carry. The determinism
//! test is the load-bearing one: law 4 requires an armed recorder to be
//! invisible to the machine.

use rf_core_api::coverage::{OPCODE, OPERAND};
use rf_core_api::{
    CodeAddr, CoreEvent, CoreSink, EmulatorCore, IndirectKind, InputFrame, InterruptKind, PpuPixel,
    ResetKind,
};
use rf_nes::core::NesCore;

struct Hasher {
    h: u64,
}

impl Hasher {
    fn new() -> Self {
        Self {
            h: 0xcbf2_9ce4_8422_2325,
        }
    }
    fn feed(&mut self, b: u8) {
        self.h ^= u64::from(b);
        self.h = self.h.wrapping_mul(0x0100_0000_01b3);
    }
}

impl CoreSink for Hasher {
    fn video_scanline(&mut self, y: u16, pixels: &[PpuPixel]) {
        self.feed(y as u8);
        for p in pixels {
            self.feed(p.palette_index);
        }
    }
    fn audio(&mut self, samples: &[i16]) {
        for s in samples {
            self.feed(*s as u8);
        }
    }
    fn event(&mut self, _event: CoreEvent) {}
}

/// iNES image: `mapper` split across flags 6/7, `prg` as given (a multiple
/// of 16 KiB), `chr_units` 8 KiB CHR banks.
fn ines(mapper: u8, prg: &[u8], chr_units: u8) -> Vec<u8> {
    assert_eq!(prg.len() % 0x4000, 0);
    let mut d = vec![0u8; 16];
    d[0..4].copy_from_slice(b"NES\x1a");
    d[4] = (prg.len() / 0x4000) as u8;
    d[5] = chr_units;
    d[6] = (mapper & 0x0F) << 4;
    d[7] = mapper & 0xF0;
    d.extend_from_slice(prg);
    d.extend(vec![0u8; usize::from(chr_units) * 0x2000]);
    d
}

fn put(prg: &mut [u8], at: usize, code: &[u8]) {
    prg[at..at + code.len()].copy_from_slice(code);
}

/// 16 KiB NROM program (mirrored at `$8000`/`$C000`):
/// RAM routine via JSR/RTS, JMP-indirect, NMI handler with RTI.
fn nrom_program() -> Vec<u8> {
    let mut prg = vec![0u8; 0x4000];
    put(
        &mut prg,
        0x00,
        &[
            0xA9, 0xE8, // 00 LDA #$E8   (INX)
            0x8D, 0x00, 0x03, // 02 STA $0300
            0xA9, 0x60, // 05 LDA #$60   (RTS)
            0x8D, 0x01, 0x03, // 07 STA $0301
            0x20, 0x00, 0x03, // 0A JSR $0300  (executes from RAM)
            0xA9, 0x30, // 0D LDA #$30
            0x8D, 0x10, 0x03, // 0F STA $0310
            0xA9, 0x80, // 12 LDA #$80
            0x8D, 0x11, 0x03, // 14 STA $0311
            0xA9, 0x80, // 17 LDA #$80   (NMI on)
            0x8D, 0x00, 0x20, // 19 STA $2000
            0x6C, 0x10, 0x03, // 1C JMP ($0310) -> $8030
        ],
    );
    put(&mut prg, 0x30, &[0x4C, 0x30, 0x80]); // JMP $8030
    put(&mut prg, 0x100, &[0xE6, 0x00, 0x40]); // NMI: INC $00; RTI
    put(&mut prg, 0x3FFA, &[0x00, 0x81, 0x00, 0x80, 0x00, 0x82]);
    prg
}

fn run_frames(core: &mut NesCore, n: usize, sink: &mut dyn CoreSink) {
    for _ in 0..n {
        core.run_frame(&InputFrame::empty(), sink);
    }
}

fn armed_nrom() -> NesCore {
    let mut core = NesCore::from_ines_bytes(&ines(0, &nrom_program(), 1)).expect("loads");
    core.set_coverage_recording(true);
    core.reset(ResetKind::Hard);
    core
}

#[test]
fn opcode_and_operand_bytes_are_flagged_and_unexecuted_bytes_are_not() {
    let mut core = armed_nrom();
    run_frames(&mut core, 4, &mut Hasher::new());
    let log = core.take_coverage().expect("armed");
    assert_eq!(log.prg_flags[0x00], OPCODE); // LDA #imm
    assert_eq!(log.prg_flags[0x01], OPERAND);
    assert_eq!(log.prg_flags[0x02], OPCODE); // STA abs
    assert_eq!(log.prg_flags[0x03], OPERAND);
    assert_eq!(log.prg_flags[0x04], OPERAND);
    assert_eq!(log.prg_flags[0x1C], OPCODE); // JMP (ind)
    assert_eq!(log.prg_flags[0x30], OPCODE); // the spin loop
    assert_eq!(log.prg_flags[0x50], 0, "never executed");
    assert!(log.instructions_recorded > 10);
    assert!(log.prg_coverage_fraction() > 0.0 && log.prg_coverage_fraction() < 0.01);
}

#[test]
fn jmp_indirect_and_rts_edges_are_recorded_with_resolved_targets() {
    let mut core = armed_nrom();
    run_frames(&mut core, 2, &mut Hasher::new());
    let log = core.take_coverage().unwrap();
    let jmp = log
        .indirect
        .iter()
        .find(|e| e.kind == IndirectKind::JmpIndirect)
        .expect("JMP ($0310) edge");
    assert_eq!(
        jmp.site,
        CodeAddr {
            cpu: 0x801C,
            rom: Some(0x1C)
        }
    );
    assert_eq!(
        jmp.target,
        CodeAddr {
            cpu: 0x8030,
            rom: Some(0x30)
        }
    );
    let rts = log
        .indirect
        .iter()
        .find(|e| e.kind == IndirectKind::Rts)
        .expect("RTS edge");
    assert_eq!(rts.site.cpu, 0x0301);
    assert_eq!(rts.site.rom, None, "the RTS ran from RAM");
    assert_eq!(rts.target.cpu, 0x800D);
    assert_eq!(rts.target.rom, Some(0x0D));
}

#[test]
fn code_executed_from_ram_lands_in_ram_exec_not_prg() {
    let mut core = armed_nrom();
    run_frames(&mut core, 2, &mut Hasher::new());
    let log = core.take_coverage().unwrap();
    assert_eq!(log.ram_exec[0x0300], OPCODE); // INX
    assert_eq!(log.ram_exec[0x0301], OPCODE); // RTS
    assert_eq!(log.executed_ram_bytes(), 2);
}

#[test]
fn nmi_and_reset_entries_are_recorded_and_rti_returns() {
    let mut core = armed_nrom();
    run_frames(&mut core, 4, &mut Hasher::new());
    let log = core.take_coverage().unwrap();
    let find = |k| log.interrupts.iter().find(|i| i.kind == k).copied();
    let reset = find(InterruptKind::Reset).expect("reset entry");
    assert_eq!(reset.target.cpu, 0x8000);
    assert_eq!(reset.target.rom, Some(0));
    let nmi = find(InterruptKind::Nmi).expect("NMI entry");
    assert_eq!(nmi.target.cpu, 0x8100);
    assert_eq!(nmi.target.rom, Some(0x100));
    assert_eq!(log.prg_flags[0x100], OPCODE, "handler's first opcode");
    let rti = log
        .indirect
        .iter()
        .find(|e| e.kind == IndirectKind::Rti)
        .expect("RTI edge");
    assert_eq!(rti.site.rom, Some(0x102));
    assert_eq!(rti.target.cpu, 0x8030);
    assert_eq!(log.interrupts.len(), 2, "NMI is deduplicated across frames");
}

#[test]
fn take_resets_the_log_and_disarm_stops_recording() {
    let mut core = armed_nrom();
    run_frames(&mut core, 2, &mut Hasher::new());
    let first = core.take_coverage().unwrap();
    assert!(first.instructions_recorded > 0);
    let second = core.take_coverage().unwrap();
    assert_eq!(second.instructions_recorded, 0, "fresh log after a take");
    run_frames(&mut core, 1, &mut Hasher::new());
    core.set_coverage_recording(false);
    assert!(core.take_coverage().is_none());
}

/// GxROM-shaped (mapper 66): bank 0 switches to bank 1 and the *same CPU
/// address* then fetches different ROM bytes.
#[test]
fn bank_switch_changes_rom_offsets_for_the_same_cpu_address() {
    let mut prg = vec![0u8; 8 * 0x4000]; // four 32 KiB banks
    put(
        &mut prg,
        0x0000,
        &[
            0xA9, 0x10, // 8000 LDA #$10  (PRG bank 1)
            0x8D, 0x00, 0x80, // 8002 STA $8000
        ],
    );
    put(&mut prg, 0x0005, &[0xEA, 0xEA]); // bank 0's $8005: never reached
    put(&mut prg, 0x8005, &[0xEA, 0x4C, 0x05, 0x80]); // bank 1: NOP; JMP $8005
    for bank in 0..4 {
        let at = bank * 0x8000 + 0x7FFC;
        put(&mut prg, at, &[0x00, 0x80]);
    }
    let mut core = NesCore::from_ines_bytes(&ines(66, &prg, 4)).expect("loads");
    core.set_coverage_recording(true);
    core.reset(ResetKind::Hard);
    run_frames(&mut core, 2, &mut Hasher::new());
    let log = core.take_coverage().unwrap();
    assert_eq!(log.prg_flags[0x0000], OPCODE);
    assert_eq!(log.prg_flags[0x0005], 0, "bank 0's $8005 never ran");
    assert_eq!(log.prg_flags[0x8005], OPCODE, "bank 1's $8005 did");
    assert_eq!(log.prg_flags[0x8006], OPCODE);
    assert_eq!(log.prg_flags[0x8007], OPERAND);
}

/// Law 4: arming changes nothing the machine can observe.
#[test]
fn armed_and_unarmed_runs_are_bit_identical() {
    let run = |armed: bool| {
        let mut core = NesCore::from_ines_bytes(&ines(0, &nrom_program(), 1)).unwrap();
        core.set_coverage_recording(armed);
        let mut sink = Hasher::new();
        run_frames(&mut core, 120, &mut sink);
        let regs = *core.cpu();
        let wram = core.state_view().wram.to_vec();
        (regs, wram, sink.h)
    };
    let (off_regs, off_wram, off_hash) = run(false);
    let (on_regs, on_wram, on_hash) = run(true);
    assert_eq!(off_regs, on_regs);
    assert_eq!(off_wram, on_wram);
    assert_eq!(off_hash, on_hash);
    assert_ne!(off_hash, Hasher::new().h, "the run produced output");
}
