//! Per-chip trace rings and the trace viewer's data model (ticket W4-10a;
//! `docs/design/DEBUGGER.md` §2-3, stories E3-S1/E3-S2).
//!
//! UI-free, like every other provider in this crate: the egui panel lives
//! in the frontend, the rtrb transport that carries entries off the core
//! thread lives in the frontend too (this crate may not depend on
//! `rf-nes`, and the transport is wired to a running machine). What lives
//! here is the part worth unit-testing — the scrollback, its filters, and
//! the accounting that says how much was lost.
//!
//! ## "Overflow drops oldest" is true of the scrollback, not the transport
//!
//! DEBUGGER.md §2 says the rings are "fixed-capacity SPSC (rtrb) drained
//! by the UI/trace-dump worker — never blocks the core thread; overflow
//! drops oldest and sets a truncation flag visible in the UI."
//!
//! **Those two sentences cannot both be satisfied by one rtrb queue, and
//! this is the seam where that is resolved.** Verified against the
//! vendored `rtrb 0.3` source: `Producer` exposes `push`, `is_full` and
//! `slots` — `pop` belongs to `Consumer`. The core thread physically
//! cannot discard the oldest entry, so a full transport leaves it exactly
//! one option that does not block: refuse the new entry.
//!
//! So the design is two rings, and each half of §2's sentence lands on
//! the one that can honour it:
//!
//! * **Transport** (frontend, rtrb): bounded, drops the **newest** on
//!   overflow, counts what it dropped. Never blocks the core thread,
//!   which is §2's hard requirement.
//! * **Scrollback** ([`TraceScrollback`], here): bounded, drops the
//!   **oldest** as it drains, which is what a user scrolling backwards
//!   expects and what "drops oldest" means for the ring they can see.
//!
//! Both losses are reported through one [`TraceStats`], because a viewer
//! that showed only one of them would tell the user their trace is
//! complete when it is not.

use std::collections::VecDeque;

/// Which chip produced an entry (DEBUGGER.md §2: "CPU, PPU register
/// writes, APU, DMA/HDMA, mapper").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TraceKind {
    Cpu,
    PpuWrite,
    Apu,
    Dma,
    Mapper,
}

impl TraceKind {
    /// Every kind, in the order the filter UI lists them.
    pub const ALL: [TraceKind; 5] = [
        TraceKind::Cpu,
        TraceKind::PpuWrite,
        TraceKind::Apu,
        TraceKind::Dma,
        TraceKind::Mapper,
    ];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            TraceKind::Cpu => "CPU",
            TraceKind::PpuWrite => "PPU writes",
            TraceKind::Apu => "APU",
            TraceKind::Dma => "DMA",
            TraceKind::Mapper => "Mapper",
        }
    }
}

/// One traced record.
///
/// `text` is pre-formatted at capture time rather than at display time,
/// and for the CPU that formatting is `rf_nes::trace::format_trace_line`
/// — the same function the nestest golden-trace gate compares
/// byte-for-byte against `nestest.log`. DEBUGGER.md §2 requires exactly
/// that ("the golden-trace diff in CI and the on-screen trace viewer
/// share one formatter"), and it is why this struct carries a `String`
/// instead of a decoded instruction: a second formatter would be a second
/// thing to keep correct, and only one of the two would be under test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceEntry {
    pub kind: TraceKind,
    /// Master cycle at capture, for correlating across chips.
    pub cycle: u64,
    /// Program counter (CPU) or the accessed address (everything else) —
    /// what [`TraceFilter::pc_range`] matches on.
    pub addr: u16,
    pub text: String,
}

