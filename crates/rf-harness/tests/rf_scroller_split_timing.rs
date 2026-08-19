//! Ticket W2-10a's primary-hazard re-measurement (the ticket's own brief:
//! "the HUD/playfield split lands correctly only because the per-frame
//! streaming work already carries execution to scanline ~16 on its own
//! ... you are adding work, so you must re-measure"). Subscribes to
//! `SCANLINE.union(SCROLL_WRITE).union(DMA_START)` and processes
//! `NesBus::drain_video`'s events as one genuinely continuous, in-order
//! stream across all 900 frames (NOT rebucketed per `run_frame` call --
//! a `run_frame` boundary is a PPU `frame_count` tick, which is a
//! DIFFERENT instant than `main.c`'s own `main_loop()` iteration boundary;
//! the two drift relative to each other, which silently corrupted an
//! earlier version of this test that rebucketed events per `run_frame`
//! call and produced a nonsense histogram spanning the whole visible
//! picture -- see the ticket report for that dead end).
//!
//! ## Disambiguating the split write from the top-of-frame write
//!
//! `main.c` performs TWO `$2005` two-byte scroll commits per iteration --
//! top-of-frame (`PPU_SCROLL=0; PPU_SCROLL=camera_y;`) then, after
//! `OAM_DMA=0x02` and `SPLIT_DELAY`, the split
//! (`PPU_SCROLL=camera_x&0xFF; PPU_SCROLL=0;`). Each `$2005` write emits
//! its own `CoreEvent::ScrollWrite` (`crates/rf-nes/src/ppu/scroll.rs::
//! write_scroll`'s own doc: fires on both the first and second write of a
//! pair), so ordinally, per iteration: `DmaStart`, then 4 `ScrollWrite`s
//! -- the 1st+2nd are the top-of-frame pair, the 3rd+4th are the split
//! pair. This ordinal position (not the write's `x`/`y` VALUE -- see
//! "Why not filter by value" below) is what this test uses to isolate
//! each pair: reset a per-iteration counter on `DmaStart`, bucket
//! `ScrollWrite`s 1-2 as "top" and 3-4 as "split".
//!
//! `CoreEvent::Scanline` only fires for the 240 VISIBLE scanlines
//! (`crates/rf-nes/src/ppu/background.rs`: `is_visible && dot==256`), so
//! tracking "the most recently seen `Scanline` value" at the moment of a
//! `ScrollWrite` gives that write's landing scanline directly (mid-scanline
//! firing means the most recent COMPLETED scanline's event is one less
//! than where the write visually lands).
//!
//! ## Why not filter by value, and a trap for consumers of `ScrollWrite`
//!
//! An earlier draft tried to isolate the split pair by filtering
//! `ScrollWrite{x, ..}` where `x != 0`. That is unreliable: `stream_chunk`
//! issues `$2006`/`$2007` writes (PPU_ADDR/PPU_DATA, not PPU_SCROLL) to
//! stream level columns into nametable RAM, and on real PPU hardware (and
//! `crates/rf-nes/src/ppu/scroll.rs`) `$2006` writes go through the SAME
//! `t`/`v` loopy-register machinery that `$2005` does -- they stomp bits
//! of `t`. `effective_scroll()` (whatever decodes `t` into an x/y pair for
//! the `ScrollWrite` event's payload) then reports whatever `t` LOOKS LIKE
//! at that instant, not "the value the game deliberately wrote as a
//! scroll offset". This is why an early debug dump showed a top-of-frame
//! write reporting `y:114` -- that's `t` mid-stomp from the PRECEDING
//! `stream_chunk` calls, not a real scroll value. Ordinal position, not
//! value, is the only reliable disambiguator. Flagged here because W3-05
//! and W4-03d both consume `ScrollWrite` and would hit the same trap.
//!
//! ## The FORMAT.md correction this test's own history required
//!
//! FORMAT.md (pre-W2-10a) documented "1682 mid-frame scroll writes, 100%
//! inside scanline 14-18, zero later than 18" as the split write's landing
//! window. That number is real but it is the TOP-OF-FRAME pair's landing
//! window, not the split pair's: 841 DMA-anchored iterations x 2 events
//! per iteration = 1682, and 14-18 is exactly where the top-of-frame write
//! lands once `main_loop()`'s per-frame work (streaming, and now this
//! ticket's tail scenes) pushes execution past the vblank boundary before
//! the top write fires. The split pair itself lands at scanline ~32 on
//! streaming and tail frames alike, on BOTH the pre-W2-10a ROM and this
//! ticket's ROM -- confirmed by raw event dumps against the checked-in
//! pre-ticket ROM rebuild (`RF_SCROLLER_W2_10_BASELINE_ROM`), not by
//! re-deriving the fix described in the ticket brief. A 2026-08-07
//! "correction" in FORMAT.md claiming the split itself lands at 14-18 was
//! itself measuring the top-of-frame pair; this test's own two wrong
//! drafts (value-filtered, and undifferentiated ordinal counting) made the
//! same mistake before this one. FORMAT.md's "Known defects" section has
//! been rewritten to reflect this (see that file's own history note).
//!
//! ## What this test actually checks
//!
//! This ticket's three new scenes are gated to cost nothing before
//! `columns_streamed` reaches 95 (`main.c`'s `TAIL_GATE_COL`), so the
//! STREAMING WINDOW (iterations before that gate opens) should be
//! completely unperturbed by this ticket -- for BOTH the top-of-frame pair
//! and the split pair. That is the regression check this test runs:
//! streaming-window histograms (top and split, separately) from a rebuild
//! of the pre-W2-10a ROM are frozen as constants below (from an actual
//! measured run, not derived), and the default test asserts this ticket's
//! ROM reproduces them exactly. The TAIL (this ticket's own new region) is
//! reported but not asserted against the pre-ticket ROM, since pre-ticket
//! tail behavior is a different, less-relevant comparison -- it's reported
//! so a future ticket has a real number to diff against, not a claim.
use rf_core_api::{CoreEvent, CoreSink, EventMask, PpuPixel};
use rf_input::NesButton;
use rf_nes::{Cpu, NesBus};
use std::path::PathBuf;

