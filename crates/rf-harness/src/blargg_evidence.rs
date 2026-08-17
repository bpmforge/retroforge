//! blargg `$6000`/`$6004` protocol runner driven against a **real**
//! [`rf_nes::Cpu`] + [`rf_nes::NesBus`] (ticket W1-05b), for
//! `docs/evidence/local-gate.json`'s third and fourth Tier-A-local rows
//! (`sprite_hit_tests`, `ppu_vbl_nmi`).
//!
//! [`run_blargg_protocol`](crate::run_blargg_protocol) (`blargg.rs`, ticket
//! W0-03) cannot be pointed at `rf-nes` directly: its signature takes
//! `&mut dyn EmulatorCore`, and `rf-nes` has no `EmulatorCore` impl (out of
//! every ticket's write scope so far — see `crate::ppu`'s module doc
//! "`CoreSink` emission seam" section). So — same shape as
//! [`crate::nestest_evidence`] and [`crate::nes6502_evidence`], both of
//! which are documented as "a second, independent peer" of an `rf-nes`-side
//! `#[cfg(test)]` module rather than a wrapper around `cargo test -p
//! rf-nes` or a fit into the `EmulatorCore` trait — this module owns
//! [`rf_nes::Cpu`]/[`rf_nes::NesBus`] directly. It reuses `blargg.rs`'s
//! already-verified status-decoding vocabulary
//! ([`crate::blargg::VALIDITY_SIGNATURE`], [`crate::blargg::BlarggStatus`],
//! [`crate::blargg::BlarggOutcome`], [`crate::blargg::RunnerError`],
//! `crate::blargg::decode_message`) rather than the driver loop, which
//! needs a different frame-boundary primitive (see below).
//!
//! `crates/rf-nes/src/ppu/tests/blargg_roms.rs` is this module's
//! independent peer inside `rf-nes` (skip-if-`roms/`-absent, exercised by
//! `cargo test --workspace`) — it deliberately reimplements this same tiny
//! `$6000+` reader itself rather than depending on `rf-harness` (which
//! would invert the crate dependency direction), the same duplication
//! discipline `nes6502_evidence.rs`'s module doc explains for its own pair.
//!
//! ## Frame boundary
//!
//! `rf_nes::NesBus::frame_count` (ticket W1-05b) increments exactly once
//! per PPU frame. [`run`] drives `Cpu::step` in a loop until that counter
//! advances, then inspects `$6000+` (`NesBus::prg_ram`) — the same
//! per-frame cadence `run_blargg_protocol`'s `EmulatorCore::run_frame` loop
//! uses, without needing a real `EmulatorCore::run_frame` to exist yet.
//!
//! `rf-harness` is allowed to depend on `rf-nes` directly —
//! `scripts/validate-arch.sh`'s layering rule exempts "the test harness" by
//! name, alongside the app shell and the cores themselves.
//!
//! [`run_ram_result`] is a second, sibling protocol reader (ticket W1-05b
//! pre-flight finding) for `sprite_hit_tests`, whose ROMs turned out NOT to
//! speak the `$6000` protocol at all — see that function's doc for the
//! primary-source citations.
use std::path::Path;

use crate::blargg::{decode_message, BlarggOutcome, BlarggStatus, RunnerError, VALIDITY_SIGNATURE};
use rf_nes::{Cpu, NesBus};

/// Outcome of a completed (non-timeout) [`run_ram_result`] run — see that
/// function's doc for the protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RamResultOutcome {
    /// The result byte stabilized at `1`.
    Passed,
    /// The result byte stabilized at this (non-zero, non-one) value.
    Failed(u8),
}

/// Loads `rom_path` as an iNES image, boots it through the REAL reset
/// vector (`Cpu::power_on`, no nestest-style `$C000` override — blargg
/// ROMs run their own reset-vector init code, same as a real cartridge),
/// and drives it frame-by-frame until the `$6000+` protocol reports a
/// final status or `max_frames` elapses.
///
/// # Errors
/// Returns a message on ROM-load failure, or `RunnerError`'s
/// [`std::fmt::Display`] text for a protocol violation/timeout — see
/// `blargg.rs`'s `RunnerError` for exactly what each case means.
pub fn run(rom_path: &Path, max_frames: u32) -> Result<BlarggOutcome, String> {
    run_with_mode(rom_path, max_frames, true)
}

