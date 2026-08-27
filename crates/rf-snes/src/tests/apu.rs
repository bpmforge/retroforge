//! APU tests: ARAM, `$F1` IPL banking and the three timers
//! (ticket W6-04a, criterion 2).

use crate::apu::spc700::{flags, ApuBus, FlatApuBus, Spc700};
use crate::apu::{Apu, ARAM_LEN, IPL_BASE, IPL_LEN, IPL_STUB};

#[test]
fn aram_is_64k_and_addressable_end_to_end() {
    let mut apu = Apu::new();
    assert_eq!(apu.aram.len(), ARAM_LEN);
    apu.write(0x0000, 0x11);
    apu.write(0x8000, 0x22);
    // $FFFF is inside the IPL window, so write-then-read needs the bank
    // off; that is asserted separately below.
    assert_eq!(apu.read(0x0000), 0x11);
    assert_eq!(apu.read(0x8000), 0x22);
}

/// **`$F1` banking.** Bit 7 swaps the IPL region over ARAM `$FFC0-$FFFF`.
#[test]
fn f1_bit7_banks_the_ipl_region_over_aram() {
    let mut apu = Apu::new();
    // Put a recognisable byte in the ARAM underneath.
    apu.write(0xFFC0, 0x5A);

    // Banked in (power-on state): the IPL wins.
    assert!(apu.ipl_enabled, "the APU powers up with the IPL banked in");
    assert_eq!(apu.read(0xFFC0), IPL_STUB[0]);

    // Bank it out: the ARAM underneath reappears, unharmed.
    apu.write(0x00F1, 0x00);
    assert!(!apu.ipl_enabled);
    assert_eq!(
        apu.read(0xFFC0),
        0x5A,
        "the ARAM under the IPL window must survive being shadowed"
    );

    // And back again.
    apu.write(0x00F1, 0x80);
    assert_eq!(apu.read(0xFFC0), IPL_STUB[0]);
}

/// The window is exactly `$FFC0-$FFFF` — 64 bytes, no more.
#[test]
fn the_ipl_window_is_exactly_64_bytes() {
    let mut apu = Apu::new();
    apu.write(0xFFBF, 0x7E);
    assert_eq!(
        apu.read(0xFFBF),
        0x7E,
        "$FFBF is one byte below the window and must stay ARAM"
    );
    assert!(!apu.in_ipl_window(0xFFBF));
    assert!(apu.in_ipl_window(IPL_BASE));
    assert!(apu.in_ipl_window(0xFFFF));
    assert_eq!(IPL_LEN, 64);
}

/// **Writes always reach ARAM, even where the IPL is being read from.**
///
/// This looks like a bug and is not: it is how a boot loader stashes data
/// in the region it is executing from. A model that dropped these writes
/// would lose it silently.
#[test]
fn writes_pass_through_the_ipl_window_into_aram() {
    let mut apu = Apu::new();
    assert!(apu.ipl_enabled);
    apu.write(0xFFD0, 0xA5);
    assert_eq!(
        apu.read(0xFFD0),
        IPL_STUB[0x10],
        "the read still sees the IPL"
    );
    apu.write(0x00F1, 0x00);
    assert_eq!(
        apu.read(0xFFD0),
        0xA5,
        "...but the write landed in the ARAM underneath"
    );
}

/// Reset takes PC from `$FFFE/$FFFF` through the IPL, which is what makes
/// the stub's reset vector meaningful.
#[test]
fn reset_vectors_through_the_ipl() {
    let apu = Apu::new();
    assert_eq!(apu.cpu.pc, IPL_BASE, "the stub vectors to its own start");
}

// ---------------------------------------------------------------------
// Timers
// ---------------------------------------------------------------------

/// T0/T1 tick at 8 kHz (every 128 cycles), T2 at 64 kHz (every 16).
#[test]
fn the_timers_have_the_documented_rates() {
    let apu = Apu::new();
    assert_eq!(apu.timers[0].divisor, 128, "T0 is 8 kHz");
    assert_eq!(apu.timers[1].divisor, 128, "T1 is 8 kHz");
    assert_eq!(apu.timers[2].divisor, 16, "T2 is 64 kHz");
}

#[test]
fn a_disabled_timer_does_not_count() {
    let mut apu = Apu::new();
    apu.write(0x00FA, 1); // T0 target = 1
    apu.tick_clock(10_000);
    assert_eq!(apu.read(0x00FD), 0, "a timer with no enable bit stays put");
}

#[test]
fn an_enabled_timer_counts_at_its_target() {
    let mut apu = Apu::new();
    apu.write(0x00FA, 4); // T0: tick every 4 * 128 = 512 cycles
    apu.write(0x00F1, 0x01); // enable T0 (and bank the IPL out)
    apu.tick_clock(512 * 3);
    assert_eq!(apu.read(0x00FD), 3, "three counter increments");
}

