//! SPC700 instruction timing (ticket W7-08, criterion 2).
//!
//! ## The table is DERIVED from the vectors, not transcribed
//!
//! Every entry below was computed from the SingleStepTests SPC700
//! vectors this repo already fetches: each case carries a `cycles` array
//! of per-cycle bus events, so `cycles.len()` *is* that execution's cycle
//! count. Across 256 opcodes x 1000 cases, 228 opcodes show a single
//! count and 28 show exactly two — and the difference is **always 2**.
//!
//! Writing 256 numbers from memory or from a hand-copied table is exactly
//! the kind of work that produces a plausible, subtly wrong emulator, so
//! it was not done that way. `spc700_cycle_table_matches_the_vectors`
//! re-derives this table from the same data and fails if a single entry
//! drifts — the table and its oracle cannot disagree silently.
//!
//! ## Why it was missing, and what it cost
//!
//! `Apu::step` charged a nominal **2 cycles per instruction** with a
//! comment saying cycle counts were "the cycle-accurate executor's
//! business". `SnesBus::catch_up_apu` then computed a cycle budget and
//! spent it as one-instruction-per-cycle, so the APU outran the CPU by
//! 2-5x. Traced in W7-08's own notes: on the first `$2140` read after
//! hand-over the SPC had already executed a 5-cycle instruction on 1
//! cycle of debt, clobbering the echo before the CPU could see it.

/// Base cycle count per opcode, indexed by opcode byte.
///
/// For the 28 conditional branches this is the **not-taken** cost; a
/// taken branch adds [`BRANCH_TAKEN_EXTRA`].
pub const CYCLES: [u8; 256] = [
    2, 8, 4, 5, 3, 4, 3, 6, 2, 6, 5, 4, 5, 4, 6, 8, // $00-$0F
    2, 8, 4, 5, 4, 5, 5, 6, 5, 5, 6, 5, 2, 2, 4, 6, // $10-$1F
    2, 8, 4, 5, 3, 4, 3, 6, 2, 6, 5, 4, 5, 4, 5, 4, // $20-$2F
    2, 8, 4, 5, 4, 5, 5, 6, 5, 5, 6, 5, 2, 2, 3, 8, // $30-$3F
    2, 8, 4, 5, 3, 4, 3, 6, 2, 6, 4, 4, 5, 4, 6, 6, // $40-$4F
    2, 8, 4, 5, 4, 5, 5, 6, 5, 5, 4, 5, 2, 2, 4, 3, // $50-$5F
    2, 8, 4, 5, 3, 4, 3, 6, 2, 6, 4, 4, 5, 4, 5, 5, // $60-$6F
    2, 8, 4, 5, 4, 5, 5, 6, 5, 5, 5, 5, 2, 2, 3, 6, // $70-$7F
    2, 8, 4, 5, 3, 4, 3, 6, 2, 6, 5, 4, 5, 2, 4, 5, // $80-$8F
    2, 8, 4, 5, 4, 5, 5, 6, 5, 5, 5, 5, 2, 2, 12, 5, // $90-$9F
    3, 8, 4, 5, 3, 4, 3, 6, 2, 6, 4, 4, 5, 2, 4, 4, // $A0-$AF
    2, 8, 4, 5, 4, 5, 5, 6, 5, 5, 5, 5, 2, 2, 3, 4, // $B0-$BF
    3, 8, 4, 5, 4, 5, 4, 7, 2, 5, 6, 4, 5, 2, 4, 9, // $C0-$CF
    2, 8, 4, 5, 5, 6, 6, 7, 4, 5, 5, 5, 2, 2, 6, 3, // $D0-$DF
    2, 8, 4, 5, 3, 4, 3, 6, 2, 4, 5, 3, 4, 3, 4, 7, // $E0-$EF
    2, 8, 4, 5, 4, 5, 5, 6, 3, 4, 5, 4, 2, 2, 4, 7, // $F0-$FF
];

