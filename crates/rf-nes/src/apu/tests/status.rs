//! `$4015` status-register tests (ticket W2-01a) — nesdev.org/wiki/APU's
//! "Status ($4015)" section.

use crate::apu::Apu;

/// "N/T/2/1 will read as 1 if the corresponding length counter has not been
/// halted through either expiring or a write of 0 to the corresponding bit.
/// For the triangle channel, the status of the linear counter is
/// irrelevant."
#[test]
fn status_reports_one_bit_per_nonzero_length_counter() {
    let mut apu = Apu::new();
    assert_eq!(apu.read_status(), 0, "power-on: everything silent");

    apu.write_register(0x4015, 0x0F);
    for (high_register, bit) in [(0x4003u16, 0x01u8), (0x4007, 0x02), (0x400B, 0x04)] {
        apu.write_register(high_register, 0x00);
        assert_eq!(apu.read_status() & bit, bit, "{high_register:#06X} loaded");
    }
    apu.write_register(0x400F, 0x00);
    assert_eq!(apu.read_status() & 0x08, 0x08, "noise loaded");

    apu.write_register(0x4015, 0x00);
    assert_eq!(apu.read_status() & 0x0F, 0, "all four disabled again");
}

/// "Reading this register clears the frame interrupt flag (but not the DMC
/// interrupt flag)" — blargg `3-irq_flag` sub-test 5 and `7-dmc_basics`
/// sub-test 10 ("Reading IRQ flag shouldn't clear it").
#[test]
fn reading_status_clears_the_frame_irq_but_not_the_dmc_irq() {
    let mut apu = Apu::new();
    apu.write_register(0x4017, 0x00);
    for _ in 0..30000 {
        apu.tick();
    }
    assert_eq!(apu.read_status() & 0x40, 0x40, "frame flag reads as set");
    assert_eq!(apu.read_status() & 0x40, 0, "and the read cleared it");

    let mut apu = Apu::new();
    apu.write_register(0x4010, 0x80);
    apu.write_register(0x4013, 0x00);
    apu.write_register(0x4015, 0x10);
    apu.dmc_supply_byte(0x00);
    assert_eq!(apu.read_status() & 0x80, 0x80, "DMC flag set at sample end");
    assert_eq!(
        apu.read_status() & 0x80,
        0x80,
        "and a read does NOT clear it"
    );

    apu.write_register(0x4015, 0x00);
    assert_eq!(
        apu.read_status() & 0x80,
        0,
        "but a $4015 write does (sub-test 11)"
    );
}

/// "If an interrupt flag was set at the same moment of the read, it will
/// read back as 1 but it will not be cleared." blargg `4-jitter` is built
/// on this; without it, a read landing on the set cycle silently loses an
/// interrupt.
#[test]
fn a_frame_irq_set_on_the_read_cycle_survives_the_read() {
    let mut apu = Apu::new();
    apu.write_register(0x4017, 0x00);

    // Find the exact cycle the flag goes up, then rebuild and read on it.
    let mut set_cycle = 0;
    for cycle in 1..40000 {
        apu.tick();
        if apu.irq_line() {
            set_cycle = cycle;
            break;
        }
    }
    assert_ne!(set_cycle, 0, "mode 0 must raise the flag");

    let mut apu = Apu::new();
    apu.write_register(0x4017, 0x00);
    for _ in 0..set_cycle {
        apu.tick();
    }
    assert_eq!(apu.read_status() & 0x40, 0x40, "reads back as set");
    assert!(
        apu.irq_line(),
        "and stays set, because it was raised on this very cycle"
    );
}
