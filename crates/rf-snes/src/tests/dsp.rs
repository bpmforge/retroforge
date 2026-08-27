//! S-DSP: Gaussian interpolation, ADSR/GAIN, echo, noise, pitch
//! modulation (ticket W7-08).

use crate::apu::dsp::{
    counter_fires, gaussian, Dsp, Echo, Envelope, EnvelopeStage, Noise, COUNTER_MAX, COUNTER_RATES,
    GAUSS, LOOP_CYCLES,
};
use crate::apu::Apu;

/// The four Gaussian taps must sum to 2048 at every fraction — that is
/// what makes the filter unity-gain.
///
/// A kernel that does not is the subtlest possible audio bug: it changes
/// volume with pitch, so a sustained note swells or fades as it bends.
#[test]
fn the_gaussian_kernel_is_unity_gain_at_every_fraction() {
    for f in 0..256usize {
        let sum = i32::from(GAUSS[255 - f])
            + i32::from(GAUSS[511 - f])
            + i32::from(GAUSS[256 + f])
            + i32::from(GAUSS[f]);
        assert_eq!(
            sum, 2048,
            "fraction {f} sums to {sum}, not 2048 — the kernel is not \
             unity-gain, so volume would change with pitch"
        );
    }
}

/// A constant input must come out constant — the filter must not ring on
/// DC.
#[test]
fn the_filter_passes_a_constant_unchanged() {
    let flat = [1000i16; 4];
    for f in [0u16, 0x400, 0x800, 0xFFF] {
        let out = gaussian(&flat, f);
        assert!(
            (i32::from(out) - 1000).abs() <= 4,
            "fraction {f:#05X}: DC in must be DC out, got {out}"
        );
    }
}

/// Interpolating between two levels must land between them.
#[test]
fn the_filter_interpolates_between_neighbouring_samples() {
    let ramp = [0i16, 0, 1000, 1000];
    let mid = gaussian(&ramp, 0x800);
    assert!(
        (0..=1000).contains(&mid),
        "an interpolated sample must lie between its neighbours, got {mid}"
    );
}

/// Drive an envelope for `n` samples on a real global counter.
///
/// The counter is not decoration: rates fire on
/// `(counter + offset) % rate == 0`, so an envelope stepped with a
/// constant counter either never moves or moves every single sample.
/// Every envelope test below needs a counter that actually counts.
fn run_envelope(e: &mut Envelope, n: u32) -> u16 {
    let mut counter = COUNTER_MAX;
    for _ in 0..n {
        e.step(counter);
        counter = if counter == 0 {
            COUNTER_MAX
        } else {
            counter - 1
        };
    }
    counter
}

/// ADSR walks attack -> decay -> sustain, and key-off drops to release.
#[test]
fn adsr_moves_through_its_stages() {
    let mut e = Envelope {
        adsr_enabled: true,
        // aaaa = %1111 is the fast attack: R=31, E += 1024, so the level
        // crosses 0x7FF in two samples.
        attack_rate: 0x0F,
        decay_rate: 7,
        sustain_rate: 0x1F,
        sustain_level: 3,
        ..Envelope::default()
    };
    e.key_on();
    assert_eq!(e.stage, EnvelopeStage::Attack);
    assert_eq!(e.level, 0, "attack starts from silence");

    let mut counter = COUNTER_MAX;
    let mut guard = 0;
    while e.stage == EnvelopeStage::Attack && guard < 10_000 {
        e.step(counter);
        counter = if counter == 0 {
            COUNTER_MAX
        } else {
            counter - 1
        };
        guard += 1;
    }
    assert_eq!(e.stage, EnvelopeStage::Decay);
    assert_eq!(e.level, Envelope::MAX, "attack ends clamped at full level");

    while e.stage == EnvelopeStage::Decay && guard < 200_000 {
        e.step(counter);
        counter = if counter == 0 {
            COUNTER_MAX
        } else {
            counter - 1
        };
        guard += 1;
    }
    assert_eq!(e.stage, EnvelopeStage::Sustain);
    // "When the upper 3 bits of E equal the Sustain Level, enter the
    // Sustain state" — so the level lands inside that 256-wide band,
    // NOT on a computed fraction of full scale.
    assert_eq!(
        (e.level >> 8) as u8,
        e.sustain_level,
        "decay hands over when the top 3 bits match lll, got {:#x}",
        e.level
    );

    e.key_off();
    assert_eq!(e.stage, EnvelopeStage::Release);
    // Release is R=31 (every sample), E -= 8: 0x7FF needs at most 256.
    run_envelope(&mut e, 1000);
    assert_eq!(e.level, 0, "release reaches silence");
    assert!(e.is_silent());
}

