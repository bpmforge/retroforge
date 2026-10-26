//! The coverage recorder and the 65816 atlas, end to end on synthetic
//! cartridges whose every executed byte is known in advance (ticket W36-08).

use rf_cart::SnesMapMode;
use rf_core_api::coverage::{mode_bit, MODE_BITS, OPCODE, OPERAND};
use rf_core_api::{CoverageLog, IndirectKind, InterruptKind};
use rf_rebuild::{EdgeKind, Flow, Isa as _, Provenance, Target, Why};

use crate::rebuild::{build_atlas, Wdc65816};
use crate::system::SnesSystem;

/// Decode modes (see `crate::coverage::mode_index`).
const EMU: u8 = 0;
const N16: u8 = 1; // native, M and X 16-bit
const M8_X16: u8 = 2;
const N8: u8 = 4; // native, M and X 8-bit

/// A 64 KiB LoROM image: `code` at `$00:8000`, `extra` at the given ROM
/// offsets, the reset vector at `$8000`, plus the given vectors
/// (`(address, target)` pairs, `$FFxx`).
fn lorom(code: &[u8], extra: &[(usize, &[u8])], vectors: &[(u16, u16)]) -> Vec<u8> {
    let mut d = vec![0u8; 0x1_0000];
    let base = 0x7FC0;
    d[base + 0x15] = 0x20;
    d[base + 0x17] = 6;
    d[base + 0x18] = 3;
    let checksum: u16 = 0xBEEF;
    d[base + 0x1C..base + 0x1E].copy_from_slice(&(checksum ^ 0xFFFF).to_le_bytes());
    d[base + 0x1E..base + 0x20].copy_from_slice(&checksum.to_le_bytes());
    d[0x7FFC..0x7FFE].copy_from_slice(&0x8000u16.to_le_bytes());
    for &(at, target) in vectors {
        let o = usize::from(at) - 0x8000;
        d[o..o + 2].copy_from_slice(&target.to_le_bytes());
    }
    d[..code.len()].copy_from_slice(code);
    for &(at, bytes) in extra {
        d[at..at + bytes.len()].copy_from_slice(bytes);
    }
    d
}

fn run(img: &[u8], steps: usize, recording: bool) -> (SnesSystem, Option<CoverageLog>) {
    let mut s = SnesSystem::load(img).expect("loads");
    s.set_coverage(recording);
    s.reset();
    for _ in 0..steps {
        s.step().expect("implemented");
    }
    let log = s.take_coverage();
    (s, log)
}

fn recorded(img: &[u8], steps: usize) -> CoverageLog {
    run(img, steps, true).1.expect("armed")
}

/// FNV-1a over everything the machine's simulation owns that these programs
/// can touch.
fn fingerprint(s: &SnesSystem) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut feed = |b: u8| {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    };
    let c = &s.cpu;
    for v in [c.a, c.x, c.y, c.sp, c.d, c.pc] {
        v.to_le_bytes().into_iter().for_each(&mut feed);
    }
    [c.dbr, c.pbr, c.p, u8::from(c.e), u8::from(c.stopped)]
        .into_iter()
        .for_each(&mut feed);
    s.master_cycles
        .to_le_bytes()
        .into_iter()
        .for_each(&mut feed);
    s.bus
        .timing
        .frame
        .to_le_bytes()
        .into_iter()
        .for_each(&mut feed);
    s.bus.wram.iter().copied().for_each(&mut feed);
    s.bus.ppu.vram.iter().copied().for_each(&mut feed);
    s.bus.ppu.oam.iter().copied().for_each(&mut feed);
    for w in s.bus.ppu.cgram {
        w.to_le_bytes().into_iter().for_each(&mut feed);
    }
    h
}

/// Straight line through every width change, a long call, a table call, a
/// table jump and a long-pointer jump, ending in a loop.
///
/// ```text
/// 8000 18 FB           CLC XCE          -> native, mode 4
/// 8002 C2 30           REP #$30         -> mode 1
/// 8004 A9 34 12        LDA #$1234       (3 bytes)
/// 8007 E2 20           SEP #$20         -> mode 2
/// 8009 A9 56           LDA #$56         (2 bytes)
/// 800B A2 00 00        LDX #$0000       (3 bytes: X is 16-bit)
/// 800E 22 00 90 00     JSL $009000
/// 8012 FC 20 80        JSR ($8020,X)    -> $9100
/// 8015 7C 22 80        JMP ($8022,X)    -> $9200
/// 9000 6B              RTL
/// 9100 60              RTS
/// 9200 DC 30 80        JML [$8030]      -> $00:A000
/// A000 80 FE           BRA *
/// ```
fn widths_program() -> Vec<u8> {
    lorom(
        &[
            0x18, 0xFB, 0xC2, 0x30, 0xA9, 0x34, 0x12, 0xE2, 0x20, 0xA9, 0x56, 0xA2, 0x00, 0x00,
            0x22, 0x00, 0x90, 0x00, 0xFC, 0x20, 0x80, 0x7C, 0x22, 0x80,
        ],
        &[
            (0x20, &[0x00, 0x91]),
            (0x22, &[0x00, 0x92]),
            (0x30, &[0x00, 0xA0, 0x00]),
            (0x1000, &[0x6B]),
            (0x1100, &[0x60]),
            (0x1200, &[0xDC, 0x30, 0x80]),
            (0x2000, &[0x80, 0xFE]),
        ],
        &[],
    )
}

