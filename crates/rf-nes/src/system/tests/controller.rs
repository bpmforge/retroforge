//! Acceptance criterion 4: "controller read via strobe protocol"
//! ([nesdev.org/wiki/Standard_controller](https://www.nesdev.org/wiki/Standard_controller)).
use super::bus_with_pattern_rom;
use crate::cpu::CpuBus;
use crate::NesBus;

/// Button bit layout per the module doc / nesdev's documented read order:
/// A, B, Select, Start, Up, Down, Left, Right = bit 0..7.
const A: u8 = 0x01;
const B: u8 = 0x02;
const SELECT: u8 = 0x04;
const START: u8 = 0x08;
const UP: u8 = 0x10;
const DOWN: u8 = 0x20;
const LEFT: u8 = 0x40;
const RIGHT: u8 = 0x80;

/// Read the pad the way a 6502 actually can: with at least one other bus
/// cycle between reads.
///
/// `bus.read` advances `master_cycle` by exactly one, so a bare
/// `(0..8).map(|_| bus.read(0x4016))` reads the port on eight CONSECUTIVE
/// cycles — a pattern no instruction sequence can produce (`LDA $4016` is
/// four cycles) and which only a DMC DMA halt creates. Since ticket
/// W2-01c that pattern means something specific and different: contiguous
/// reads hold one strobe asserted and clock the shift register once, not
/// once per read (see `NesBus::last_joy_read_cycle`). These tests are
/// about BUTTON ORDER, so they read the way software does; the contiguous
/// case has its own test below.
fn read_bits_like_software(bus: &mut NesBus, count: usize) -> Vec<u8> {
    (0..count)
        .map(|_| {
            let bit = bus.read(0x4016) & 1;
            bus.read(0x0000); // any non-port cycle, as real code would have
            bit
        })
        .collect()
}

#[test]
fn full_8_bit_sequence_in_documented_button_order() {
    let mut bus = bus_with_pattern_rom(1, 1);
    // B, Start, Down, Right pressed; A, Select, Up, Left released.
    bus.set_controller_buttons(0, B | START | DOWN | RIGHT);
    bus.write(0x4016, 1);
    bus.write(0x4016, 0); // 1->0 transition latches

    let bits = read_bits_like_software(&mut bus, 8);
    assert_eq!(
        bits,
        vec![0, 1, 0, 1, 0, 1, 0, 1],
        "A B Select Start Up Down Left Right order"
    );
}

#[test]
fn complementary_bit_pattern_confirms_the_order_is_not_coincidental() {
    let mut bus = bus_with_pattern_rom(1, 1);
    // The complement of the previous test's pattern: A, Select, Up, Left
    // pressed; B, Start, Down, Right released.
    bus.set_controller_buttons(0, A | SELECT | UP | LEFT);
    bus.write(0x4016, 1);
    bus.write(0x4016, 0);

    let bits = read_bits_like_software(&mut bus, 8);
    assert_eq!(
        bits,
        vec![1, 0, 1, 0, 1, 0, 1, 0],
        "A Select Up Left set, in that same documented order"
    );
}

#[test]
fn reads_past_the_eighth_return_1() {
    let mut bus = bus_with_pattern_rom(1, 1);
    bus.set_controller_buttons(0, 0xFF);
    bus.write(0x4016, 1);
    bus.write(0x4016, 0);
    read_bits_like_software(&mut bus, 8);
    for _ in 0..5 {
        assert_eq!(
            read_bits_like_software(&mut bus, 1)[0],
            1,
            "standard pad reports 1 past the 8th read"
        );
    }
}

#[test]
fn the_1_to_0_transition_is_what_latches_not_the_reads() {
    let mut bus = bus_with_pattern_rom(1, 1);
    bus.set_controller_buttons(0, A); // only A pressed at latch time
    bus.write(0x4016, 1);
    bus.write(0x4016, 0); // latches "only A pressed"

    // Changing buttons *after* the latch must not affect the in-progress
    // shift-out.
    bus.set_controller_buttons(0, RIGHT); // now only Right pressed
    let bits = read_bits_like_software(&mut bus, 8);
    assert_eq!(
        bits,
        vec![1, 0, 0, 0, 0, 0, 0, 0],
        "still reflects the state latched at 1->0, not live buttons"
    );
}

#[test]
fn strobe_held_high_continuously_reloads_from_live_buttons() {
    let mut bus = bus_with_pattern_rom(1, 1);
    bus.set_controller_buttons(0, A);
    bus.write(0x4016, 1); // strobe high: continuous reload
    assert_eq!(bus.read(0x4016) & 1, 1, "A pressed");

    bus.set_controller_buttons(0, 0); // release while still strobed
    assert_eq!(
        bus.read(0x4016) & 1,
        0,
        "live value tracked while strobe is high"
    );

    bus.set_controller_buttons(0, A);
    assert_eq!(bus.read(0x4016) & 1, 1, "still continuously reloading");
}

