//! S-DSP: Gaussian interpolation, ADSR/GAIN, echo, noise, pitch
//! modulation (ticket W7-08).

use crate::apu::dsp::{gaussian, Dsp, Echo, Envelope, EnvelopeStage, Noise, GAUSS};

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

/// ADSR walks attack -> decay -> sustain, and key-off drops to release.
#[test]
fn adsr_moves_through_its_stages() {
    let mut e = Envelope {
        adsr_enabled: true,
        attack_rate: 16,
        decay_rate: 32,
        sustain_rate: 8,
        sustain_level: 3,
        ..Envelope::default()
    };
    e.key_on();
    assert_eq!(e.stage, EnvelopeStage::Attack);
    assert_eq!(e.level, 0, "attack starts from silence");

    let mut guard = 0;
    while e.stage == EnvelopeStage::Attack && guard < 10_000 {
        e.step();
        guard += 1;
    }
    assert_eq!(e.stage, EnvelopeStage::Decay);
    assert_eq!(e.level, Envelope::MAX, "attack ends at full level");

    while e.stage == EnvelopeStage::Decay && guard < 20_000 {
        e.step();
        guard += 1;
    }
    assert_eq!(e.stage, EnvelopeStage::Sustain);
    let expected = (i32::from(e.sustain_level) + 1) * i32::from(Envelope::MAX) / 8;
    assert!(
        (i32::from(e.level) - expected).abs() < 64,
        "decay should settle near the sustain level: {} vs {expected}",
        e.level
    );

    e.key_off();
    assert_eq!(e.stage, EnvelopeStage::Release);
    for _ in 0..1000 {
        e.step();
    }
    assert_eq!(e.level, 0, "release reaches silence");
    assert!(e.is_silent());
}

/// A voice with no envelope configured is SILENT, not full volume — the
/// safe default.
#[test]
fn an_unconfigured_envelope_is_silent() {
    let mut e = Envelope::default();
    e.step();
    assert_eq!(e.level, 0);
    assert!(e.is_silent());
}

#[test]
fn gain_mode_sets_the_level_directly() {
    let mut e = Envelope {
        adsr_enabled: false,
        gain: 0x40,
        ..Envelope::default()
    };
    e.step();
    assert_eq!(e.level, 0x400);
    assert!(!e.is_silent());
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
    assert_eq!(e.buffer_len(), 16, "EDL 0 is a 4-sample minimum, not zero");
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