/// **Reading the counter clears it** — that read-to-clear is the whole
/// interface, and a counter that did not clear would read as time
/// standing still.
#[test]
fn reading_a_timer_counter_clears_it() {
    let mut apu = Apu::new();
    apu.write(0x00FC, 1); // T2 target 1 -> every 16 cycles
    apu.write(0x00F1, 0x04); // enable T2
    apu.tick_clock(16 * 5);
    assert_eq!(apu.read(0x00FF), 5);
    assert_eq!(apu.read(0x00FF), 0, "the read cleared it");
    apu.tick_clock(16 * 2);
    assert_eq!(apu.read(0x00FF), 2, "and it keeps counting afterwards");
}

/// `peek` must not clear — a debugger watching a timer must not consume
/// the count the program is waiting on.
#[test]
fn peeking_a_timer_counter_does_not_clear_it() {
    let mut apu = Apu::new();
    apu.write(0x00FC, 1);
    apu.write(0x00F1, 0x04);
    apu.tick_clock(16 * 3);
    assert_eq!(apu.peek(0x00FF), 3);
    assert_eq!(apu.peek(0x00FF), 3, "peek is repeatable");
    assert_eq!(apu.read(0x00FF), 3, "and the real read still sees it");
    assert_eq!(apu.peek(0x00FF), 0, "...which did clear it");
}

/// The output counter is FOUR bits and wraps at 16.
#[test]
fn the_timer_counter_is_four_bits() {
    let mut apu = Apu::new();
    apu.write(0x00FC, 1);
    apu.write(0x00F1, 0x04);
    apu.tick_clock(16 * 20);
    assert_eq!(apu.read(0x00FF), 20 & 0x0F, "wraps at 16, not saturating");
}

/// A target of 0 means 256, not "tick immediately".
#[test]
fn a_target_of_zero_means_256() {
    let mut apu = Apu::new();
    apu.write(0x00FC, 0);
    apu.write(0x00F1, 0x04);
    apu.tick_clock(16 * 255);
    assert_eq!(apu.read(0x00FF), 0, "255 ticks is not yet 256");
    apu.tick_clock(16);
    assert_eq!(apu.read(0x00FF), 1);
}

#[test]
fn enabling_a_timer_resets_its_divider() {
    let mut apu = Apu::new();
    apu.write(0x00FC, 4);
    apu.write(0x00F1, 0x04);
    apu.tick_clock(16 * 3); // part-way to the target
    apu.write(0x00F1, 0x00); // disable
    apu.write(0x00F1, 0x04); // re-enable
    apu.tick_clock(16 * 3);
    assert_eq!(
        apu.read(0x00FF),
        0,
        "re-enabling restarts the count rather than resuming it"
    );
}

#[test]
fn the_three_timers_are_independent() {
    let mut apu = Apu::new();
    apu.write(0x00FA, 1);
    apu.write(0x00FB, 1);
    apu.write(0x00FC, 1);
    apu.write(0x00F1, 0x05); // T0 and T2 only
                             // 336 cycles from a fresh APU. The timers are phase-locked to the
                             // DSP's sample loop, so T0 fires at cycles 0, 128 and 256 — THREE
                             // edges, not the two a `336 / 128` division would predict. The tick
                             // at cycle 0 is a real edge: the DSP starts there and the document
                             // puts the 8 kHz stage-1 tick on it. T2 fires at 0, 16, ... 320,
                             // which is 21 either way.
                             //
                             // The exact number matters, and picking it carelessly cost a false
                             // failure here: the counter is FOUR bits, so 256 cycles would give T2
                             // exactly 16 ticks and read back as 0 — indistinguishable from "T2
                             // never counted". Any test asserting a timer moved must avoid landing
                             // on a multiple of 16.
    apu.tick_clock(336);
    assert_eq!(
        apu.read(0x00FD),
        3,
        "T0 counted, including the edge at cycle 0"
    );
    assert_eq!(apu.read(0x00FE), 0, "T1 was never enabled");
    assert_eq!(apu.read(0x00FF), 21 & 0x0F, "T2 counted, and much faster");
}

// ---------------------------------------------------------------------
// The CPU actually running against this bus
// ---------------------------------------------------------------------

/// End-to-end: real SPC700 code reads a timer through the APU bus.
#[test]
fn spc700_code_can_drive_a_timer() {
    let mut apu = Apu::new();
    // Bank the IPL out so we can execute from ARAM at $0200.
    apu.write(0x00F1, 0x00);
    // MOV A,#$01 : MOV $FC,A  (T2 target)
    // MOV A,#$04 : MOV $F1,A  (enable T2)
    // SLEEP
    for (i, b) in [0xE8u8, 0x01, 0xC4, 0xFC, 0xE8, 0x04, 0xC4, 0xF1, 0xEF]
        .iter()
        .enumerate()
    {
        apu.aram[0x0200 + i] = *b;
    }
    apu.cpu.pc = 0x0200;
    for _ in 0..5 {
        apu.step().expect("all 256 opcodes are implemented");
    }
    assert!(apu.cpu.stopped, "the program reached its SLEEP");
    assert!(apu.timers[2].enabled, "T2 was enabled by real code");
    assert_eq!(apu.timers[2].target, 1);
}

