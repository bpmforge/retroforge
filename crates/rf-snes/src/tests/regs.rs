//! 5A22 register tests: multiply/divide latency, NMITIMEN, H/V IRQ and
//! the WRAM port (ticket W6-02a).

use crate::bus::SnesBus;
use crate::cpu::CpuBus;
use crate::regs::{IrqMode, IrqTimer, MathUnit, NmiTimen, WramPort, DIV_STEPS, MUL_STEPS};
use rf_cart::SnesMapMode;

fn bus() -> SnesBus {
    SnesBus::new(vec![0xEA; 32 * 1024], 8192, SnesMapMode::LoRom)
}

#[test]
fn multiply_produces_the_right_product_after_eight_steps() {
    for (a, b) in [(0u8, 0u8), (1, 1), (0xFF, 0xFF), (37, 91), (0x80, 0x02)] {
        let mut m = MathUnit::default();
        m.wrmpya = a;
        m.start_multiply(b);
        m.settle();
        assert_eq!(m.rdmpy, u16::from(a) * u16::from(b), "{a} * {b}");
    }
}

#[test]
fn divide_produces_quotient_and_remainder_after_sixteen_steps() {
    for (dividend, divisor) in [
        (1000u16, 7u8),
        (0, 1),
        (0xFFFF, 0xFF),
        (65535, 1),
        (12345, 100),
    ] {
        let mut m = MathUnit::default();
        m.wrdiv = dividend;
        m.start_divide(divisor);
        m.settle();
        assert_eq!(
            m.rddiv,
            dividend / u16::from(divisor),
            "{dividend} / {divisor} quotient"
        );
        assert_eq!(
            m.rdmpy,
            dividend % u16::from(divisor),
            "{dividend} % {divisor} remainder"
        );
    }
}

/// **The latency is the point.** A model that completed instantly would
/// pass both tests above and still be wrong in the direction that
/// matters, because software reads these registers mid-operation.
#[test]
fn results_are_not_ready_before_the_operation_completes() {
    let mut m = MathUnit::default();
    m.wrmpya = 0xFF;
    m.start_multiply(0xFF);
    assert!(m.busy(), "a multiply must not complete on the write");
    for step in 0..MUL_STEPS {
        assert!(m.busy(), "still running after {step} steps");
        m.step();
    }
    assert!(!m.busy(), "exactly {MUL_STEPS} steps");
    assert_eq!(m.rdmpy, 0xFE01);

    let mut d = MathUnit::default();
    d.wrdiv = 1000;
    d.start_divide(7);
    for step in 0..DIV_STEPS {
        assert!(d.busy(), "divide still running after {step} steps");
        d.step();
    }
    assert!(!d.busy(), "exactly {DIV_STEPS} steps");
}

/// An intermediate read must return the shift register's ACTUAL state,
/// which is generally neither zero nor the final answer.
///
/// The exact bit patterns are not independently verifiable here — their
/// oracle is gilyon `cputest`, which is W6-02b. What this asserts is the
/// observable contract: the value changes as the operation proceeds, so
/// a read partway through is genuinely partial rather than a stand-in.
#[test]
fn intermediate_reads_see_the_operation_in_progress() {
    let mut m = MathUnit::default();
    m.wrmpya = 0xFF;
    m.start_multiply(0xFF);
    let mut seen = Vec::new();
    for _ in 0..MUL_STEPS {
        seen.push(m.rdmpy);
        m.step();
    }
    let final_value = m.rdmpy;
    assert_eq!(final_value, 0xFE01);
    assert!(
        seen.iter().any(|&v| v != 0 && v != final_value),
        "at least one intermediate must be neither zero nor the final product; saw {seen:02X?}"
    );
    assert!(
        seen.windows(2).any(|w| w[0] != w[1]),
        "the intermediate state must actually advance"
    );
}

/// Retriggering while busy is ignored — otherwise a game that wrote twice
/// would restart the unit and read a result that never existed.
#[test]
fn a_retrigger_while_busy_is_ignored() {
    let mut m = MathUnit::default();
    m.wrmpya = 10;
    m.start_multiply(10);
    m.step();
    m.wrmpya = 3;
    m.start_multiply(3);
    m.settle();
    assert_eq!(m.rdmpy, 100, "the first multiply must complete undisturbed");
}

