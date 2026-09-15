//! Drives [`rf_harness::run_blargg_protocol`] against a small mock
//! [`EmulatorCore`], the same pattern `crates/rf-core-api/tests/mock_core.rs`
//! uses to prove the core traits without simulating a real console (ticket
//! W0-03: "There is no working emulator core yet... tested with a mock
//! core / synthetic memory that presents the $6000/$6004 pattern").
//!
//! The mock repurposes [`StateView::mapper_state`] as the synthetic
//! `$6000+` window (documented here as a *mock-only* convention — see
//! `crates/rf-harness/src/blargg.rs` module doc for why no fixed
//! `StateView` field can mean "$6000" for a real NES core).

use rf_core_api::{
    CartImage, CoreConfig, CoreError, CoreEvent, CoreSink, EmulatorCore, InputFrame, PpuPixel,
    ResetKind, StateError, StateReader, StateView, StateWriter, Step, StepResult,
};
use rf_harness::{run_blargg_protocol, BlarggStatus, RunnerError, VALIDITY_SIGNATURE};

/// A scripted sequence of `$6000+`-region snapshots, one per frame.
/// `state_view()` after the Nth `run_frame` call (0-indexed) exposes
/// `script[N]` (clamped to the last entry once the script is exhausted) —
/// i.e. loop iteration N of [`run_blargg_protocol`] (which always calls
/// `run_frame` then reads `state_view` within the same iteration) sees
/// `script[N]`.
struct ScriptedCore {
    script: Vec<Vec<u8>>,
    calls: usize,
    config: CoreConfig,
}

impl ScriptedCore {
    fn new(script: Vec<Vec<u8>>) -> Self {
        assert!(!script.is_empty(), "script must have at least one frame");
        ScriptedCore {
            script,
            calls: 0,
            config: CoreConfig::default(),
        }
    }

    fn current_index(&self) -> usize {
        self.calls.saturating_sub(1).min(self.script.len() - 1)
    }
}

struct NullSink;
impl CoreSink for NullSink {
    fn video_scanline(&mut self, _y: u16, _pixels: &[PpuPixel]) {}
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, _ev: CoreEvent) {}
}

impl EmulatorCore for ScriptedCore {
    fn load(&mut self, cart: CartImage<'_>) -> Result<(), CoreError> {
        cart.validate()
    }

    fn reset(&mut self, _kind: ResetKind) {}

    fn run_frame(&mut self, _input: &InputFrame, _sink: &mut dyn CoreSink) {
        self.calls += 1;
    }

    fn step(&mut self, _granularity: Step, _sink: &mut dyn CoreSink) -> StepResult {
        StepResult {
            cycles: 0,
            frame_complete: true,
        }
    }

    fn save_state(&self, _w: &mut dyn StateWriter) -> Result<(), StateError> {
        Ok(())
    }

    fn load_state(&mut self, _r: &mut dyn StateReader) -> Result<(), StateError> {
        Ok(())
    }

    fn state_view(&self) -> StateView<'_> {
        StateView {
            cpu_regs: rf_core_api::CpuRegs::None,
            wram: &[],
            vram: &[],
            cgram: &[],
            oam: &[],
            ppu_regs: &[],
            mapper_state: &self.script[self.current_index()],
        }
    }

    fn config(&mut self) -> &mut CoreConfig {
        &mut self.config
    }

    fn peek(&self, addr: u32) -> u8 {
        // This core's "memory" is the scripted frame it is currently
        // serving, which `state_view` already lends as `mapper_state`.
        // Reading through the same buffer keeps the two views consistent;
        // anything past its end is quiescent zero, as an unmapped read
        // would be.
        self.script[self.current_index()]
            .get(addr as usize)
            .copied()
            .unwrap_or(0)
    }
}

fn mock_region(view: StateView<'_>) -> &[u8] {
    view.mapper_state
}

fn valid_frame(status: u8, message: &[u8]) -> Vec<u8> {
    let mut v = vec![
        status,
        VALIDITY_SIGNATURE[0],
        VALIDITY_SIGNATURE[1],
        VALIDITY_SIGNATURE[2],
    ];
    v.extend_from_slice(message);
    v.push(0); // NUL terminator
    v
}

fn uninitialized_frame() -> Vec<u8> {
    vec![0x00, 0x00, 0x00, 0x00] // garbage: signature not yet valid
}