/// Attack with `aaaa == %1111` is the documented special case: rate 31
/// and `E += 1024`, so it saturates in two steps rather than following
/// the pretend-GAIN path the other fifteen rates use.
#[test]
fn the_fastest_attack_saturates_in_two_steps() {
    let mut e = Envelope {
        adsr_enabled: true,
        attack_rate: 0x0F,
        ..Envelope::default()
    };
    e.key_on();
    run_envelope(&mut e, 1);
    assert_eq!(e.level, 1024);
    run_envelope(&mut e, 1);
    assert_eq!(e.level, Envelope::MAX, "clamped, not wrapped");
    assert_eq!(e.stage, EnvelopeStage::Decay, "overflow ends the attack");
}

/// Rate 0 is `Inf` in the counter table: it NEVER fires.
///
/// This is how a voice holds a level indefinitely, and it is the one
/// entry a naive "every N samples" divider cannot express — dividing by
/// zero is not the same as never.
#[test]
fn rate_zero_never_fires() {
    for counter in [0u16, 1, 1040, 536, COUNTER_MAX, 0x1234] {
        assert!(!counter_fires(counter, 0), "rate 0 fired at {counter}");
    }
}

/// The counter offsets keep different rates in a fixed relative phase.
///
/// Rates 2 and 3 have offsets 1040 and 536 against periods 1536 and
/// 1280. If the offsets were dropped — the obvious simplification — both
/// would fire together whenever the counter hit a common multiple, and
/// two voices set to neighbouring rates would beat against each other
/// instead of staying spread out.
#[test]
fn counter_offsets_are_not_decoration() {
    let fires = |rate: u8| -> Vec<u16> {
        (0..=COUNTER_MAX)
            .filter(|&c| counter_fires(c, rate))
            .collect()
    };
    let two = fires(2);
    let three = fires(3);
    assert!(!two.is_empty() && !three.is_empty());
    // With the offsets applied the two rates never land on the same
    // counter value across the whole cycle.
    let shared: Vec<_> = two.iter().filter(|c| three.contains(c)).collect();
    assert!(
        shared.is_empty(),
        "rates 2 and 3 should stay out of phase, but share {} values",
        shared.len()
    );
}

/// Every rate in the table fires at exactly its documented period.
#[test]
fn each_rate_fires_at_its_documented_period() {
    for rate in 1..32u8 {
        let period = u32::from(COUNTER_RATES[usize::from(rate)]);
        let hits = (0..period * 4).filter(|&i| {
            let c = (COUNTER_MAX as u32).wrapping_sub(i) as u16;
            counter_fires(c, rate)
        });
        assert_eq!(
            hits.count() as u32,
            4,
            "rate {rate} should fire 4 times in {} samples",
            period * 4
        );
    }
}

/// An unconfigured envelope is SILENT, not full volume — the safe
/// default.
#[test]
fn an_unconfigured_envelope_is_silent() {
    let mut e = Envelope::default();
    run_envelope(&mut e, 1);
    assert_eq!(e.level, 0);
    assert!(e.is_silent());
}