#[test]
fn every_fetch_is_keyed_by_the_width_it_ran_in() {
    let log = recorded(&widths_program(), 40);
    let modes = |o: usize| log.modes_at(o).collect::<Vec<_>>();
    // Reset leaves the chip in emulation mode.
    assert_eq!(modes(0x00), [EMU], "CLC");
    assert_eq!(modes(0x01), [EMU], "XCE");
    assert_eq!(
        modes(0x02),
        [N8],
        "REP after XCE: native, M and X still set"
    );
    assert_eq!(modes(0x04), [N16], "LDA #imm16");
    assert_eq!(modes(0x07), [N16], "SEP");
    assert_eq!(modes(0x09), [M8_X16], "LDA #imm8");
    assert_eq!(modes(0x0B), [M8_X16], "LDX #imm16");
    // Opcode flags, then operand flags on the bytes after it.
    let f = |o: usize| log.prg_flags[o];
    assert_eq!(f(0x04) & !MODE_BITS, OPCODE);
    assert_eq!(f(0x05), OPERAND);
    assert_eq!(f(0x06), OPERAND);
    assert_eq!(f(0x09) & !MODE_BITS, OPCODE);
    assert_eq!(f(0x0A), OPERAND);
    assert_eq!(f(0x0B) & !MODE_BITS, OPCODE);
    assert_eq!(f(0x0D), OPERAND);
    // The JSL/JSR/JMP targets ran in the caller's width.
    assert_eq!(modes(0x1000), [M8_X16]);
    assert_eq!(f(0x1000), OPCODE | mode_bit(M8_X16));
    assert_eq!(modes(0x1100), [M8_X16]);
    assert_eq!(modes(0x1200), [M8_X16]);
    assert_eq!(modes(0x2000), [M8_X16]);
    assert!(log.instructions_recorded >= 16);
    assert_eq!((log.coproc_exec, log.other_exec), (0, 0));
}

#[test]
fn indirect_transfers_and_the_reset_entry_are_recorded_with_bank_and_width() {
    let log = recorded(&widths_program(), 40);
    let edge = |kind: IndirectKind| {
        let v: Vec<_> = log.indirect.iter().filter(|e| e.kind == kind).collect();
        assert_eq!(v.len(), 1, "{kind:?}: {v:?}");
        *v[0]
    };
    let rtl = edge(IndirectKind::Rtl);
    assert_eq!(
        (rtl.site.rom, rtl.site.pbr, rtl.site.cpu),
        (Some(0x1000), 0, 0x9000)
    );
    assert_eq!((rtl.target.rom, rtl.target.cpu), (Some(0x12), 0x8012));
    assert_eq!(rtl.target.mode, M8_X16);
    let jsr = edge(IndirectKind::JsrIndirect);
    assert_eq!((jsr.site.rom, jsr.target.rom), (Some(0x12), Some(0x1100)));
    assert_eq!(jsr.target.cpu, 0x9100);
    let rts = edge(IndirectKind::Rts);
    assert_eq!((rts.site.rom, rts.target.rom), (Some(0x1100), Some(0x15)));
    // JMP (abs,X) and JML [abs] are both indirect jumps.
    let jumps: Vec<_> = log
        .indirect
        .iter()
        .filter(|e| e.kind == IndirectKind::JmpIndirect)
        .map(|e| (e.site.rom, e.target.rom))
        .collect();
    assert_eq!(
        jumps,
        [(Some(0x15), Some(0x1200)), (Some(0x1200), Some(0x2000))]
    );
    // The reset vector, in emulation mode.
    assert_eq!(log.interrupts.len(), 1);
    let reset = log.interrupts[0];
    assert_eq!(reset.kind, InterruptKind::Reset);
    assert_eq!(
        (reset.target.rom, reset.target.cpu, reset.target.mode),
        (Some(0), 0x8000, EMU)
    );
}

