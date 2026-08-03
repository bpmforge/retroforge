//! blargg `$6000`/`$6004` test-ROM protocol runner, driven against the
//! `rf-core-api` traits (ticket W0-03).
//!
//! Protocol verified against **primary source**, not a wiki page or
//! memory (project law 5): `cpu_reset/readme.txt` in
//! christopherpow/nes-test-roms ("Output at $6000" section) and the
//! actual assembly source (`instr_test-v5/source/common/text_out.s`,
//! `.../build_rom.s`) at commit `95d8f621ae55cee0d09b91519a8989ae0e64753b`.
//! Quoting the readme:
//!
//! > The test status is written to $6000. $80 means the test is running,
//! > $81 means the test needs the reset button pressed, but delayed by at
//! > least 100 msec from now. $00-$7F means the test has completed and
//! > given that result code. ... To allow an emulator to know when one of
//! > these tests is running and the data at $6000+ is valid ... $DE $B0
//! > $61 is written to $6001-$6003.
//!
//! and `text_out.s`: `final_result = $6000`, `text_out_base = $6004`
//! (NUL-terminated ASCII, terminator moves forward as more text is
//! written). See also nesdev.org/wiki/Emulator_tests (catalog page, not
//! protocol spec — the readme above is the actual source of truth).
//!
//! # Where does "$6000" live in a real core's `StateView`?
//!
//! `$6000-$7FFF` is the NES's cartridge PRG-RAM window (board-dependent,
//! not the console's fixed 2KB internal WRAM at `$0000-$07FF`), so it is
//! **not** any single fixed [`rf_core_api::StateView`] field — which field
//! (if any) a real NES core exposes it through is that core's own
//! decision, not this crate's (rf-nes doesn't exist yet — W1-01). This
//! runner therefore takes a `region_of` extractor closure rather than
//! assuming a field name: callers supply "how to find the $6000+ window in
//! this core's `StateView`", and this crate's tests supply one for a mock.

use rf_core_api::{CoreSink, EmulatorCore, InputFrame, StateView};
use std::fmt;

/// The 3-byte signature blargg test ROMs write to `$6001-$6003` once the
/// `$6000+` region holds valid data (verified against `text_out.s`, see
/// module doc).
pub const VALIDITY_SIGNATURE: [u8; 3] = [0xDE, 0xB0, 0x61];

/// Outcome of a completed (non-timeout) blargg run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlarggStatus {
    /// `$6000 == 0`: all tests passed.
    Passed,
    /// `$6000` in `1..=0x7F`: failed with this result code.
    Failed(u8),
    /// `$6000 == 0x81`: the ROM wants the reset button pressed (delayed by
    /// at least 100ms per the readme) before it can continue. Not a
    /// pass/fail verdict — the caller decides whether/how to honor it.
    NeedsReset,
}

/// Full result of [`run_blargg_protocol`] on success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlarggOutcome {
    pub status: BlarggStatus,
    /// NUL-terminated text at `$6004+`, decoded lossily and with the
    /// terminator stripped.
    pub message: String,
    /// Frames actually run before the outcome was observed.
    pub frames_run: u32,
}

/// [`run_blargg_protocol`] failure modes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunnerError {
    /// `max_frames` elapsed without the `$6000+` region ever showing a
    /// valid signature (TESTING.md §2: "Timeout = declared frame budget
    /// per ROM ⇒ fail") — i.e. the ROM never initialized the protocol at
    /// all within budget.
    TimeoutBeforeSignatureValid { max_frames: u32 },
    /// `max_frames` elapsed with a valid signature present but `$6000`
    /// never leaving the `$80` (running) state.
    TimeoutStillRunning { max_frames: u32 },
    /// The region extractor returned fewer than 4 bytes — not long enough
    /// to hold even the status byte + signature.
    RegionTooShort { len: usize },
    /// `$6000` held a byte outside the three ranges the readme actually
    /// defines (`$80` running, `$81` needs-reset, `$00-$7F` completed) —
    /// `0x82..=0xFF` is not a documented result code, so it is a protocol
    /// violation, not a very-failed test.
    InvalidStatus { status: u8 },
}

