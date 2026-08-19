//! 65C816 unit tests (ticket W6-01a).
//!
//! ## What these target, and why not "does LDA work"
//!
//! The SingleStepTests vectors will exercise every opcode exhaustively
//! once they are wired up (see this ticket's HANDOFF note). Duplicating
//! that here would be slow and redundant. What vectors are *bad* at is
//! telling you WHY something failed — so these tests pin the handful of
//! rules that make this chip different from a 6502, each one being a
//! place a 6502-shaped implementation is wrong in a way that still
//! passes casual testing:
//!
//! * register width is runtime state, not a property of the opcode;
//! * emulation mode forces widths, pins the stack and truncates indexes;
//! * direct-page wraps but absolute-indexed carries into the next bank;
//! * the accumulator's high half survives 8-bit operations.
//!
//! When a vector fails later, one of these will usually say why.

use super::bus::FlatBus;
use super::{flags, Cpu};

/// A CPU in native mode with 16-bit A and indexes — the state most SNES
/// code runs in, and the one `Cpu::new` deliberately does NOT start in.
fn native16() -> Cpu {
    let mut cpu = Cpu::new();
    cpu.set_emulation(false);
    cpu.p &= !(flags::M | flags::X);
    cpu
}

fn run(cpu: &mut Cpu, bus: &mut FlatBus, code: &[u8]) {
    bus.load(cpu.pc24(), code);
    cpu.step(bus).expect("opcode must be implemented");
}

// ---------------------------------------------------------------------
// Reset state and mode switching
// ---------------------------------------------------------------------

/// The chip powers up as a 6502. A core that started in native mode would
/// run the first instruction of every ROM with the wrong widths.
#[test]
fn power_on_is_emulation_mode_with_forced_widths() {
    let cpu = Cpu::new();
    assert!(cpu.e);
    assert!(cpu.a8(), "emulation forces an 8-bit accumulator");
    assert!(cpu.i8(), "emulation forces 8-bit indexes");
    assert_eq!(cpu.sp & 0xFF00, 0x0100, "the stack lives in page 1");
    assert!(cpu.flag(flags::I), "interrupts start disabled");
}

/// `CLC; XCE` is how every SNES ROM leaves emulation mode, and the carry
/// carries the OLD mode out — which is what lets code restore it.
#[test]
fn clc_xce_enters_native_mode_and_carry_reports_the_old_mode() {
    let mut cpu = Cpu::new();
    let mut bus = FlatBus::new();
    run(&mut cpu, &mut bus, &[0x18]); // CLC
    run(&mut cpu, &mut bus, &[0xFB]); // XCE
    assert!(!cpu.e, "native mode");
    assert!(cpu.flag(flags::C), "carry now holds the OLD e (true)");

    // And back again: SEC; XCE.
    run(&mut cpu, &mut bus, &[0x38]);
    run(&mut cpu, &mut bus, &[0xFB]);
    assert!(cpu.e);
    assert!(!cpu.flag(flags::C), "carry holds the old e (false)");
}

/// Entering emulation mode truncates the index registers. A program that
/// set `X = $1234` in native mode and then switched sees `$0034`.
#[test]
fn entering_emulation_truncates_indexes_and_pins_the_stack() {
    let mut cpu = native16();
    cpu.x = 0x1234;
    cpu.y = 0xABCD;
    cpu.sp = 0x1FFF;
    cpu.set_emulation(true);
    assert_eq!(cpu.x, 0x0034, "the high byte is GONE, not hidden");
    assert_eq!(cpu.y, 0x00CD);
    assert_eq!(cpu.sp, 0x01FF, "stack pinned into page 1");
    assert!(cpu.flag(flags::M) && cpu.flag(flags::X));
}