/// Direct Gain (`$x7` bit 7 clear) sets the level outright: `E = g * 16`,
/// and the rate does not matter.
#[test]
fn gain_mode_sets_the_level_directly() {
    let mut e = Envelope {
        adsr_enabled: false,
        gain: 0x40,
        stage: EnvelopeStage::Attack,
        ..Envelope::default()
    };
    run_envelope(&mut e, 1);
    assert_eq!(e.level, 0x400);
    assert!(!e.is_silent());
}

/// The four GAIN modes each move the level their documented way.
#[test]
fn the_four_gain_modes_move_the_level_as_documented() {
    // Rate 31 fires every sample, so one step is one adjustment.
    let mk = |mode: u8, level: i16| Envelope {
        adsr_enabled: false,
        gain: 0x80 | (mode << 5) | 0x1F,
        level,
        stage: EnvelopeStage::Attack,
        ..Envelope::default()
    };

    let mut lin_dec = mk(0, 0x400);
    run_envelope(&mut lin_dec, 1);
    assert_eq!(lin_dec.level, 0x400 - 32, "linear decrease is E -= 32");

    let mut exp_dec = mk(1, 0x400);
    run_envelope(&mut exp_dec, 1);
    assert_eq!(
        exp_dec.level,
        0x400 - (((0x400 - 1) >> 8) + 1),
        "exp decrease is E -= ((E-1)>>8)+1"
    );

    let mut lin_inc = mk(2, 0x400);
    run_envelope(&mut lin_inc, 1);
    assert_eq!(lin_inc.level, 0x400 + 32, "linear increase is E += 32");

    // Bent increase steps by 32 below 0x600 and by 8 at or above it.
    let mut bent_low = mk(3, 0x100);
    run_envelope(&mut bent_low, 1);
    assert_eq!(bent_low.level, 0x100 + 32);

    let mut bent_high = mk(3, 0x600);
    // The bend reads the PRE-CLAMP level from the previous update, so
    // prime it by running one step from a level already in the high
    // region rather than asserting on the very first sample.
    bent_high.step(COUNTER_MAX);
    let before = bent_high.level;
    bent_high.step(COUNTER_MAX - 1);
    assert_eq!(bent_high.level, before + 8, "above 0x600 the bend is +8");
}

/// The LFSR must never latch to zero — an all-zero state is a fixed
/// point that would produce silence forever.
#[test]
fn the_noise_lfsr_actually_varies() {
    let mut n = Noise::default();
    let mut seen = std::collections::HashSet::new();
    for _ in 0..40_000 {
        seen.insert(n.step());
    }
    assert!(seen.len() > 100, "noise must vary: {} values", seen.len());
}

/// Noise REPLACES a voice's sample stream rather than mixing into it.
#[test]
fn noise_replaces_a_voices_sample_source() {
    let mut aram = vec![0u8; 4096];
    let mut dsp = Dsp::new();
    dsp.voices[0].vol_left = 0x7F;
    dsp.voices[0].envelope.gain = 0x7F;
    dsp.voices[0].pitch = 0x1000;
    dsp.noise_enable = 0x01;
    dsp.key_on(0x01);
    let energy: i32 = (0..64).map(|_| i32::from(dsp.mix(&mut aram).0.abs())).sum();
    assert!(energy > 0, "a noise voice must be audible with empty ARAM");
}

#[test]
fn the_echo_buffer_length_follows_edl() {
    let mut e = Echo::default();
    // **FOUR bytes — one stereo sample — not sixteen.** This assertion
    // used to read 16 with the comment "EDL 0 is a 4-sample minimum",
    // which was an assumption written down rather than checked. The
    // buffer is EDL<<11 bytes, and at EDL=0 hardware still reserves one
    // sample so the ring has somewhere to live (SNESdev/sneslab: "when
    // EDL=0, the buffer is 4 bytes (1 sample) rather than 0").
    assert_eq!(e.buffer_len(), 4, "EDL 0 is ONE stereo sample: 4 bytes");
    e.delay = 1;
    assert_eq!(e.buffer_len(), 2048);
    e.delay = 4;
    assert_eq!(e.buffer_len(), 8192);
}