#[test]
fn strobe_write_reaches_both_controllers() {
    let mut bus = bus_with_pattern_rom(1, 1);
    bus.set_controller_buttons(0, A);
    bus.set_controller_buttons(1, B);
    bus.write(0x4016, 1); // one strobe write, both controllers see it
    bus.write(0x4016, 0);

    assert_eq!(bus.read(0x4016) & 1, 1, "controller 1 latched A");
    assert_eq!(
        bus.read(0x4017) & 1,
        0,
        "controller 2 latched B (not A) at bit 0"
    );
}

#[test]
fn open_bus_upper_bits_reflect_the_last_driven_byte_not_a_hardcoded_constant() {
    let mut bus = bus_with_pattern_rom(1, 1);
    bus.set_controller_buttons(0, A);
    bus.write(0x4016, 1);
    bus.write(0x4016, 0); // latches; also drives open_bus to 0 (the byte written)
                          // Drive a known, distinct byte via an unrelated write *after* strobing
                          // (a write to $4016 itself would drive the latch with whatever byte
                          // was written there, masking the effect we're trying to isolate).
    bus.write(0x0000, 0x37);

    let value = bus.read(0x4016);
    assert_eq!(value & 0x01, 1, "D0 is the real controller bit");
    assert_eq!(
        value & 0xFE,
        0x37 & 0xFE,
        "D1-D7 carry the open-bus latch, not a magic constant"
    );
}

/// Ticket W1-03: `peek` (the trace logger's disassembly read, via
/// `Controller::peek_bit`) must never advance the shift register — unlike
/// a real `CpuBus::read`, which shifts one bit out per call. This is the
/// load-bearing regression for that distinction: nothing else fails if a
/// future edit "simplifies" `peek_bit` into calling `read_bit`, and that
/// would silently corrupt controller state every time a trace line is
/// rendered.
#[test]
fn peek_never_advances_the_shift_register_unlike_a_real_read() {
    let mut bus = bus_with_pattern_rom(1, 1);
    // A pressed (bit 0 = 1), B released (bit 1 = 0) — first two shifted
    // bits differ, so an accidental advance is observable.
    bus.set_controller_buttons(0, A);
    bus.write(0x4016, 1);
    bus.write(0x4016, 0); // latches

    let first_peek = bus.peek(0x4016) & 1;
    let second_peek = bus.peek(0x4016) & 1;
    let third_peek = bus.peek(0x4016) & 1;
    assert_eq!(
        (first_peek, second_peek, third_peek),
        (1, 1, 1),
        "peek must keep reporting the same (first) bit — the shift register never advances"
    );

    // A real read, by contrast, does advance: first bit is A (1), second is
    // B (0).
    assert_eq!(
        read_bits_like_software(&mut bus, 2),
        vec![1, 0],
        "real reads, spaced as software does them, advance: A then B"
    );
}

/// Contiguous `$4016` reads clock the pad ONCE, not once per read
/// (ticket W2-01c).
///
/// A standard controller's shift register is clocked by the edge of the
/// read strobe. Reads on back-to-back CPU cycles hold that strobe
/// continuously asserted, so they yield one rising edge between them.
/// Software cannot produce this pattern — a DMC DMA halt can, which is
/// exactly what blargg's `dmc_dma_during_read4/dma_4016_read` measures:
/// its expected `08 08 07 08 08` says the colliding iteration costs
/// **one** extra bit, though the halt puts three extra reads on the bus.
///
/// Anti-vacuity: the spaced case is asserted alongside, so an
/// implementation that simply stopped shifting on every read fails.
#[test]
fn contiguous_port_reads_clock_the_shift_register_only_once() {
    // Bit pattern 0,1,0,1,... so a suppressed shift is visible as a
    // repeated bit rather than as an unchanged constant.
    let mut bus = bus_with_pattern_rom(1, 1);
    bus.set_controller_buttons(0, B | START | DOWN | RIGHT);
    bus.write(0x4016, 1);
    bus.write(0x4016, 0);

    let contiguous: Vec<u8> = (0..4).map(|_| bus.read(0x4016) & 1).collect();
    assert_eq!(
        contiguous,
        vec![0, 0, 0, 0],
        "four reads on consecutive cycles are one strobe: the first bit, held"
    );

    // And the same four reads, spaced, DO advance — otherwise this test
    // would pass on an engine that never shifts at all.
    let mut bus = bus_with_pattern_rom(1, 1);
    bus.set_controller_buttons(0, B | START | DOWN | RIGHT);
    bus.write(0x4016, 1);
    bus.write(0x4016, 0);
    assert_eq!(
        read_bits_like_software(&mut bus, 4),
        vec![0, 1, 0, 1],
        "spaced reads advance one bit each"
    );
}
