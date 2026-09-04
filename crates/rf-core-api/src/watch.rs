//! Memory watchpoints — the shared shape a debugger installs and a core
//! evaluates (ticket W13-02e; `docs/design/DEBUGGER.md` §1's "memory
//! read/write/access (CPU and PPU address spaces separately),
//! value-conditional (`addr==X && val&mask`)").
//!
//! ## Why this lives in the contract crate
//!
//! [`crate::CoreEvent::MemWatch`] and [`crate::EventMask::MEM_WATCH`] have
//! been in this crate since W4-00/W4-01 **with no producer** — the channel
//! was designed for exactly this and then had nothing to carry. The
//! matching rule has to be identical in every core or a watchpoint would
//! mean different things on NES and SNES, so it is written once here and
//! called from each core's bus rather than reimplemented per core.
//!
//! ## Why the core evaluates, rather than reporting every access
//!
//! A debugger cannot filter what it never sees, so the naive shape is "emit
//! an event per bus access and let the debugger decide". A NES frame is
//! ~30,000 CPU cycles plus every PPU fetch; that channel would move
//! millions of events a second to discard nearly all of them. So the core
//! holds the (small, plain-data) table and emits only on a hit — which is
//! also what DEBUGGER.md §1 already specifies: "a compiled breakpoint
//! table — zero cost when the table is empty (checked by a single bool
//! before the match)". [`WatchTable::is_armed`] is that bool.
//!
//! ## Observation only
//!
//! Nothing here mutates a core. A watch produces an event on a channel that
//! is already gated by [`crate::EventMask`], and project law 4's
//! determinism invariant is what proves it: the `mode_invariant_*` suites
//! compare state hashes with the enhancement side subscribed and
//! unsubscribed, and a watchpoint that perturbed the machine would show up
//! there as a divergence.

/// Which address space a watch is expressed in.
///
/// Separate spaces rather than one flat range, because `$2000` means two
/// unrelated things on a NES: PPUCTRL on the CPU bus and the first
/// nametable byte on the PPU bus. A watch that could not say which it
/// meant would be ambiguous exactly where a ROM hacker is most precise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchSpace {
    /// The CPU bus (`$0000-$FFFF` on NES; the full 24-bit map on SNES).
    Cpu,
    /// The video-memory bus (`$0000-$3FFF` on NES).
    Ppu,
}

/// Which kind of access trips a watch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchAccess {
    Read,
    Write,
    /// Either — DEBUGGER.md §1's "access".
    Any,
}

impl WatchAccess {
    /// Does an access of `kind` (never [`WatchAccess::Any`] — that is a
    /// watch's setting, not an event) trip a watch armed for `self`?
    #[must_use]
    pub const fn matches(self, kind: WatchAccess) -> bool {
        matches!(
            (self, kind),
            (WatchAccess::Any, _)
                | (WatchAccess::Read, WatchAccess::Read)
                | (WatchAccess::Write, WatchAccess::Write)
        )
    }
}

/// One armed watchpoint.
///
/// `Copy` and free of allocation on purpose: a core stores these inline in
/// [`WatchTable`], which is itself part of [`crate::CoreConfig`], and that
/// type is `Copy` because consumers pass it around by value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemWatch {
    /// The id reported in [`crate::CoreEvent::MemWatch`]. Assigned by
    /// whoever set the watch; the core never invents one.
    pub id: u32,
    pub space: WatchSpace,
    /// First address covered, inclusive.
    pub start: u32,
    /// Last address covered, **inclusive** — so a one-byte watch has
    /// `start == end` rather than needing a length nobody can get wrong in
    /// only one direction.
    pub end: u32,
    pub access: WatchAccess,
    /// Value condition: the watch trips only when
    /// `value & value_mask == value_equals` (DEBUGGER.md §1's
    /// `addr==X && val&mask`). A `value_mask` of `0` means "any value",
    /// because `v & 0 == 0` and [`MemWatch::unconditional`] sets
    /// `value_equals` to `0` to match.
    pub value_mask: u8,
    pub value_equals: u8,
}

impl MemWatch {
    /// A watch on one address, any value.
    #[must_use]
    pub const fn unconditional(id: u32, space: WatchSpace, addr: u32, access: WatchAccess) -> Self {
        MemWatch {
            id,
            space,
            start: addr,
            end: addr,
            access,
            value_mask: 0,
            value_equals: 0,
        }
    }

    /// Does this watch trip on `access` of `value` at `addr` in `space`?
    #[must_use]
    pub const fn trips(self, space: WatchSpace, access: WatchAccess, addr: u32, value: u8) -> bool {
        matches!(
            (self.space, space),
            (WatchSpace::Cpu, WatchSpace::Cpu) | (WatchSpace::Ppu, WatchSpace::Ppu)
        ) && addr >= self.start
            && addr <= self.end
            && self.access.matches(access)
            && (value & self.value_mask) == self.value_equals
    }
}

/// How many watchpoints a core holds at once.
///
/// Fixed and small so [`crate::CoreConfig`] stays `Copy` and the hot-path
/// check needs no allocation and no indirection. Eight is not a
/// placeholder: it is the same order as the hardware debuggers this
/// feature imitates (a 65C816 ICE has a handful of comparators), it keeps
/// the linear scan on a hit shorter than a cache line, and a UI that lets
/// someone arm hundreds of watchpoints is a UI that has stopped being a
/// debugger. [`WatchTable::set`] reports the overflow rather than dropping
/// it silently.
pub const MAX_WATCHES: usize = 8;

