//! The trace transport and the trace-to-file writer (ticket W4-10a;
//! `docs/design/DEBUGGER.md` §2).
//!
//! `rf_debugger::trace` owns the data model and the scrollback the user
//! scrolls; this module owns the two things that need a running machine
//! and a thread: getting entries off the core thread without ever
//! blocking it, and writing them to disk.
//!
//! ## Why the transport drops the NEWEST
//!
//! DEBUGGER.md §2 asks for both "never blocks the core thread" and
//! "overflow drops oldest". Verified against the vendored `rtrb 0.3`
//! source, one queue cannot do both: `Producer` has `push`, `is_full` and
//! `slots`, and `pop` belongs to `Consumer`. The producer — the core
//! thread — physically cannot evict the oldest entry, so a full ring
//! leaves exactly one non-blocking option, which is to refuse the new
//! one and count it.
//!
//! "Drops oldest" is then honoured where the user can actually observe
//! it, in `rf_debugger::trace::TraceScrollback`. See that module's doc
//! for the full seam; the short version is that transport loss means the
//! trace has **holes**, scrollback loss means it has merely **scrolled**,
//! and [`rf_debugger::trace::TraceStats`] keeps them apart because they
//! tell the user to do different things.
//!
//! ## Pay-for-use (DEBUGGER.md §6)
//!
//! Nothing here is allocated or entered unless the user armed a trace.
//! The core thread holds an `Option<TraceProducer>`; `None` is the
//! shipped, untraced state, and the run loop's fast path is one
//! `Option::is_some` test per frame — see `crate::core_thread` and the
//! `debugger_idle` bench, which measures exactly that claim rather than
//! asserting it.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;

use rf_debugger::trace::{TraceEntry, TraceFilter, TraceKind, TraceScrollback};

/// Transport capacity, in entries per chip.
///
/// Sized against the worst realistic burst rather than picked round: the
/// 2A03 retires roughly 30 000 instructions per frame at NTSC speed, and
/// the UI drains once per repaint. One frame of CPU trace therefore has
/// to fit with headroom for a repaint that ran late, so this holds about
/// two frames' worth.
pub const TRANSPORT_CAPACITY: usize = 65_536;

/// How many entries the viewer keeps. ~24 MB at a nestest line's ~73
/// bytes plus `String` overhead — large enough to scroll back through
/// several frames, small enough that an armed trace left running does not
/// grow without bound (which is what the file writer is for).
pub const SCROLLBACK_CAPACITY: usize = 200_000;

/// The core-thread half. Never blocks, never allocates on the hot path
/// beyond the entry itself.
pub struct TraceProducer {
    tx: rtrb::Producer<TraceEntry>,
    dropped: Arc<AtomicU64>,
    /// Capture-side filter. Applying it HERE rather than in the viewer is
    /// the difference between a trace that costs a filtered fraction of
    /// full speed and one that costs all of it — DEBUGGER.md §2's own
    /// advice ("suggests filtered traces") only helps if the filter runs
    /// before the entry is formatted and shipped.
    pub filter: TraceFilter,
    /// Optional fan-out to the file writer.
    file: Option<mpsc::Sender<TraceEntry>>,
}

impl TraceProducer {
    /// Whether `kind` is wanted at all — checked by the caller *before*
    /// formatting an entry, since formatting is the expensive part.
    #[must_use]
    pub fn wants(&self, kind: TraceKind) -> bool {
        self.filter.kinds.contains(&kind)
    }