/// **`$6C` bit 5 disables echo WRITES.** Games use it to freeze a tail
/// without clearing it, so a disabled echo must still read.
#[test]
fn disabling_echo_writes_leaves_aram_untouched() {
    let mut aram = vec![0u8; 8192];
    let mut e = Echo::default();
    e.delay = 1;
    e.vol_left = 0x7F;
    e.vol_right = 0x7F;
    e.write_disabled = true;
    e.process(&mut aram, (10_000, 10_000));
    assert!(
        aram.iter().all(|&b| b == 0),
        "with writes disabled, echo must not touch ARAM"
    );

    e.write_disabled = false;
    e.process(&mut aram, (10_000, 10_000));
    assert!(
        aram.iter().any(|&b| b != 0),
        "with writes enabled it must write its ring buffer"
    );
}

/// The echo buffer really is ARAM at `ESA` — a game that miscomputes it
/// can overwrite its own samples, and the model must express that rather
/// than hide it in a private buffer.
#[test]
fn the_echo_buffer_lives_in_aram_at_esa() {
    let mut aram = vec![0u8; 16384];
    let mut e = Echo::default();
    e.base_page = 0x10; // $1000
    e.delay = 1;
    e.write_disabled = false;
    e.process(&mut aram, (0x4000, 0x4000));
    assert!(
        aram[0x1000..0x1008].iter().any(|&b| b != 0),
        "the ring buffer must start at ESA * 256"
    );
    assert!(
        aram[..0x1000].iter().all(|&b| b == 0),
        "and must not touch memory below it"
    );
}

#[test]
fn zero_echo_volume_is_silent() {
    let mut aram = vec![0u8; 8192];
    let mut e = Echo::default();
    e.delay = 1;
    e.write_disabled = false;
    e.fir = [127, 0, 0, 0, 0, 0, 0, 0];
    for _ in 0..32 {
        assert_eq!(e.process(&mut aram, (10_000, 10_000)), (0, 0));
    }
}

/// **PMON bit 0 does nothing**, because voice 0 has no predecessor. A
/// model that wrapped to voice 7 would sound plausible and be wrong.
#[test]
fn pitch_modulation_cannot_apply_to_voice_zero() {
    let mut plain = Dsp::new();
    let mut modulated = Dsp::new();
    for d in [&mut plain, &mut modulated] {
        d.voices[0].vol_left = 0x7F;
        d.voices[0].envelope.gain = 0x7F;
        d.voices[0].pitch = 0x1000;
        d.noise_enable = 0x01;
        d.key_on(0x01);
    }
    modulated.pitch_mod = 0x01;

    let mut a1 = vec![0u8; 4096];
    let mut a2 = vec![0u8; 4096];
    let a: Vec<i16> = (0..32).map(|_| plain.mix(&mut a1).0).collect();
    let b: Vec<i16> = (0..32).map(|_| modulated.mix(&mut a2).0).collect();
    assert_eq!(a, b, "PMON bit 0 must have no effect");
}

/// Silence in, silence out — still true with everything wired up.
#[test]
fn a_dsp_with_nothing_keyed_on_is_still_silent() {
    let mut aram = vec![0u8; 4096];
    let mut dsp = Dsp::new();
    for _ in 0..128 {
        assert_eq!(dsp.mix(&mut aram), (0, 0));
    }
}

// ---------------------------------------------------------------------
// The DSP register file and its $F2/$F3 plumbing (ticket W7-08).
//
// Before this existed the S-DSP was unreachable: Apu::write_register had
// no $F3 arm at all and its read arm was a literal 0, so no SPC700
// program could program a single DSP register and a running machine
// produced no sound whatever the mixer could do.
// ---------------------------------------------------------------------