impl fmt::Display for RunnerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunnerError::TimeoutBeforeSignatureValid { max_frames } => write!(
                f,
                "blargg protocol: {max_frames} frame(s) elapsed and the $6001-$6003 signature never became valid (DE B0 61) — ROM likely never reached its test-status init"
            ),
            RunnerError::TimeoutStillRunning { max_frames } => write!(
                f,
                "blargg protocol: {max_frames} frame(s) elapsed, signature valid, but $6000 never left $80 (running)"
            ),
            RunnerError::RegionTooShort { len } => write!(
                f,
                "blargg protocol: status region extractor returned {len} byte(s), need at least 4"
            ),
            RunnerError::InvalidStatus { status } => write!(
                f,
                "blargg protocol: $6000 = ${status:02X}, which is none of $80 (running), $81 (needs reset), or $00-$7F (completed) — not a documented status"
            ),
        }
    }
}

impl std::error::Error for RunnerError {}

/// Run `core` frame-by-frame (via `run_frame`) until the blargg `$6000+`
/// protocol reports a final status (pass/fail/needs-reset) or `max_frames`
/// is exhausted.
///
/// `region_of` extracts the conceptual `$6000+` byte window from a
/// [`StateView`] — see module doc for why this can't be a fixed field.
/// The status byte (`region[0]`) is only trusted once
/// `region[1..4] == `[`VALIDITY_SIGNATURE`], so uninitialized memory can
/// never be misread as a real result.
///
/// # Errors
/// See [`RunnerError`].
pub fn run_blargg_protocol(
    core: &mut dyn EmulatorCore,
    sink: &mut dyn CoreSink,
    input: &InputFrame,
    max_frames: u32,
    region_of: impl for<'a> Fn(StateView<'a>) -> &'a [u8],
) -> Result<BlarggOutcome, RunnerError> {
    let mut signature_ever_valid = false;

    for frame in 0..max_frames {
        core.run_frame(input, sink);
        let region = region_of(core.state_view());

        if region.len() < 4 {
            return Err(RunnerError::RegionTooShort { len: region.len() });
        }
        if region[1..4] != VALIDITY_SIGNATURE {
            continue;
        }
        signature_ever_valid = true;

        let status = region[0];
        if status == 0x80 {
            continue; // still running
        }
        if status > 0x7F && status != 0x81 {
            return Err(RunnerError::InvalidStatus { status });
        }
        let outcome_status = if status == 0x81 {
            BlarggStatus::NeedsReset
        } else if status == 0 {
            BlarggStatus::Passed
        } else {
            BlarggStatus::Failed(status)
        };
        return Ok(BlarggOutcome {
            status: outcome_status,
            message: decode_message(&region[4..]),
            frames_run: frame + 1,
        });
    }

    if signature_ever_valid {
        Err(RunnerError::TimeoutStillRunning { max_frames })
    } else {
        Err(RunnerError::TimeoutBeforeSignatureValid { max_frames })
    }
}

/// Decode a NUL-terminated (or unterminated, if truncated) byte slice as
/// the `$6004+` message text.
fn decode_message(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_message_strips_nul_terminator() {
        assert_eq!(
            decode_message(b"All tests passed\0garbage"),
            "All tests passed"
        );
    }

    #[test]
    fn decode_message_handles_unterminated_slice() {
        assert_eq!(decode_message(b"no terminator"), "no terminator");
    }

    #[test]
    fn validity_signature_matches_verified_primary_source() {
        // cpu_reset/readme.txt: "$DE $B0 $61 is written to $6001-$6003."
        assert_eq!(VALIDITY_SIGNATURE, [0xDE, 0xB0, 0x61]);
    }
}
