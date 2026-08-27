//! **`rf-snes` implements `EmulatorCore`, and frames come out of it**
//! (ticket W11-11).
//!
//! The SNES core has passed 65816 and SPC700 vector suites, gilyon
//! cputest, PeterLemon PPU goldens, Mode 7 and colour-math tests for a
//! long time — **every one of them through `rf-harness`, never through
//! the trait the shell speaks.** That is why a fully-tested core cannot
//! be opened in the application.
//!
//! This drives the trait, with a real fixture, and asserts on pixels.

use rf_core_api::{CoreEvent, CoreSink, EmulatorCore, InputFrame, OverlayPixel, PpuPixel, Step};
use rf_snes::core::SnesCore;

#[derive(Default)]
struct Recorder {
    rows: Vec<(u16, usize)>,
    distinct: std::collections::HashSet<u8>,
    overlay_rows: usize,
}

impl CoreSink for Recorder {
    fn video_scanline(&mut self, y: u16, pixels: &[PpuPixel]) {
        self.rows.push((y, pixels.len()));
        for p in pixels {
            self.distinct.insert(p.palette_index);
        }
    }
    fn overlay_scanline(&mut self, _y: u16, _pixels: &[OverlayPixel]) {
        self.overlay_rows += 1;
    }
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, _event: CoreEvent) {}
}

fn fixture() -> Option<Vec<u8>> {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/snes/rf-scroller-s/build/rf-scroller-s.sfc");
    std::fs::read(p).ok()
}

#[test]
fn a_snes_frame_reaches_a_core_sink() {
    let Some(rom) = fixture() else {
        panic!("SNES fixture missing — this test is about frames and cannot run without one");
    };
    let mut core = SnesCore::load(&rom).expect("the fixture is a valid SNES image");
    let mut sink = Recorder::default();

    // A few frames, so the ROM is past reset and has drawn something.
    for _ in 0..8 {
        core.run_frame(&InputFrame::empty(), &mut sink);
    }

    assert!(
        !sink.rows.is_empty(),
        "no scanline reached the sink. The SNES core renders on demand from settled state \
         rather than draining mid-frame, so if the adapter does not replay the frame, a sink \
         sees nothing at all — and every other assertion here would be vacuous."
    );
    // 224 normally, 239 with SETINI overscan. Asserting the SET rather
    // than a number, because a core that always emitted 224 would
    // silently crop an overscan game's bottom fifteen rows (W7-06).
    let per_frame = sink.rows.len() / 8;
    assert!(
        per_frame == 224 || per_frame == 239,
        "a frame was {per_frame} scanlines; SETINI allows 224 or 239 and nothing else"
    );
    assert!(
        sink.rows.iter().all(|(_, w)| *w == 256 || *w == 512),
        "scanlines must be 256 dots, or 512 in a hires mode — W11-08 is what made the second \
         representable at all"
    );
    // **Not a blank screen.** A core that emitted the right number of
    // rows of a single palette index would satisfy every count above.
    assert!(
        sink.distinct.len() > 1,
        "every pixel of every scanline is palette index {:?} — the frames are uniform, which \
         is what an adapter that replays an unrendered PPU looks like",
        sink.distinct
    );
    // The dropped-sprite overlay rides alongside, and forwarding only the
    // pixels would make the 32-per-line suppression invisible to the
    // feature built to show it.
    assert_eq!(
        sink.overlay_rows,
        sink.rows.len(),
        "every video scanline must be accompanied by its overlay scanline"
    );
}

/// **A frame that cannot complete is reported, not spun on.** In the app
/// this runs inside the core thread's guarded closure, where an unbounded
/// loop would block `CoreCommand::Shutdown` from ever being drained.
#[test]
fn an_impossible_budget_reports_incompletion_instead_of_hanging() {
    let Some(rom) = fixture() else { return };
    let mut core = SnesCore::load(&rom).expect("fixture loads");
    let mut sink = Recorder::default();
    core.set_frame_budget(4); // far below a frame's worth of instructions

    let result = core.step(Step::Frame, &mut sink);
    assert!(
        !result.frame_complete,
        "four instructions cannot complete a frame; reporting completion means the bound is \
         not wired into the loop"
    );
    assert!(
        sink.rows.is_empty(),
        "an incomplete frame must not be replayed into the sink — half a picture presented as \
         a whole one is worse than none"
    );
}

/// `peek` answers for memory and refuses registers, because a SNES
/// register read can latch (`$2139` advances the VRAM read address) and a
/// viewer that changes what it observes is worse than one with blanks.
#[test]
fn peek_reads_memory_without_touching_registers() {
    let Some(rom) = fixture() else { return };
    let core = SnesCore::load(&rom).expect("fixture loads");
    // $7E0000 is the first byte of work RAM — a memory target.
    let _ = core.peek(0x7E_0000);
    // $002139 is VMDATALREAD, whose real read advances the VRAM address.
    assert_eq!(
        core.peek(0x00_2139),
        0,
        "a register address must report the quiescent 0, not be read for real"
    );
}