/// `$F2` selects and `$F3` reads/writes — the only path there is.
#[test]
fn dsp_registers_are_reachable_through_f2_and_f3() {
    let mut apu = Apu::new();
    // $00 is voice 0's left volume.
    apu.write_register(0xF2, 0x00);
    apu.write_register(0xF3, 0x5A);
    assert_eq!(apu.dsp.voices[0].vol_left, 0x5A, "the write must DECODE");

    apu.write_register(0xF2, 0x00);
    assert_eq!(
        apu.read_register(0xF3),
        0x5A,
        "and read back — a literal 0 here is what made every DSP register \
         write invisible to the program that made it"
    );
}

/// Addresses are 7-bit: `$80`+ mirrors `$00`+.
#[test]
fn the_dsp_address_is_seven_bits() {
    let mut apu = Apu::new();
    apu.write_register(0xF2, 0x80); // mirrors $00
    apu.write_register(0xF3, 0x33);
    assert_eq!(apu.dsp.voices[0].vol_left, 0x33);
}

/// PITCH spans two registers, so each write must preserve the other half.
#[test]
fn pitch_is_fourteen_bits_across_two_registers() {
    let mut dsp = Dsp::new();
    let aram = [0u8; 64];
    dsp.write_register(0x02, 0x34, &aram); // voice 0 pitch low
    dsp.write_register(0x03, 0x12, &aram); // voice 0 pitch high
    assert_eq!(dsp.voices[0].pitch, 0x1234);
    // Rewriting only the low half must not clear the high half.
    dsp.write_register(0x02, 0x78, &aram);
    assert_eq!(
        dsp.voices[0].pitch, 0x1278,
        "a half-write that clobbers the other half retunes every voice \
         whose pitch is set one byte at a time"
    );
    // The high register is 6 bits; bits above that are not pitch.
    dsp.write_register(0x03, 0xFF, &aram);
    assert_eq!(dsp.voices[0].pitch, 0x3F78);
}

/// Key-on resolves the sample address through the `$5D` DIR directory,
/// and does it AT KEY-ON — not when SRCN is written.
///
/// The ordinary program order is SRCN first, DIR second. Resolving at
/// SRCN-write time would use whatever DIR happened to hold then, which is
/// the previous song's directory.
#[test]
fn key_on_resolves_the_sample_address_through_dir() {
    let mut dsp = Dsp::new();
    let mut aram = vec![0u8; 0x10000];
    // Directory page $02, entry 3 -> start $ABCD, loop $1234.
    let entry = 0x02 * 0x100 + 3 * 4;
    aram[entry] = 0xCD;
    aram[entry + 1] = 0xAB;
    aram[entry + 2] = 0x34;
    aram[entry + 3] = 0x12;

    dsp.write_register(0x04, 3, &aram); // voice 0 SRCN = 3, DIR still 0
    dsp.write_register(0x5D, 0x02, &aram); // DIR = $02, written AFTER
    dsp.write_register(0x4C, 0x01, &aram); // KON voice 0

    // **The write alone does nothing**, and that is the accurate
    // behaviour: KON is polled at cycle 30 every other sample and each
    // voice acts at its own S3c "using previously loaded values". A
    // key-on that took effect the instant the register was written was
    // the old model, and it is exactly the timing blargg's suite
    // measures.
    assert!(
        !dsp.voices[0].keyed_on,
        "KON is latched at cycle 30, not acted on at write time"
    );

    let mut aram = vec![0u8; 0x10000];
    aram[entry] = 0xCD;
    aram[entry + 1] = 0xAB;
    aram[entry + 2] = 0x34;
    aram[entry + 3] = 0x12;
    // Two samples covers the every-other-sample poll from any starting
    // phase.
    for _ in 0..2 {
        let _ = dsp.mix(&mut aram);
    }

    assert_eq!(dsp.voices[0].start, 0xABCD);
    assert_eq!(dsp.voices[0].loop_addr, 0x1234);
    assert!(dsp.voices[0].keyed_on);
}