/// What the viewer must tell the user about completeness.
///
/// Two separate counters, deliberately: they mean different things and a
/// single "dropped" number would hide which ring is too small. A large
/// `dropped_in_transport` means the core outran the drain (raise the
/// transport capacity, or filter at capture); a large
/// `dropped_from_scrollback` is normal and merely says the scrollback has
/// scrolled past.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TraceStats {
    /// Entries the core thread could not hand over because the transport
    /// was full. **This is the truncation flag's evidence** — it means
    /// the trace has holes in the middle, not just a missing tail.
    pub dropped_in_transport: u64,
    /// Entries evicted from the scrollback's tail to make room.
    pub dropped_from_scrollback: u64,
    /// Entries currently held.
    pub held: usize,
}

impl TraceStats {
    /// DEBUGGER.md §2's "truncation flag visible in the UI".
    ///
    /// True only for transport loss. Scrollback eviction is not
    /// truncation — it is what a bounded scrollback does every second of
    /// normal use, and flagging it would train the user to ignore the
    /// flag that matters.
    #[must_use]
    pub fn truncated(&self) -> bool {
        self.dropped_in_transport > 0
    }
}

/// The ring the user scrolls: newest at the back, oldest evicted first.
#[derive(Debug)]
pub struct TraceScrollback {
    entries: VecDeque<TraceEntry>,
    capacity: usize,
    stats: TraceStats,
}

impl TraceScrollback {
    /// # Panics
    /// Panics if `capacity` is 0 — a zero-capacity scrollback would
    /// silently discard everything and report a full, healthy-looking
    /// `dropped_from_scrollback` while showing nothing.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "TraceScrollback capacity must be non-zero");
        Self {
            entries: VecDeque::with_capacity(capacity.min(4096)),
            capacity,
            stats: TraceStats::default(),
        }
    }

    /// Append one entry, evicting the oldest if full.
    pub fn push(&mut self, entry: TraceEntry) {
        if self.entries.len() == self.capacity {
            self.entries.pop_front();
            self.stats.dropped_from_scrollback += 1;
        }
        self.entries.push_back(entry);
        self.stats.held = self.entries.len();
    }

    /// Record that the transport could not hand over `n` entries.
    pub fn note_transport_drops(&mut self, n: u64) {
        self.stats.dropped_in_transport += n;
    }

    #[must_use]
    pub fn stats(&self) -> TraceStats {
        self.stats
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Newest entry, or `None` when empty.
    #[must_use]
    pub fn newest(&self) -> Option<&TraceEntry> {
        self.entries.back()
    }

    #[must_use]
    pub fn oldest(&self) -> Option<&TraceEntry> {
        self.entries.front()
    }

    /// Clear entries **and** the loss counters: after an explicit clear
    /// the trace genuinely has no holes, and leaving a stale truncation
    /// flag set would make the viewer lie in the safe direction forever.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.stats = TraceStats::default();
    }

    /// Entries passing `filter`, oldest first.
    pub fn filtered<'a>(
        &'a self,
        filter: &'a TraceFilter,
    ) -> impl Iterator<Item = &'a TraceEntry> + 'a {
        self.entries.iter().filter(move |e| filter.matches(e))
    }
}

/// The viewer's filters (DEBUGGER.md §2: "the UI warns and suggests
/// filtered traces (PC ranges, event types)").
#[derive(Debug, Clone)]
pub struct TraceFilter {
    /// Kinds to show. Empty means **nothing**, not everything — see
    /// [`TraceFilter::matches`].
    pub kinds: Vec<TraceKind>,
    /// Inclusive `addr` range, or `None` for no range restriction.
    pub pc_range: Option<(u16, u16)>,
    /// Case-insensitive substring of `text`; empty matches everything.
    pub contains: String,
}

impl Default for TraceFilter {
    fn default() -> Self {
        Self {
            kinds: TraceKind::ALL.to_vec(),
            pc_range: None,
            contains: String::new(),
        }
    }
}