#[test]
fn nmi_entry_rti_and_brk_cop_through_their_vectors() {
    // Emulation mode throughout: enable NMI, spin; BRK and COP in the
    // handler-less part first.
    //   8000 00 00   BRK #0     -> ($FFFE) $9100: RTI
    //   8002 02 00   COP #0     -> ($FFF4) $9200: RTI
    //   8004 A9 80   LDA #$80
    //   8006 8D 00 42 STA $4200  (NMI on)
    //   8009 80 FE   BRA *
    //   9000 40      RTI        <- NMI ($FFFA)
    let img = lorom(
        &[
            0x00, 0x00, 0x02, 0x00, 0xA9, 0x80, 0x8D, 0x00, 0x42, 0x80, 0xFE,
        ],
        &[(0x1000, &[0x40]), (0x1100, &[0x40]), (0x1200, &[0x40])],
        &[(0xFFFE, 0x9100), (0xFFF4, 0x9200), (0xFFFA, 0x9000)],
    );
    let log = recorded(&img, 200_000);
    let kinds: Vec<_> = log.indirect.iter().map(|e| (e.kind, e.site.rom)).collect();
    assert!(kinds.contains(&(IndirectKind::Brk, Some(0))), "{kinds:?}");
    assert!(kinds.contains(&(IndirectKind::Cop, Some(2))), "{kinds:?}");
    // BRK/COP land on their handlers; the RTIs come back past the
    // signature byte.
    let to = |kind, site: u32| {
        log.indirect
            .iter()
            .find(|e| e.kind == kind && e.site.rom == Some(site))
            .and_then(|e| e.target.rom)
    };
    assert_eq!(to(IndirectKind::Brk, 0), Some(0x1100));
    assert_eq!(to(IndirectKind::Cop, 2), Some(0x1200));
    assert_eq!(to(IndirectKind::Rti, 0x1100), Some(2));
    assert_eq!(to(IndirectKind::Rti, 0x1200), Some(4));
    // The vblank NMI reached its handler, and its RTI went back to the loop.
    let nmi: Vec<_> = log
        .interrupts
        .iter()
        .filter(|i| i.kind == InterruptKind::Nmi)
        .collect();
    assert_eq!(nmi.len(), 1, "{:?}", log.interrupts);
    assert_eq!((nmi[0].target.rom, nmi[0].target.mode), (Some(0x1000), EMU));
    let back = to(IndirectKind::Rti, 0x1000).expect("NMI's RTI");
    assert!(
        back == 9 || back == 0x0A,
        "resumes in the loop, got {back:#x}"
    );
}

#[test]
fn code_run_from_work_ram_is_volatile_not_rom() {
    // Build NOP; BRA * at $7E:0100 byte by byte, then JML there.
    let mut code = Vec::new();
    for (i, b) in [0xEAu8, 0x80, 0xFE].into_iter().enumerate() {
        code.extend([0xA9, b, 0x8F, i as u8, 0x01, 0x7E]);
    }
    code.extend([0x5C, 0x00, 0x01, 0x7E]);
    let log = recorded(&lorom(&code, &[], &[]), 100);
    assert_eq!(log.ram_exec.len(), 128 * 1024);
    assert_eq!(log.ram_exec[0x100] & OPCODE, OPCODE);
    assert_eq!(log.ram_exec[0x101] & OPCODE, OPCODE);
    assert_eq!(log.ram_exec[0x102], OPERAND);
    assert_eq!(log.executed_ram_bytes(), 3);
    // No ROM byte of the RAM routine, and the loop did not get marked as ROM.
    assert_eq!(log.modes_at(0x100).count(), 0);
    let atlas = build_atlas(&lorom(&code, &[], &[]), SnesMapMode::LoRom, &log);
    assert_eq!(
        atlas.volatile,
        [rf_rebuild::VolatileRegion {
            start: 0x100,
            len: 3
        }]
    );
    assert_eq!(atlas.stats.observed_uncovered, 0);
}

#[test]
fn fetches_from_coprocessor_memory_are_counted_not_mapped() {
    let img = lorom(&[0xEA], &[], &[]);
    let mut s = SnesSystem::load(&img).unwrap();
    s.bus
        .install_gsu(rf_cart::SuperFxVersion::Gsu1, img.len(), 32);
    s.set_coverage(true);
    s.reset();
    // The GSU's RAM window: $00:6000. The main CPU runs there (zeroed RAM
    // decodes as BRK) and the recorder counts it.
    s.cpu.pbr = 0;
    s.cpu.pc = 0x6000;
    s.step().unwrap();
    let log = s.take_coverage().unwrap();
    assert_eq!((log.coproc_exec, log.other_exec), (1, 0));
    assert_eq!(log.executed_prg_bytes(), 0);
    assert_eq!(log.executed_ram_bytes(), 0);

    // Plain save-RAM/register space is "other".
    let mut s = SnesSystem::load(&img).unwrap();
    s.set_coverage(true);
    s.reset();
    s.cpu.pbr = 0;
    s.cpu.pc = 0x2100;
    s.step().unwrap();
    let log = s.take_coverage().unwrap();
    assert_eq!((log.coproc_exec, log.other_exec), (0, 1));
}