/// ENDX, OUTX and ENVX become readable at three DIFFERENT cycles.
///
/// This is the property the whole 32-cycle machine exists to provide, and
/// the one a sample-granular mixer cannot have at all: it updates every
/// register at one instant, so the three distances below are all zero.
///
/// For voice 0 the document places the preparations at S5/S6/S7 and the
/// reads at S7/S8/S9 — cycles 0, 1, 2 and 2, 3, 4 of the loop. So a value
/// the CPU writes into ENVX survives until cycle 4, OUTX until 3, and the
/// three do not fall together.
///
/// Source: anomie's `apudsp.txt` `$Revision: 1212$`, SOUND GENERATION.
#[test]
fn outx_and_envx_become_visible_at_their_own_cycles() {
    let overwritten_after = |reg: u8| -> Option<u16> {
        let mut dsp = Dsp::new();
        let mut aram = vec![0u8; 0x10000];
        dsp.write_register(reg, 0x88, &aram);
        assert_eq!(dsp.read_register(reg), 0x88, "the write must stick");
        // One full 64-cycle loop is enough to see any placement.
        for c in 0..LOOP_CYCLES {
            dsp.tick(&mut aram);
            if dsp.read_register(reg) != 0x88 {
                return Some(c + 1);
            }
        }
        None
    };

    let outx = overwritten_after(0x09).expect("OUTX must be overwritten within one loop");
    let envx = overwritten_after(0x08).expect("ENVX must be overwritten within one loop");

    // Voice 0's S8 is cycle 3 and its S9 is cycle 4; a fresh DSP starts
    // at cycle 0, so the tick that lands on each is the 4th and the 5th.
    assert_eq!(outx, 4, "OUTX becomes readable at voice 0's S8 (cycle 3)");
    assert_eq!(envx, 5, "ENVX becomes readable at voice 0's S9 (cycle 4)");
    assert!(
        envx > outx,
        "ENVX must land AFTER OUTX — they are one step apart, not simultaneous"
    );
}

/// A sample takes exactly 32 cycles, and the loop is 64 long.
#[test]
fn a_sample_is_thirty_two_cycles_and_the_loop_is_sixty_four() {
    let mut dsp = Dsp::new();
    let mut aram = vec![0u8; 0x10000];
    let mut produced = 0;
    for _ in 0..LOOP_CYCLES {
        if dsp.tick(&mut aram).sample.is_some() {
            produced += 1;
        }
    }
    assert_eq!(produced, 2, "64 cycles is exactly two samples");
}

/// A program-controlled DIR/SRCN can point past ARAM; that must not panic.
#[test]
fn a_directory_entry_past_aram_is_not_a_panic() {
    let mut dsp = Dsp::new();
    let aram = [0u8; 64]; // deliberately tiny
    dsp.write_register(0x04, 0xFF, &aram);
    dsp.write_register(0x5D, 0xFF, &aram);
    dsp.write_register(0x4C, 0x01, &aram);
    assert_eq!(dsp.voices[0].start, 0, "out-of-range reads as zero");
}