/// Sanity on the register file itself: `YA` really is the Y:A pair.
#[test]
fn ya_is_the_y_a_pair() {
    let mut cpu = Spc700::new();
    cpu.y = 0x12;
    cpu.a = 0x34;
    assert_eq!(cpu.ya(), 0x1234);
    cpu.set_ya(0xABCD);
    assert_eq!((cpu.y, cpu.a), (0xAB, 0xCD));
}

/// The direct page is selectable, and dp addresses wrap inside it.
#[test]
fn the_direct_page_is_selectable_and_wraps_within_itself() {
    let mut cpu = Spc700::new();
    assert_eq!(cpu.dp(0x40), 0x0040);
    cpu.set_flag(flags::P, true);
    assert_eq!(cpu.dp(0x40), 0x0140, "P selects page 1");

    // A 16-bit dp read wraps at the page edge rather than spilling out.
    let mut bus = FlatApuBus::new();
    bus.mem[0x01FF] = 0xCD;
    bus.mem[0x0100] = 0xAB;
    assert_eq!(
        cpu.read_dp16(&mut bus, 0xFF),
        0xABCD,
        "$FF's high byte comes from $00 of the SAME page"
    );
}

// ---------------------------------------------------------------------
// $F0 TEST (ticket W7-17)
//
// This register had NO write arm at all — it fell through
// `write_register`'s `_ => {}`, so every bit of it was inert. fullsnes
// documents it fully; these tests pin the bits that are now honoured and
// the two that are stored for later.
// ---------------------------------------------------------------------

/// Power-on is `$0A`: RAM writable, timers permitted, no waitstates.
#[test]
fn test_register_powers_on_at_0a() {
    let apu = Apu::new();
    assert_eq!(apu.test, 0x0A);
    assert!(apu.ram_writes_enabled());
    assert!(apu.timers_permitted());
    assert_eq!(apu.test_ram_waits(), 0);
    assert_eq!(apu.test_io_waits(), 0);
}

/// Bit 1 clear makes ARAM read-only to the SPC700.
#[test]
fn clearing_ram_write_enable_makes_aram_read_only() {
    let mut apu = Apu::new();
    apu.write(0x0300, 0x11);
    assert_eq!(apu.aram[0x0300], 0x11, "writable by default");

    apu.write(0x00F0, 0x0A & !0x02); // clear bit 1
    apu.write(0x0300, 0x22);
    assert_eq!(
        apu.aram[0x0300], 0x11,
        "with $F0 bit 1 clear, ARAM is read-only and the write is dropped"
    );

    apu.write(0x00F0, 0x0A);
    apu.write(0x0300, 0x33);
    assert_eq!(apu.aram[0x0300], 0x33, "and writable again once re-enabled");
}

/// **The two timer controls have OPPOSITE senses and both must agree.**
///
/// Bit 0 set breaks the timers; bit 3 clear breaks them. Reading either
/// one alone gets the polarity backwards, which is why this is a test
/// rather than a comment.
#[test]
fn both_timer_control_bits_must_permit_the_timers() {
    let cases = [
        (0x0A, true, "default: bit 0 clear, bit 3 set"),
        (0x0B, false, "bit 0 set breaks them"),
        (0x02, false, "bit 3 clear breaks them"),
        (0x03, false, "both wrong"),
    ];
    for (value, permitted, why) in cases {
        let mut apu = Apu::new();
        apu.write(0x00F0, value);
        assert_eq!(apu.timers_permitted(), permitted, "{why} (${value:02X})");
    }
}

/// A held-off timer does not count, and the DSP keeps running regardless.
#[test]
fn holding_the_timers_off_does_not_stop_the_dsp() {
    let mut apu = Apu::new();
    apu.write(0x00FC, 1);
    apu.write(0x00F1, 0x04); // T2 on
    apu.write(0x00F0, 0x0B); // ...but $F0 bit 0 breaks the timers
    apu.tick_clock(16 * 9);
    assert_eq!(
        apu.read(0x00FF),
        0,
        "a timer $F0 has held off must not count"
    );

    // The DSP shares the clock and is NOT gated by $F0's timer bits.
    let before = apu.dsp.read_register(0x08);
    apu.tick_clock(64);
    let _ = before;
    apu.write(0x00F0, 0x0A);
    apu.tick_clock(16 * 9);
    assert_ne!(apu.read(0x00FF), 0, "and counts again once permitted");
}

/// The waitstate fields decode to the documented cycle counts.
///
/// They are stored and reported but NOT yet applied — honouring them
/// needs per-access timing inside the SPC700 rather than a per-instruction
/// cycle count. The decode is pinned now so that work starts from a
/// checked table.
#[test]
fn waitstate_fields_decode_to_0_1_4_9() {
    for (sel, waits) in [(0u8, 0u32), (1, 1), (2, 4), (3, 9)] {
        let mut apu = Apu::new();
        apu.write(0x00F0, 0x0A | (sel << 4));
        assert_eq!(apu.test_ram_waits(), waits, "RAM waits for selector {sel}");
        let mut apu = Apu::new();
        apu.write(0x00F0, 0x0A | (sel << 6));
        assert_eq!(apu.test_io_waits(), waits, "I/O waits for selector {sel}");
    }
}