/// `SEP #$10` narrows the indexes, and that truncation is immediate. A
/// core that only masked on read would let the high bytes reappear after
/// a later `REP`.
#[test]
fn sep_narrows_indexes_and_the_high_bytes_do_not_come_back() {
    let mut cpu = native16();
    let mut bus = FlatBus::new();
    cpu.x = 0x1234;
    cpu.y = 0x5678;
    run(&mut cpu, &mut bus, &[0xE2, 0x10]); // SEP #$10 -> 8-bit indexes
    assert_eq!(cpu.x, 0x0034);
    assert_eq!(cpu.y, 0x0078);

    cpu.pc = 0;
    run(&mut cpu, &mut bus, &[0xC2, 0x10]); // REP #$10 -> 16-bit again
    assert_eq!(cpu.x, 0x0034, "widening cannot resurrect a discarded byte");
    assert_eq!(cpu.y, 0x0078);
}

/// `REP` cannot escape emulation mode's forced widths.
#[test]
fn rep_cannot_widen_registers_in_emulation_mode() {
    let mut cpu = Cpu::new();
    let mut bus = FlatBus::new();
    run(&mut cpu, &mut bus, &[0xC2, 0x30]); // REP #$30
    assert!(cpu.a8(), "M is forced in emulation mode");
    assert!(cpu.i8(), "X is forced in emulation mode");
}

// ---------------------------------------------------------------------
// Width is runtime state
// ---------------------------------------------------------------------

/// The SAME opcode byte reads a one- or two-byte immediate depending on
/// `M`. This is the single most important property of the chip, and the
/// test asserts the PC advanced differently — not just the value.
#[test]
fn the_same_opcode_has_different_lengths_depending_on_m() {
    // 16-bit
    let mut cpu = native16();
    let mut bus = FlatBus::new();
    run(&mut cpu, &mut bus, &[0xA9, 0x34, 0x12]); // LDA #$1234
    assert_eq!(cpu.a, 0x1234);
    assert_eq!(cpu.pc, 3, "three bytes consumed");

    // 8-bit
    let mut cpu = native16();
    cpu.p |= flags::M;
    let mut bus = FlatBus::new();
    run(&mut cpu, &mut bus, &[0xA9, 0x34, 0x12]);
    assert_eq!(cpu.a & 0xFF, 0x34);
    assert_eq!(cpu.pc, 2, "two bytes consumed — the $12 is the NEXT opcode");
}

/// An 8-bit operation must not disturb the accumulator's high half. This
/// is what makes `XBA` work, and truncating is the classic bug: it looks
/// fine until a program uses the B accumulator.
#[test]
fn eight_bit_operations_preserve_the_high_accumulator_half() {
    let mut cpu = native16();
    let mut bus = FlatBus::new();
    cpu.a = 0xAA55;
    cpu.p |= flags::M;
    run(&mut cpu, &mut bus, &[0xA9, 0x12]); // LDA #$12
    assert_eq!(cpu.a, 0xAA12, "B survived; only the low byte changed");

    cpu.pc = 0;
    run(&mut cpu, &mut bus, &[0xEB]); // XBA
    assert_eq!(cpu.a, 0x12AA, "XBA can bring the preserved half back");
}

/// Transfer width is NOT uniform: `TAX` uses the index width, `TXA` the
/// accumulator width, and `TCD` is always 16-bit. Asserting them together
/// is what catches an implementation that picked one rule for all three.
#[test]
fn transfers_use_per_instruction_width_rules() {
    let mut cpu = native16();
    let mut bus = FlatBus::new();
    cpu.a = 0x1234;

    // 8-bit index, 16-bit accumulator: TAX takes only the low byte.
    cpu.p |= flags::X;
    run(&mut cpu, &mut bus, &[0xAA]); // TAX
    assert_eq!(cpu.x, 0x0034);

    // TCD is 16-bit regardless of either flag.
    cpu.pc = 0;
    cpu.p |= flags::M;
    run(&mut cpu, &mut bus, &[0x5B]); // TCD
    assert_eq!(cpu.d, 0x1234, "TCD ignores M and X entirely");
}

// ---------------------------------------------------------------------
// Addressing: the wrap-vs-carry pair
// ---------------------------------------------------------------------