impl TraceFilter {
    /// Whether `entry` should be shown.
    ///
    /// An empty `kinds` matches nothing rather than everything. That is
    /// the opposite of the usual "empty filter means unfiltered"
    /// convention and it is chosen on purpose: `kinds` is driven by a row
    /// of checkboxes, so empty means the user unticked all of them, and
    /// answering that with the full trace would be the UI ignoring a
    /// direct instruction.
    #[must_use]
    pub fn matches(&self, entry: &TraceEntry) -> bool {
        if !self.kinds.contains(&entry.kind) {
            return false;
        }
        if let Some((lo, hi)) = self.pc_range {
            // Callers may hand the range over in either order; a range
            // typed backwards into two text boxes should filter, not
            // silently match nothing.
            let (lo, hi) = if lo <= hi { (lo, hi) } else { (hi, lo) };
            if entry.addr < lo || entry.addr > hi {
                return false;
            }
        }
        if !self.contains.is_empty()
            && !entry
                .text
                .to_ascii_lowercase()
                .contains(&self.contains.to_ascii_lowercase())
        {
            return false;
        }
        true
    }
}

/// Advice for the "trace to file" control (DEBUGGER.md §2: "a 60s
/// full-speed CPU trace is ~GBs raw — the UI warns and suggests filtered
/// traces").
///
/// The estimate is deliberately about the **raw** bytes rather than the
/// lz4 output: compression ratio depends on the trace's content, and a
/// warning that quoted a guessed post-compression figure would be a made-
/// up number in the one place the user is deciding whether to fill their
/// disk. The wording says raw, and says lz4 will help.
#[must_use]
pub fn size_warning(
    seconds: u32,
    bytes_per_entry: usize,
    entries_per_second: u64,
) -> Option<String> {
    let raw = u64::from(seconds) * entries_per_second * bytes_per_entry as u64;
    // One gibibyte: the point where DEBUGGER.md's own "~GBs" starts.
    if raw < 1 << 30 {
        return None;
    }
    #[allow(clippy::cast_precision_loss)]
    let gib = raw as f64 / (1u64 << 30) as f64;
    Some(format!(
        "About {gib:.1} GiB raw for {seconds}s at full speed (lz4 framing shrinks the file, by \
         how much depends on the trace). Narrow it with a PC range or by tracing fewer chips."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: TraceKind, addr: u16, text: &str) -> TraceEntry {
        TraceEntry {
            kind,
            cycle: u64::from(addr),
            addr,
            text: text.to_string(),
        }
    }

    /// **The drop-oldest half of §2, and the assertion that makes it more
    /// than a length check.** A scrollback that dropped the *newest* on
    /// overflow would keep exactly the same number of entries, so
    /// asserting `len()` alone would pass against the opposite of the
    /// required behaviour. The identities of both ends are what pin it.
    #[test]
    fn scrollback_drops_oldest_and_keeps_the_newest() {
        let mut sb = TraceScrollback::new(3);
        for i in 0..5u16 {
            sb.push(entry(TraceKind::Cpu, i, &format!("line {i}")));
        }
        assert_eq!(sb.len(), 3);
        assert_eq!(sb.oldest().unwrap().addr, 2, "entries 0 and 1 must be gone");
        assert_eq!(
            sb.newest().unwrap().addr,
            4,
            "the newest entry must survive — dropping it would be the opposite of drop-oldest"
        );
        assert_eq!(sb.stats().dropped_from_scrollback, 2);
        assert!(
            !sb.stats().truncated(),
            "scrollback eviction is not truncation: flagging it would train the user to ignore \
             the flag that means the trace has holes"
        );
    }

    /// The truncation flag is about TRANSPORT loss, and it must be
    /// reachable — a flag no code path can set is indistinguishable from
    /// one that is broken.
    #[test]
    fn transport_drops_set_the_truncation_flag() {
        let mut sb = TraceScrollback::new(8);
        sb.push(entry(TraceKind::Cpu, 0, "a"));
        assert!(!sb.stats().truncated());
        sb.note_transport_drops(17);
        assert!(sb.stats().truncated());
        assert_eq!(sb.stats().dropped_in_transport, 17);
        assert_eq!(
            sb.stats().dropped_from_scrollback,
            0,
            "the two counters must stay distinct — they tell the user to do different things"
        );
    }

    #[test]
    fn clear_resets_the_flag_so_the_viewer_stops_lying_about_old_holes() {
        let mut sb = TraceScrollback::new(4);
        sb.note_transport_drops(3);
        sb.push(entry(TraceKind::Cpu, 1, "a"));
        assert!(sb.stats().truncated());
        sb.clear();
        assert!(!sb.stats().truncated());
        assert_eq!(sb.stats(), TraceStats::default());
    }

    #[test]
    fn filters_select_by_kind_range_and_substring() {
        let mut sb = TraceScrollback::new(16);
        sb.push(entry(TraceKind::Cpu, 0xC000, "LDA #$00"));
        sb.push(entry(TraceKind::Cpu, 0x8000, "STA $2001"));
        sb.push(entry(TraceKind::PpuWrite, 0x2001, "$2001 <- 1E"));
        sb.push(entry(TraceKind::Apu, 0x4000, "$4000 <- 9F"));

        let kinds_only = TraceFilter {
            kinds: vec![TraceKind::Cpu],
            ..TraceFilter::default()
        };
        assert_eq!(sb.filtered(&kinds_only).count(), 2);

        let ranged = TraceFilter {
            pc_range: Some((0xC000, 0xFFFF)),
            ..TraceFilter::default()
        };
        let hit: Vec<_> = sb.filtered(&ranged).collect();
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].addr, 0xC000);

        // Typed backwards: same result, because two text boxes are two
        // chances to enter them in the wrong order.
        let backwards = TraceFilter {
            pc_range: Some((0xFFFF, 0xC000)),
            ..TraceFilter::default()
        };
        assert_eq!(sb.filtered(&backwards).count(), 1);

        let text = TraceFilter {
            contains: "sta".to_string(), // lowercase against uppercase text
            ..TraceFilter::default()
        };
        assert_eq!(sb.filtered(&text).count(), 1);

        // Combined: the filters must AND together, not OR.
        let both = TraceFilter {
            kinds: vec![TraceKind::Cpu],
            pc_range: Some((0x0000, 0x9000)),
            contains: "sta".to_string(),
        };
        assert_eq!(sb.filtered(&both).count(), 1);
        let contradictory = TraceFilter {
            kinds: vec![TraceKind::Apu],
            pc_range: Some((0x0000, 0x9000)),
            contains: "sta".to_string(),
        };
        assert_eq!(
            sb.filtered(&contradictory).count(),
            0,
            "filters must AND: an OR would show the APU entry here"
        );
    }

    /// Unticking every kind means "show nothing", not "show everything" —
    /// see [`TraceFilter::matches`].
    #[test]
    fn an_empty_kind_set_shows_nothing() {
        let mut sb = TraceScrollback::new(4);
        sb.push(entry(TraceKind::Cpu, 0, "x"));
        let none = TraceFilter {
            kinds: Vec::new(),
            ..TraceFilter::default()
        };
        assert_eq!(sb.filtered(&none).count(), 0);
    }

    /// The warning must fire where DEBUGGER.md says the problem starts
    /// ("~GBs") and stay quiet for a short trace, or it is either noise or
    /// absent exactly when it is needed.
    #[test]
    fn the_size_warning_fires_only_for_traces_that_are_actually_huge() {
        // A 60 s CPU trace at NTSC speed: ~1.79 M instructions/s is the
        // 2A03's cycle rate divided by an average instruction length, so
        // ~500 k entries/s is a conservative floor, and a nestest-format
        // line is ~73 bytes.
        let sixty = size_warning(60, 73, 500_000).expect("60 s must warn");
        assert!(sixty.contains("GiB"), "{sixty}");
        assert!(
            sixty.contains("PC range"),
            "the warning must say what to DO about it: {sixty}"
        );
        assert!(
            size_warning(1, 73, 500_000).is_none(),
            "a one-second trace is ~35 MB and must not warn"
        );
    }
}