/// ENVX and OUTX are ORDINARY STORAGE that the DSP overwrites once per
/// sample — they are not read-only.
///
/// **This test used to assert the opposite**, on the reasoning that
/// returning a written value "would be a convincing lie: a program
/// polling ENVX for an envelope to decay would spin forever". That
/// reasoning was wrong, and blargg's `spc_dsp6` is what caught it — its
/// very first check writes `$88` to `$08` and reads it straight back, and
/// against the old model it read `00` and the ROM gave up.
///
/// SNESdev is explicit: "VxENVX is technically writable and not intended
/// to be written to. The S-DSP updates this register once per sample."
/// Both halves matter — the write sticks, AND the next sample lands on
/// it, which is what stops a poller spinning forever.
#[test]
fn envx_and_outx_are_storage_the_dsp_overwrites_each_sample() {
    let mut dsp = Dsp::new();
    let aram = [0u8; 64];

    // The write sticks and reads straight back.
    dsp.write_register(0x08, 0x88, &aram);
    assert_eq!(
        dsp.read_register(0x08),
        0x88,
        "a written ENVX must read back before the next sample overwrites it"
    );
    dsp.write_register(0x09, 0x77, &aram);
    assert_eq!(dsp.read_register(0x09), 0x77, "and OUTX likewise");

    // ...and one sample of mixing lands on it.
    let mut aram = vec![0u8; 0x10000];
    dsp.voices[0].envelope.level = 0x7F0;
    let _ = dsp.mix(&mut aram);
    // The envelope also STEPS during that sample, so the exact value is
    // whatever the envelope now holds — asserting a hardcoded number here
    // would pin the envelope's rate model rather than this register's
    // update cadence, which is what the test is about.
    // **ENVX lags the envelope by design.** The value is prepared at the
    // voice's S7 and only becomes readable at S9, so a read taken after
    // a whole sample sees the level as it stood when S7 ran — not the
    // live level now. That gap is not slop: it is the quantity
    // `spc_dsp6` counts reads to measure.
    let live = (dsp.voices[0].envelope.level >> 4) as u8;
    let visible = dsp.read_register(0x08);
    assert!(
        visible.abs_diff(live) <= 1,
        "ENVX must track the envelope within one preparation step: \
         visible {visible}, live {live}"
    );
    assert_ne!(
        visible, 0x88,
        "and that update must have landed on the CPU's written value"
    );
}

/// ENDX is cleared by ANY write to `$7C`, and the value written is
/// ignored — storing it would leave a program that writes $FF believing
/// every voice had ended.
#[test]
fn writing_endx_clears_it_regardless_of_value() {
    let mut dsp = Dsp::new();
    let aram = [0u8; 64];
    dsp.endx = 0b1010_1010;
    dsp.write_register(0x7C, 0xFF, &aram);
    assert_eq!(dsp.read_register(0x7C), 0);
}

/// `$6C` FLG bit 5 disables echo WRITES while reads continue — how a game
/// freezes an echo tail without clearing it.
#[test]
fn flg_bit_five_disables_echo_writes() {
    let mut dsp = Dsp::new();
    let aram = [0u8; 64];
    dsp.write_register(0x6C, 0x00, &aram);
    assert!(!dsp.echo.write_disabled);
    dsp.write_register(0x6C, 0x20, &aram);
    assert!(dsp.echo.write_disabled);
}

/// The eight FIR coefficients live one per voice row at `$xF`.
#[test]
fn the_fir_coefficients_are_one_per_voice_row() {
    let mut dsp = Dsp::new();
    let aram = [0u8; 64];
    for i in 0..8u8 {
        dsp.write_register(i * 0x10 + 0x0F, (i as i8 - 4) as u8, &aram);
    }
    assert_eq!(dsp.echo.fir, [-4, -3, -2, -1, 0, 1, 2, 3]);
}

/// The DSP is CLOCKED, not merely programmable: 32 SPC cycles per sample.
///
/// Being reachable through $F2/$F3 is not enough — without a sample clock
/// nothing advances an envelope or moves the echo buffer, and a program
/// polling ENVX still spins forever.
#[test]
fn the_dsp_is_clocked_by_the_apu() {
    let mut apu = Apu::new();
    apu.dsp.voices[0].envelope.level = 0x400;
    apu.dsp.voices[0].envelope.stage = crate::apu::dsp::EnvelopeStage::Release;
    let before = apu.dsp.voices[0].envelope.level;
    // Drive the APU the way the machine does — through `step`, NOT by
    // calling the sample clock directly. Calling `tick_dsp` here would
    // test that the function works while leaving "does anything CALL it?"
    // unasked, which is exactly how the mixer sat wired to nothing for
    // three tickets.
    for _ in 0..20_000 {
        let _ = apu.step();
    }
    assert!(
        apu.dsp.voices[0].envelope.level < before,
        "a released envelope must decay once the DSP is actually running;          level stayed at {before}"
    );
}
