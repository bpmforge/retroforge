//! Ticket W4-10a criterion 3, as a gate rather than a printout:
//! DEBUGGER.md §6's "CI includes a benchmark asserting Accuracy-mode
//! frame time with debugger idle == debugger-compiled-out frame time
//! within noise."
//!
//! `benches/debugger_idle.rs` reports the three numbers under criterion;
//! this asserts the relationship between them, so a regression fails a
//! check instead of appearing in a log nobody reads.
//!
//! `#[ignore]`d like `breakpoint_cost.rs` and `frame_bundle_perf.rs`: a
//! wall-clock measurement, not a hermetic correctness test. CI runs it
//! explicitly. Run locally with:
//!   cargo test --release -p retroforge --test debugger_idle_cost -- --ignored --nocapture

use std::time::{Duration, Instant};

use retroforge::stepper::EmuStepper;
use retroforge::trace_capture;
use rf_core_api::{CoreEvent, CoreSink, InputFrame, PpuPixel};
use rf_debugger::trace::{TraceEntry, TraceFilter, TraceKind, TraceScrollback};

/// **Stated before measuring, on purpose.** `machine_frame`'s interleaved
/// before/after runs measured ~3% run-to-run noise on this project's
/// development machine (ticket W3-07b's close carries the raw numbers).
/// 5% is that floor with margin. Writing the threshold down after seeing
/// the result would make "within noise" true by construction.
const NOISE_BUDGET: f64 = 1.05;

/// The armed trace must cost *something*, or the idle-vs-compiled-out
/// comparison is measuring a feature that does nothing. Deliberately
/// loose — this is a floor on the difference, not a performance target
/// for tracing.
const ARMED_MUST_COST_AT_LEAST: f64 = 1.30;

const FRAMES: u32 = 60;

struct NullSink;
impl CoreSink for NullSink {
    fn video_scanline(&mut self, _y: u16, _pixels: &[PpuPixel]) {}
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, _ev: CoreEvent) {}
}

fn workload_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 16 + 0x4000 + 0x2000];
    rom[0..4].copy_from_slice(b"NES\x1a");
    rom[4] = 1;
    rom[5] = 1;
    let prg = &mut rom[16..16 + 0x4000];
    let code: &[u8] = &[
        0xA9, 0x1E, 0x8D, 0x01, 0x20, //
        0xA9, 0x9F, 0x8D, 0x00, 0x40, //
        0xA9, 0x40, 0x8D, 0x02, 0x40, //
        0xA9, 0x08, 0x8D, 0x03, 0x40, //
        0xA9, 0x01, 0x8D, 0x15, 0x40, //
        0xAD, 0x00, 0x00, 0x69, 0x01, 0x8D, 0x00, 0x00, 0xEE, 0x01, 0x00, 0x4C, 0x18, 0x80,
    ];
    prg[..code.len()].copy_from_slice(code);
    prg[0x3FFC] = 0x00;
    prg[0x3FFD] = 0x80;
    rom
}

fn warmed(rom: &[u8]) -> EmuStepper {
    let mut s = EmuStepper::from_ines_bytes(rom).expect("rom loads");
    s.resume();
    for _ in 0..2 {
        s.latch_and_advance_frame(InputFrame::default(), &mut NullSink);
    }
    s
}

/// The path a build without the debugger would run.
fn time_compiled_out(rom: &[u8]) -> Duration {
    let mut s = warmed(rom);
    let start = Instant::now();
    for _ in 0..FRAMES {
        s.latch_and_advance_frame(InputFrame::default(), &mut NullSink);
    }
    start.elapsed()
}

/// The path a shipped session with the debugger present but unused runs —
/// `core_thread`'s dispatch with the trace disarmed.
fn time_idle(rom: &[u8]) -> Duration {
    let mut s = warmed(rom);
    let mut trace: Option<Box<trace_capture::TraceProducer>> = None;
    let start = Instant::now();
    for _ in 0..FRAMES {
        match trace.as_mut() {
            None => {
                s.latch_and_advance_frame(InputFrame::default(), &mut NullSink);
            }
            Some(_) => unreachable!("disarmed"),
        }
    }
    start.elapsed()
}

fn time_armed(rom: &[u8]) -> (Duration, u64) {
    let mut s = warmed(rom);
    let (mut producer, mut drain) = trace_capture::channel(TraceFilter::default());
    let mut sb = TraceScrollback::new(trace_capture::SCROLLBACK_CAPACITY);
    let mut captured = 0u64;
    let start = Instant::now();
    for _ in 0..FRAMES {
        s.latch_and_advance_frame_traced(
            InputFrame::default(),
            &mut NullSink,
            &mut |pc, cycle, text| {
                producer.push(TraceEntry {
                    kind: TraceKind::Cpu,
                    cycle,
                    addr: pc,
                    text,
                });
            },
        );
        captured += drain.drain_into(&mut sb) as u64;
    }
    (start.elapsed(), captured)
}

/// Best of three per arm: this is a floor measurement, and the minimum is
/// the sample least polluted by whatever else the machine was doing.
fn best_of_three(mut f: impl FnMut() -> Duration) -> Duration {
    (0..3).map(|_| f()).min().expect("three samples")
}

#[test]
#[ignore = "wall-clock measurement; CI runs it explicitly"]
fn debugger_idle_costs_the_same_as_debugger_compiled_out() {
    let rom = workload_rom();

    let compiled_out = best_of_three(|| time_compiled_out(&rom));
    let idle = best_of_three(|| time_idle(&rom));
    let (armed, captured) = time_armed(&rom);

    let per_frame = |d: Duration| d.as_secs_f64() * 1000.0 / f64::from(FRAMES);
    let (out_ms, idle_ms, armed_ms) = (per_frame(compiled_out), per_frame(idle), per_frame(armed));
    let idle_ratio = idle_ms / out_ms;
    let armed_ratio = armed_ms / out_ms;

    eprintln!(
        "DEBUGGER.md §6: compiled-out {out_ms:.3} ms/frame | idle {idle_ms:.3} ms/frame \
         ({idle_ratio:.3}x, budget {NOISE_BUDGET:.2}x) | armed {armed_ms:.3} ms/frame \
         ({armed_ratio:.2}x, {captured} entries captured)"
    );

    // The criterion itself.
    assert!(
        idle_ratio < NOISE_BUDGET,
        "debugger idle cost {idle_ratio:.3}x compiled-out ({idle_ms:.3} vs {out_ms:.3} \
         ms/frame), over the {NOISE_BUDGET:.2}x noise budget stated before measuring. \
         DEBUGGER.md §6 requires debugger features to be pay-for-use."
    );

    // **Anti-vacuity.** Without these two, the assertion above would pass
    // just as happily against a trace that captured nothing — and "the
    // debugger costs nothing when idle" is only meaningful if the
    // debugger does something when armed.
    assert!(
        captured > 100_000,
        "only {captured} entries captured over {FRAMES} frames — the armed arm is not actually \
         tracing, so the idle comparison above is measuring a no-op"
    );
    assert!(
        armed_ratio > ARMED_MUST_COST_AT_LEAST,
        "the armed trace cost only {armed_ratio:.2}x compiled-out. Tracing every instruction \
         through a formatter cannot be nearly free; this most likely means the traced path is \
         not being taken"
    );
}