/// The core's installed watchpoints.
///
/// [`WatchTable::is_armed`] is DEBUGGER.md §1's "single bool before the
/// match": a core checks it first, and an empty table costs one predictable
/// branch per access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WatchTable {
    entries: [Option<MemWatch>; MAX_WATCHES],
    armed: bool,
}

impl WatchTable {
    #[must_use]
    pub const fn new() -> Self {
        WatchTable {
            entries: [None; MAX_WATCHES],
            armed: false,
        }
    }

    /// Replace the whole table.
    ///
    /// Returns how many watches were **refused** for want of room, so a
    /// caller can say so. Silently keeping the first eight would leave a
    /// user watching an address they believe is armed and is not — the
    /// worst failure a debugger has, because it looks like the game not
    /// touching the address.
    pub fn set(&mut self, watches: &[MemWatch]) -> usize {
        self.entries = [None; MAX_WATCHES];
        for (slot, watch) in self.entries.iter_mut().zip(watches.iter()) {
            *slot = Some(*watch);
        }
        self.armed = !watches.is_empty();
        watches.len().saturating_sub(MAX_WATCHES)
    }

    /// Remove every watch.
    pub fn clear(&mut self) {
        self.entries = [None; MAX_WATCHES];
        self.armed = false;
    }

    /// Whether anything is armed — the bool a core checks before it
    /// bothers to look at an access at all.
    #[must_use]
    pub const fn is_armed(&self) -> bool {
        self.armed
    }

    /// The id of the first watch this access trips, or `None`.
    ///
    /// First match wins: two watches covering one address is the user's
    /// choice, and reporting both would double every event on a range they
    /// deliberately overlapped.
    #[must_use]
    pub fn hit(&self, space: WatchSpace, access: WatchAccess, addr: u32, value: u8) -> Option<u32> {
        if !self.armed {
            return None;
        }
        self.entries
            .iter()
            .flatten()
            .find(|w| w.trips(space, access, addr, value))
            .map(|w| w.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_table_is_unarmed_and_never_hits() {
        let table = WatchTable::new();
        assert!(!table.is_armed());
        assert_eq!(
            table.hit(WatchSpace::Cpu, WatchAccess::Read, 0x0086, 0),
            None
        );
    }

    #[test]
    fn a_watch_distinguishes_space_access_range_and_value() {
        let mut table = WatchTable::new();
        table.set(&[MemWatch {
            id: 7,
            space: WatchSpace::Cpu,
            start: 0x0086,
            end: 0x0087,
            access: WatchAccess::Write,
            value_mask: 0xF0,
            value_equals: 0x10,
        }]);
        assert!(table.is_armed());

        let hit = |space, access, addr, value| table.hit(space, access, addr, value);
        assert_eq!(
            hit(WatchSpace::Cpu, WatchAccess::Write, 0x0086, 0x1F),
            Some(7)
        );
        // The range is inclusive at both ends.
        assert_eq!(
            hit(WatchSpace::Cpu, WatchAccess::Write, 0x0087, 0x1F),
            Some(7)
        );
        assert_eq!(hit(WatchSpace::Cpu, WatchAccess::Write, 0x0088, 0x1F), None);
        // Wrong space: $2000 on the PPU bus is not $2000 on the CPU bus.
        assert_eq!(hit(WatchSpace::Ppu, WatchAccess::Write, 0x0086, 0x1F), None);
        // Wrong access.
        assert_eq!(hit(WatchSpace::Cpu, WatchAccess::Read, 0x0086, 0x1F), None);
        // Value condition: high nibble must be 1.
        assert_eq!(hit(WatchSpace::Cpu, WatchAccess::Write, 0x0086, 0x2F), None);
    }

    #[test]
    fn an_any_access_watch_trips_on_both_and_a_zero_mask_means_any_value() {
        let mut table = WatchTable::new();
        table.set(&[MemWatch::unconditional(
            1,
            WatchSpace::Ppu,
            0x2000,
            WatchAccess::Any,
        )]);
        assert_eq!(
            table.hit(WatchSpace::Ppu, WatchAccess::Read, 0x2000, 0),
            Some(1)
        );
        assert_eq!(
            table.hit(WatchSpace::Ppu, WatchAccess::Write, 0x2000, 0xFF),
            Some(1)
        );
    }

    /// Overflow is REPORTED. A watch someone believes is armed and is not
    /// looks exactly like the game never touching the address.
    #[test]
    fn setting_more_than_the_table_holds_reports_the_refusals() {
        let mut table = WatchTable::new();
        let many: Vec<MemWatch> = (0..MAX_WATCHES as u32 + 3)
            .map(|i| MemWatch::unconditional(i, WatchSpace::Cpu, i, WatchAccess::Any))
            .collect();
        assert_eq!(table.set(&many), 3);
        assert_eq!(table.hit(WatchSpace::Cpu, WatchAccess::Read, 0, 0), Some(0));
        assert_eq!(
            table.hit(WatchSpace::Cpu, WatchAccess::Read, MAX_WATCHES as u32, 0),
            None,
            "the ninth watch was refused, not silently kept"
        );
    }

    #[test]
    fn clearing_disarms() {
        let mut table = WatchTable::new();
        table.set(&[MemWatch::unconditional(
            1,
            WatchSpace::Cpu,
            0x10,
            WatchAccess::Any,
        )]);
        table.clear();
        assert!(!table.is_armed());
        assert_eq!(table.hit(WatchSpace::Cpu, WatchAccess::Read, 0x10, 0), None);
    }
}