const TOTAL_FRAMES: u64 = 900;
const COLUMNS_STREAMED_ADDR: u16 = 0x602D;
const TAIL_GATE_COL: u8 = 95;

fn resolve_rom() -> Option<PathBuf> {
    if let Ok(configured) = std::env::var("RF_SCROLLER_ROM") {
        let path = PathBuf::from(configured);
        return if path.is_file() { Some(path) } else { None };
    }
    let default = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/nes/rf-scroller/build/rf-scroller.nes");
    if default.is_file() {
        Some(default)
    } else {
        None
    }
}

fn scripted_buttons_right() -> u8 {
    1u8 << NesButton::Right.bit()
}

// NOTE on `later_than_18`: `CoreEvent::Scanline` only fires for the 240
// VISIBLE scanlines (module doc), so a write that happens during vblank
// (after scanline 239 completes, before scanline 0 of the next frame)
// reports `last_scanline == Some(239)` -- numerically `239 > 18`, so it
// counts as "later_than_18" even though semantically it is the GOOD case
// (safely inside vblank, not visibly tearing the picture). `later_than_18`
// is therefore a coarse "not in the 14-18 window" signal, not a "the
// picture visibly tore" signal -- read the full per-scanline breakdown
// (`report()`) to tell the two apart, and see the frozen constants' own
// comment below for the real measured split between them.
#[derive(Default, Clone, PartialEq, Eq, Debug)]
struct Histogram {
    /// scanline -> count
    counts: std::collections::BTreeMap<u16, u32>,
    total: u32,
    later_than_18: u32,
}

impl Histogram {
    fn report(&self, label: &str, iterations: u32) {
        eprintln!(
            "{label}: {} writes over {} DMA-anchored iterations, later-than-18={}",
            self.total, iterations, self.later_than_18,
        );
        for (y, c) in &self.counts {
            eprintln!("  scanline {y} -> {c}");
        }
    }
}

/// Continuous whole-run tracker (module doc). `CoreSink::event` is called
/// in true chronological order by `NesBus::drain_video` (ARCHITECTURE
/// §5's single ordered event FIFO per core), so no per-`run_frame`-call
/// rebucketing is needed. `columns_streamed_now` is sampled directly off
/// `NesBus` at each `DmaStart` via `measure`'s outer loop (see its own
/// comment for why the timing of that sample is exact, not approximated).
struct SplitTracker {
    last_scanline: Option<u16>,
    writes_since_dma: u32,
    in_tail_this_iteration: bool,
    columns_streamed_now: u8,
    streaming_iterations: u32,
    tail_iterations: u32,
    streaming_top: Histogram,
    streaming_split: Histogram,
    tail_top: Histogram,
    tail_split: Histogram,
}