/// [`run`], but with [`rf_nes::NesBus::set_accuracy_mode`] chosen by the
/// caller (ticket W3-07). `accuracy = true` is what every existing caller
/// gets from [`run`]; `false` is the compatibility path, used only by
/// [`crate::mode_diff`].
pub fn run_with_mode(
    rom_path: &Path,
    max_frames: u32,
    accuracy: bool,
) -> Result<BlarggOutcome, String> {
    let rom_bytes = std::fs::read(rom_path)
        .map_err(|e| format!("failed to read {}: {e}", rom_path.display()))?;
    let mut bus = NesBus::from_ines_bytes(&rom_bytes)
        .map_err(|e| format!("invalid rom image {}: {e}", rom_path.display()))?;
    bus.set_accuracy_mode(accuracy);
    let mut cpu = Cpu::power_on(&mut bus);

    let mut signature_ever_valid = false;

    for frame in 0..max_frames {
        let start_frame = bus.frame_count();
        while bus.frame_count() == start_frame {
            cpu.step(&mut bus);
        }

        let region = bus.prg_ram();
        if region[1..4] != VALIDITY_SIGNATURE {
            continue;
        }
        signature_ever_valid = true;

        let status = region[0];
        if status == 0x80 {
            continue; // still running
        }
        if status > 0x7F && status != 0x81 {
            return Err(RunnerError::InvalidStatus { status }.to_string());
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

    Err(if signature_ever_valid {
        RunnerError::TimeoutStillRunning { max_frames }
    } else {
        RunnerError::TimeoutBeforeSignatureValid { max_frames }
    }
    .to_string())
}

/// Drives `rom_path` the same way [`run`] does, but polls a single RAM
/// byte at `result_addr` each frame instead of the `$6000+` text protocol —
/// for `sprite_hit_tests` (ticket W1-05b pre-flight finding), whose ROMs
/// predate blargg's shared `$6000` runtime entirely.
///
/// ## Why this exists (verified against primary source, not assumed)
///
/// `sprite_hit_tests_2005.10.05/readme.txt` (same pinned commit as every
/// other manifest URL, `95d8f621ae55cee0d09b91519a8989ae0e64753b`), quoted
/// verbatim: "Each test ROM runs several tests and reports the result on
/// screen and by beeping a number of times." There is no `$6000`/`$6004`
/// signature anywhere in this ROM family's runtime (verified by fetching
/// and grepping every file under `sprite_hit_tests_2005.10.05/source/
/// runtime/` at that commit for `6000`/`DE B0 61`: zero matches) — the
/// manifest's pre-W1-05b `protocol = "six_thousand"` tag for this suite was
/// simply wrong, discovered empirically this ticket (11/11 ROMs timing out
/// with `TimeoutBeforeSignatureValid` before this fix). But the SAME
/// runtime's `source/runtime/validation.a` (also fetched, that commit)
/// defines a real, RAM-resident result byte:
/// ```text
/// result = $f8
/// ...
/// tests_passed:
///       lda   #1
///       sta   result
/// report_final_result:
/// report_final_result_:
///       sei               ; disable interrupts
///       lda   #0
///       sta   $2000
///       jsr   init_runtime
///       ... (prints PASSED/FAILED text + beeps, then `jmp forever`)
/// ```
/// `error_if_ne`/`error_if_eq` show `result` ALSO holds live scratch values
/// *during* a test's own pass/fail checks, not only at the very end: `sta
/// <result` (in `prefix_sprite_hit.a`'s `test_for_hit`) writes that check's
/// own error code to `result` BEFORE running the check, and a passing
/// check's `rts` never resets it — so `result` legitimately holds a fixed
/// small integer for roughly one whole frame at a time (`test_for_hit`'s
/// own `wait_vbl` + a multi-thousand-cycle delay) for EVERY sub-test, not
/// just the final one. An earlier version of this function tried to detect
/// completion by requiring the same non-zero value on two consecutive
/// per-frame polls; empirically (verified against the real fetched ROMs,
/// this ticket) that is indistinguishable from an ordinary in-progress
/// sub-test's own hold, and produced false `Failed(2)`/`Failed(3)`-shaped
/// reports on every one of the 11 real ROMs. This function instead runs
/// the FULL `max_frames` budget unconditionally — no early-exit heuristic —
/// and reads `result_addr` once at the very end; `report_final_result_`'s
/// `jmp forever` means the true final value, once reached, never changes
/// again, so as long as `max_frames` comfortably exceeds every sub-test's
/// own ~1-2 frame hold (a small, fixed count per ROM — `01.basics.asm` has
/// 11), reading late is always safe and reading a still-in-progress value
/// is not.
///
/// # Errors
/// A message on ROM-load failure. Never times out in the `run`/`RunnerError`
/// sense — `max_frames` here is a budget, not a completion signal; a
/// `result_addr` that's still `0` after the full budget (never even started)
/// or something else the caller doesn't expect is reported as `Ok`, not
/// `Err`, and it is the caller's job to notice an unexpected value.
pub fn run_ram_result(
    rom_path: &Path,
    max_frames: u32,
    result_addr: u16,
) -> Result<RamResultOutcome, String> {
    let rom_bytes = std::fs::read(rom_path)
        .map_err(|e| format!("failed to read {}: {e}", rom_path.display()))?;
    let mut bus = NesBus::from_ines_bytes(&rom_bytes)
        .map_err(|e| format!("invalid rom image {}: {e}", rom_path.display()))?;
    let mut cpu = Cpu::power_on(&mut bus);

    for _ in 0..max_frames {
        let start_frame = bus.frame_count();
        while bus.frame_count() == start_frame {
            cpu.step(&mut bus);
        }
    }

    let value = bus.peek(result_addr);
    Ok(if value == 1 {
        RamResultOutcome::Passed
    } else {
        RamResultOutcome::Failed(value)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // Same "collision-proof tempdir" discipline as `nestest_evidence.rs`/
    // `nes6502_evidence.rs` (see either's doc comment for why pid+nanos
    // alone isn't unique under parallel test threads).
    fn tempdir() -> std::path::PathBuf {
        static TEMPDIR_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "rf-harness-blargg-evidence-test-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            TEMPDIR_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn ines_header(prg_banks: u8, chr_banks: u8) -> Vec<u8> {
        let mut h = vec![b'N', b'E', b'S', 0x1A];
        h.push(prg_banks);
        h.push(chr_banks);
        h.extend_from_slice(&[0u8; 10]); // flags 6/7 + 8 reserved: mapper 0
        h
    }

    /// A hand-assembled NROM image (reset vector -> $C000, mirrored from
    /// $8000) that writes the `$6001-$6003` validity signature then `$00`
    /// (pass) to `$6000`, then spins forever — just enough to prove [`run`]
    /// reads the real `$6000+` protocol from a real `NesBus`, not a claim
    /// about any specific real blargg ROM.
    fn build_six_thousand_pass_rom() -> Vec<u8> {
        let mut data = ines_header(1, 1);
        let mut prg = vec![0xEAu8; 16 * 1024]; // NOP-filled
        #[rustfmt::skip]
        let code: [u8; 23] = [
            0xA9, 0xDE,       // LDA #$DE
            0x8D, 0x01, 0x60, // STA $6001
            0xA9, 0xB0,       // LDA #$B0
            0x8D, 0x02, 0x60, // STA $6002
            0xA9, 0x61,       // LDA #$61
            0x8D, 0x03, 0x60, // STA $6003
            0xA9, 0x00,       // LDA #$00
            0x8D, 0x00, 0x60, // STA $6000 (0x00 = passed)
            0x4C, 0x14, 0xC0, // JMP $C014 (this instruction itself: loop forever)
        ];
        prg[..code.len()].copy_from_slice(&code);
        prg[0x3FFC] = 0x00; // reset vector low -> $C000
        prg[0x3FFD] = 0xC0; // reset vector high
        data.extend_from_slice(&prg);
        data.extend(vec![0u8; 8 * 1024]); // CHR
        data
    }

    #[test]
    fn run_reads_a_real_passing_six_thousand_protocol_from_a_real_bus() {
        let dir = tempdir();
        let rom_path = dir.join("pass.nes");
        fs::write(&rom_path, build_six_thousand_pass_rom()).unwrap();

        let outcome = run(&rom_path, 5).expect("must reach a final status within 5 frames");
        assert_eq!(outcome.status, BlarggStatus::Passed);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn run_reports_missing_rom_as_an_error_not_a_fake_result() {
        let err = run(Path::new("/definitely/does/not/exist.nes"), 5).unwrap_err();
        assert!(err.contains("failed to read"));
    }

    /// A hand-assembled NROM image that writes `1` (pass, per
    /// `validation.a`'s `result = $f8` convention) to zero-page `$00F8`,
    /// then spins forever.
    fn build_ram_result_pass_rom() -> Vec<u8> {
        let mut data = ines_header(1, 1);
        let mut prg = vec![0xEAu8; 16 * 1024];
        #[rustfmt::skip]
        let code: [u8; 7] = [
            0xA9, 0x01,       // LDA #$01
            0x85, 0xF8,       // STA $F8 (zero page)
            0x4C, 0x04, 0xC0, // JMP $C004 (this instruction itself: loop forever)
        ];
        prg[..code.len()].copy_from_slice(&code);
        prg[0x3FFC] = 0x00;
        prg[0x3FFD] = 0xC0;
        data.extend_from_slice(&prg);
        data.extend(vec![0u8; 8 * 1024]);
        data
    }

    #[test]
    fn run_ram_result_reads_a_real_passing_result_byte_from_a_real_bus() {
        let dir = tempdir();
        let rom_path = dir.join("pass_ram.nes");
        fs::write(&rom_path, build_ram_result_pass_rom()).unwrap();

        let outcome = run_ram_result(&rom_path, 5, 0x00F8).expect("rom must be readable");
        assert_eq!(outcome, RamResultOutcome::Passed);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn run_ram_result_reports_a_never_started_rom_as_failed_zero_not_a_fake_pass() {
        let dir = tempdir();
        let rom_path = dir.join("never_writes.nes");
        // A ROM whose reset code never touches $00F8 at all: pure NOPs,
        // reset vector -> $C000 (an infinite NOP slide -- harmless, this
        // just needs to run for a few frames without writing the result
        // byte).
        let mut data = ines_header(1, 1);
        let mut prg = vec![0xEAu8; 16 * 1024];
        prg[0x3FFC] = 0x00;
        prg[0x3FFD] = 0xC0;
        data.extend_from_slice(&prg);
        data.extend(vec![0u8; 8 * 1024]);
        fs::write(&rom_path, data).unwrap();

        let outcome = run_ram_result(&rom_path, 2, 0x00F8).expect("rom must be readable");
        assert_eq!(outcome, RamResultOutcome::Failed(0));

        fs::remove_dir_all(&dir).ok();
    }
}

/// What a screen-only ROM printed, and whether it says it passed
/// (ticket W2-12; [`crate::Protocol::ScreenText`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScreenOutcome {
    /// The screen contains "PASSED" and no "FAIL".
    Passed(String),
    /// The screen contains "FAIL".
    Failed(String),
    /// Neither word appeared. NOT treated as a pass: a ROM that never
    /// printed a verdict has not given one, and calling that green is how a
    /// suite silently stops testing anything.
    NoVerdict(String),
}

/// Run `rom_path` for `frames` and read the verdict off the screen.
///
/// The scoring rule is blargg's own, quoted from his readmes: "If a test
/// prints 'passed', it passed". Case-insensitive, because his ROM
/// generations disagree about capitalisation.
///
/// # Errors
/// Returns a message if the image cannot be read or parsed.
pub fn run_screen_text(rom_path: &Path, frames: u32) -> Result<ScreenOutcome, String> {
    run_screen_text_with_mode(rom_path, frames, true)
}

/// [`run_screen_text`], with the accuracy/compatibility switch exposed
/// (ticket W3-07) — see [`run_with_mode`].
pub fn run_screen_text_with_mode(
    rom_path: &Path,
    frames: u32,
    accuracy: bool,
) -> Result<ScreenOutcome, String> {
    let bytes = std::fs::read(rom_path).map_err(|e| format!("cannot read rom: {e}"))?;
    let mut bus = NesBus::from_ines_bytes(&bytes).map_err(|e| format!("invalid rom: {e}"))?;
    bus.set_accuracy_mode(accuracy);
    let mut cpu = Cpu::power_on(&mut bus);
    for _ in 0..frames {
        let start = bus.frame_count();
        while bus.frame_count() == start {
            cpu.step(&mut bus);
        }
    }
    let text = bus.ppu_vram_ascii();
    let upper = text.to_ascii_uppercase();
    Ok(if upper.contains("FAIL") {
        ScreenOutcome::Failed(text)
    } else if upper.contains("PASSED") {
        ScreenOutcome::Passed(text)
    } else {
        ScreenOutcome::NoVerdict(text)
    })
}
