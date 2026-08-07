//! Criterion benchmark for ticket W4-00 acceptance criterion 2:
//! "`EventMask::NONE` -- the Accuracy-mode default -- is measurably
//! ~zero-cost". Lives outside `src/` for the same reason `cpu_step.rs`
//! does (wall-clock timing shouldn't trip `scripts/validate-arch.sh`'s
//! determinism grep, which only scans `crates/rf-nes/src`).
//!
//! Run with `cargo bench -p rf-nes --bench event_emission`.
//!
//! ## Two arms, at two granularities
//!
//! A whole-frame `Ppu::tick()` loop only touches an `EventMask::is_subscribed`
//! guard on ~243 of 89,342 dots (0.27%) -- FrameEnd/FrameStart once,
//! VblankStart once, Scanline 240 times. That is too small a fraction for
//! criterion's own noise floor to discriminate a non-free guard from a free
//! one, so this file benches at two granularities:
//!
//! - `scroll_write_event_mask_none`: 10,000 `$2005` writes
//!   (`Ppu::write_register(5, ..)`), where EVERY call evaluates the
//!   `EventMask::SCROLL_WRITE` guard -- the highest guard-density site in
//!   the crate, and the one an added-cost regression would show up in
//!   first.
//! - `frame_tick_event_mask_none`: one full 89,342-dot frame, exercising
//!   FrameStart/FrameEnd/VblankStart/Scanline at their REAL call
//!   frequency -- the realistic whole-frame number, even though its guard
//!   fraction is small. Drains into a null [`CoreSink`] once per simulated
//!   frame, INSIDE the timed closure -- `Ppu::drain`'s own doc: "a caller
//!   that goes more than one frame without draining grows [`completed`]
//!   further (240 more rows per undrained frame) rather than silently
//!   losing scanlines". A first version of this arm never drained at all;
//!   `completed`'s unbounded growth across ~20,000 timed iterations (each
//!   appending 240 more `PpuPixel` rows) dominated the measurement with
//!   allocation-growth drift, not the event-mask guard this bench exists to
//!   measure -- see this ticket's report for the (invalid) numbers that
//!   produced. Draining is itself part of the honest per-frame cost this
//!   arm claims to represent, so it belongs in both trees' timed region,
//!   not just a fix for this file.
//!
//! ## The "no-event baseline" comparison
//!
//! Criterion's own `--save-baseline`/`--baseline` compares this exact file
//! (unmodified) run from a git worktree at the pre-ticket commit
//! (`b30584a`, before `Ppu::event_mask`/`Ppu::events`/the `is_subscribed`
//! guards existed) against this working tree, via a SHARED
//! `CARGO_TARGET_DIR` so both runs write into the same
//! `target/criterion/` store:
//!
//! ```sh
//! git worktree add /tmp/rf-nes-w4-00-base b30584a
//! cp crates/rf-nes/benches/event_emission.rs /tmp/rf-nes-w4-00-base/crates/rf-nes/benches/  # will fail to build there -- see report
//! ```
//!
//! See this ticket's report for whether that comparison held together and
//! the actual recorded numbers -- the pre-ticket worktree has no
//! `set_event_mask` method (the field/method are new in this ticket), so
//! the bench file itself can't be reused verbatim there; the report states
//! plainly whether a reduced/adapted version was benched instead and what
//! that means for the comparison's strength.
use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion};
use rf_cart::Mirroring;
use rf_core_api::{CoreEvent, CoreSink, EventMask, PpuPixel};
use rf_nes::Ppu;

fn new_ppu() -> Ppu {
    Ppu::new(vec![0u8; 0x2000], false, Mirroring::Horizontal)
}

/// Discards everything -- used only so `bench_frame_tick` can drain
/// `Ppu::completed`/`Ppu::events` every simulated frame without the sink
/// itself becoming part of what's measured.
struct NullSink;
impl CoreSink for NullSink {
    fn video_scanline(&mut self, _y: u16, _pixels: &[PpuPixel]) {}
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, _ev: CoreEvent) {}
}

fn bench_scroll_write(c: &mut Criterion) {
    let mut ppu = new_ppu();
    ppu.set_event_mask(EventMask::NONE);
    let mut toggle = 0u8;

    c.bench_function("scroll_write_event_mask_none", |b| {
        b.iter(|| {
            toggle = toggle.wrapping_add(1);
            ppu.write_register(5, black_box(toggle));
        });
    });
}

fn bench_frame_tick(c: &mut Criterion) {
    let mut ppu = new_ppu();
    ppu.set_event_mask(EventMask::NONE);
    const DOTS_PER_FRAME: u32 = 262 * 341;
    let mut sink = NullSink;

    c.bench_function("frame_tick_event_mask_none", |b| {
        b.iter(|| {
            for _ in 0..DOTS_PER_FRAME {
                ppu.tick();
            }
            ppu.drain(&mut sink); // see module doc: must happen every frame
        });
    });
}

criterion_group!(benches, bench_scroll_write, bench_frame_tick);
criterion_main!(benches);
