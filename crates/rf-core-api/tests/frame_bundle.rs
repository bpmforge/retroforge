//! `FrameBundle` + [`rf_core_api::triple_buffer`] tests (ticket W4-01,
//! acceptance criterion 2: "triple-buffered `FrameBundle` to render/debug
//! threads"). Mirrors `tests/mock_core.rs`'s convention of testing
//! `rf-core-api` public types from an integration test rather than an
//! inline `#[cfg(test)]` module (this crate's `src/*.rs` files have none).
use std::sync::Arc;
use std::thread;

use rf_core_api::{
    triple_buffer, CoreEvent, CoreSink, EventMask, FrameBundle, FrameBundleBuilder, PixelLayer,
    PpuPixel,
};

fn pixel(palette_index: u8) -> PpuPixel {
    PpuPixel {
        palette_index,
        layer: PixelLayer::Background(0),
        sprite_id: None,
        priority: 0,
    }
}

fn bundle_with_frame_count(frame_count: u64) -> FrameBundle {
    FrameBundle::new(frame_count, 2, 1, vec![pixel(1), pixel(2)], Vec::new())
}

// ---------------------------------------------------------------------
// FrameBundle / FrameBundleBuilder
// ---------------------------------------------------------------------

#[test]
fn new_accepts_a_correctly_sized_video_buffer() {
    let bundle = FrameBundle::new(1, 2, 2, vec![pixel(0); 4], vec![CoreEvent::FrameStart]);
    assert_eq!(bundle.frame_count, 1);
    assert_eq!(bundle.width, 2);
    assert_eq!(bundle.height, 2);
    assert_eq!(bundle.video.len(), 4);
    assert_eq!(bundle.events, vec![CoreEvent::FrameStart]);
}

#[test]
#[should_panic(expected = "FrameBundle: 3 pixels for a 2x2 frame")]
fn new_panics_on_a_mismatched_video_length() {
    let _ = FrameBundle::new(0, 2, 2, vec![pixel(0); 3], Vec::new());
}

#[test]
fn empty_is_zero_sized_and_matches_default() {
    let empty = FrameBundle::empty();
    assert_eq!(empty.frame_count, 0);
    assert_eq!(empty.width, 0);
    assert_eq!(empty.height, 0);
    assert!(empty.video.is_empty());
    assert!(empty.events.is_empty());
    assert_eq!(empty, FrameBundle::default());
}

#[test]
fn builder_accumulates_scanlines_into_a_row_major_buffer() {
    let mut builder = FrameBundleBuilder::new(2, 2);
    builder.video_scanline(0, &[pixel(10), pixel(11)]);
    builder.video_scanline(1, &[pixel(20), pixel(21)]);
    let bundle = builder.take(7);
    assert_eq!(bundle.frame_count, 7);
    assert_eq!(
        bundle
            .video
            .iter()
            .map(|p| p.palette_index)
            .collect::<Vec<_>>(),
        vec![10, 11, 20, 21]
    );
}

#[test]
fn builder_ignores_overlay_scanline_by_design() {
    // FrameBundle::video must stay accuracy-exact regardless of an
    // enhancement overlay -- see FrameBundleBuilder's CoreSink impl doc.
    let mut builder = FrameBundleBuilder::new(2, 1);
    builder.video_scanline(0, &[pixel(5), pixel(6)]);
    builder.overlay_scanline(
        0,
        &[
            rf_core_api::OverlayPixel {
                palette_index: 0x3F,
                opaque: true,
            },
            rf_core_api::OverlayPixel {
                palette_index: 0x3F,
                opaque: true,
            },
        ],
    );
    let bundle = builder.take(0);
    assert_eq!(
        bundle
            .video
            .iter()
            .map(|p| p.palette_index)
            .collect::<Vec<_>>(),
        vec![5, 6],
        "an opaque overlay row must never leak into FrameBundle::video"
    );
}

#[test]
fn builder_drops_out_of_range_scanlines_without_panicking() {
    let mut builder = FrameBundleBuilder::new(2, 1);
    builder.video_scanline(5, &[pixel(1), pixel(2)]);
    let bundle = builder.take(0);
    assert!(
        bundle.video.iter().all(|p| p.palette_index == 0),
        "an out-of-range scanline must be dropped, not panic or write past the buffer"
    );
}

#[test]
fn builder_take_clears_events_but_persists_pixels_across_calls() {
    let mut builder = FrameBundleBuilder::new(1, 1);
    builder.video_scanline(0, &[pixel(9)]);
    builder.event(CoreEvent::FrameStart);
    let first = builder.take(0);
    assert_eq!(first.events, vec![CoreEvent::FrameStart]);

    // No new video_scanline call before the second take: the pixel buffer
    // must still carry the previous frame's content (module doc), while
    // the event log must have been cleared by the first `take`.
    let second = builder.take(1);
    assert_eq!(second.video[0].palette_index, 9);
    assert!(second.events.is_empty());
}

// ---------------------------------------------------------------------
// triple_buffer
// ---------------------------------------------------------------------

#[test]
fn reader_sees_the_initial_value_before_any_publish() {
    let (_writer, reader) = triple_buffer(bundle_with_frame_count(0));
    assert_eq!(reader.latest().frame_count, 0);
}