/// W37-02: while the Super FX runs (GO and RON) the SNES does not read ROM
/// where the map says ROM is, so a fetch there is neither recorded as ROM code
/// nor resolved to a ROM offset; with the chip idle it is ROM again.
#[test]
fn rom_held_by_a_running_gsu_is_not_rom_code() {
    let img = lorom(&[0xEA], &[], &[]);
    let mut s = SnesSystem::load(&img).unwrap();
    s.bus
        .install_gsu(rf_cart::SuperFxVersion::Gsu1, img.len(), 32);
    s.reset();
    assert_eq!(s.bus.rom_offset(0x00_8000), Some(0), "chip idle: ROM");
    let gsu = &mut s.bus.gsu.as_mut().unwrap().regs;
    gsu.write(0x303A, 0x10); // SCMR: RON, the GSU owns the ROM bus
    gsu.write(0x301E, 0x00);
    gsu.write(0x301F, 0x00); // R15 high byte: GO
    assert!(gsu.owns_rom_bus());
    assert_eq!(s.bus.rom_offset(0x00_8000), None, "GO and RON: not ROM");
    s.set_coverage(true);
    s.cpu.pbr = 0;
    s.cpu.pc = 0x8000;
    s.step().unwrap();
    let log = s.take_coverage().unwrap();
    assert_eq!((log.coproc_exec, log.other_exec), (1, 0));
    assert_eq!(log.executed_prg_bytes(), 0, "recorded as no ROM code");
}

/// W37-02: where the static ISA places an address on a Super FX board, it
/// is where the bus reads it (the `$40-$5F` linear window, the GSU's RAM at
/// `$70-$71`), and the other way round.
#[test]
fn the_super_fx_isa_places_addresses_where_the_bus_does() {
    let img = lorom(&[0xEA], &[], &[]);
    let mut s = SnesSystem::load(&img).unwrap();
    s.bus
        .install_gsu(rf_cart::SuperFxVersion::Gsu1, img.len(), 32);
    let chip = rf_cart::Coprocessor::SuperFx {
        version: rf_cart::SuperFxVersion::Gsu1,
        ram_kib: 32,
    };
    let isa = Wdc65816::for_board(SnesMapMode::LoRom, img.len(), chip);
    let plain = Wdc65816::new(SnesMapMode::LoRom, img.len());
    let (mut checked, mut plain_differs) = (0u32, 0u32);
    for bank in 0..=0xFFu32 {
        for offset in (0..=0xFFFFu32).step_by(0x1FD) {
            let addr = (bank << 16) | offset;
            let bus = s.bus.rom_offset(addr).and_then(|o| u32::try_from(o).ok());
            let got = isa.rom_offset(addr);
            // Never a different place than the bus's; the linear window is
            // placed exactly.
            assert!(got.is_none() || got == bus, "{addr:#08x}");
            if (0x40..=0x5F).contains(&bank) {
                assert_eq!(got, bus, "{addr:#08x}");
            }
            plain_differs += u32::from(plain.rom_offset(addr) != bus);
            checked += 1;
        }
    }
    assert!(checked > 8000);
    assert!(
        plain_differs > 0,
        "plain LoROM really is wrong on this board"
    );
}

#[test]
fn a_halted_cpu_fetches_nothing_and_marks_nothing() {
    // WAI with interrupts masked and no source: the CPU parks.
    let img = lorom(&[0xCB, 0xEA], &[], &[]);
    let (_, log) = run(&img, 1000, true);
    let log = log.unwrap();
    assert_eq!(log.instructions_recorded, 1, "only the WAI itself");
    assert_eq!(log.executed_prg_bytes(), 1);
    assert_eq!(log.prg_flags[1], 0, "the NOP after WAI never ran");
}

