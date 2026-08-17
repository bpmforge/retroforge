//! Per-channel unit tests (ticket W2-01a): one nesdev-documented behavior
//! each, so a regression names the unit rather than just the ROM.

use crate::apu::units::LENGTH_TABLE;
use crate::apu::Apu;

/// nesdev.org/wiki/APU_Length_Counter's table, transcribed and checked
/// against the page's own "index to length map" restatement (the second,
/// independent listing on that page) rather than against itself: entries
/// with bit 0 set are the linear lengths, entries with bit 0 clear are the
/// note lengths with base 10 (MSB clear) or 12 (MSB set). blargg's
/// `2-len_table` verifies all 32 from the CPU side.
#[test]
fn length_table_matches_the_nesdev_index_to_length_map() {
    // Linear lengths: index 0bx1 -> 2*value, except $01 -> 254.
    for index in (1..32).step_by(2) {
        let expected = if index == 1 { 254 } else { index as u8 - 1 };
        assert_eq!(LENGTH_TABLE[index], expected, "linear entry {index:#04X}");
    }
    // Note lengths: base 10 for the low half, base 12 for the high half,
    // with relative durations 1, 2, 4, 8, 1.5x4, 1/3x8, 1/3x16 ... in the
    // page's own ordering.
    assert_eq!(
        [
            LENGTH_TABLE[0x00],
            LENGTH_TABLE[0x02],
            LENGTH_TABLE[0x04],
            LENGTH_TABLE[0x06],
            LENGTH_TABLE[0x08],
            LENGTH_TABLE[0x0A],
            LENGTH_TABLE[0x0C],
            LENGTH_TABLE[0x0E]
        ],
        [10, 20, 40, 80, 160, 60, 14, 26]
    );
    assert_eq!(
        [
            LENGTH_TABLE[0x10],
            LENGTH_TABLE[0x12],
            LENGTH_TABLE[0x14],
            LENGTH_TABLE[0x16],
            LENGTH_TABLE[0x18],
            LENGTH_TABLE[0x1A],
            LENGTH_TABLE[0x1C],
            LENGTH_TABLE[0x1E]
        ],
        [12, 24, 48, 96, 192, 72, 16, 32]
    );
}

/// "When the enabled bit is cleared (via $4015), the length counter is
/// forced to 0 and cannot be changed until enabled is set again" — blargg
/// `1-len_ctr` sub-tests 6 and 7.
#[test]
fn disabling_a_channel_clears_its_length_and_blocks_reloads() {
    let mut apu = Apu::new();
    apu.write_register(0x4015, 0x01);
    apu.write_register(0x4003, 0x08); // index 1 -> 254
    assert_eq!(apu.pulse1.length.counter(), 254);

    apu.write_register(0x4015, 0x00);
    assert_eq!(apu.pulse1.length.counter(), 0, "cleared by the disable");

    apu.write_register(0x4003, 0x08);
    assert_eq!(
        apu.pulse1.length.counter(),
        0,
        "a load while disabled must not take"
    );
}

/// "the length counter is decremented except when: the length counter is 0,
/// or the halt flag is set" — blargg `1-len_ctr` sub-test 8.
#[test]
fn the_halt_bit_suspends_length_clocking() {
    let mut apu = Apu::new();
    apu.write_register(0x4015, 0x01);
    apu.write_register(0x4000, 0x20); // halt/loop set
    apu.write_register(0x4003, 0x00); // length = 10

    // Two half-frame clocks' worth of 5-step sequence.
    apu.write_register(0x4017, 0x80);
    for _ in 0..20000 {
        apu.tick();
    }
    assert_eq!(
        apu.pulse1.length.counter(),
        10,
        "halted, so never decrements"
    );

    apu.write_register(0x4000, 0x00); // clear halt
    apu.write_register(0x4017, 0x80); // immediate half-frame clock
    for _ in 0..4 {
        apu.tick();
    }
    assert_eq!(apu.pulse1.length.counter(), 9);
}