impl SplitTracker {
    fn new() -> Self {
        SplitTracker {
            last_scanline: None,
            writes_since_dma: 0,
            in_tail_this_iteration: false,
            columns_streamed_now: 0,
            streaming_iterations: 0,
            tail_iterations: 0,
            streaming_top: Histogram::default(),
            streaming_split: Histogram::default(),
            tail_top: Histogram::default(),
            tail_split: Histogram::default(),
        }
    }

    fn record(&mut self, is_split: bool) {
        let Some(y) = self.last_scanline else {
            return;
        };
        let hist = match (self.in_tail_this_iteration, is_split) {
            (false, false) => &mut self.streaming_top,
            (false, true) => &mut self.streaming_split,
            (true, false) => &mut self.tail_top,
            (true, true) => &mut self.tail_split,
        };
        *hist.counts.entry(y).or_insert(0) += 1;
        hist.total += 1;
        if y > 18 {
            hist.later_than_18 += 1;
        }
    }
}

impl CoreSink for SplitTracker {
    fn video_scanline(&mut self, _y: u16, _pixels: &[PpuPixel]) {}
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, ev: CoreEvent) {
        match ev {
            CoreEvent::DmaStart { .. } => {
                self.writes_since_dma = 0;
                self.in_tail_this_iteration = self.columns_streamed_now >= TAIL_GATE_COL;
                if self.in_tail_this_iteration {
                    self.tail_iterations += 1;
                } else {
                    self.streaming_iterations += 1;
                }
            }
            CoreEvent::Scanline(y) => self.last_scanline = Some(y),
            CoreEvent::ScrollWrite { .. } => {
                self.writes_since_dma += 1;
                match self.writes_since_dma {
                    1 | 2 => self.record(false),
                    3 | 4 => self.record(true),
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

fn run_frame(bus: &mut NesBus, cpu: &mut Cpu, buttons: u8, sink: &mut dyn CoreSink) {
    bus.set_controller_buttons(0, buttons);
    let start = bus.frame_count();
    let mut guard = 0u64;
    while bus.frame_count() == start {
        cpu.step(bus);
        bus.drain_video(sink);
        guard += 1;
        assert!(
            guard <= 400_000,
            "frame did not complete within guard cycles"
        );
    }
}

fn measure(rom_bytes: &[u8]) -> SplitTracker {
    let mut bus = NesBus::from_ines_bytes(rom_bytes).expect("rf-scroller.nes must be valid iNES");
    bus.set_event_mask(
        EventMask::SCANLINE
            .union(EventMask::SCROLL_WRITE)
            .union(EventMask::DMA_START),
    );
    let mut cpu = Cpu::power_on(&mut bus);
    let mut tracker = SplitTracker::new();

    for _frame in 0..TOTAL_FRAMES {
        // Sampled BEFORE the frame's own DmaStart fires -- reflects
        // whatever columns_streamed was left at by the PREVIOUS
        // iteration, which is what's in effect at the moment THIS
        // iteration's own DmaStart (and therefore its tail classification)
        // occurs, since the only writer (stream_chunk) already ran earlier
        // in the SAME iteration, before OAM_DMA -- so this is exact, not
        // approximated.
        tracker.columns_streamed_now = bus.peek(COLUMNS_STREAMED_ADDR);
        run_frame(&mut bus, &mut cpu, scripted_buttons_right(), &mut tracker);
    }
    tracker
}

/// Streaming-window histograms measured against a rebuild of the
/// pre-W2-10a `main.c` (git rev `a486c6d`, the commit that introduced
/// `fixtures/nes/rf-scroller`, before this ticket's changes -- rebuilt at
/// `/tmp/orig_check2/rf-scroller-orig.nes` from `git show
/// a486c6d:fixtures/nes/rf-scroller/src/{main.c,level_data.c,
/// leveldata.h}` plus that rev's unchanged `chr.s`/`build.sh`), via:
/// ```text
/// RF_SCROLLER_W2_10_BASELINE_ROM=/tmp/orig/rf-scroller-orig.nes \
///   cargo test -p rf-harness --test rf_scroller_split_timing \
///   -- --ignored measure_and_print_streaming_baseline --nocapture
/// ```
/// 591 DMA-anchored iterations landed in the streaming window out of 900
/// frames on that ROM: `streaming_top` bimodal at 14-19/239 (239 = still
/// in vblank, the good case), `streaming_split` bimodal at 16-17/32-37
/// (the split pair NEVER lands in vblank on that ROM either -- the 32-37
/// cluster is what the pre-W2-10a "corrected" FORMAT.md entry
/// mis-measured as fixed; see module doc).
///
/// This ticket's FIRST measurement against that same method (before the
/// `read_buttons()`/call-site-gating fix below existed) showed the
/// streaming window shifted ~19-20 scanlines later across the board, and
/// -- the actually blocking finding -- 62 of the TAIL's 243 split writes
/// (26%) landed at scanline 239, meaning the split was MISSED entirely on
/// those frames (the HUD scroll bled into the playfield for the rest of
/// the visible picture). Root-caused via four isolation builds (revert
/// `read_buttons()` to W2-10's read-cost shape; skip the three
/// `update_*()` calls entirely; revert `camera_y` writes to literal 0;
/// each alone, against the same tracker) to `read_buttons()`'s
/// shift-accumulate loop (~11-12 of ~19-20 scanlines -- cc65 -O0 reloads
/// the local accumulator from the C stack every iteration) and the three
/// `update_*()` calls' redundant per-call JSR/RTS + gate-check overhead
/// even when each immediately early-returns (~5-8 scanlines); `camera_y`
/// itself was negligible. Fixed by rewriting `read_buttons()` to an
/// unrolled 8-read sequence with no loop counter (still hardware-correct:
/// 8 reads to drain the shift register, but only 3 conditional ORs
/// instead of an accumulate-every-iteration pattern) and by gating the
/// three `update_*()` calls behind ONE shared `columns_streamed >=
/// TAIL_GATE_COL` check in `main_loop()` instead of three redundant
/// per-function ones (see both functions' own doc comments in
/// `main.c`). Not a reorder of `main_loop()`'s structure (streaming's
/// `$2006`/`$2007` writes are only PPU-safe during vblank/forced-blank
/// and cannot be deferred past the split without corrupting the picture
/// on real hardware, so "move stream_chunk after the split" was not
/// viable) -- a narrower, measured fix at the two actual cost centers.
///
/// The numbers below are THIS TICKET's OWN post-fix measurement, frozen
/// as the regression baseline future changes to this fixture are checked
/// against (not the pre-ticket ancestor's numbers -- those are reported
/// above for the historical record, but this ticket's additions
/// legitimately and permanently changed the streaming-window cost floor,
/// so re-deriving from the ancestor forever would be asserting a fixture
/// property this ROM can no longer have). Full per-scanline equality, not
/// just totals, so a shift that happens to preserve the total (writes
/// moving between scanlines) still fails.
/// RE-MEASURED after the `git checkout` incident recorded on the ticket
/// (`plan.json`'s W2-10a notes): `main.c` was rebuilt from scratch and, in
/// the rebuild, the gem/blink/vertical-area update block was moved to run
/// AFTER the split write instead of before it (the fix for the
/// gem-transfer-corruption bug -- see `rf_scroller_red_fixture_scenes.rs`'s
/// module doc, "Criterion 1" section, for the `OAMADDR`-reset-during-DMA
/// mechanism this closes). That reorder means NOTHING new runs before
/// either scroll-write pair on ANY frame, streaming or tail -- so these
/// numbers are a fresh, independent measurement, not a restoration of the
/// pre-incident values (which described a ROM with the update block in the
/// OLD position and no longer exist for that reason).
/// RE-BASELINED 2026-08-17 by ticket **W2-21**, and this time the cause
/// was NOT the fixture. W2-21 flipped the OAM-DMA get/put phase, which
/// `crate::system`'s `is_get_cycle` had picked arbitrarily ("at power-on,
/// whether the first CPU cycle is get or put is random" — so the crate
/// picked one and documented it), because blargg's `cpu_interrupts_v2`
/// `4-irq_and_dma` pins it: under the old phase its DMA ran one cycle
/// short and the ROM's `8`/`9` boundary landed at `+526` instead of
/// `+527`. A one-cycle change in DMA length moves where a few of this
/// fixture's writes land, and exactly three moved: 13 gained one, 14 lost
/// its only one, 15 gained one and 16 lost one.
///
/// This is a legitimate re-baseline, not a masked regression, and the
/// numbers that carry the SAFETY meaning are untouched: `total` is still
/// 1182, `later_than_18` still 734, and `FROZEN_STREAMING_ITERATIONS` is
/// unchanged. What this histogram guards — the fixture reintroducing
/// per-frame cost before the tail gate — is unaffected by an emulator
/// timing correction.
fn frozen_streaming_top() -> Histogram {
    Histogram {
        // Re-baselined by ticket W5-02c's streaming-order fix
        // (`stream_chunk()` moved ahead of `read_buttons()` and the
        // camera arithmetic, so the chunk no longer overruns vblank —
        // FORMAT.md's "Streaming order and vblank budget" section).
        //
        // **The invariant this histogram exists to protect is
        // UNCHANGED**: `later_than_18` is still exactly 734 and the
        // scanline-239 count is still exactly 734, so nothing moved into
        // vblank-crossing territory — the hard failure mode this test
        // names. What changed is a redistribution WITHIN the safe early
        // window (11-16), which is what moving a few hundred cycles of
        // work earlier in the frame does. Total 1182 -> 1180 and
        // iterations 591 -> 590 because the fix costs one frame of
        // streamer lookahead.
        counts: [
            (11, 267),
            (12, 51),
            (13, 44),
            (14, 20),
            (15, 64),
            (239, 734),
        ]
        .into_iter()
        .collect(),
        total: 1180,
        later_than_18: 734,
    }
}

fn frozen_streaming_split() -> Histogram {
    Histogram {
        counts: [
            // Re-baselined by W5-02c with the rest — see
            // `frozen_streaming_top` for the reason and for why the
            // vblank-crossing invariant is intact. `later_than_18` moves
            // 448 -> 446, i.e. two writes, which is the same one-frame
            // lookahead shift that took `total` from 1182 to 1180; the
            // distribution's shape is unchanged.
            (14, 679),
            (15, 55),
            (29, 241),
            (30, 57),
            (31, 59),
            (32, 25),
            (33, 64),
        ]
        .into_iter()
        .collect(),
        total: 1180,
        later_than_18: 446,
    }
}

const FROZEN_STREAMING_ITERATIONS: u32 = 590;

/// Re-derives the historical pre-W2-10a streaming histograms documented
/// above, for comparison only (not asserted against -- see that doc
/// comment for why). `#[ignore]`'d because that ROM isn't checked in
/// (D-001); run explicitly with `RF_SCROLLER_W2_10_BASELINE_ROM` set, per
/// the rebuild recipe in the frozen histograms' own doc comment above.
#[test]
#[ignore]
fn measure_and_print_streaming_baseline() {
    let Ok(configured) = std::env::var("RF_SCROLLER_W2_10_BASELINE_ROM") else {
        panic!(
            "set RF_SCROLLER_W2_10_BASELINE_ROM to a build of the pre-W2-10a source \
             (git rev a486c6d) to re-derive the historical streaming histograms"
        );
    };
    let rom_bytes = std::fs::read(PathBuf::from(&configured))
        .unwrap_or_else(|e| panic!("failed to read {configured}: {e}"));
    let tracker = measure(&rom_bytes);
    tracker
        .streaming_top
        .report("streaming_top", tracker.streaming_iterations);
    tracker
        .streaming_split
        .report("streaming_split", tracker.streaming_iterations);
    tracker.tail_top.report(
        "tail_top (pre-ticket has no gated scenes)",
        tracker.tail_iterations,
    );
    tracker.tail_split.report(
        "tail_split (pre-ticket has no gated scenes)",
        tracker.tail_iterations,
    );
}

/// Re-measures the split-scanline histogram over 900 frames of held Right
/// (module doc), split into the streaming window (before
/// `columns_streamed` reaches 95) and the tail (this ticket's own scenes'
/// only active region), and each further split into the top-of-frame pair
/// vs the split pair, so a tail-only or split-only drift isn't hidden
/// inside an aggregate. Not `#[ignore]`'d: 900 frames of interpretation is
/// well under the budget the existing fast anti-vacuity test already
/// spends.
#[test]
fn split_timing_histogram_streaming_window_vs_tail() {
    let Some(rom_path) = resolve_rom() else {
        eprintln!(
            "SKIP split_timing_histogram_streaming_window_vs_tail: rf-scroller.nes not built. \
             Build it first: cd fixtures/nes/rf-scroller && ./build.sh (requires cc65 -- brew \
             install cc65 on macOS)"
        );
        return;
    };
    let rom_bytes = std::fs::read(&rom_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", rom_path.display()));
    let tracker = measure(&rom_bytes);

    tracker.streaming_top.report(
        "STREAMING WINDOW top-of-frame pair",
        tracker.streaming_iterations,
    );
    tracker
        .streaming_split
        .report("STREAMING WINDOW split pair", tracker.streaming_iterations);
    tracker
        .tail_top
        .report("TAIL top-of-frame pair", tracker.tail_iterations);
    tracker
        .tail_split
        .report("TAIL split pair", tracker.tail_iterations);

    // Regression check: this is NOT asserted against the pre-W2-10a
    // ancestor ROM (frozen-histogram doc comment above explains why: this
    // ticket's read_buttons()/call-site-gating fix legitimately changed
    // the streaming-window cost floor from the ancestor's). It IS
    // asserted against THIS ticket's own post-fix measurement, frozen
    // above, so a FUTURE change to this fixture that reintroduces
    // per-frame cost before the tail gate gets caught -- full
    // per-scanline breakdown, both pairs, plus the iteration count
    // (catches movement-pacing changes that would shift when the tail
    // gate opens).
    assert_eq!(
        tracker.streaming_iterations, FROZEN_STREAMING_ITERATIONS,
        "streaming-window DMA-anchored iteration count regressed from this ticket's own \
         verified-safe baseline"
    );
    assert_eq!(
        tracker.streaming_top,
        frozen_streaming_top(),
        "streaming-window top-of-frame pair histogram regressed from this ticket's own \
         verified-safe baseline"
    );
    assert_eq!(
        tracker.streaming_split,
        frozen_streaming_split(),
        "streaming-window split pair histogram regressed from this ticket's own verified-safe \
         baseline -- note the split pair legitimately lands as late as scanline 29-34 even in \
         this baseline (see the frozen-histogram doc comment); this asserts the DISTRIBUTION is \
         unchanged, not that it's all in a narrow window"
    );

    // The tail is this ticket's own new territory -- reported above for a
    // future ticket to diff against, not asserted against the pre-ticket
    // ROM (whose tail had no gated scenes running). Sanity-checked here:
    // 900 frames of held Right must actually reach the tail at least
    // once, and the split pair must not regress into vblank-crossing
    // territory (a hard failure mode: it would visibly tear the picture
    // every tail frame, not just run a little later).
    assert!(
        tracker.tail_iterations > 0,
        "900 frames of held Right must reach the tail (columns_streamed>=95) at least once, \
         per the same wraparound witness W2-10's own tests rely on (columns_streamed==95 by \
         frame 900)"
    );
    // The split pair NEVER legitimately lands in vblank (frozen-histogram
    // doc comment: confirmed on the pre-ticket ROM too, whose tail split
    // sat cleanly at 17-18, never 239). A tail split write reporting
    // `last_scanline == 239` therefore does NOT mean "landed safely at
    // the end of the picture" (that reading is only valid for the
    // top-of-frame pair) -- it means the split write's own scanline
    // event never arrived before `Scanline(239)` completed, i.e. the
    // split was effectively SKIPPED for that iteration and the HUD's
    // scroll bled into the entire playfield for the rest of that frame.
    // This is the exact defect this ticket's read_buttons()/call-site-gating
    // fix (frozen-histogram doc comment) exists to close: measured at 62
    // of 243 tail iterations (26%) before that fix, 0 of 239 after.
    let tail_split_missed_frame_count = tracker.tail_split.counts.get(&239).copied().unwrap_or(0);
    assert_eq!(
        tail_split_missed_frame_count, 0,
        "TAIL split write missed the frame entirely (landed at/after scanline 239, i.e. after \
         the last completed Scanline event) on {tail_split_missed_frame_count} of {} tail \
         iterations -- the HUD's horizontal scroll bled into the whole playfield on those \
         frames",
        tracker.tail_iterations
    );
}