#[test]
fn armed_and_unarmed_machines_are_bit_identical() {
    for img in [
        widths_program(),
        lorom(
            &[
                0xA9, 0x80, 0x8D, 0x00, 0x42, 0xA9, 0x0F, 0x8D, 0x00, 0x21, 0x80, 0xFE,
            ],
            &[(0x1000, &[0x40])],
            &[(0xFFFA, 0x9000)],
        ),
    ] {
        let (a, log) = run(&img, 300_000, true);
        let (b, none) = run(&img, 300_000, false);
        assert!(none.is_none() && log.is_some());
        assert_eq!(fingerprint(&a), fingerprint(&b));
        assert_eq!(a.bus.timing.frame, b.bus.timing.frame);
        assert!(a.bus.timing.frame >= 2, "ran frames");
    }
}

#[test]
fn recording_survives_in_pieces_and_merges() {
    let (mut s, _) = run(&widths_program(), 10, true);
    let first = s.take_coverage().unwrap();
    for _ in 0..30 {
        s.step().unwrap();
    }
    let second = s.take_coverage().unwrap();
    let mut both = first.clone();
    assert!(both.merge(&second));
    assert!(both.executed_prg_bytes() >= first.executed_prg_bytes());
    assert_eq!(
        both.instructions_recorded,
        first.instructions_recorded + second.instructions_recorded
    );
    // A log for another volatile size never merges.
    assert!(!both.merge(&CoverageLog::new(0x1_0000)));
}

// ---- the atlas ------------------------------------------------------------

fn atlas_of(img: &[u8], log: &CoverageLog) -> rf_rebuild::Atlas {
    build_atlas(img, SnesMapMode::LoRom, log)
}

/// Mark `bytes` of `img` executed at `rom` in `mode` (the opcode and its
/// operand bytes), as the recorder would.
fn mark(log: &mut CoverageLog, rom: usize, len: usize, mode: u8) {
    log.mark_prg(rom, OPCODE | mode_bit(mode));
    for i in 1..len {
        log.mark_prg(rom + i, OPERAND);
    }
}

/// Two callers reach `A9 01 EA 60` with different widths: `LDA #$01; NOP;
/// RTS` at 8 bits, `LDA #$EA01; RTS` at 16.
fn two_widths() -> (Vec<u8>, CoverageLog) {
    //   8000 18 FB        CLC XCE          mode 0 -> 4
    //   8002 20 00 90     JSR $9000        mode 4
    //   8005 C2 30        REP #$30         -> 1
    //   8007 20 00 90     JSR $9000        mode 1
    //   800A 80 FE        BRA *
    //   9000 A9 01 EA 60
    let img = lorom(
        &[
            0x18, 0xFB, 0x20, 0x00, 0x90, 0xC2, 0x30, 0x20, 0x00, 0x90, 0x80, 0xFE,
        ],
        &[(0x1000, &[0xA9, 0x01, 0xEA, 0x60])],
        &[],
    );
    let mut log = CoverageLog::with_volatile_len(img.len(), 128 * 1024);
    for (rom, len, mode) in [
        (0, 1, EMU),
        (1, 1, EMU),
        (2, 3, N8),
        (5, 2, N8),
        (7, 3, N16),
        (0xA, 2, N16),
        // 8-bit entry: LDA #$01 (2), NOP, RTS.
        (0x1000, 2, N8),
        (0x1002, 1, N8),
        (0x1003, 1, N8),
        // 16-bit entry: LDA #$EA01 (3), RTS.
        (0x1000, 3, N16),
        (0x1003, 1, N16),
    ] {
        mark(&mut log, rom, len, mode);
    }
    let at = |cpu: u16, rom: u32, mode: u8| rf_core_api::CodeAddr {
        cpu,
        pbr: 0,
        mode,
        rom: Some(rom),
    };
    log.note_interrupt(rf_core_api::InterruptEntry {
        kind: InterruptKind::Reset,
        target: at(0x8000, 0, EMU),
    });
    for (target, mode) in [(5, N8), (0xA, N16)] {
        log.note_indirect(rf_core_api::IndirectEdge {
            site: at(0x9003, 0x1003, mode),
            target: at(0x8000 + target as u16, target, mode),
            kind: IndirectKind::Rts,
        });
    }
    (img, log)
}

