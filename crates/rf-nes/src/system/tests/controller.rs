//! Acceptance criterion 4: "controller read via strobe protocol"
//! ([nesdev.org/wiki/Standard_controller](https://www.nesdev.org/wiki/Standard_controller)).
use super::bus_with_pattern_rom;
use crate::cpu::CpuBus;

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

#[test]
fn full_8_bit_sequence_in_documented_button_order() {
    let mut bus = bus_with_pattern_rom(1, 1);
    // B, Start, Down, Right pressed; A, Select, Up, Left released.
    bus.set_controller_buttons(0, B | START | DOWN | RIGHT);
    bus.write(0x4016, 1);
    bus.write(0x4016, 0); // 1->0 transition latches

    let bits: Vec<u8> = (0..8).map(|_| bus.read(0x4016) & 1).collect();
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

    let bits: Vec<u8> = (0..8).map(|_| bus.read(0x4016) & 1).collect();
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
    for _ in 0..8 {
        bus.read(0x4016);
    }
    for _ in 0..5 {
        assert_eq!(
            bus.read(0x4016) & 1,
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
    let bits: Vec<u8> = (0..8).map(|_| bus.read(0x4016) & 1).collect();
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