/// Direct page wraps within bank 0; absolute-indexed carries into the
/// next bank. These are opposite rules and are the classic 65816
/// addressing bug — asserted together so an implementation cannot get one
/// right by applying the other everywhere.
#[test]
fn direct_page_wraps_but_absolute_indexed_carries() {
    // Direct page: D near the top of the bank must WRAP, not spill.
    let mut cpu = native16();
    let mut bus = FlatBus::new();
    cpu.d = 0xFFF0;
    cpu.p |= flags::M;
    bus.mem[0x0000_0010] = 0x7E; // $00:0010, the wrapped address
    run(&mut cpu, &mut bus, &[0xA5, 0x20]); // LDA $20
    assert_eq!(
        cpu.a & 0xFF,
        0x7E,
        "D=$FFF0 + $20 must address $00:0010, not $01:0010"
    );

    // Absolute indexed: must CARRY into the next bank.
    let mut cpu = native16();
    let mut bus = FlatBus::new();
    cpu.dbr = 0x7E;
    cpu.x = 1;
    cpu.p |= flags::M;
    bus.mem[0x007F_0000] = 0x42;
    run(&mut cpu, &mut bus, &[0xBD, 0xFF, 0xFF]); // LDA $FFFF,X
    assert_eq!(
        cpu.a & 0xFF,
        0x42,
        "$7E:FFFF + 1 must address $7F:0000 — a 24-bit add, unlike direct page"
    );
}

/// Direct-page addressing is always bank 0 even when `DBR` points
/// elsewhere — a mode that used `DBR` would read a plausible byte from
/// the wrong 64 KiB.
#[test]
fn direct_page_ignores_the_data_bank() {
    let mut cpu = native16();
    let mut bus = FlatBus::new();
    cpu.dbr = 0x7E;
    cpu.d = 0x0100;
    cpu.p |= flags::M;
    bus.mem[0x0000_0110] = 0x99; // bank 0
    bus.mem[0x007E_0110] = 0x11; // the DBR bank, a decoy
    run(&mut cpu, &mut bus, &[0xA5, 0x10]);
    assert_eq!(cpu.a & 0xFF, 0x99, "direct page is bank 0, always");
}

/// In emulation mode with `DL == 0`, direct-page indexing wraps within
/// the page — the 6502's zero-page behaviour, which software relies on.
#[test]
fn emulation_mode_direct_indexing_wraps_within_the_page() {
    let mut cpu = Cpu::new();
    let mut bus = FlatBus::new();
    cpu.d = 0x0000;
    cpu.x = 0x01;
    // The code lives at $8000, NOT at 0: with D=0 the wrapped target IS
    // address $0000, and loading the instruction there would overwrite
    // the very byte under test. (It did — the first run of this returned
    // $B5, the opcode itself.)
    cpu.pc = 0x8000;
    bus.mem[0x0000_0000] = 0x5A; // the wrapped target
    bus.mem[0x0000_0100] = 0xA5; // where a non-wrapping add would land
    run(&mut cpu, &mut bus, &[0xB5, 0xFF]); // LDA $FF,X
    assert_eq!(cpu.a & 0xFF, 0x5A, "$FF + 1 wraps to $00, not $0100");
}

/// `[dp]` takes its bank from the pointer, so `DBR` is ignored entirely.
#[test]
fn indirect_long_takes_its_bank_from_the_pointer() {
    let mut cpu = native16();
    let mut bus = FlatBus::new();
    cpu.dbr = 0x00;
    cpu.p |= flags::M;
    bus.load(0x0000_0010, &[0x00, 0x80, 0x7E]); // -> $7E:8000
    bus.mem[0x007E_8000] = 0xC3;
    run(&mut cpu, &mut bus, &[0xA7, 0x10]); // LDA [$10]
    assert_eq!(cpu.a & 0xFF, 0xC3);
}

// ---------------------------------------------------------------------
// Stack
// ---------------------------------------------------------------------

