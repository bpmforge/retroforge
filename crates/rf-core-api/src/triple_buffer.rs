//! Lock-light triple buffer for cross-thread `FrameBundle` handoff (ticket
//! W4-01; ARCHITECTURE §6: "Cross-thread handoff is a triple-buffered
//! `FrameBundle`"; RENDERER §1: "All passes run on the render thread
//! against triple-buffered `FrameBundle`s").
//!
//! ## Placement: why this lives in `rf-core-api`, not `rf-enhance`
//!
//! Both `rf-renderer` (RENDERER §1's render thread) and `rf-enhance`
//! (ENHANCEMENT_RUNTIME §1's `FrameBundle -> EnhancementRuntime::on_frame`)
//! need to consume triple-buffered `FrameBundle`s, and neither may depend
//! on the other (ARCHITECTURE §3: Host and Enhance are siblings under the
//! app shell, no edge drawn between them). The same cross-consumer
//! reasoning the ticket's placement ruling applies to [`crate::FrameBundle`]
//! itself applies here: `rf-core-api` is the one crate both already depend
//! on.
//!
//! ## Design: why a `Mutex` per slot rather than fully lock-free
//!
//! Exactly one writer (the core thread, ARCHITECTURE §6's single core
//! thread), any number of readers (render thread, debug panels, tests).
//! Each of the 3 slots holds an `Arc<T>`; [`TripleBufferWriter::publish`]
//! swaps in a fresh `Arc` rather than mutating the pointee of an `Arc`
//! clone a reader might already be holding — see `tests/frame_bundle.rs`'s
//! `each_publish_creates_a_genuinely_new_buffer_not_a_mutated_alias` for
//! the property this guarantees (a reader's already-taken snapshot can
//! never change under it, and `Arc::ptr_eq` proves two different
//! publishes never hand out literally the same buffer). A reader's
//! critical section is "clone one `Arc` out of a `Mutex`" — O(1)
//! regardless of `T`'s size or how long the reader then spends using its
//! clone, since that work happens *after* the lock is released. With 3
//! slots written round-robin, the writer only revisits a given slot once
//! every 3 publishes, so in practice it is never still inside a reader's
//! O(1) critical section when it comes back around — the core thread
//! (ARCHITECTURE §6: "the core is never blocked by GPU work") is never
//! meaningfully delayed by a slow reader. A real lock-free SPMC ring is
//! possible but was not needed to satisfy that invariant, and would add
//! `unsafe` code this crate has none of today.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

struct Shared<T> {
    slots: [Mutex<Arc<T>>; 3],
    latest: AtomicUsize,
}

/// The single-owner write side of a [`triple_buffer`] pair — the core
/// thread's handle. Deliberately not `Clone`: exactly one writer per
/// buffer (ARCHITECTURE §6's single core thread), so `write_idx` needs no
/// synchronization of its own.
pub struct TripleBufferWriter<T> {
    shared: Arc<Shared<T>>,
    write_idx: usize,
}

/// The cloneable read side — the render thread, debug panels, and tests
/// can each hold (or clone further) their own [`TripleBufferReader`], all
/// backed by the same underlying buffer. Cloning is cheap (one `Arc`
/// refcount bump, not a copy of `T`).
pub struct TripleBufferReader<T> {
    shared: Arc<Shared<T>>,
}

impl<T> Clone for TripleBufferReader<T> {
    fn clone(&self) -> Self {
        TripleBufferReader {
            shared: Arc::clone(&self.shared),
        }
    }
}

/// Create a linked writer/reader pair. All 3 slots start holding the same
/// `Arc` around `initial` (a cheap pointer clone, not a `T::clone` — `T`
/// need not implement `Clone` at all), so a reader that polls before the
/// first real [`TripleBufferWriter::publish`] gets a well-defined value
/// rather than an `Option` it has to unwrap.
pub fn triple_buffer<T>(initial: T) -> (TripleBufferWriter<T>, TripleBufferReader<T>) {
    let a = Arc::new(initial);
    let shared = Arc::new(Shared {
        slots: [
            Mutex::new(Arc::clone(&a)),
            Mutex::new(Arc::clone(&a)),
            Mutex::new(a),
        ],
        latest: AtomicUsize::new(0),
    });
    (
        TripleBufferWriter {
            shared: Arc::clone(&shared),
            write_idx: 0,
        },
        TripleBufferReader { shared },
    )
}

impl<T> TripleBufferWriter<T> {
    /// Publish a new value: write it into the next slot in round-robin
    /// order, then publish that slot's index. Never blocks on a reader for
    /// longer than an `Arc` clone takes (module doc).
    pub fn publish(&mut self, value: T) {
        self.write_idx = (self.write_idx + 1) % 3;
        {
            let mut slot = self.shared.slots[self.write_idx].lock().expect(
                "triple buffer slot mutex poisoned (a reader must have panicked while holding it)",
            );
            *slot = Arc::new(value);
        }
        self.shared.latest.store(self.write_idx, Ordering::Release);
    }
}

impl<T> TripleBufferReader<T> {
    /// The most recently published snapshot. Cheap (one `Arc` clone under
    /// a short-lived lock) and safe to call at any rate — a reader that
    /// polls faster than the writer publishes just sees the same `Arc` (by
    /// pointer) again; one that polls slower simply misses intermediate
    /// frames, which is the point of "latest wins" (ARCHITECTURE §6:
    /// workers may lag, they never block the writer).
    #[must_use]
    pub fn latest(&self) -> Arc<T> {
        let idx = self.shared.latest.load(Ordering::Acquire);
        Arc::clone(&self.shared.slots[idx].lock().expect(
            "triple buffer slot mutex poisoned (a reader must have panicked while holding it)",
        ))
    }
}