#[test]
fn the_same_bytes_entered_with_two_widths_are_two_functions() {
    let (img, log) = two_widths();
    let atlas = atlas_of(&img, &log);
    assert_eq!(atlas.mode_count, 5);
    let entries: Vec<_> = atlas
        .functions
        .iter()
        .filter(|f| f.entry_rom == 0x1000)
        .map(|f| (f.entry_mode, f.insn_count, f.byte_len))
        .collect();
    assert_eq!(
        entries,
        [(N16, 2, 4), (N8, 3, 4)],
        "two functions at one offset, sorted by mode"
    );
    let hash = |m: u8| {
        atlas
            .functions
            .iter()
            .find(|f| f.entry_rom == 0x1000 && f.entry_mode == m)
            .map(|f| (f.byte_hash, f.shape_hash))
            .unwrap()
    };
    // The same bytes hash the same: a recipe matches bytes. What differs is
    // how they split into instructions.
    assert_eq!(hash(N8), hash(N16));
    // Both are CALL entries of the code that called them.
    assert!(atlas
        .functions
        .iter()
        .filter(|f| f.entry_rom == 0x1000)
        .all(|f| f.entry_kinds & rf_rebuild::entry::CALL != 0));
    // Every executed (offset, mode) is an instruction start, at its width.
    assert_eq!(atlas.stats.observed_uncovered, 0);
    assert_eq!(atlas.stats.undecodable_observed, 0);
    let starts: std::collections::BTreeSet<_> =
        atlas.insns().map(|i| (i.rom, i.mode, i.len)).collect();
    for (rom, mode, len) in [
        (0x1000, N8, 2),
        (0x1002, N8, 1),
        (0x1000, N16, 3),
        (4, N16, 0),
    ] {
        if len != 0 {
            assert!(starts.contains(&(rom, mode, len)), "{rom:#x} {mode} {len}");
        }
    }
    assert!(
        !starts.contains(&(0x1002, N16, 1)),
        "0x1002 is the middle of the 16-bit LDA"
    );
    assert!(!atlas.stats.budget_hit);
}

#[test]
fn rep_inside_a_block_changes_the_width_of_the_next_decode() {
    // Never executed: only a seed. `C2 30` then `A9 34 12`, `E2 20`,
    // `A9 56`, `60`. The decode after each REP/SEP must use the new width.
    let img = lorom(
        &[
            0xC2, 0x30, 0xA9, 0x34, 0x12, 0xE2, 0x20, 0xA9, 0x56, 0xA2, 0x01, 0x02, 0x60,
        ],
        &[],
        &[],
    );
    let mut log = CoverageLog::with_volatile_len(img.len(), 128 * 1024);
    mark(&mut log, 0, 2, N8); // only the REP ran
    let atlas = atlas_of(&img, &log);
    let got: Vec<_> = atlas
        .insns()
        .filter(|i| i.rom < 0x10)
        .map(|i| (i.rom, i.mode, i.len, i.prov))
        .collect();
    use Provenance::{Inferred as I, Observed as O};
    assert_eq!(
        got,
        [
            (0, N8, 2, O),
            (2, N16, 3, I), // after REP #$30: 16-bit LDA
            (5, N16, 2, I), // SEP #$20
            (7, M8_X16, 2, I),
            (9, M8_X16, 3, I), // LDX #imm16 still 16-bit
            (12, M8_X16, 1, I),
        ]
    );
}

#[test]
fn after_xce_only_the_widths_the_recording_saw_are_followed() {
    // XCE then an LDA: unobserved, so nothing is guessed; observed, so it
    // is decoded in the mode it ran in.
    let img = lorom(&[0xFB, 0xA9, 0x01, 0x02, 0x60], &[], &[]);
    let mut log = CoverageLog::with_volatile_len(img.len(), 128 * 1024);
    mark(&mut log, 0, 1, EMU);
    let atlas = atlas_of(&img, &log);
    assert_eq!(atlas.insns().count(), 1);
    let edges: Vec<_> = atlas
        .blocks
        .iter()
        .flat_map(|b| &b.edges)
        .map(|e| (e.kind, e.to))
        .collect();
    assert_eq!(
        edges,
        [(
            EdgeKind::Fall,
            Target::Unresolved {
                cpu: 0x80_8001,
                why: Why::WidthUnknown
            }
        )]
    );
    // Now it ran, in native 16-bit mode.
    mark(&mut log, 1, 3, N16);
    let atlas = atlas_of(&img, &log);
    let lda = atlas.insns().find(|i| i.rom == 1).unwrap();
    assert_eq!(
        (lda.mode, lda.len, lda.prov),
        (N16, 3, Provenance::Observed)
    );
    let xce = atlas.insns().find(|i| i.rom == 0).unwrap();
    assert_eq!(xce.flow, Flow::Fall);
    // The LDA continues the block XCE started, in the mode it ran in and only
    // that mode.
    assert_eq!(atlas.blocks[0].insns[1].rom, 1);
    assert_eq!(atlas.insns().filter(|i| i.rom == 1).count(), 1);
    // And what follows it (the RTS at 4) is decoded after the 16-bit LDA.
    let rts = atlas
        .insns()
        .find(|i| i.rom == 4)
        .expect("fall-through decoded");
    assert_eq!((rts.mode, rts.prov), (N16, Provenance::Inferred));
}

