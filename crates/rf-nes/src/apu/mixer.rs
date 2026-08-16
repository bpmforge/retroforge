//! The non-linear output mixer (ticket W2-01b) —
//! nesdev.org/wiki/APU_Mixer.
//!
//! ## Why lookup tables, and which ones
//!
//! The page gives three options: the exact two-term formula, a two-table
//! lookup, and a linear approximation. This module builds the **lookup
//! tables**, which is what the ticket's acceptance criterion names, using
//! the page's own constants verbatim:
//!
//! ```text
//! pulse_table[n] = 95.52  / (8128.0  / n + 100)
//! tnd_table[n]   = 163.67 / (24329.0 / n + 100)
//! output = pulse_table[pulse1 + pulse2] + tnd_table[3*triangle + 2*noise + dmc]
//! ```
//!
//! The `tnd` numerator/denominator differ from the exact formula's 159.79 /
//! (8227, 12241, 22638) because the table folds the three channels onto one
//! index by their relative DAC weights; the page states the resulting error
//! ("approximated (within 4%)"), and that 4% is a real, quantified deviation
//! from the exact formula, not a rounding artifact — see
//! `crate::apu::tests::mixer`'s `lookup_tables_track_the_exact_formula`,
//! which measures it rather than assuming it.
//!
//! ## What is NOT built here
//!
//! The three analog filters the page describes ("a first-order high-pass
//! filter at 90 Hz, another at 440 Hz, a first-order low-pass filter at 14
//! kHz") are **not** applied, and neither is any resampling: this module
//! produces one amplitude per CPU cycle at the 2A03's own rate. Filtering
//! and the band-limited downsample to a fixed internal rate belong with the
//! audio path (`EMULATION_CORES.md` §2.3 assigns rate control to `rf-audio`
//! and the downsample to the core's sink emission, which W2-05 builds);
//! putting a filter here would silently change what `apu_mixer`'s
//! cancellation test measures.

/// `pulse1 + pulse2` ranges 0..=30, so the table is 31 entries.
const PULSE_TABLE_LEN: usize = 31;
/// `3*triangle + 2*noise + dmc` maxes out at 3*15 + 2*15 + 127 = 202.
const TND_TABLE_LEN: usize = 203;

const PULSE_TABLE: [f32; PULSE_TABLE_LEN] = build_pulse_table();
const TND_TABLE: [f32; TND_TABLE_LEN] = build_tnd_table();

const fn build_pulse_table() -> [f32; PULSE_TABLE_LEN] {
    let mut table = [0.0; PULSE_TABLE_LEN];
    let mut n = 1;
    while n < PULSE_TABLE_LEN {
        table[n] = 95.52 / (8128.0 / n as f32 + 100.0);
        n += 1;
    }
    table
}

const fn build_tnd_table() -> [f32; TND_TABLE_LEN] {
    let mut table = [0.0; TND_TABLE_LEN];
    let mut n = 1;
    while n < TND_TABLE_LEN {
        table[n] = 163.67 / (24329.0 / n as f32 + 100.0);
        n += 1;
    }
    table
}

/// The mixed output level for one set of channel outputs, in the page's own
/// "range of 0.0 to 1.0".
///
/// "When the values for one of the groups are all zero, the result for that
/// group should be treated as zero rather than undefined due to the division
/// by 0" — index 0 of each table is exactly that zero.
pub(super) fn mix(pulse1: u8, pulse2: u8, triangle: u8, noise: u8, dmc: u8) -> f32 {
    let pulse_index = pulse1 as usize + pulse2 as usize;
    let tnd_index = 3 * triangle as usize + 2 * noise as usize + dmc as usize;
    PULSE_TABLE[pulse_index] + TND_TABLE[tnd_index]
}

/// The exact two-term formula from the top of the nesdev page, used only by
/// this module's tests as an independent check on the tables (a table that
/// were merely self-consistent would pass any test written against itself).
#[cfg(test)]
pub(super) fn mix_exact(pulse1: u8, pulse2: u8, triangle: u8, noise: u8, dmc: u8) -> f32 {
    let pulse_sum = pulse1 as f32 + pulse2 as f32;
    let pulse_out = if pulse_sum == 0.0 {
        0.0
    } else {
        95.88 / (8128.0 / pulse_sum + 100.0)
    };
    let tnd_sum = triangle as f32 / 8227.0 + noise as f32 / 12241.0 + dmc as f32 / 22638.0;
    let tnd_out = if tnd_sum == 0.0 {
        0.0
    } else {
        159.79 / (1.0 / tnd_sum + 100.0)
    };
    pulse_out + tnd_out
}