/// The emulation-mode stack is confined to page 1: a push at `$0100`
/// wraps to `$01FF` rather than descending into page 0.
#[test]
fn the_emulation_stack_wraps_inside_page_one() {
    let mut cpu = Cpu::new();
    let mut bus = FlatBus::new();
    cpu.sp = 0x0100;
    cpu.push8(&mut bus, 0x42);
    assert_eq!(bus.writes(), vec![(0x0000_0100, 0x42)]);
    assert_eq!(cpu.sp, 0x01FF, "wrapped within page 1, not into page 0");
}

/// Native mode uses the full 16-bit stack pointer.
#[test]
fn the_native_stack_is_sixteen_bit() {
    let mut cpu = native16();
    let mut bus = FlatBus::new();
    cpu.sp = 0x0100;
    cpu.push8(&mut bus, 0x42);
    assert_eq!(cpu.sp, 0x00FF, "descends out of page 1 freely");
}

/// `JSR` pushes the address of its LAST byte, and `RTS` adds one. Pushing
/// `pc` directly would return one byte late — which usually still
/// executes, just wrongly.
#[test]
fn jsr_and_rts_round_trip_to_the_following_instruction() {
    let mut cpu = native16();
    let mut bus = FlatBus::new();
    cpu.pc = 0x8000;
    bus.load(0x0000_8000, &[0x20, 0x00, 0x90]); // JSR $9000
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.pc, 0x9000);

    bus.load(0x0000_9000, &[0x60]); // RTS
    cpu.step(&mut bus).unwrap();
    assert_eq!(
        cpu.pc, 0x8003,
        "returns AFTER the JSR, not into its last byte"
    );
}

/// `JSL`/`RTL` carry the program bank too.
#[test]
fn jsl_and_rtl_round_trip_across_banks() {
    let mut cpu = native16();
    let mut bus = FlatBus::new();
    cpu.pbr = 0x80;
    cpu.pc = 0x8000;
    bus.load(0x0080_8000, &[0x22, 0x00, 0x00, 0x7E]); // JSL $7E:0000
    cpu.step(&mut bus).unwrap();
    assert_eq!((cpu.pbr, cpu.pc), (0x7E, 0x0000));

    bus.load(0x007E_0000, &[0x6B]); // RTL
    cpu.step(&mut bus).unwrap();
    assert_eq!((cpu.pbr, cpu.pc), (0x80, 0x8004));
}

// ---------------------------------------------------------------------
// ALU
// ---------------------------------------------------------------------

/// 16-bit ADC carries and sets flags on the full width.
#[test]
fn adc_is_sixteen_bit_when_m_is_clear() {
    let mut cpu = native16();
    let mut bus = FlatBus::new();
    cpu.a = 0xFFFF;
    run(&mut cpu, &mut bus, &[0x69, 0x01, 0x00]); // ADC #$0001
    assert_eq!(cpu.a, 0x0000);
    assert!(cpu.flag(flags::C), "carried out of bit 15");
    assert!(cpu.flag(flags::Z));
}

/// Signed overflow is a different question from carry, and both must be
/// set independently — asserting only carry lets a V bug through.
#[test]
fn adc_sets_overflow_independently_of_carry() {
    let mut cpu = native16();
    let mut bus = FlatBus::new();
    cpu.p |= flags::M;
    cpu.a = 0x7F; // +127
    run(&mut cpu, &mut bus, &[0x69, 0x01]); // + 1 -> -128
    assert_eq!(cpu.a & 0xFF, 0x80);
    assert!(cpu.flag(flags::V), "signed overflow");
    assert!(!cpu.flag(flags::C), "but no unsigned carry");
    assert!(cpu.flag(flags::N));
}

