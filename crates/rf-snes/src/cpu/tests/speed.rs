//! Tests for the 5A22 memory-speed model (ticket W6-01b).

use super::super::speed::{access_cycles, AccessCost, FAST, MASTER_CLOCK_HZ, SLOW, XSLOW};
use super::super::{Cpu, CpuBus, FlatBus};

/// The compact form bsnes/higan uses, reproduced here as an INDEPENDENT
/// second opinion.
///
/// This is not the implementation and must never become it: the point is
/// that a readable region table and an opaque bit trick, written from
/// different descriptions of the same hardware, agree on all 16,777,216
/// addresses. Either alone could be confidently wrong; agreeing on every
/// address is much harder to do by accident than agreeing on the dozen
/// boundaries a hand-written test would think to check.
fn bit_trick(addr: u32, fast_rom: bool) -> u8 {
    if addr & 0x0040_8000 != 0 {
        if addr & 0x0080_0000 != 0 {
            return if fast_rom { 6 } else { 8 };
        }
        return 8;
    }
    if (addr.wrapping_add(0x6000)) & 0x4000 != 0 {
        return 8;
    }
    if (addr.wrapping_sub(0x4000)) & 0x7E00 != 0 {
        return 6;
    }
    12
}

#[test]
fn region_table_agrees_with_the_bit_trick_on_every_address() {
    for fast_rom in [false, true] {
        for addr in 0..=0x00FF_FFFFu32 {
            let ours = access_cycles(addr, fast_rom);
            let theirs = bit_trick(addr, fast_rom);
            assert_eq!(
                ours, theirs,
                "addr {addr:#08X} fast_rom={fast_rom}: table says {ours}, bit trick says {theirs}"
            );
        }
    }
}

#[test]
fn the_documented_regions_have_the_documented_costs() {
    // Table rows from the module doc, checked one by one so a failure
    // names the region rather than an address.
    for (addr, want, what) in [
        (0x00_0000, SLOW, "WRAM mirror"),
        (0x00_1FFF, SLOW, "WRAM mirror, last byte"),
        (0x00_2100, FAST, "PPU registers"),
        (0x00_3FFF, FAST, "APU region, last byte"),
        (0x00_4000, XSLOW, "joypad, first byte"),
        (0x00_41FF, XSLOW, "joypad, last byte"),
        (0x00_4200, FAST, "CPU registers, first byte"),
        (0x00_5FFF, FAST, "CPU/DMA region, last byte"),
        (0x00_6000, SLOW, "expansion"),
        (0x00_8000, SLOW, "SlowROM"),
        (0x40_0000, SLOW, "bank $40"),
        (0x7E_0000, SLOW, "WRAM"),
        (0x7F_FFFF, SLOW, "WRAM, last byte"),
    ] {
        assert_eq!(
            access_cycles(addr, false),
            want,
            "{what} at {addr:#08X} with FastROM off"
        );
        assert_eq!(
            access_cycles(addr, true),
            want,
            "{what} at {addr:#08X} — FastROM must not affect this region"
        );
    }
}

#[test]
fn fastrom_applies_only_above_8000_in_banks_80_and_up() {
    // Where it DOES apply.
    for addr in [0x80_8000, 0x80_FFFF, 0xBF_9000, 0xC0_0000, 0xFF_FFFF] {
        assert_eq!(access_cycles(addr, false), SLOW, "{addr:#08X} SlowROM");
        assert_eq!(access_cycles(addr, true), FAST, "{addr:#08X} FastROM");
    }
    // Where it does NOT: the register window of banks $80-$BF mirrors
    // $00-$3F and is unaffected, as is every bank below $80.
    for addr in [
        0x80_2100, 0x80_4000, 0x80_0000, 0x00_8000, 0x3F_8000, 0x40_8000,
    ] {
        assert_eq!(
            access_cycles(addr, false),
            access_cycles(addr, true),
            "{addr:#08X} must not depend on FastROM"
        );
    }
}

/// The joypad region is the only 12 on the machine — asserted globally
/// rather than trusted, because it is the row most likely to be widened
/// by accident (it is 512 bytes inside a 8 KiB register window).
#[test]
fn twelve_cycles_occurs_only_in_the_joypad_window() {
    for addr in 0..=0x00FF_FFFFu32 {
        if access_cycles(addr, false) == XSLOW {
            let bank = (addr >> 16) as u8;
            let offset = addr as u16;
            assert!(
                (bank < 0x40 || (0x80..0xC0).contains(&bank))
                    && (0x4000..=0x41FF).contains(&offset),
                "{addr:#08X} costs 12 but is outside the joypad window"
            );
        }
    }
}

#[test]
fn master_clock_is_the_ntsc_snes_rate() {
    assert_eq!(MASTER_CLOCK_HZ, 21_477_270);
    // A scanline is 1364 master cycles; 262 lines a frame. Pins the units
    // as MASTER cycles rather than CPU cycles, which is the whole point.
    let frame = 1364u64 * 262;
    let fps = f64::from(MASTER_CLOCK_HZ) / frame as f64;
    assert!(
        (fps - 60.0).abs() < 0.1,
        "master clock over a 1364x262 frame should be ~60 Hz, got {fps}"
    );
}

#[test]
fn the_cost_wrapper_charges_real_instruction_accesses() {
    // LDA #$42 in bank $00 (SlowROM): two fetches at 8 each.
    let mut bus = FlatBus::new();
    bus.mem[0x00_8000] = 0xA9;
    bus.mem[0x00_8001] = 0x42;
    let mut cpu = Cpu::new();
    cpu.pc = 0x8000;

    let mut counting = AccessCost::new(&mut bus, false);
    cpu.step(&mut counting).expect("LDA is implemented");
    assert_eq!(counting.accesses, 2, "opcode plus one immediate byte");
    assert_eq!(counting.master_cycles, 16, "two SlowROM fetches at 8 each");
    assert_eq!(cpu.a & 0xFF, 0x42);
}

#[test]
fn fastrom_makes_the_same_instruction_cheaper_in_a_fast_bank() {
    let mut bus = FlatBus::new();
    bus.mem[0x80_8000] = 0xA9;
    bus.mem[0x80_8001] = 0x42;

    let mut costs = Vec::new();
    for fast_rom in [false, true] {
        let mut cpu = Cpu::new();
        cpu.pbr = 0x80;
        cpu.pc = 0x8000;
        let mut counting = AccessCost::new(&mut bus, fast_rom);
        cpu.step(&mut counting).expect("LDA is implemented");
        costs.push(counting.master_cycles);
    }
    assert_eq!(costs, vec![16, 12], "SlowROM 2x8 then FastROM 2x6");
}

/// Anti-vacuity: the wrapper must not be a pass-through that always
/// reports zero, and `peek` must stay free.
#[test]
fn peek_is_not_charged_but_read_is() {
    let mut bus = FlatBus::new();
    let mut counting = AccessCost::new(&mut bus, false);

    let _ = counting.peek(0x00_4000);
    assert_eq!(
        (counting.accesses, counting.master_cycles),
        (0, 0),
        "peek is a debugger read, not a bus access"
    );

    let _ = counting.read(0x00_4000);
    assert_eq!(
        (counting.accesses, counting.master_cycles),
        (1, 12),
        "a real read of the joypad window costs 12"
    );
}