#[test]
fn reports_pass_with_message() {
    let mut core = ScriptedCore::new(vec![
        uninitialized_frame(),
        valid_frame(0x80, b"running..."),
        valid_frame(0x00, b"All tests passed"),
    ]);
    let mut sink = NullSink;
    let outcome = run_blargg_protocol(&mut core, &mut sink, &InputFrame::empty(), 10, mock_region)
        .expect("should reach a final status");
    assert_eq!(outcome.status, BlarggStatus::Passed);
    assert_eq!(outcome.message, "All tests passed");
    assert_eq!(outcome.frames_run, 3);
}

#[test]
fn reports_failure_with_code_and_message() {
    let mut core = ScriptedCore::new(vec![
        valid_frame(0x80, b""),
        valid_frame(0x80, b""),
        valid_frame(0x03, b"Failed #3\nRTI"),
    ]);
    let mut sink = NullSink;
    let outcome =
        run_blargg_protocol(&mut core, &mut sink, &InputFrame::empty(), 10, mock_region).unwrap();
    assert_eq!(outcome.status, BlarggStatus::Failed(3));
    assert_eq!(outcome.message, "Failed #3\nRTI");
}

#[test]
fn reports_needs_reset() {
    let mut core = ScriptedCore::new(vec![valid_frame(0x80, b""), valid_frame(0x81, b"")]);
    let mut sink = NullSink;
    let outcome =
        run_blargg_protocol(&mut core, &mut sink, &InputFrame::empty(), 10, mock_region).unwrap();
    assert_eq!(outcome.status, BlarggStatus::NeedsReset);
}

#[test]
fn never_trusts_status_byte_before_signature_is_valid() {
    // Frame 0's region has status byte 0x00 (would mean "passed" if
    // trusted) but an invalid signature — must be ignored, not reported
    // as a pass. Only frame 1, with a valid signature, is a real result.
    let mut core = ScriptedCore::new(vec![
        vec![0x00, 0xAA, 0xBB, 0xCC], // looks like "pass" but signature is garbage
        valid_frame(0x00, b"real pass"),
    ]);
    let mut sink = NullSink;
    let outcome =
        run_blargg_protocol(&mut core, &mut sink, &InputFrame::empty(), 10, mock_region).unwrap();
    assert_eq!(outcome.status, BlarggStatus::Passed);
    assert_eq!(outcome.message, "real pass");
    assert_eq!(outcome.frames_run, 2);
}

#[test]
fn times_out_before_signature_ever_valid() {
    let mut core = ScriptedCore::new(vec![uninitialized_frame()]);
    let mut sink = NullSink;
    let err = run_blargg_protocol(&mut core, &mut sink, &InputFrame::empty(), 5, mock_region)
        .unwrap_err();
    assert_eq!(
        err,
        RunnerError::TimeoutBeforeSignatureValid { max_frames: 5 }
    );
}

#[test]
fn times_out_still_running_after_signature_valid() {
    let mut core = ScriptedCore::new(vec![valid_frame(0x80, b"")]);
    let mut sink = NullSink;
    let err = run_blargg_protocol(&mut core, &mut sink, &InputFrame::empty(), 5, mock_region)
        .unwrap_err();
    assert_eq!(err, RunnerError::TimeoutStillRunning { max_frames: 5 });
}

#[test]
fn rejects_region_shorter_than_four_bytes() {
    let mut core = ScriptedCore::new(vec![vec![0x80, 0xDE]]); // only 2 bytes
    let mut sink = NullSink;
    let err = run_blargg_protocol(&mut core, &mut sink, &InputFrame::empty(), 5, mock_region)
        .unwrap_err();
    assert_eq!(err, RunnerError::RegionTooShort { len: 2 });
}

/// `cpu_reset/readme.txt` defines exactly three status ranges: `$80`
/// running, `$81` needs-reset, `$00-$7F` completed. `$82..=$FF` is not a
/// documented result code and must not be silently treated as
/// `Failed(status)`.
#[test]
fn rejects_status_byte_outside_documented_ranges() {
    let mut core = ScriptedCore::new(vec![valid_frame(0x9C, b"")]);
    let mut sink = NullSink;
    let err = run_blargg_protocol(&mut core, &mut sink, &InputFrame::empty(), 5, mock_region)
        .unwrap_err();
    assert_eq!(err, RunnerError::InvalidStatus { status: 0x9C });
}