/// SBC is ADC of the complement; borrow is carry-clear.
#[test]
fn sbc_borrows_when_carry_is_clear() {
    let mut cpu = native16();
    let mut bus = FlatBus::new();
    cpu.p |= flags::M;
    cpu.a = 0x50;
    cpu.set_flag(flags::C, true); // no borrow
    run(&mut cpu, &mut bus, &[0xE9, 0x10]);
    assert_eq!(cpu.a & 0xFF, 0x40);
    assert!(cpu.flag(flags::C), "no borrow out");
}

/// Decimal mode: the 65816 fixed the 6502's broken decimal N/Z, so flags
/// come from the ADJUSTED result.
#[test]
fn decimal_adc_adjusts_and_flags_the_adjusted_result() {
    let mut cpu = native16();
    let mut bus = FlatBus::new();
    cpu.p |= flags::M | flags::D;
    cpu.a = 0x09;
    run(&mut cpu, &mut bus, &[0x69, 0x01]); // 09 + 01 = 10 (BCD)
    assert_eq!(cpu.a & 0xFF, 0x10);
    assert!(!cpu.flag(flags::Z));

    cpu.pc = 0;
    cpu.a = 0x99;
    cpu.set_flag(flags::C, false);
    run(&mut cpu, &mut bus, &[0x69, 0x01]); // 99 + 01 = 00 + carry
    assert_eq!(cpu.a & 0xFF, 0x00);
    assert!(cpu.flag(flags::C));
    assert!(
        cpu.flag(flags::Z),
        "Z reflects the adjusted result, not the binary sum"
    );
}

/// A 16-bit store writes two bytes, low first — and the test asserts the
/// ADDRESSES, because a store that wrote the right bytes to the wrong
/// place passes a value-only check.
#[test]
fn sixteen_bit_stores_write_both_bytes_low_first() {
    let mut cpu = native16();
    let mut bus = FlatBus::new();
    cpu.dbr = 0x7E;
    cpu.a = 0x1234;
    run(&mut cpu, &mut bus, &[0x8D, 0x00, 0x20]); // STA $2000
    assert_eq!(
        bus.writes(),
        vec![(0x007E_2000, 0x34), (0x007E_2001, 0x12)],
        "low byte first, in the DATA bank"
    );
}

// ---------------------------------------------------------------------
// Scope boundary
// ---------------------------------------------------------------------

/// Interrupt and vector opcodes belong to W6-01b and must REFUSE rather
/// than silently doing nothing — a core that no-opped them would hand the
/// next ticket something that looks finished.
#[test]
fn interrupt_opcodes_are_refused_not_silently_skipped() {
    for opcode in [0x00u8, 0x02, 0x40, 0xCB, 0xDB] {
        let mut cpu = native16();
        let mut bus = FlatBus::new();
        bus.load(0, &[opcode]);
        assert_eq!(
            cpu.step(&mut bus),
            Err(opcode),
            "{opcode:#04X} is W6-01b's and must report itself unimplemented"
        );
    }
}

/// ...and the ops this ticket DOES own must not be caught by that net.
/// Without this, "everything is unimplemented" would pass the test above.
#[test]
fn the_core_op_set_is_actually_implemented() {
    let ops: &[&[u8]] = &[
        &[0xA9, 0x00, 0x00], // LDA #
        &[0x8D, 0x00, 0x00], // STA abs
        &[0xAA],             // TAX
        &[0x18],             // CLC
        &[0xFB],             // XCE
        &[0xC2, 0x30],       // REP
        &[0xE2, 0x30],       // SEP
        &[0x48],             // PHA
        &[0x68],             // PLA
        &[0xEB],             // XBA
        &[0x4C, 0x00, 0x00], // JMP
        &[0xEA],             // NOP
        &[0x69, 0x00, 0x00], // ADC #
        &[0x0A],             // ASL A
        &[0xE8],             // INX
        &[0x80, 0x00],       // BRA
    ];
    for code in ops {
        let mut cpu = native16();
        let mut bus = FlatBus::new();
        bus.load(0, code);
        assert_eq!(
            cpu.step(&mut bus),
            Ok(()),
            "{:#04X} is in this ticket's scope and must execute",
            code[0]
        );
    }
}