/// nesdev.org/wiki/APU_Sweep: "Pulse 1 adds the ones' complement (−c − 1).
/// Making 20 negative produces a change amount of −21. Pulse 2 adds the
/// two's complement (−c). Making 20 negative produces a change amount of
/// −20." This is the two channels' only behavioral difference, so it gets
/// the page's own worked example as its test.
#[test]
fn sweep_negate_differs_between_the_two_pulse_channels() {
    let mut apu = Apu::new();
    // Both channels: period 100, shift 0 (so the change amount is the
    // period itself), negate on. Pulse 1's target is 100 - 101 clamped to
    // 0... which coincides with pulse 2's 100 - 100 = 0, so a shift of 1 is
    // used instead to keep the two answers distinguishable: change = 50,
    // pulse 1 subtracts 51, pulse 2 subtracts 50.
    apu.write_register(0x4002, 100);
    apu.write_register(0x4003, 0x00);
    apu.write_register(0x4001, 0x09); // negate, shift 1
    apu.write_register(0x4006, 100);
    apu.write_register(0x4007, 0x00);
    apu.write_register(0x4005, 0x09);
    assert_eq!(
        apu.pulse1.sweep_target_for_test(),
        49,
        "pulse 1 adds the ones' complement: 100 - (50 + 1)"
    );
    assert_eq!(
        apu.pulse2.sweep_target_for_test(),
        50,
        "pulse 2 adds the two's complement: 100 - 50"
    );
}

/// nesdev.org/wiki/APU_Noise: "On power-up, the shift register is loaded
/// with the value 1", feedback is bit 0 XOR bit 1 (mode clear) or bit 0 XOR
/// bit 6 (mode set), and the sequence is 32767 steps long in mode 0.
#[test]
fn noise_lfsr_period_is_32767_steps_in_mode_zero() {
    let mut apu = Apu::new();
    apu.write_register(0x400E, 0x00); // shortest period, mode 0
    let start = apu.noise.shift_register_for_test();
    assert_eq!(start, 1, "power-on value");

    let mut steps = 0u32;
    // Period $0 is 4 CPU cycles per shift.
    for _ in 0..(32767u32 * 4 + 8) {
        apu.tick();
        if apu.noise.shift_register_for_test() != start {
            steps += 1;
        } else if steps > 0 {
            break;
        }
    }
    assert_eq!(
        steps,
        32766 * 4,
        "the LFSR returns to 1 after exactly 32767 shifts"
    );
}

/// nesdev.org/wiki/APU_Triangle: "If the linear counter reload flag is set,
/// the linear counter is reloaded with the counter reload value, otherwise
/// if the linear counter is non-zero, it is decremented. If the control
/// flag is clear, the linear counter reload flag is cleared." — and "the
/// reload flag is not cleared unless the control flag is also clear".
#[test]
fn triangle_linear_counter_reloads_then_counts_down_only_once_control_clears() {
    let mut apu = Apu::new();
    apu.write_register(0x4015, 0x04);
    apu.write_register(0x4008, 0x83); // control set, reload value 3
    apu.write_register(0x400B, 0x00); // sets the reload flag

    // Three quarter-frame clocks with control still set: reloads each time.
    for _ in 0..3 {
        apu.write_register(0x4017, 0x80);
        for _ in 0..4 {
            apu.tick();
        }
        assert_eq!(apu.triangle.linear_counter_for_test(), 3);
    }

    apu.write_register(0x4008, 0x03); // control clear -> reload flag clears
    apu.write_register(0x4017, 0x80);
    for _ in 0..4 {
        apu.tick();
    }
    assert_eq!(
        apu.triangle.linear_counter_for_test(),
        3,
        "reload, then flag clears"
    );
    apu.write_register(0x4017, 0x80);
    for _ in 0..4 {
        apu.tick();
    }
    assert_eq!(
        apu.triangle.linear_counter_for_test(),
        2,
        "now it decrements"
    );
}

/// nesdev.org/wiki/APU_DMC: "Sample address = %11AAAAAA.AA000000 = $C000 +
/// (A * 64)" and "Sample length = %LLLL.LLLL0001 = (L * 16) + 1 bytes" —
/// blargg `7-dmc_basics` sub-tests 3 and 18 lean on both.
#[test]
fn dmc_decodes_sample_address_and_length_per_nesdev() {
    let mut apu = Apu::new();
    apu.write_register(0x4012, 0x00);
    apu.write_register(0x4013, 0x00);
    apu.write_register(0x4015, 0x10);
    assert_eq!(apu.dmc_fetch_request().map(|(addr, _)| addr), Some(0xC000));

    let mut apu = Apu::new();
    apu.write_register(0x4012, 0xFF);
    apu.write_register(0x4013, 0x01);
    apu.write_register(0x4015, 0x10);
    assert_eq!(
        apu.dmc_fetch_request().map(|(addr, _)| addr),
        Some(0xC000 + 0xFF * 64)
    );

    // $4013 = 0 gives a 1-byte sample: after one supplied byte the channel
    // is no longer active.
    let mut apu = Apu::new();
    apu.write_register(0x4013, 0x00);
    apu.write_register(0x4015, 0x10);
    apu.dmc_supply_byte(0xAA);
    assert_eq!(
        apu.read_status() & 0x10,
        0,
        "one-byte sample ends after a single fetch"
    );
}