#[test]
fn reader_sees_each_published_value_in_order() {
    let (mut writer, reader) = triple_buffer(FrameBundle::empty());
    writer.publish(bundle_with_frame_count(1));
    assert_eq!(reader.latest().frame_count, 1);
    writer.publish(bundle_with_frame_count(2));
    assert_eq!(reader.latest().frame_count, 2);
}

/// The mutation-test target (ticket W4-01 mutation #2): a reader's
/// already-taken snapshot must never change under it, and two different
/// publishes must never hand out the literal same buffer. `Arc::ptr_eq` is
/// the precise tool for "hand out the same buffer twice" -- see this
/// file's own mutation-test record in `docs/STATUS.md` for how this was
/// verified to actually fail against a broken `latest()`.
#[test]
fn each_publish_creates_a_genuinely_new_buffer_not_a_mutated_alias() {
    let (mut writer, reader) = triple_buffer(FrameBundle::empty());
    writer.publish(bundle_with_frame_count(1));
    let first = reader.latest();
    writer.publish(bundle_with_frame_count(2));
    writer.publish(bundle_with_frame_count(3));
    let second = reader.latest();

    assert!(
        !Arc::ptr_eq(&first, &second),
        "two different publishes must never hand out the same underlying buffer"
    );
    assert_eq!(
        first.frame_count, 1,
        "a reader's already-taken snapshot must be unaffected by later publishes"
    );
    assert_eq!(
        second.frame_count, 3,
        "a fresh read must see the newest publish"
    );
}

#[test]
fn reader_handles_clone_independently_and_share_the_same_stream() {
    let (mut writer, reader_a) = triple_buffer(FrameBundle::empty());
    let reader_b = reader_a.clone();
    writer.publish(bundle_with_frame_count(42));
    assert_eq!(reader_a.latest().frame_count, 42);
    assert_eq!(reader_b.latest().frame_count, 42);
}

/// "triple-buffered to render/debug threads" (acceptance criterion 2),
/// exercised literally: one writer thread publishing while two independent
/// reader threads (standing in for the render thread and a debug panel)
/// poll concurrently. Never panics, never observes a torn/invalid bundle
/// (every observed `frame_count` must be one that was genuinely
/// published), and the writer is not handed any reader's lock to wait on.
#[test]
fn one_writer_and_two_reader_threads_never_observe_a_torn_or_bogus_bundle() {
    // Sized to DISCRIMINATE, not merely to exercise (ticket W4-01a). The
    // race this guards has a window of a few instructions -- the gap
    // between `publish` releasing a slot and advancing `latest` -- so the
    // original 2 readers x 500 publishes caught the pre-fix defect only
    // ~1-5 runs in 60. A guard that misses its own bug 95% of the time
    // lets the next regression through CI unnoticed. More readers than
    // the writer has slots, each polling far more often than the writer
    // publishes, keeps at least one reader parked on the mutex the writer
    // is about to release, which is exactly the interleaving required.
    const N: u64 = 4_000;
    const READERS: usize = 6;

    let (mut writer, reader) = triple_buffer(FrameBundle::empty());
    let readers: Vec<_> = (0..READERS).map(|_| reader.clone()).collect();
    drop(reader);

    let writer_thread = thread::spawn(move || {
        for i in 1..=N {
            writer.publish(bundle_with_frame_count(i));
        }
    });

    let spin = |r: rf_core_api::TripleBufferReader<FrameBundle>| {
        move || {
            let mut last_seen = 0u64;
            for _ in 0..N * 4 {
                let seen = r.latest().frame_count;
                assert!(
                    seen <= N,
                    "observed frame_count {seen} was never published (torn read)"
                );
                assert!(
                    seen >= last_seen,
                    "frame_count must never go backwards: saw {seen} after {last_seen} \
                     (W4-01a -- `latest` must be advanced INSIDE the slot's critical section, \
                     or a reader blocked on that mutex reads the new frame before `latest` \
                     names its slot, then resolves the stale index to an older one)"
                );
                last_seen = seen;
            }
        }
    };

    let reader_threads: Vec<_> = readers
        .into_iter()
        .map(|r| thread::spawn(spin(r)))
        .collect();

    writer_thread.join().expect("writer thread must not panic");
    for (i, t) in reader_threads.into_iter().enumerate() {
        t.join()
            .unwrap_or_else(|_| panic!("reader thread {i} must not panic"));
    }
}

// ---------------------------------------------------------------------
// CoreEvent::mask_bit
// ---------------------------------------------------------------------

#[test]
fn mask_bit_round_trips_through_is_subscribed_for_every_variant() {
    let events = [
        CoreEvent::FrameStart,
        CoreEvent::FrameEnd,
        CoreEvent::VblankStart,
        CoreEvent::Scanline(10),
        CoreEvent::OamRewrite,
        CoreEvent::ScrollWrite {
            x: 1,
            y: 2,
            layer: PixelLayer::Background(0),
        },
        CoreEvent::MapperIrq,
        CoreEvent::DmaStart { chan: 0 },
        CoreEvent::MemWatch { id: 1 },
    ];
    for ev in events {
        let bit = ev.mask_bit();
        assert!(
            EventMask::ALL.is_subscribed(bit),
            "{ev:?}'s mask_bit must be one of the bits ALL sets"
        );
        assert!(
            !EventMask::NONE.is_subscribed(bit),
            "{ev:?}'s mask_bit must not be set under NONE"
        );
    }
}