#[test]
fn nmitimen_decodes_its_fields() {
    assert!(NmiTimen(0x80).nmi_enabled());
    assert!(!NmiTimen(0x00).nmi_enabled());
    assert!(NmiTimen(0x01).auto_joypad());
    assert_eq!(NmiTimen(0x00).irq_mode(), IrqMode::Off);
    assert_eq!(NmiTimen(0x10).irq_mode(), IrqMode::Horizontal);
    assert_eq!(NmiTimen(0x20).irq_mode(), IrqMode::Vertical);
    assert_eq!(NmiTimen(0x30).irq_mode(), IrqMode::Both);
}

#[test]
fn hv_irq_compares_only_what_its_mode_selects() {
    let t = IrqTimer {
        htime: 100,
        vtime: 200,
        fired: false,
    };
    assert!(t.matches(IrqMode::Horizontal, 100, 5));
    assert!(!t.matches(IrqMode::Horizontal, 101, 5));
    assert!(t.matches(IrqMode::Vertical, 0, 200));
    assert!(
        !t.matches(IrqMode::Vertical, 50, 200),
        "a V-only IRQ fires at the start of the line, not anywhere on it"
    );
    assert!(t.matches(IrqMode::Both, 100, 200));
    assert!(!t.matches(IrqMode::Both, 100, 201));
    assert!(!t.matches(IrqMode::Off, 100, 200));
}

/// `$4211` acknowledges on read — and `peek` must not.
#[test]
fn reading_timeup_acknowledges_but_peeking_does_not() {
    let mut b = bus();
    b.irq.fired = true;

    assert_eq!(
        b.peek(0x00_4211) & 0x80,
        0x00,
        "peek must not report or clear"
    );
    assert!(b.irq.fired, "peek must leave the flag alone");

    assert_eq!(b.read(0x00_4211) & 0x80, 0x80, "read reports the flag");
    assert!(!b.irq.fired, "...and acknowledges it");
    assert_eq!(
        b.read(0x00_4211) & 0x80,
        0x00,
        "second read sees it cleared"
    );
}

#[test]
fn the_wram_port_addresses_all_128k_and_auto_increments() {
    let mut p = WramPort::default();
    p.set_low(0x34);
    p.set_mid(0x12);
    p.set_high(1);
    assert_eq!(p.address, 0x0001_1234, "17 bits, so bank bit included");
    p.advance();
    assert_eq!(p.address, 0x0001_1235);

    // Wraps within 128 KiB rather than running off the end.
    let mut q = WramPort {
        address: 0x0001_FFFF,
    };
    q.advance();
    assert_eq!(q.address, 0);
}

#[test]
fn the_wram_port_reaches_wram_the_cpu_cannot_address_directly() {
    let mut b = bus();
    // $1:8000 in WRAM is bank $7F:8000 — outside the low-8K mirror.
    b.write(0x00_2181, 0x00);
    b.write(0x00_2182, 0x80);
    b.write(0x00_2183, 0x01);
    b.write(0x00_2180, 0x5A);
    assert_eq!(b.wram[0x0001_8000], 0x5A);

    // Reading back auto-increments, so a second read sees the NEXT byte.
    b.wram[0x0001_8001] = 0xA5;
    b.write(0x00_2181, 0x00);
    b.write(0x00_2182, 0x80);
    b.write(0x00_2183, 0x01);
    assert_eq!(b.read(0x00_2180), 0x5A);
    assert_eq!(b.read(0x00_2180), 0xA5, "the port auto-increments on read");
}

#[test]
fn the_math_registers_are_reachable_through_the_bus() {
    let mut b = bus();
    b.write(0x00_4202, 20); // WRMPYA
    b.write(0x00_4203, 6); // WRMPYB — starts the multiply
    b.math.settle();
    assert_eq!(b.read(0x00_4216), 120, "RDMPYL");
    assert_eq!(b.read(0x00_4217), 0, "RDMPYH");

    b.write(0x00_4204, 0xE8); // WRDIVL  (1000)
    b.write(0x00_4205, 0x03); // WRDIVH
    b.write(0x00_4206, 7); // WRDIVB — starts the divide
    b.math.settle();
    assert_eq!(b.read(0x00_4214), 142, "quotient low (1000/7)");
    assert_eq!(b.read(0x00_4216), 6, "remainder (1000%7)");
}
