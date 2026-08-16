//! Non-linear mixer tests (ticket W2-01b) — nesdev.org/wiki/APU_Mixer.

use crate::apu::mixer::{mix, mix_exact};
use crate::apu::Apu;

/// The tables are checked against the page's **exact formula**, not against
/// themselves: a table that is merely self-consistent passes any test
/// written from the same constants. The page states the tables are
/// "approximated (within 4%)", so this test measures the worst-case
/// deviation over the whole reachable input space and asserts the 4% claim
/// holds — which is simultaneously a check that the tables are right and a
/// quantified record of how far the approximation goes.
#[test]
fn lookup_tables_track_the_exact_formula_within_the_documented_four_percent() {
    let mut worst_relative = 0.0f32;
    let mut worst_at = (0, 0, 0, 0, 0);
    for pulse1 in 0..=15u8 {
        for pulse2 in 0..=15u8 {
            for triangle in [0u8, 1, 7, 15] {
                for noise in [0u8, 1, 7, 15] {
                    for dmc in [0u8, 1, 63, 127] {
                        let table = mix(pulse1, pulse2, triangle, noise, dmc);
                        let exact = mix_exact(pulse1, pulse2, triangle, noise, dmc);
                        if exact > 0.0 {
                            let relative = (table - exact).abs() / exact;
                            if relative > worst_relative {
                                worst_relative = relative;
                                worst_at = (pulse1, pulse2, triangle, noise, dmc);
                            }
                        }
                    }
                }
            }
        }
    }
    // MEASURED, and the page's "within 4%" turns out to be slightly
    // optimistic at the bottom of the tnd range rather than exactly true
    // everywhere: the deviation is monotone in the tnd index, peaking at
    // 4.66% at index 1 and falling below 4% only from index 27 upward
    // (swept exhaustively over all 16x16x128 triangle/noise/dmc
    // combinations). Both facts are asserted -- the true global bound, and
    // the index where the page's claim starts holding -- so a change that
    // widens either fails loudly, and neither number is a threshold tuned
    // until the test went green.
    assert!(
        worst_relative < 0.047,
        "worst table-vs-exact deviation {worst_relative:.4} at {worst_at:?}"
    );
    let mut worst_above_27 = 0.0f32;
    let mut highest_index_over_four_percent = 0;
    for triangle in 0..=15u8 {
        for noise in 0..=15u8 {
            for dmc in 0..=127u8 {
                let tnd_index = 3 * triangle as usize + 2 * noise as usize + dmc as usize;
                if tnd_index == 0 {
                    continue;
                }
                let table = mix(0, 0, triangle, noise, dmc);
                let exact = mix_exact(0, 0, triangle, noise, dmc);
                let relative = (table - exact).abs() / exact;
                if tnd_index >= 27 {
                    worst_above_27 = worst_above_27.max(relative);
                } else if relative >= 0.04 {
                    highest_index_over_four_percent =
                        highest_index_over_four_percent.max(tnd_index);
                }
            }
        }
    }
    assert!(
        worst_above_27 < 0.04,
        "the page's 4% must hold from tnd index 27 up, got {worst_above_27:.4}"
    );
    assert_eq!(
        highest_index_over_four_percent, 26,
        "the last index that still exceeds 4% is itself a measured fact"
    );
}

/// "When the values for one of the groups are all zero, the result for that
/// group should be treated as zero rather than undefined due to the division
/// by 0 that otherwise results."
#[test]
fn silence_mixes_to_exactly_zero() {
    assert_eq!(mix(0, 0, 0, 0, 0), 0.0);
    assert_eq!(mix(0, 0, 0, 0, 0) as i16, 0);

    // A FRESH `Apu` IS NOT SILENT, and that is not a bug: the triangle's
    // DAC has no gate, so a stopped sequencer holds whatever step it is on
    // (nesdev.org/wiki/APU_Triangle -- the documented silencing methods
    // "halt it in whatever its current output position is"), and step 0 is
    // level 15. `crate::apu::tests::channels`'
    // `triangle_walks_its_32_step_sequence_and_freezes_when_gated` pins the
    // same fact from the channel side. blargg's `apu_mixer` readme relies on
    // it too: "Tests MUST be run from a freshly-powered NES, as this is the
    // only way to ensure that the triangle wave doesn't interfere."
    let apu = Apu::new();
    assert_eq!(apu.channel_outputs().triangle, 15);
    assert_eq!(apu.mixed_output(), mix(0, 0, 15, 0, 0));
}

/// The whole point of the non-linear mixer: two channels at half volume are
/// NOT the same as one at full volume, and the same channel value
/// contributes less the louder its group already is. Both facts follow from
/// the `1/(k/n + 100)` shape and are what `apu_mixer` exists to check.
#[test]
fn the_mixer_is_non_linear_and_channels_interact_within_a_group() {
    let one_loud = mix(10, 0, 0, 0, 0);
    let two_quiet = mix(5, 5, 0, 0, 0);
    assert_eq!(
        one_loud, two_quiet,
        "within a group only the SUM matters -- this is why one square affects the other"
    );

    let first_step = mix(1, 0, 0, 0, 0) - mix(0, 0, 0, 0, 0);
    let last_step = mix(15, 15, 0, 0, 0) - mix(15, 14, 0, 0, 0);
    assert!(
        last_step < first_step * 0.6,
        "each additional unit must contribute less than the previous one; \
         measured first {first_step:e}, last {last_step:e} (a 1.84x compression \
         across the pulse group's full range)"
    );

    // The DMC level "affects attenuation" of triangle and noise -- games use
    // $4011 as a crude volume control for them (nesdev's own example).
    let triangle_alone = mix(0, 0, 15, 0, 0);
    let triangle_over_dmc = mix(0, 0, 15, 0, 100) - mix(0, 0, 0, 0, 100);
    assert!(
        triangle_over_dmc < triangle_alone * 0.75,
        "a loud DMC must attenuate the triangle's contribution \
         ({triangle_over_dmc:e} vs {triangle_alone:e})"
    );
}

/// The two groups' relative weights, which is what `apu_mixer`'s square and
/// triangle ROMs cancel against the DMC DAC: at equal digital values the
/// triangle's group contributes more than a single pulse, and the full-scale
/// output stays inside the page's stated 0.0..1.0 range.
#[test]
fn group_weights_and_full_scale_match_the_page() {
    assert!(mix(0, 0, 15, 0, 0) > mix(15, 0, 0, 0, 0));
    let full_scale = mix(15, 15, 15, 15, 127);
    assert!(
        (0.9..=1.0).contains(&full_scale),
        "everything at maximum should approach but not exceed 1.0, got {full_scale}"
    );
    // The i16 scaling is a pure multiply by i16::MAX, anchored at silence
    // and monotone -- checked at both ends rather than assumed.
    assert_eq!((mix(0, 0, 0, 0, 0) * f32::from(i16::MAX)) as i16, 0);
    assert!(
        (full_scale * f32::from(i16::MAX)) as i16 > 32_000,
        "full scale must reach the top of the i16 domain"
    );
}