/// "if the bytes remaining counter becomes zero and the IRQ enabled flag is
/// set, the interrupt flag is set" — and a looped sample "shouldn't ever
/// set IRQ flag" (`7-dmc_basics` sub-tests 9 and 14).
#[test]
fn dmc_irq_fires_at_sample_end_only_when_enabled_and_not_looping() {
    let mut apu = Apu::new();
    apu.write_register(0x4010, 0x80); // IRQ enabled, no loop
    apu.write_register(0x4013, 0x00); // 1 byte
    apu.write_register(0x4015, 0x10);
    apu.dmc_supply_byte(0x00);
    // `irq_line` is the CPU's view and lags the flag by one cycle
    // (W2-21) -- assert BOTH halves, so the lag is pinned rather than
    // merely tolerated.
    assert!(
        !apu.irq_line(),
        "the CPU cannot see the line in the same cycle the flag is raised"
    );
    apu.tick();
    assert!(apu.irq_line(), "sample ended with IRQ enabled");

    let mut apu = Apu::new();
    apu.write_register(0x4010, 0xC0); // IRQ enabled + loop
    apu.write_register(0x4013, 0x00);
    apu.write_register(0x4015, 0x10);
    for _ in 0..8 {
        apu.dmc_supply_byte(0x00);
        apu.tick();
    }
    assert!(
        !apu.irq_line(),
        "a looping sample never ends, so never fires"
    );
}

/// nesdev.org/wiki/APU_Pulse, "Pulse channel output to mixer": the mixer
/// gets the envelope volume "except when the sequencer output is zero, or
/// overflow from the sweep unit's adder is silencing the channel, or the
/// length counter is zero, or the timer has a value less than eight".
/// Nothing in `apu_test` is audible, so these four gates would otherwise
/// ship unverified — this test walks each one independently.
#[test]
fn pulse_output_is_gated_by_all_four_documented_conditions() {
    let mut apu = Apu::new();
    // Constant volume 9, duty 2 (50%), period $100 (>= 8), length loaded.
    apu.write_register(0x4015, 0x01);
    apu.write_register(0x4000, 0x99); // duty 2, halt, constant volume 9
    apu.write_register(0x4002, 0x00);
    apu.write_register(0x4003, 0x01); // period $100, length index 0
    apu.write_register(0x4001, 0x08); // sweep negate, shift 0: never muting

    // Step the sequencer until a step whose duty bit is 1.
    let mut audible = false;
    for _ in 0..64 {
        apu.tick();
        if apu.channel_outputs().pulse1 == 9 {
            audible = true;
            break;
        }
    }
    assert!(audible, "a 50%-duty pulse must reach its envelope volume");

    // Gate 1: length counter zero.
    apu.write_register(0x4015, 0x00);
    assert_eq!(apu.channel_outputs().pulse1, 0, "silenced by length == 0");
    apu.write_register(0x4015, 0x01);
    apu.write_register(0x4003, 0x01);

    // Gate 2: timer period below 8.
    apu.write_register(0x4002, 0x07);
    apu.write_register(0x4003, 0x00); // period 7
    assert_eq!(apu.channel_outputs().pulse1, 0, "silenced by period < 8");

    // Gate 3: sweep-adder overflow (target > $7FF) with the sweep disabled,
    // which nesdev calls out explicitly: "muting happens regardless of
    // whether the sweep unit is disabled".
    apu.write_register(0x4001, 0x00); // no negate, shift 0 -> target = 2x period
    apu.write_register(0x4002, 0x00);
    apu.write_register(0x4003, 0x04); // period $400 -> target $800 > $7FF
    assert_eq!(
        apu.channel_outputs().pulse1,
        0,
        "silenced by a target-period overflow even with sweep disabled"
    );

    // Gate 4: a duty step whose sequence bit is 0.
    let mut apu = Apu::new();
    apu.write_register(0x4015, 0x01);
    apu.write_register(0x4000, 0x39); // duty 0 (12.5%), halt, constant vol 9
    apu.write_register(0x4002, 0x00);
    apu.write_register(0x4003, 0x01);
    apu.write_register(0x4001, 0x08);
    let mut silent_steps = 0;
    let mut audible_steps = 0;
    for _ in 0..(8 * 2 * 0x101) {
        apu.tick();
        match apu.channel_outputs().pulse1 {
            0 => silent_steps += 1,
            9 => audible_steps += 1,
            other => panic!("unexpected pulse output {other}"),
        }
    }
    assert!(
        audible_steps > 0 && silent_steps > audible_steps * 5,
        "12.5% duty: 1 of 8 steps audible, got {audible_steps} vs {silent_steps} silent"
    );
}