#[test]
fn a_static_call_in_another_width_is_inferred_only_when_the_bytes_allow_it() {
    let (img, mut log) = two_widths();
    // Forget the 16-bit execution of the callee: the 16-bit caller's JSR now
    // targets bytes that only ran at 8 bits.
    for (o, f) in log.prg_flags.iter_mut().enumerate().skip(0x1000).take(4) {
        *f &= !mode_bit(N16);
        if o == 0x1000 || o == 0x1003 {
            *f |= OPCODE;
        }
    }
    log.prg_flags[0x1001] = OPERAND;
    log.prg_flags[0x1002] = OPCODE | mode_bit(N8);
    let atlas = atlas_of(&img, &log);
    // `LDA #imm` is 2 bytes at 8 bits and 3 at 16: not the same extent as
    // what ran, so the 16-bit copy is refused, not guessed.
    assert!(atlas.stats.overlap_rejected >= 1, "{:?}", atlas.stats);
    let call_edges: Vec<_> = atlas
        .blocks
        .iter()
        .flat_map(|b| &b.edges)
        .filter(|e| e.kind == EdgeKind::Call)
        .map(|e| e.to)
        .collect();
    assert!(call_edges.contains(&Target::Unresolved {
        cpu: 0x9000,
        why: Why::Undecodable
    }));
    assert!(call_edges.iter().any(|t| t.node() == Some((0x1000, N8))));

    // A width-independent first instruction is acceptable in a new width:
    // swap LDA for NOP.
    let mut img2 = img.clone();
    img2[0x1000..0x1004].copy_from_slice(&[0xEA, 0xEA, 0xEA, 0x60]);
    let mut log2 = CoverageLog::with_volatile_len(img2.len(), 128 * 1024);
    for (rom, len, mode) in [
        (0, 1, EMU),
        (1, 1, EMU),
        (2, 3, N8),
        (5, 2, N8),
        (7, 3, N16),
        (0x1000, 1, N8),
        (0x1001, 1, N8),
        (0x1002, 1, N8),
        (0x1003, 1, N8),
    ] {
        mark(&mut log2, rom, len, mode);
    }
    let atlas = atlas_of(&img2, &log2);
    let f: Vec<_> = atlas
        .functions
        .iter()
        .filter(|f| f.entry_rom == 0x1000)
        .map(|f| (f.entry_mode, f.entry_kinds & rf_rebuild::entry::CALL != 0))
        .collect();
    assert_eq!(f, [(N16, true), (N8, true)]);
    let inferred_n16 = atlas
        .insns()
        .filter(|i| i.rom >= 0x1000 && i.mode == N16)
        .all(|i| i.prov == Provenance::Inferred);
    assert!(inferred_n16);
}

#[test]
fn long_calls_and_jumps_resolve_through_the_cartridge_map() {
    // JSL $009000, JML $019000-like targets are 24-bit; JMP stays in bank.
    //   8000 22 00 90 80   JSL $809000   (the $80 alias of bank 0)
    //   8004 5C 00 80 00   JML $008000   (back, bank 0)
    let img = lorom(
        &[0x22, 0x00, 0x90, 0x80, 0x5C, 0x00, 0x80, 0x00],
        &[(0x1000, &[0x6B])],
        &[],
    );
    let mut log = CoverageLog::with_volatile_len(img.len(), 128 * 1024);
    mark(&mut log, 0, 4, N8);
    let atlas = atlas_of(&img, &log);
    let jsl = atlas.insns().find(|i| i.rom == 0).unwrap();
    assert_eq!(jsl.flow, Flow::CallLong { target: 0x80_9000 });
    let rtl = atlas
        .insns()
        .find(|i| i.rom == 0x1000)
        .expect("callee decoded");
    assert_eq!(
        (rtl.flow, rtl.prov),
        (Flow::ReturnLong, Provenance::Inferred)
    );
    // The JML's target is the first instruction again.
    let jml = atlas.insns().find(|i| i.rom == 4).expect("JML decoded");
    assert_eq!(jml.flow, Flow::JumpLong { target: 0x00_8000 });
    let to_zero = atlas
        .blocks
        .iter()
        .flat_map(|b| &b.edges)
        .any(|e| e.kind == EdgeKind::Jump && e.to.node() == Some((0, N8)));
    assert!(to_zero);
    // 16-bit jumps keep the bank of the site: site in $80, target resolved
    // there, same ROM byte as the $00 alias.
    let ctx = Wdc65816::new(SnesMapMode::LoRom, img.len());
    assert_eq!(
        rf_rebuild::Isa::rom_offset(&ctx, 0x80_9000),
        rf_rebuild::Isa::rom_offset(&ctx, 0x00_9000)
    );
}