    /// Hand one entry over. Drops and counts it if the transport is full.
    pub fn push(&mut self, entry: TraceEntry) {
        if !self.filter.matches(&entry) {
            return;
        }
        if let Some(file) = &self.file {
            // A full file channel means the writer thread is behind; the
            // send is unbounded so this cannot block, and a disconnected
            // channel (writer stopped or failed) is not fatal to the
            // in-memory trace.
            let _ = file.send(entry.clone());
        }
        if self.tx.push(entry).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// The UI-thread half.
pub struct TraceDrain {
    rx: rtrb::Consumer<TraceEntry>,
    dropped: Arc<AtomicU64>,
    seen_dropped: u64,
}

impl TraceDrain {
    /// Move everything currently queued into `scrollback`, and fold in
    /// however many entries the transport had to refuse since last time.
    ///
    /// Returns how many entries were moved.
    pub fn drain_into(&mut self, scrollback: &mut TraceScrollback) -> usize {
        let mut moved = 0;
        while let Ok(entry) = self.rx.pop() {
            scrollback.push(entry);
            moved += 1;
        }
        // Report the DELTA, not the total: `note_transport_drops`
        // accumulates, so handing it the running total every drain would
        // multiply the reported loss by the number of repaints.
        let total = self.dropped.load(Ordering::Relaxed);
        if total > self.seen_dropped {
            scrollback.note_transport_drops(total - self.seen_dropped);
            self.seen_dropped = total;
        }
        moved
    }
}

/// Build a connected producer/drain pair, with entries also fanned out to
/// a [`TraceFileWriter`]'s channel.
#[must_use]
pub fn channel_with_file(
    filter: TraceFilter,
    file: mpsc::Sender<TraceEntry>,
) -> (TraceProducer, TraceDrain) {
    let (mut tx, rx) = channel(filter);
    tx.file = Some(file);
    (tx, rx)
}

/// Build a connected producer/drain pair.
#[must_use]
pub fn channel(filter: TraceFilter) -> (TraceProducer, TraceDrain) {
    let (tx, rx) = rtrb::RingBuffer::new(TRANSPORT_CAPACITY);
    let dropped = Arc::new(AtomicU64::new(0));
    (
        TraceProducer {
            tx,
            dropped: Arc::clone(&dropped),
            filter,
            file: None,
        },
        TraceDrain {
            rx,
            dropped,
            seen_dropped: 0,
        },
    )
}

/// A background thread writing entries to an lz4-framed file
/// (DEBUGGER.md §2: "background writer with lz4 framing").
///
/// Background because the alternative is writing from the core thread,
/// and a disk stall there is a dropped frame — the same reason the
/// transport above exists.
pub struct TraceFileWriter {
    handle: Option<JoinHandle<Result<u64, String>>>,
    path: PathBuf,
}

impl TraceFileWriter {
    /// Spawn the writer and return it plus the sender to hand to a
    /// [`TraceProducer`].
    ///
    /// # Errors
    /// Returns the OS error text if the file cannot be created — reported
    /// rather than panicked, because "the user picked a read-only
    /// directory" is a normal thing to do.
    pub fn spawn(path: &Path) -> Result<(Self, mpsc::Sender<TraceEntry>), String> {
        let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let (tx, rx) = mpsc::channel::<TraceEntry>();
        let handle = std::thread::Builder::new()
            .name("rf-trace-writer".to_string())
            .spawn(move || write_loop(file, &rx))
            .map_err(|e| format!("could not spawn trace writer: {e}"))?;
        Ok((
            Self {
                handle: Some(handle),
                path: path.to_path_buf(),
            },
            tx,
        ))
    }

    /// The file being written.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Stop the writer and return how many entries it wrote.
    ///
    /// The caller must have dropped the [`mpsc::Sender`] first (in
    /// practice: dropped the `TraceProducer`), or this blocks until it is
    /// — the writer loop ends when the channel disconnects.
    ///
    /// # Errors
    /// Returns the writer thread's own error, or a message if it panicked.
    pub fn finish(mut self) -> Result<u64, String> {
        match self.handle.take() {
            Some(h) => h
                .join()
                .unwrap_or_else(|_| Err("trace writer panicked".to_string())),
            None => Ok(0),
        }
    }
}

/// Write until the channel disconnects, then finish the lz4 frame.
///
/// **`finish()` is not optional and is the whole reason this is a named
/// function rather than a closure.** `lz4_flex`'s `FrameEncoder` buffers,
/// and a frame that is never finished is a truncated file that decoders
/// reject — so a writer that merely dropped the encoder on shutdown would
/// produce a file the user could not open, having reported success.
fn write_loop(file: std::fs::File, rx: &mpsc::Receiver<TraceEntry>) -> Result<u64, String> {
    let mut encoder = lz4_flex::frame::FrameEncoder::new(std::io::BufWriter::new(file));
    let mut written = 0u64;
    while let Ok(entry) = rx.recv() {
        writeln!(
            encoder,
            "{:<12} {:>10} {:04X}  {}",
            entry.kind.label(),
            entry.cycle,
            entry.addr,
            entry.text
        )
        .map_err(|e| format!("trace write failed: {e}"))?;
        written += 1;
    }
    let mut inner = encoder
        .finish()
        .map_err(|e| format!("trace lz4 frame could not be finished: {e}"))?;
    inner
        .flush()
        .map_err(|e| format!("trace file could not be flushed: {e}"))?;
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rf_debugger::trace::TraceKind;

    fn entry(i: u16) -> TraceEntry {
        TraceEntry {
            kind: TraceKind::Cpu,
            cycle: u64::from(i),
            addr: i,
            text: format!("line {i}"),
        }
    }

    /// **The truncation trap, run for real.** A flag no code path sets is
    /// indistinguishable from one that is broken, so this overruns the
    /// transport deliberately.
    ///
    /// The second assertion is the one with teeth: a transport that
    /// silently dropped *everything* once full would also set the flag, so
    /// this pins that the entries which DID make it are the oldest ones
    /// and that the count of survivors plus drops equals what was pushed.
    /// Nothing may vanish unaccounted for.
    #[test]
    fn overrunning_the_transport_flags_truncation_and_accounts_for_every_entry() {
        let (mut tx, mut rx) = channel(TraceFilter::default());
        let pushed = TRANSPORT_CAPACITY + 5_000;
        for i in 0..pushed {
            tx.push(entry(u16::try_from(i % 65_536).unwrap()));
        }

        let mut sb = TraceScrollback::new(SCROLLBACK_CAPACITY);
        let moved = rx.drain_into(&mut sb);
        let stats = sb.stats();

        assert!(
            stats.truncated(),
            "pushing {pushed} entries into a {TRANSPORT_CAPACITY}-slot transport must flag \
             truncation"
        );
        assert_eq!(
            moved as u64 + stats.dropped_in_transport,
            pushed as u64,
            "every pushed entry must be either delivered or counted as dropped"
        );
        assert!(
            moved > 0,
            "a transport that delivered nothing at all would also set the flag — the flag alone \
             is not evidence the transport works"
        );
        // Drop-NEWEST at the transport: the entries that survived are the
        // ones pushed first.
        assert_eq!(
            sb.oldest().unwrap().addr,
            0,
            "the transport drops the newest, so the first entry pushed must have survived"
        );
    }

    /// Draining repeatedly must not multiply the reported loss — the
    /// counter is a running total and the scrollback accumulates, so
    /// handing over the total each time would report loss proportional to
    /// the repaint rate.
    #[test]
    fn transport_drops_are_reported_once_not_once_per_drain() {
        let (mut tx, mut rx) = channel(TraceFilter::default());
        for i in 0..(TRANSPORT_CAPACITY + 10) {
            tx.push(entry(u16::try_from(i % 65_536).unwrap()));
        }
        let mut sb = TraceScrollback::new(SCROLLBACK_CAPACITY);
        rx.drain_into(&mut sb);
        let after_first = sb.stats().dropped_in_transport;
        assert_eq!(after_first, 10);
        for _ in 0..5 {
            rx.drain_into(&mut sb);
        }
        assert_eq!(
            sb.stats().dropped_in_transport,
            after_first,
            "five idle drains must not inflate the drop count"
        );
    }

    /// The capture-side filter is what makes a filtered trace cheaper
    /// rather than merely tidier: a rejected entry must never reach the
    /// transport at all.
    #[test]
    fn the_capture_filter_keeps_rejected_entries_out_of_the_transport() {
        let filter = TraceFilter {
            kinds: vec![TraceKind::Apu],
            ..TraceFilter::default()
        };
        let (mut tx, mut rx) = channel(filter);
        for i in 0..100 {
            tx.push(entry(i)); // all TraceKind::Cpu
        }
        let mut sb = TraceScrollback::new(SCROLLBACK_CAPACITY);
        assert_eq!(rx.drain_into(&mut sb), 0);
        assert!(
            !sb.stats().truncated(),
            "filtered-out entries are not losses and must not raise the truncation flag"
        );
    }

    /// **The file writer, checked by decompressing what it wrote.**
    /// Asserting the file merely exists and is non-empty would pass
    /// against an unfinished lz4 frame, which is exactly the failure mode
    /// `write_loop`'s `finish()` exists to prevent — and a truncated
    /// frame is a file the user cannot open, after being told the trace
    /// was saved.
    #[test]
    fn the_file_writer_produces_a_frame_that_decompresses_to_every_entry() {
        let dir = std::env::temp_dir().join(format!("rf_trace_writer_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("trace.lz4");

        let (writer, file_tx) = TraceFileWriter::spawn(&path).expect("writer spawns");
        let (mut tx, _rx) = channel(TraceFilter::default());
        tx.file = Some(file_tx);
        for i in 0..1_000u16 {
            tx.push(entry(i));
        }
        drop(tx); // closes the file channel, ending the writer loop
        let written = writer.finish().expect("writer finishes cleanly");
        assert_eq!(written, 1_000);

        let compressed = std::fs::read(&path).expect("trace file exists");
        assert!(!compressed.is_empty());
        let mut text = String::new();
        std::io::Read::read_to_string(
            &mut lz4_flex::frame::FrameDecoder::new(std::io::Cursor::new(&compressed)),
            &mut text,
        )
        .expect("the lz4 frame must decode — an unfinished frame fails here");

        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 1_000, "every entry must be in the file");
        assert!(lines[0].contains("line 0"), "{:?}", lines[0]);
        assert!(lines[999].contains("line 999"), "{:?}", lines[999]);
        assert!(
            compressed.len() < text.len(),
            "lz4 framing must actually compress this ({} bytes vs {} raw)",
            compressed.len(),
            text.len()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