/// Extra cycles when a conditional branch is taken.
///
/// Uniformly 2 across every branching opcode in the vectors — checked
/// rather than assumed, and asserted by the vector test.
pub const BRANCH_TAKEN_EXTRA: u8 = 2;

/// The opcodes whose cost depends on whether the branch was taken:
/// the eight `Bcc` forms, the eight `BBS`/`BBC` bit tests, `CBNE`
/// (`$2E`, `$DE`) and `DBNZ` (`$6E`, `$FE`).
pub const BRANCHING: &[u8] = &[
    0x03, 0x10, 0x13, 0x23, 0x2E, 0x30, 0x33, 0x43, 0x50, 0x53, 0x63, 0x6E, 0x70, 0x73, 0x83, 0x90,
    0x93, 0xA3, 0xB0, 0xB3, 0xC3, 0xD0, 0xD3, 0xDE, 0xE3, 0xF0, 0xF3, 0xFE,
];

/// Is this opcode's cost conditional on a branch being taken?
#[must_use]
pub fn is_branching(opcode: u8) -> bool {
    BRANCHING.contains(&opcode)
}

/// Cycles for `opcode`, given whether its branch was taken.
///
/// `taken` is ignored for non-branching opcodes, so a caller that does
/// not track branch outcomes still gets every fixed instruction right.
#[must_use]
pub fn cycles(opcode: u8, taken: bool) -> u8 {
    CYCLES[opcode as usize]
        + if taken && is_branching(opcode) {
            BRANCH_TAKEN_EXTRA
        } else {
            0
        }
}

/// How many of an opcode's INTERNAL cycles take I/O timing rather than
/// RAM timing.
///
/// Transcribed from fullsnes, "SPC700 Waitstates on Internal Cycles":
/// "Below lists the number of I/O-Waitstates applied on Internal Cycles
/// of SPC700 opcodes 00h..FFh (that implies: any further Internal Cycles
/// have RAM-Waitstates)."
///
/// Its own worked example: "Opcode 00h (NOP) has one internal cycle (and
/// it's having RAM timings). Opcode 01h (TCALL) has 3 internal cycles
/// (and all 3 of them have I/O timings)." — which is why entry `00` is 0
/// and entry `01` is 3.
///
/// **This only matters when `$F0` asks for waitstates.** At the power-on
/// `$0A` both wait fields are zero, every product below is zero, and the
/// cycle count is exactly what it always was.
#[rustfmt::skip]
pub const IO_WAIT_INTERNAL_CYCLES: [u8; 256] = [
    0, 3, 0, 1, 0, 0, 0, 1, 0, 0, 1, 0, 0, 1, 0, 2,
    2, 3, 0, 3, 1, 1, 1, 1, 0, 0, 0, 1, 0, 0, 0, 1,
    0, 3, 0, 1, 0, 0, 0, 1, 0, 0, 1, 0, 0, 1, 3, 2,
    0, 3, 0, 3, 1, 1, 1, 1, 0, 0, 0, 1, 0, 0, 0, 3,
    0, 3, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 1, 0, 3,
    2, 3, 0, 3, 1, 1, 1, 1, 0, 0, 0, 1, 0, 0, 0, 0,
    0, 3, 0, 1, 0, 0, 0, 1, 0, 1, 0, 0, 0, 1, 2, 1,
    0, 3, 0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 1,
    0, 3, 0, 1, 0, 0, 0, 1, 0, 0, 1, 0, 0, 0, 1, 0,
    2, 3, 0, 3, 1, 1, 1, 1, 0, 0, 1, 1, 0, 0, 10, 3,
    1, 3, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 1, 1,
    0, 3, 0, 3, 1, 1, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1,
    1, 3, 0, 1, 0, 0, 0, 1, 0, 0, 1, 0, 0, 0, 1, 7,
    2, 3, 0, 3, 1, 1, 1, 1, 0, 1, 0, 1, 0, 0, 4, 1,
    0, 3, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 1, 1, 0,
    0, 3, 0, 3, 1, 1, 1, 1, 0, 1, 0, 1, 0, 0, 3, 0,
];
