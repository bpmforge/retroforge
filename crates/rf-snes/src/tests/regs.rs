//! 5A22 register tests: multiply/divide latency, NMITIMEN, H/V IRQ and
//! the WRAM port (ticket W6-02a).

use crate::bus::SnesBus;
use crate::cpu::CpuBus;
use crate::regs::{IrqMode, IrqTimer, MathUnit, NmiTimen, WramPort, DIV_STEPS, MUL_STEPS};
use crate::timing::MASTER_PER_DOT;
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

/// Ticket W14-39: `MathUnit::tick` must clock the divider off real CPU
/// cycles (fullsnes: the `$42xx` ports are "clocked by the CPU Clock"),
/// not off master cycles re-bucketed at `speed::FAST`. `DIV_STEPS` (16)
/// CPU cycles is the divide's whole latency; one cycle short must still
/// be busy, and the exact latency must complete it with the right
/// quotient/remainder — this is the SMRPG bug (`101 / 3`) as an in-repo
/// test rather than only a ROM trace.
#[test]
fn tick_completes_a_divide_after_its_real_cpu_cycle_latency() {
    let mut d = MathUnit::default();
    d.wrdiv = 0x0065; // 101, the SMRPG boot-upload dividend that exposed this.
    d.start_divide(3);
    d.tick(u32::from(DIV_STEPS - 1));
    assert!(d.busy(), "one CPU cycle short of 16 must still be busy");
    d.tick(1);
    assert!(!d.busy(), "exactly 16 CPU cycles");
    assert_eq!(d.rddiv, 0x0021, "101 / 3 quotient");
    assert_eq!(d.rdmpy, 2, "101 / 3 remainder");
}

/// Spreading the same total CPU-cycle count across many small `tick`
/// calls (one per instruction, as `SnesSystem::step` actually does) must
/// produce the same result as one large call — each call is now an exact
/// integer number of CPU cycles, so there is no remainder to lose.
#[test]
fn tick_gives_the_same_result_whether_fed_in_one_call_or_many() {
    let total = u32::from(DIV_STEPS);

    let mut one_shot = MathUnit::default();
    one_shot.wrdiv = 1000;
    one_shot.start_divide(7);
    one_shot.tick(total);

    let mut drip_fed = MathUnit::default();
    drip_fed.wrdiv = 1000;
    drip_fed.start_divide(7);
    // Feed it back in small, uneven CPU-cycle-sized pieces (real
    // instruction costs, e.g. 2 and 4 cycles) that do not divide `total`
    // evenly.
    let mut fed = 0u32;
    while fed < total {
        let piece = if fed.is_multiple_of(3) { 2 } else { 4 };
        drip_fed.tick(piece);
        fed += piece;
    }
    assert!(!one_shot.busy());
    assert!(!drip_fed.busy(), "many small ticks must still finish");
    assert_eq!(one_shot.rddiv, drip_fed.rddiv);
    assert_eq!(one_shot.rdmpy, drip_fed.rdmpy);
}