/// nesdev.org/wiki/APU_Noise: "The mixer receives the current envelope
/// volume except when bit 0 of the shift register is set, or the length
/// counter is zero"; and nesdev.org/wiki/APU_Envelope: the constant-volume
/// flag "has no effect besides selecting the volume source".
#[test]
fn noise_output_follows_the_lfsr_bit_and_the_envelope_source_select() {
    let mut apu = Apu::new();
    apu.write_register(0x4015, 0x08);
    apu.write_register(0x400C, 0x3B); // halt, constant volume 11
    apu.write_register(0x400E, 0x00); // shortest period
    apu.write_register(0x400F, 0x00); // load length, restart envelope

    let mut seen_zero = false;
    let mut seen_volume = false;
    for _ in 0..256 {
        apu.tick();
        match apu.channel_outputs().noise {
            0 => seen_zero = true,
            11 => seen_volume = true,
            other => panic!("unexpected noise output {other}"),
        }
    }
    assert!(
        seen_zero && seen_volume,
        "the LFSR's bit 0 must gate the output both ways"
    );

    // Envelope mode (constant-volume flag clear) starts at decay level 15.
    let mut apu = Apu::new();
    apu.write_register(0x4015, 0x08);
    apu.write_register(0x400C, 0x2F); // halt/loop, envelope mode, period 15
    apu.write_register(0x400E, 0x00);
    apu.write_register(0x400F, 0x00);
    apu.write_register(0x4017, 0x80); // immediate quarter frame: starts envelope
    let mut peak = 0;
    for _ in 0..256 {
        apu.tick();
        peak = peak.max(apu.channel_outputs().noise);
    }
    assert_eq!(peak, 15, "a freshly started envelope decays from 15");
}

/// nesdev.org/wiki/APU_Triangle: the 32-step sequence runs 15..0, 0..15,
/// and "the sequencer is clocked by the timer as long as both the linear
/// counter and the length counter are nonzero" — a halted triangle holds
/// its current step rather than dropping to zero.
#[test]
fn triangle_walks_its_32_step_sequence_and_freezes_when_gated() {
    let mut apu = Apu::new();
    apu.write_register(0x4015, 0x04);
    apu.write_register(0x4008, 0xFF); // control set, max linear reload
    apu.write_register(0x400A, 0x02); // short period
    apu.write_register(0x400B, 0x00); // length load + linear reload flag
    apu.write_register(0x4017, 0x80); // quarter frame: load the linear counter

    let mut seen = [false; 16];
    for _ in 0..4096 {
        apu.tick();
        seen[apu.channel_outputs().triangle as usize] = true;
    }
    assert!(
        seen.iter().all(|&s| s),
        "every one of the 16 output levels must appear: {seen:?}"
    );

    // Disable via $4015: the length counter goes to zero, so the sequencer
    // stops -- and the output holds its last step (it is not zeroed).
    apu.write_register(0x4015, 0x00);
    let held = apu.channel_outputs().triangle;
    for _ in 0..512 {
        apu.tick();
        assert_eq!(
            apu.channel_outputs().triangle,
            held,
            "a gated triangle freezes rather than silencing"
        );
    }
}

/// nesdev.org/wiki/APU_DMC: "The output level is sent to the mixer whether
/// the channel is enabled or not", is loaded with 0 on power-up, and moves
/// by +/-2 per timer clock according to the shift register's bit 0 --
/// clamped rather than wrapped at the ends of the 0-127 range.
#[test]
fn dmc_output_level_steps_by_two_and_clamps() {
    let mut apu = Apu::new();
    assert_eq!(apu.channel_outputs().dmc, 0, "power-on level");

    apu.write_register(0x4011, 0x7F);
    assert_eq!(apu.channel_outputs().dmc, 127, "direct load");

    // An all-ones sample byte tries to add 2 eight times from 127.
    apu.write_register(0x4010, 0x0F); // fastest rate, no IRQ, no loop
    apu.write_register(0x4013, 0x01);
    apu.write_register(0x4015, 0x10);
    apu.dmc_supply_byte(0xFF);
    for _ in 0..(54 * 10) {
        apu.tick();
    }
    assert_eq!(
        apu.channel_outputs().dmc,
        127,
        "clamped at the top, never wrapped"
    );

    apu.write_register(0x4011, 0x00);
    apu.dmc_supply_byte(0x00); // all zeros: tries to subtract below 0
    for _ in 0..(54 * 10) {
        apu.tick();
    }
    assert_eq!(apu.channel_outputs().dmc, 0, "clamped at the bottom");
}