#[test]
fn hirom_images_resolve_the_same_way() {
    // HiROM: `JSR $8010` from $C0:8000... code lives at ROM 0x8000.
    let mut img = vec![0u8; 0x1_0000];
    img[0xFFC0 + 0x15] = 0x21;
    img[0xFFC0 + 0x17] = 6;
    img[0x8000..0x8003].copy_from_slice(&[0x20, 0x10, 0x80]);
    img[0x8010] = 0x60;
    let mut log = CoverageLog::with_volatile_len(img.len(), 128 * 1024);
    mark(&mut log, 0x8000, 3, N8);
    let atlas = build_atlas(&img, SnesMapMode::HiRom, &log);
    let rts = atlas.insns().find(|i| i.rom == 0x8010).expect("callee");
    assert_eq!(rts.flow, Flow::Return);
    assert_eq!(rts.cpu >> 16 & 0xFF, 0xC0, "canonical HiROM bank");
}

#[test]
fn random_images_terminate_and_decode_every_offset_in_every_mode() {
    // 64 KiB of xorshift noise, no recording: the superset pass and seeded
    // closure must finish, and the superset covers all five modes.
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    let img: Vec<u8> = (0..0x1_0000)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect();
    let mut log = CoverageLog::with_volatile_len(img.len(), 128 * 1024);
    // Pretend a couple of thousand scattered instructions ran, in assorted
    // widths, so the closure has plenty to chase through the noise (and plenty
    // of mutually contradictory executed bytes to survive).
    let isa = Wdc65816::new(SnesMapMode::LoRom, img.len());
    let mut y = 0x1234_5678_9ABC_DEF1u64;
    for _ in 0..2000 {
        y ^= y << 13;
        y ^= y >> 7;
        y ^= y << 17;
        let rom = (y >> 8) as usize % (img.len() - 4);
        let mode = (y % 5) as u8;
        let d = rf_rebuild::Isa::decode(&isa, &img[rom..], 0x8000, rf_rebuild::DecodeMode(mode))
            .unwrap();
        mark(&mut log, rom, usize::from(d.len), mode);
    }
    let atlas = atlas_of(&img, &log);
    assert!(!atlas.stats.budget_hit, "{:?}", atlas.stats);
    assert!(
        atlas.stats.superset_candidates > 4 * 0x1_0000,
        "five modes of ~every offset"
    );
    assert!(atlas.stats.inferred_insns > 1000, "{:?}", atlas.stats);
    assert_eq!(atlas.stats.observed_uncovered, 0);
    // Serialisation round trips.
    assert_eq!(rf_rebuild::Atlas::from_bytes(&atlas.to_bytes()), Ok(atlas));
}

#[test]
fn self_loops_and_fall_through_cycles_terminate() {
    // BRA *, BRL *, JMP *, JML *, and a bank of NOPs that wraps onto itself
    // (a fetch at $xx:FFFF continues at $xx:0000).
    for code in [
        vec![0x80u8, 0xFE],
        vec![0x82, 0xFD, 0xFF],
        vec![0x4C, 0x00, 0x80],
        vec![0x5C, 0x00, 0x80, 0x00],
        vec![0xD0, 0xFE],
    ] {
        let img = lorom(&code, &[], &[]);
        let mut log = CoverageLog::with_volatile_len(img.len(), 128 * 1024);
        mark(&mut log, 0, code.len(), N8);
        let atlas = atlas_of(&img, &log);
        assert!(!atlas.stats.budget_hit);
        assert_eq!(atlas.stats.observed_uncovered, 0, "{code:02x?}");
    }
    let mut img = lorom(&[], &[], &[]);
    img[0x7FF0..0x8000].fill(0xEA); // NOPs running off the end of the image
    img[..0x4000].fill(0xEA);
    img[0x7FFC..0x7FFE].copy_from_slice(&[0xEA, 0xEA]);
    let mut log = CoverageLog::with_volatile_len(img.len(), 128 * 1024);
    mark(&mut log, 0x7FF0, 1, N8);
    let atlas = atlas_of(&img, &log);
    assert!(!atlas.stats.budget_hit);
}

#[test]
fn the_atlas_of_a_recording_is_deterministic_and_carries_no_rom_bytes() {
    let (img, log) = two_widths();
    let a = atlas_of(&img, &log).to_bytes();
    assert_eq!(a, atlas_of(&img, &log).to_bytes());
    let atlas = rf_rebuild::Atlas::from_bytes(&a).unwrap();
    assert_eq!(atlas.prg_len, 0x1_0000);
}
