//! Frame-counter sequence, IRQ and `$4017`-delay tests (ticket W2-01a).

use crate::apu::Apu;

/// Ticks `n` CPU cycles, returning the cycle indices (1-based, relative to
/// the first tick) on which the frame IRQ flag was newly raised.
fn irq_cycles(apu: &mut Apu, n: u32) -> Vec<u32> {
    let mut raised = Vec::new();
    let mut was = apu.irq_line();
    for cycle in 1..=n {
        apu.tick();
        let now = apu.irq_line();
        if now && !was {
            raised.push(cycle);
        }
        was = now;
    }
    raised
}

/// nesdev.org/wiki/APU_Frame_Counter, mode 0: the interrupt flag is set on
/// three consecutive CPU cycles at the end of the sequence, and the whole
/// sequence is 29830 CPU cycles long. blargg's `6-irq_flag_timing`
/// measures the same fact from the CPU side ("Frame interrupt flag is set
/// three times in a row 29831 clocks after writing $00 to $4017").
#[test]
fn mode_zero_sets_the_frame_irq_at_the_end_of_a_29830_cycle_sequence() {
    let mut apu = Apu::new();
    apu.write_register(0x4017, 0x00);
    // The write's own 3-4 cycle delay is absorbed first; after it, cycle
    // counting restarts from the reset.
    let raised = irq_cycles(&mut apu, 4 + 29830 + 10);
    assert_eq!(
        raised.len(),
        1,
        "the flag is raised once and then stays asserted (level, not edge)"
    );
    let first = raised[0];
    assert!(
        (29828 + 3..=29828 + 4).contains(&first),
        "first set lands 29828 sequence cycles after the 3-4 cycle $4017 delay, got {first}"
    );
}

/// "In this mode, the frame interrupt flag is never set."
#[test]
fn mode_one_never_sets_the_frame_irq() {
    let mut apu = Apu::new();
    apu.write_register(0x4017, 0x80);
    assert!(irq_cycles(&mut apu, 40000).is_empty());
}

/// "Interrupt inhibit flag. If set, the frame interrupt flag is cleared" —
/// blargg `3-irq_flag` sub-tests 2, 6 and 7.
#[test]
fn setting_the_inhibit_flag_clears_a_pending_frame_irq() {
    let mut apu = Apu::new();
    apu.write_register(0x4017, 0x00);
    for _ in 0..30000 {
        apu.tick();
    }
    assert!(apu.irq_line(), "flag should be set by now in mode 0");

    apu.write_register(0x4017, 0x40);
    // The FLAG clears immediately; the CPU's view of the line catches up
    // one cycle later (W2-21's `irq_line_delayed`), which is why this
    // ticks once rather than asserting in the same breath as the write.
    apu.tick();
    assert!(
        !apu.irq_line(),
        "the inhibit bit clears the flag immediately, without waiting for the reset delay"
    );
}

/// "Writing to $4017 with bit 7 set ($80) will immediately clock all of its
/// controlled units at the beginning of the 5-step sequence; with bit 7
/// clear, only the sequence is reset without clocking any of its units."
/// blargg `1-len_ctr` sub-tests 4 and 5 are exactly this pair.
#[test]
fn writing_eighty_to_4017_clocks_length_immediately_and_writing_zero_does_not() {
    for (mode, expected) in [(0x80u8, 9u8), (0x00, 10)] {
        let mut apu = Apu::new();
        // Enable pulse 1 and load length index 0 -> 10.
        apu.write_register(0x4015, 0x01);
        apu.write_register(0x4003, 0x00);
        apu.write_register(0x4017, mode);
        // The reset (and its immediate clock, in 5-step mode) resolves
        // within 4 CPU cycles.
        for _ in 0..4 {
            apu.tick();
        }
        assert_eq!(
            apu.pulse1.length.counter(),
            expected,
            "mode ${mode:02X} should {} clock the length counter",
            if mode == 0x80 { "" } else { "not" }
        );
    }
}

/// The `$4017` write takes effect after 3 or 4 CPU cycles depending on APU
/// parity — nesdev: "If the write occurs during an APU cycle, the effects
/// occur 3 CPU cycles after the $4017 write cycle, and if the write occurs
/// between APU cycles, the effects occurs 4 CPU cycles after". This test
/// pins that the delay is real and parity-dependent, without asserting
/// which parity is which (blargg `4-jitter` is the oracle for that; see
/// `crate::apu`'s module doc).
#[test]
fn the_4017_reset_is_delayed_by_three_or_four_cycles_depending_on_parity() {
    let mut delays = Vec::new();
    for offset in 0..2 {
        let mut apu = Apu::new();
        apu.write_register(0x4015, 0x01);
        for _ in 0..offset {
            apu.tick();
        }
        apu.write_register(0x4003, 0x00); // length = 10
        apu.write_register(0x4017, 0x80); // 5-step: clocks length on reset
        let mut delay = 0;
        for cycle in 1..=8 {
            apu.tick();
            if apu.pulse1.length.counter() == 9 {
                delay = cycle;
                break;
            }
        }
        delays.push(delay);
    }
    assert!(
        delays.iter().all(|d| (3..=4).contains(d)),
        "both parities must resolve in 3-4 cycles, got {delays:?}"
    );
    assert_ne!(
        delays[0], delays[1],
        "the two APU parities must differ by exactly the one cycle nesdev describes"
    );
}