/// The `$4204`-`$4217` window through `SnesBus`, driven the way
/// `tick_math` actually is (ticket W14-39): a read that lands before the
/// real 16-CPU-cycle latency has elapsed must NOT see the final high
/// byte, and one that lands after must.
#[test]
fn bus_tick_math_gates_the_divide_quotient_by_real_cpu_cycles() {
    let mut b = bus();
    b.write(0x4204, 0x65); // WRDIV low: dividend 0x0065 = 101.
    b.write(0x4205, 0x00); // WRDIV high.
    b.write(0x4206, 3); // start a divide by 3: 101 / 3 = 33 (0x0021) r 2.

    b.tick_math(u32::from(DIV_STEPS - 1));
    assert_ne!(
        b.read(0x4215),
        0x00,
        "a read one CPU cycle short of the real latency must not already \
         show the finished high byte — if it does, this test stopped \
         detecting the W14-39 regression"
    );

    b.tick_math(1);
    assert_eq!(b.read(0x4214), 0x21, "101 / 3 quotient low byte");
    assert_eq!(b.read(0x4215), 0x00, "101 / 3 quotient high byte");
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

/// **Disabling the H/V IRQ in `$4200` drops a latched flag** (ticket
/// W14-10; fullsnes `$4211`, bsnes `cpu/io.cpp`).
///
/// Final Fantasy Mystic Quest writes `$4200 = 0` inside a routine and
/// then `PLP`s with I clear. A flag latched by an earlier H/V IRQ used to
/// survive the disable and fire there, into a handler that is a `BRK`
/// whose vector is a `STP` trap.
#[test]
fn disabling_hv_irq_in_4200_drops_a_latched_irq() {
    let mut b = bus();
    b.write(0x00_4200, 0x20); // V-IRQ enabled
    b.irq.fired = true;
    b.write(0x00_4200, 0x00); // ...and disabled again, flag still latched
    assert!(!b.irq.fired, "the disable must deassert the line");
    assert_eq!(b.read(0x00_4211) & 0x80, 0, "and TIMEUP reads clear");

    // Disabling NMI alone, with the IRQ still enabled, must NOT touch it.
    b.write(0x00_4200, 0xA0);
    b.irq.fired = true;
    b.write(0x00_4200, 0x20);
    assert!(b.irq.fired, "an IRQ that is still enabled stays pending");
}

/// **`$2137` latches the beam and `$213C`/`$213D` read it in two halves**
/// (ticket W14-10; fullsnes SLHV / OPHCT / OPVCT / STAT78).
#[test]
fn slhv_latches_the_beam_and_the_counters_read_low_then_high() {
    let mut b = bus();
    b.timing.line = 0x101;
    b.timing.line_cycles = 0x12A * MASTER_PER_DOT;
    let _ = b.read(0x00_2137);
    let _ = b.read(0x00_213F); // reset both flip-flops
    assert_eq!(b.read(0x00_213D), 0x01, "OPVCT low byte first");
    assert_eq!(b.read(0x00_213D) & 1, 1, "then bit 8");
    assert_eq!(b.read(0x00_213C), 0x2A, "OPHCT low byte first");
    assert_eq!(b.read(0x00_213C) & 1, 1, "then bit 8");
    // A third read wraps back to the low byte.
    assert_eq!(b.read(0x00_213D), 0x01);
}

/// STAT78 reports the latch flag once, resets the flip-flops, and says
/// NTSC. `peek` must do none of that.
#[test]
fn stat78_reports_the_latch_once_and_resets_the_flip_flops() {
    let mut b = bus();
    assert_eq!(b.read(0x00_213F) & 0x40, 0, "nothing latched yet");
    b.timing.line = 42;
    let _ = b.read(0x00_2137);
    assert_eq!(b.peek(0x00_213F) & 0x40, 0x40, "peek sees the flag");
    assert_eq!(b.peek(0x00_213F) & 0x40, 0x40, "...and leaves it");
    let _ = b.read(0x00_213D); // flip-flop now on the high half
    let stat = b.read(0x00_213F);
    assert_eq!(stat & 0x40, 0x40, "the read reports the latch");
    assert_eq!(stat & 0x10, 0, "NTSC");
    assert_eq!(b.read(0x00_213F) & 0x40, 0, "and clears it");
    assert_eq!(
        b.read(0x00_213D),
        42,
        "the flip-flop was reset to the low half"
    );
}

/// `$4201` WRIO bit 7 going 1-to-0 latches like `$2137`; going 0-to-1
/// does not.
#[test]
fn wrio_falling_edge_latches_the_counters() {
    let mut b = bus();
    b.timing.line = 77;
    b.write(0x00_4201, 0x80);
    assert_eq!(b.read(0x00_213F) & 0x40, 0, "a rising edge latches nothing");
    b.write(0x00_4201, 0x00);
    assert_eq!(b.read(0x00_213F) & 0x40, 0x40, "the falling edge latches");
    assert_eq!(b.read(0x00_213D), 77);
}
