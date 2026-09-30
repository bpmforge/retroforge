//! S-DSP: Gaussian interpolation, ADSR/GAIN, echo, noise, pitch
//! modulation (ticket W7-08).

use crate::apu::dsp::{
    counter_fires, decode_brr, gaussian, pmon_step, Dsp, Echo, EchoChannel, Envelope,
    EnvelopeStage, Noise, COUNTER_MAX, COUNTER_RATES, GAUSS, LOOP_CYCLES,
};
use crate::apu::Apu;

/// The four Gaussian taps must sum within the hardware's own documented
/// tolerance at every fraction.
///
/// `GAUSS` is now the S-DSP's actual ROM table (fullsnes, "4-Point
/// Gaussian Interpolation"), which that same source says is "slightly
/// bugged": "Theoretically, each four values ... should sum up to 800h,
/// but in practice they do sum up to 7FFh..801h." This asserts the
/// documented range, not exact unity — a table that clears this bound is
/// either not the hardware table, or is transcribed wrong, either of
/// which is worth catching.
#[test]
fn the_gaussian_kernel_sums_within_the_documented_hardware_tolerance() {
    for f in 0..256usize {
        let sum = i32::from(GAUSS[255 - f])
            + i32::from(GAUSS[511 - f])
            + i32::from(GAUSS[256 + f])
            + i32::from(GAUSS[f]);
        assert!(
            (0x7FF..=0x801).contains(&sum),
            "fraction {f} sums to {sum:#X}, outside fullsnes's documented \
             0x7FF..=0x801 hardware tolerance"
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
    // `Dsp::new()` now defaults FLG to hardware's `E0h` reset value (soft
    // reset + mute + echo-write-disable) — clear it so key-on can
    // actually be heard.
    dsp.write_register(0x6C, 0x20, &aram);
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

/// **The echo buffer's 16-bit word is halved (`SAR 1`) before the FIR
/// sees it.** fullsnes ("SNES APU DSP", `xFh - FIRx`): "buf[(i-0) AND 7]
/// = EchoRAM[addr] SAR 1 ;-input 15bit from Echo RAM" — the buffer stores
/// a 15-bit sample with bit 0 always zero (`6Dh - ESA`'s byte layout:
/// "Byte 0: Lower 7bit of Left sample (stored in bit1-7) (bit0=unused/
/// zero)"), and this is what turns that stored word back into the value
/// the FIR taps operate on.
///
/// This was missing entirely (ticket W7-08 stage 2, spc_dsp6.sfc's
/// `Echo/echo calc` subtest) — the FIR ran on the raw 16-bit word, so
/// every echo round-trip was twice as loud as hardware. With `fir =
/// [0,0,0,0,0,0,0,64]` (`FIR7 = 0x40`, an exact `>>6` divide-by-1 tap)
/// and a raw stored word of `6`, the documented pipeline is
/// `(6 SAR 1) * 0x40 SAR 6 = 3 * 64 / 64 = 3` — an ODD result, which
/// also proves this sum is not itself bit-0-masked (see the companion
/// test on `write_back`).
#[test]
fn echo_read_applies_the_documented_sar_one_before_the_fir() {
    let mut aram = vec![0u8; 8192];
    let mut e = Echo::default();
    e.base_page = 0x10; // ESA -> $1000
    e.fir = [0, 0, 0, 0, 0, 0, 0, 64];
    aram[0x1000] = 0x06; // raw stored word = 6, little-endian
    aram[0x1001] = 0x00;
    e.latch();
    let (sum_left, _sum_right) = e.read_and_filter(&aram);
    assert_eq!(
        sum_left, 3,
        "FIR sum must be computed from the SAR-1-halved sample (6 -> 3), not the raw word (6)"
    );
}

/// **The `AND FFFEh` mask belongs on the echo write-back value
/// (`echo_input`), not on the FIR `sum`.** fullsnes: `echo_input =
/// EchoVoices + ((sum*EFB) SAR 7)` then `echo_input = echo_input AND
/// FFFEh` — two separate quantities, only the second of which is masked.
///
/// With `feedback = 0`, `echo_input` reduces to `dry` alone: an odd
/// `dry` of `1` must be written back as the even `0`, proving the mask
/// runs on the write-back path (companion to the read-side test above,
/// which proves the FIR `sum` itself is allowed to stay odd).
#[test]
fn echo_write_back_masks_bit_zero_not_the_fir_sum() {
    let mut aram = vec![0u8; 16];
    let mut e = Echo::default();
    e.write_disabled = false;
    e.feedback = 0;
    e.write_back(&mut aram, 1, EchoChannel::Left);
    let written = i16::from_le_bytes([aram[0], aram[1]]);
    assert_eq!(
        written, 0,
        "an odd echo_input (dry=1, feedback=0) must be masked to even on write-back"
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
    // `Dsp::new()` now defaults FLG to hardware's `E0h` reset value (soft
    // reset + mute + echo-write-disable) — clear it, or soft reset would
    // zero the envelope this test sets below on the very first sample.
    dsp.write_register(0x6C, 0x20, &aram);

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

/// `$6C` FLG bit 6 (mute) silences only the DAC sample; the echo write
/// still lands in ARAM the same cycle. fullsnes ("SNES APU DSP Control
/// Registers", `6Ch - FLG`): "6 Mute Amplifier (0=Normal, 1=Mute)
/// (doesn't stop internal processing)".
///
/// **No voice, deliberately** — this drives the echo path directly by
/// pre-seeding the ARAM the echo ring reads from, which sidesteps BRR
/// decode, key-on latency and envelope ramp-up entirely (all of them
/// genuinely fragile to hand-tune, as an earlier version of this test
/// on a real BRR voice discovered — a hand-set `last_output` happened to
/// leak through voice 0's `S5`, and a real looping block only ever
/// produced a brief transient blip rather than a sustained tone, both
/// found via mutation-testing and neither worth chasing further here).
/// `ESA`/`EDL` point the ring at `$2000` in an otherwise-empty ARAM;
/// `FIR7` alone is non-zero so the FIR sum comes ONLY from the freshly
/// seeded word (`Echo::read_and_filter`'s history starts at all zero);
/// `EVOL` makes that sum audible and `EFB` makes it feed back into the
/// write. Three `mix()` calls run in sequence: the first only lets
/// `ESA`'s cycle-29 latch take effect (`Echo::latch`) and is not asserted
/// on; the second (unmuted) is checked for audible output and an ARAM
/// write; the third (muted) is checked for a silent DAC sample with ARAM
/// still changing.
#[test]
fn flg_bit_six_mutes_the_dac_without_stopping_the_echo_write() {
    let mut aram = vec![0u8; 0x10000]; // a real 64 KiB ARAM — ESA*256 needs room
                                       // The same loud word ($4000, SAR 1 -> $2000) repeated across the
                                       // first several ring entries, not just one — the read offset
                                       // advances by 4 bytes every sample (`Echo::advance`), so a single
                                       // seeded word would only be under the read head for exactly one of
                                       // the three samples this test runs.
    for w in aram[0x2000..0x2020].chunks_exact_mut(2) {
        w[0] = 0x00;
        w[1] = 0x40;
    }
    let mut dsp = Dsp::new();
    dsp.write_register(0x6D, 0x20, &aram); // ESA = page $20 ($2000)
    dsp.write_register(0x7D, 0x01, &aram); // EDL = 1
    dsp.write_register(0x7F, 0x40, &aram); // FIR7 only, so sum = seed alone
    dsp.write_register(0x2C, 0x7F, &aram); // EVOLL
    dsp.write_register(0x3C, 0x7F, &aram); // EVOLR
    dsp.write_register(0x0D, 0x40, &aram); // EFB, so the write-back is non-zero
    dsp.write_register(0x6C, 0x00, &aram); // unmuted, echo writes enabled

    dsp.mix(&mut aram); // let the ESA latch (cycle 29) take effect

    // The whole seeded region, not one fixed 4-byte slice: the read/write
    // offset advances by 4 bytes every sample (`Echo::advance`), so each
    // of the three `mix()` calls below touches a DIFFERENT 4-byte word
    // within it.
    const ECHO: std::ops::Range<usize> = 0x2000..0x2020;
    let unmuted = dsp.mix(&mut aram);
    assert_ne!(unmuted, (0, 0), "test setup must be audible before muting");
    let aram_after_unmuted = aram[ECHO].to_vec();
    assert_ne!(
        aram_after_unmuted,
        vec![0u8; ECHO.len()],
        "the echo send must have reached ARAM while unmuted"
    );

    dsp.write_register(0x6C, 0x40, &aram); // mute (bit 6), echo writes still on
    let muted = dsp.mix(&mut aram);
    assert_eq!(muted, (0, 0), "FLG.MUTE must zero the DAC sample");
    assert_ne!(
        aram[ECHO],
        aram_after_unmuted[..],
        "echo write-back must keep running while muted — \
         fullsnes says mute \"doesn't stop internal processing\""
    );
}

/// `$6C` FLG bit 7 (soft reset) forces a voice's envelope to exactly 0
/// on the very next sample, which plain KOFF does not — KOFF only starts
/// an 8-per-sample Release decay from wherever the level already was.
/// fullsnes ("KON/KOFF Notes"): "If FLG bit 7 or the KOFF bit for the
/// channel is set, transition to the Release state. If FLG bit 7 is set,
/// also set the envelope to 0."
#[test]
fn flg_bit_seven_zeroes_the_envelope_immediately_unlike_koff_alone() {
    let mut aram = vec![0u8; 64];

    let mut via_koff = Dsp::new();
    // `Dsp::new()` defaults FLG to `E0h` (soft reset + mute + echo-write
    // disable all set, matching hardware's power-on value) — clear it
    // first so this half of the test isolates KOFF from the OTHER new
    // behaviour this ticket added.
    via_koff.write_register(0x6C, 0x20, &aram);
    via_koff.voices[0].envelope.stage = EnvelopeStage::Sustain;
    via_koff.voices[0].envelope.level = 0x400;
    // ADSR enabled with sustain_rate 0 ("rate zero never fires" —
    // `rate_zero_never_fires` above) so the level does not move on its
    // own before KOFF's forced Release takes over; without this, the
    // Sustain stage's un-set GAIN byte decodes as Direct(0) and zeroes
    // the level on the very first sample regardless of KOFF.
    via_koff.voices[0].envelope.adsr_enabled = true;
    via_koff.voices[0].envelope.sustain_rate = 0;
    via_koff.write_register(0x5C, 0x01, &aram); // KOFF voice 0
    via_koff.mix(&mut aram); // one sample: KOFF polls every OTHER sample
    via_koff.mix(&mut aram); // ensure the poll has definitely run once
    assert_eq!(
        via_koff.voices[0].envelope.stage,
        EnvelopeStage::Release,
        "KOFF alone must still enter Release"
    );
    assert!(
        via_koff.voices[0].envelope.level > 0,
        "KOFF alone must NOT jump straight to 0 — it decays by 8/sample \
         from {:#X}, got {:#X} after two samples",
        0x400,
        via_koff.voices[0].envelope.level
    );

    let mut via_soft_reset = Dsp::new();
    via_soft_reset.voices[0].envelope.stage = EnvelopeStage::Sustain;
    via_soft_reset.voices[0].envelope.level = 0x400;
    via_soft_reset.voices[0].envelope.adsr_enabled = true;
    via_soft_reset.voices[0].envelope.sustain_rate = 0;
    via_soft_reset.write_register(0x6C, 0x80, &aram); // FLG bit 7
    via_soft_reset.mix(&mut aram);
    assert_eq!(
        via_soft_reset.voices[0].envelope.level, 0,
        "FLG bit 7 must zero the envelope on the very next sample"
    );
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

// ---------------------------------------------------------------------
// BRR sample-exactness (FR-CORE-036)
// ---------------------------------------------------------------------
//
// The filter formulas below (`decode_brr`'s own comment cites the same
// source) are Nocash's fullsnes, "Bit Rate Reduction (BRR) Format",
// "the exact formulas are": filter 1 `old*1+((-old*1) SAR 4)`, filter 2
// `old*2+((-old*3) SAR 5) - older+((older*1) SAR 4)`, filter 3
// `old*2+((-old*13) SAR 6) - older+((older*3) SAR 4)`. The expected
// arrays here were computed from those formulas independently (a small
// Python transcription, not `decode_brr`'s Rust), so this is a real
// cross-check rather than the function re-stating its own arithmetic.

fn nibbles_to_block(range: u8, filter: u8, nibbles: [i8; 16]) -> [u8; 9] {
    let mut block = [0u8; 9];
    block[0] = (range << 4) | (filter << 2);
    for i in 0..8 {
        let hi = (nibbles[2 * i] as u8) & 0x0F;
        let lo = (nibbles[2 * i + 1] as u8) & 0x0F;
        block[1 + i] = (hi << 4) | lo;
    }
    block
}

/// Filter 1 against the documented formula, starting from silence.
#[test]
fn brr_filter_one_matches_the_documented_formula() {
    let nibbles: [i8; 16] = [1, -1, 2, -2, 3, -3, 4, -4, 5, -5, 6, -6, 7, -7, 1, -1];
    let block = nibbles_to_block(12, 1, nibbles);
    let out = decode_brr(&block, [0, 0]);
    let expected: [i16; 16] = [
        2048, -128, 3976, -369, 5798, -709, 7527, -1136, 9175, -1639, 10751, -2209, 12265, -2838,
        -613, -2623,
    ];
    assert_eq!(out.samples, expected);
}

/// Filter 2 against the documented formula, starting from silence.
#[test]
fn brr_filter_two_matches_the_documented_formula() {
    let nibbles: [i8; 16] = [1, -1, 2, -2, 3, -3, 4, -4, 5, -5, 6, -6, 7, -7, 1, -1];
    let block = nibbles_to_block(12, 2, nibbles);
    let out = decode_brr(&block, [0, 0]);
    let expected: [i16; 16] = [
        2048, 1856, 5714, 5056, 10425, 8988, 15551, 13025, -12280, 0, -8968, 3384, -3575, 8444,
        -11273, 1313,
    ];
    assert_eq!(out.samples, expected);
}

/// Filter 3 against the documented formula, starting from silence.
#[test]
fn brr_filter_three_matches_the_documented_formula() {
    let nibbles: [i8; 16] = [1, -1, 2, -2, 3, -3, 4, -4, 5, -5, 6, -6, 7, -7, 1, -1];
    let block = nibbles_to_block(12, 3, nibbles);
    let out = decode_brr(&block, [0, 0]);
    let expected: [i16; 16] = [
        2048, 1632, 5364, 4216, 9360, 7248, 13610, 10374, -14947, 0, -8336, 5501, -1775, 10772,
        -9923, 4136,
    ];
    assert_eq!(out.samples, expected);
}

/// The "lost sign" 15-bit clip: a value in `0x4000..=0x7FFF` after the
/// 16-bit clamp wraps to `-0x4000..=-1` rather than saturating — fullsnes,
/// same section: "If new=(+4000h..+7FFFh) then new=(-4000h..-1)". Only
/// out-of-spec BRR data (predictor history the encoder's own contract
/// says should never occur) reaches this path; it is exercised here by
/// feeding `decode_brr` a `prev` outside any value real decoding would
/// produce, which is exactly how an encoder bug would surface it.
#[test]
fn brr_positive_overflow_wraps_to_negative_not_saturating() {
    let block = nibbles_to_block(12, 1, [7; 16]);
    // filter 1: add = 32767 + ((-32767) >> 4) = 30719; raw = (7<<12)>>1 =
    // 14336; sum = 45055, clamps to 16-bit 32767 (0x7FFF), which is in
    // 0x4000..=0x7FFF, so it wraps to 32767 - 0x8000 = -1.
    let out = decode_brr(&block, [32767, 0]);
    assert_eq!(
        out.samples[0], -1,
        "a 16-bit-clamped 0x7FFF must wrap to -1 (lost-sign glitch), not \
         saturate at 0x7FFF cast down"
    );
}

/// The negative half of the same glitch: fullsnes "If
/// new=(-8000h..-4001h) then new=(-0..-3FFFh)".
#[test]
fn brr_negative_overflow_wraps_to_zero_not_saturating() {
    let block = nibbles_to_block(12, 1, [-8; 16]);
    // filter 1: add = -32768 + ((32768) >> 4) = -30720; raw = (-8<<12)>>1
    // = -16384; sum = -47104, clamps to 16-bit -32768 (0x8000), which is
    // in -0x8000..=-0x4001, so it wraps to -32768 + 0x8000 = 0.
    let out = decode_brr(&block, [-32768, 0]);
    assert_eq!(
        out.samples[0], 0,
        "a 16-bit-clamped -0x8000 must wrap to 0 (lost-sign glitch)"
    );
}

/// Range 13-15 behave as range 12 with the nibble arithmetic-shifted
/// right by 3 first ("decoding works as if shift=12 and
/// nibble=(nibble SAR 3)") — not "shift further", which is the mistake
/// fullsnes calls out as the detail most decoders miss.
#[test]
fn brr_range_thirteen_to_fifteen_use_the_reserved_case_not_a_bigger_shift() {
    for range in [13u8, 14, 15] {
        let block = nibbles_to_block(range, 0, [4; 16]);
        let out = decode_brr(&block, [0, 0]);
        // nibble 4 SAR 3 = 0, so with filter 0 the sample is exactly 0 —
        // NOT `(4 << range) >> 1`, which for range=13 would be 16384.
        assert_eq!(
            out.samples[0], 0,
            "range {range}: must use the reserved shift=12-with-SAR-3 \
             case, not keep shifting"
        );
    }
}

/// KON is consumed when it is polled: one write is ONE key-on, however
/// long the value stays readable in the register (W7-08, Blackthorne's
/// ENVX poller).
///
/// fullsnes, "KON/KOFF Notes": "KON effectively takes effect 'on write',
/// even though a non-zero value can be read back much later. KOFF and
/// FLG.7, on the other hand, exert their influence constantly until a new
/// value is written", and the interaction list ends "Set the 'internal'
/// value of KON to 0". A model that re-latches the register at every
/// poll re-keys the voice every other sample for ever, so its envelope
/// never gets past the first attack step and a KOFF written afterwards
/// can never be seen to win — a driver that waits for the released voice's
/// ENVX to fall below 8 then waits for ever.
#[test]
fn one_kon_write_keys_on_once_not_at_every_poll() {
    let mut dsp = Dsp::new();
    let mut aram = vec![0u8; 0x10000];
    dsp.write_register(0x6C, 0x00, &aram); // FLG: leave reset (E0 = soft reset + mute)
    dsp.write_register(0x05, 0x8F, &aram); // ADSR1: ADSR on, attack $F (fast)
    dsp.write_register(0x06, 0xE0, &aram); // ADSR2: sustain level 7, rate 0 (hold)
    dsp.write_register(0x4C, 0x01, &aram); // KON voice 0, never rewritten

    let mut prev = 0i16;
    for n in 0..400 {
        let _ = dsp.mix(&mut aram);
        let level = dsp.voices[0].envelope.level;
        // Once the attack is over the level only ever creeps down through
        // the decay step; a re-key would drop it to 0 and climb again.
        if n >= 8 {
            assert!(
                level >= 0x700,
                "sample {n}: level {level} (was {prev}); the stale KON value \
                 re-keyed the voice"
            );
        }
        prev = level;
    }
    assert!(
        prev >= 0x700,
        "a fast attack held at sustain level 7 must sit at the top; got {prev}"
    );
}

/// The Blackthorne shape: KON and KOFF both left set for a voice. KON
/// zeroes the envelope once, then KOFF wins and the release runs the level
/// down to 0, where it stays (fullsnes: "Setting both KOFF and KON for a
/// channel will turn the channel off much faster than just KOFF alone").
#[test]
fn a_standing_koff_releases_a_voice_that_was_keyed_once() {
    let mut dsp = Dsp::new();
    let mut aram = vec![0u8; 0x10000];
    dsp.write_register(0x6C, 0x00, &aram); // FLG: leave reset (E0 = soft reset + mute)
    dsp.write_register(0x05, 0x8F, &aram);
    dsp.write_register(0x06, 0xE0, &aram);
    dsp.write_register(0x4C, 0x01, &aram);
    for _ in 0..40 {
        let _ = dsp.mix(&mut aram);
    }
    assert!(dsp.voices[0].envelope.level > 0x600);
    dsp.write_register(0x5C, 0x01, &aram); // KOFF, left standing
                                           // 0x7FF / 8 = 256 samples of release, plus the poll latency.
    for _ in 0..300 {
        let _ = dsp.mix(&mut aram);
    }
    assert_eq!(dsp.voices[0].envelope.level, 0);
    assert_eq!(dsp.read_register(0x08), 0, "ENVX reads 0 once released");
}

/// PMON's step is the documented integer formula, not a float estimate
/// (fullsnes "Pitch Counter": `Factor = (OUTX SAR 4) + 400h`,
/// `Step = (Step * Factor) SAR 10`).
#[test]
fn pmon_step_is_the_documented_integer_formula() {
    assert_eq!(pmon_step(0x1000, 0), 0x1000, "silence leaves pitch alone");
    assert_eq!(pmon_step(0x1000, 0x2000), 0x1800, "factor 0x600");
    assert_eq!(pmon_step(0x1000, -0x4000), 0, "factor 0");
    assert_eq!(pmon_step(0x1000, 0x3FFF), 0x1FFC, "factor 0x7FF, exact");
    assert_eq!(pmon_step(0x3000, 0x3FFF), 0x3FFF, "clamped to 14 bits");
}
