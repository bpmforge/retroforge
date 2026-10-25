//! Code-coverage recording — the shared shape a core fills in while armed
//! and a later tool (the code atlas, `docs/design/REBUILD_LAB.md` §5-7)
//! reads (ticket W36-01).
//!
//! ## Why this lives in the contract crate
//!
//! Same reason as [`crate::WatchTable`]: the log is plain data with no
//! behaviour a core could get wrong, and the debugger, the atlas builder
//! and any future core must agree on it without depending on a concrete
//! core. A core records into it; everything else only reads and
//! [merges](CoverageLog::merge).
//!
//! ## Pay for use, observation only
//!
//! A core holds an `Option` of this log and tests it once per instruction,
//! so a core that is not recording pays one predictable branch. While
//! recording a core may only *peek* (no side-effecting bus reads) and
//! mutates nothing but the log, so the `mode_invariant_*` determinism
//! suites hold with it armed. The log is **session observation, like a
//! watchpoint table: it is not part of a savestate** and a state load
//! neither restores nor clears it.
//!
//! ## What is recorded
//!
//! * `prg_flags` — one byte per PRG ROM byte: [`OPCODE`] when that byte was
//!   the first byte of an executed instruction, [`OPERAND`] when it was an
//!   operand byte of one. Indexing is by ROM offset, not CPU address, so
//!   one ROM byte seen through two bank windows is still one byte.
//! * `ram_exec` — the same flags per **CPU address** for instructions
//!   fetched from anywhere that does not resolve to a PRG ROM offset (work
//!   RAM, PRG RAM, a mapper that does not report offsets). It is a flat
//!   64 KiB rather than a sparse set: RAM code is rare but, when present,
//!   dense (a copied routine), so a sparse map would cost more per hit
//!   than the 64 KiB costs once, and it makes [`CoverageLog::merge`] a
//!   plain byte-wise OR.
//! * `indirect` — control transfers whose target is not in the opcode
//!   bytes: `JMP (ind)`, `RTS`, `RTI`, `BRK`. These are exactly the edges
//!   static disassembly cannot follow.
//! * `interrupts` — Reset / NMI / IRQ entries and where they landed.
//! * `windows` — snapshots of the whole CPU-window to PRG-ROM mapping, taken
//!   whenever an executing instruction shows the mapping changed (ticket
//!   W36-02). A static `JSR`/`JMP` target is only a CPU address; which ROM
//!   bytes it names depends on the bank state in force, and the atlas can
//!   only recover that from mappings the recording actually saw. A snapshot
//!   is one *simultaneous* state, so "these two pages were mapped together"
//!   is a fact in the log, not an inference.
//!
//! ## Extending to the 65816
//!
//! [`CodeAddr`] is `{ cpu, rom }` for a 16-bit CPU. A later SNES ticket
//! adds the program bank (`pbr: u8`) and the M/X/E width state that
//! decides operand length; both belong in `CodeAddr` (as new fields, with
//! `rom` widening to the cart offset), and the width bits can ride in the
//! spare high bits of the `prg_flags` byte ([`OPCODE`]/[`OPERAND`] use
//! only bits 0-1). Nothing here assumes 16-bit addresses beyond that one
//! struct.

use std::collections::BTreeSet;

/// `prg_flags`/`ram_exec` bit: first byte of an executed instruction.
pub const OPCODE: u8 = 0b01;
/// `prg_flags`/`ram_exec` bit: operand byte of an executed instruction.
pub const OPERAND: u8 = 0b10;

/// First CPU address covered by [`WindowMap`] (PRG ROM starts at `$8000` on
/// every board the NES core models).
pub const WINDOW_BASE: u16 = 0x8000;
/// log2 of one [`WindowMap`] page. 4 KiB is finer than every PRG banking
/// granule the NES mappers use (8/16/32 KiB, nesdev "Mapper" pages), so a
/// page always lies inside one bank and the map is mapper-agnostic.
pub const WINDOW_PAGE_SHIFT: u32 = 12;
/// Pages in a [`WindowMap`]: the 32 KiB `$8000-$FFFF` in 4 KiB steps.
pub const WINDOW_PAGES: usize = 8;
/// [`WindowMap`] entry for a page that is not PRG ROM right now.
pub const NO_ROM: u32 = u32::MAX;
/// Hard cap on [`CoverageLog::windows`]. A game thrashing banks cannot grow
/// the log without bound; snapshots past the cap are dropped (the atlas then
/// falls back to the weaker "unique base" rule for the pages involved).
pub const MAX_WINDOW_MAPS: usize = 1 << 16;

/// Size of [`CoverageLog::ram_exec`]: one flag byte per 6502 CPU address.
pub const RAM_EXEC_LEN: usize = 0x1_0000;

/// A code location: its CPU address and, when the cartridge maps that
/// address to PRG ROM right now, the ROM offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CodeAddr {
    pub cpu: u16,
    /// PRG ROM byte offset, `None` for RAM or anything the mapper does not
    /// resolve.
    pub rom: Option<u32>,
}

/// Which instruction produced an [`IndirectEdge`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IndirectKind {
    /// `JMP (addr)`, opcode `$6C`.
    JmpIndirect,
    /// `RTS`, opcode `$60`.
    Rts,
    /// `RTI`, opcode `$40`.
    Rti,
    /// `BRK`, opcode `$00` (target is the vector actually taken, which an
    /// NMI arriving during the sequence can hijack — nesdev, "CPU
    /// interrupts").
    Brk,
}

/// One observed indirect control transfer: the instruction at `site`
/// went to `target`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IndirectEdge {
    pub site: CodeAddr,
    pub target: CodeAddr,
    pub kind: IndirectKind,
}

/// Which hardware event started an [`InterruptEntry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum InterruptKind {
    Reset,
    Nmi,
    Irq,
}

/// One observed interrupt/reset entry and the handler address it reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InterruptEntry {
    pub kind: InterruptKind,
    pub target: CodeAddr,
}

/// One simultaneous CPU-window mapping: for each 4 KiB page of
/// `$8000-$FFFF`, the PRG ROM offset of that page's first byte, or
/// [`NO_ROM`] (PRG RAM, an unmapped window, a mapper that does not report).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WindowMap(pub [u32; WINDOW_PAGES]);

impl WindowMap {
    /// A map with every page [`NO_ROM`].
    pub const EMPTY: Self = Self([NO_ROM; WINDOW_PAGES]);

    /// Page index (`0..8`) of CPU address `cpu`, `None` below `$8000`.
    #[must_use]
    pub fn page_of(cpu: u16) -> Option<usize> {
        cpu.checked_sub(WINDOW_BASE)
            .map(|d| usize::from(d >> WINDOW_PAGE_SHIFT))
    }
}

/// Everything a recording session observed. See the module doc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageLog {
    /// One flag byte per PRG ROM byte ([`OPCODE`] | [`OPERAND`]).
    pub prg_flags: Vec<u8>,
    /// Flags per CPU address for code fetched from non-ROM addresses;
    /// always [`RAM_EXEC_LEN`] long.
    pub ram_exec: Vec<u8>,
    /// Deduplicated, sorted by `Ord`.
    pub indirect: Vec<IndirectEdge>,
    /// Deduplicated, sorted by `Ord`.
    pub interrupts: Vec<InterruptEntry>,
    /// Deduplicated, sorted by `Ord`, at most [`MAX_WINDOW_MAPS`] long.
    pub windows: Vec<WindowMap>,
    /// Instructions executed while recording (interrupt entries are not
    /// instructions and are not counted).
    pub instructions_recorded: u64,
}

impl CoverageLog {
    /// An empty log for a cartridge with `prg_len` bytes of PRG ROM.
    #[must_use]
    pub fn new(prg_len: usize) -> Self {
        Self {
            prg_flags: vec![0; prg_len],
            ram_exec: vec![0; RAM_EXEC_LEN],
            indirect: Vec::new(),
            interrupts: Vec::new(),
            windows: Vec::new(),
            instructions_recorded: 0,
        }
    }

    /// OR `flag` into the PRG byte at `offset`; out-of-range is ignored
    /// (a mapper reporting a bad offset must not be able to panic a
    /// recording session).
    pub fn mark_prg(&mut self, offset: usize, flag: u8) {
        if let Some(b) = self.prg_flags.get_mut(offset) {
            *b |= flag;
        }
    }

    /// OR `flag` into the RAM-executed flag for CPU address `addr`.
    pub fn mark_ram(&mut self, addr: u16, flag: u8) {
        if let Some(b) = self.ram_exec.get_mut(usize::from(addr)) {
            *b |= flag;
        }
    }

    /// Insert an edge, keeping `indirect` sorted and duplicate-free.
    pub fn note_indirect(&mut self, edge: IndirectEdge) {
        if let Err(at) = self.indirect.binary_search(&edge) {
            self.indirect.insert(at, edge);
        }
    }

    /// Insert an interrupt entry, keeping `interrupts` sorted and
    /// duplicate-free.
    pub fn note_interrupt(&mut self, entry: InterruptEntry) {
        if let Err(at) = self.interrupts.binary_search(&entry) {
            self.interrupts.insert(at, entry);
        }
    }

    /// Insert a window snapshot, keeping `windows` sorted, duplicate-free
    /// and under [`MAX_WINDOW_MAPS`].
    pub fn note_windows(&mut self, map: WindowMap) {
        if let Err(at) = self.windows.binary_search(&map) {
            if self.windows.len() < MAX_WINDOW_MAPS {
                self.windows.insert(at, map);
            }
        }
    }

    /// Union `other` into `self` (flags OR'd, edge sets united, instruction
    /// counts added). Returns `false` and changes nothing when the two logs
    /// are for different PRG sizes — they describe different cartridges.
    pub fn merge(&mut self, other: &CoverageLog) -> bool {
        if self.prg_flags.len() != other.prg_flags.len()
            || self.ram_exec.len() != other.ram_exec.len()
        {
            return false;
        }
        for (a, b) in self.prg_flags.iter_mut().zip(&other.prg_flags) {
            *a |= *b;
        }
        for (a, b) in self.ram_exec.iter_mut().zip(&other.ram_exec) {
            *a |= *b;
        }
        let edges: BTreeSet<_> = self
            .indirect
            .iter()
            .chain(&other.indirect)
            .copied()
            .collect();
        self.indirect = edges.into_iter().collect();
        let ints: BTreeSet<_> = self
            .interrupts
            .iter()
            .chain(&other.interrupts)
            .copied()
            .collect();
        self.interrupts = ints.into_iter().collect();
        let maps: BTreeSet<_> = self.windows.iter().chain(&other.windows).copied().collect();
        self.windows = maps.into_iter().take(MAX_WINDOW_MAPS).collect();
        self.instructions_recorded += other.instructions_recorded;
        true
    }

    /// PRG ROM bytes that executed as an opcode or operand.
    #[must_use]
    pub fn executed_prg_bytes(&self) -> usize {
        self.prg_flags.iter().filter(|&&b| b != 0).count()
    }

    /// PRG ROM bytes seen as the first byte of an instruction.
    #[must_use]
    pub fn opcode_prg_bytes(&self) -> usize {
        self.prg_flags.iter().filter(|&&b| b & OPCODE != 0).count()
    }

    /// CPU addresses outside PRG ROM that were executed.
    #[must_use]
    pub fn executed_ram_bytes(&self) -> usize {
        self.ram_exec.iter().filter(|&&b| b != 0).count()
    }

    /// Executed fraction of PRG ROM, `0.0..=1.0` (`0.0` for an empty PRG).
    #[must_use]
    pub fn prg_coverage_fraction(&self) -> f64 {
        if self.prg_flags.is_empty() {
            return 0.0;
        }
        self.executed_prg_bytes() as f64 / self.prg_flags.len() as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(cpu: u16, rom: u32) -> CodeAddr {
        CodeAddr {
            cpu,
            rom: Some(rom),
        }
    }

    #[test]
    fn marking_counts_and_fraction() {
        let mut l = CoverageLog::new(8);
        l.mark_prg(0, OPCODE);
        l.mark_prg(1, OPERAND);
        l.mark_prg(1, OPERAND);
        l.mark_prg(99, OPCODE); // out of range: ignored
        l.mark_ram(0x0300, OPCODE);
        assert_eq!(l.executed_prg_bytes(), 2);
        assert_eq!(l.opcode_prg_bytes(), 1);
        assert_eq!(l.executed_ram_bytes(), 1);
        assert!((l.prg_coverage_fraction() - 0.25).abs() < 1e-12);
        assert_eq!(CoverageLog::new(0).prg_coverage_fraction(), 0.0);
    }

    #[test]
    fn edges_dedupe_and_sort() {
        let mut l = CoverageLog::new(0);
        let e = |s, t| IndirectEdge {
            site: at(s, 0),
            target: at(t, 0),
            kind: IndirectKind::Rts,
        };
        l.note_indirect(e(0x9000, 1));
        l.note_indirect(e(0x8000, 2));
        l.note_indirect(e(0x9000, 1));
        assert_eq!(l.indirect.len(), 2);
        assert!(l.indirect.windows(2).all(|w| w[0] < w[1]));
        let i = InterruptEntry {
            kind: InterruptKind::Nmi,
            target: at(0x8100, 0),
        };
        l.note_interrupt(i);
        l.note_interrupt(i);
        assert_eq!(l.interrupts.len(), 1);
    }

    #[test]
    fn window_snapshots_dedupe_sort_and_merge() {
        let map = |b: u32| {
            let mut m = WindowMap::EMPTY;
            m.0[0] = b;
            m
        };
        let mut a = CoverageLog::new(0);
        a.note_windows(map(0x4000));
        a.note_windows(map(0x2000));
        a.note_windows(map(0x4000));
        assert_eq!(a.windows, vec![map(0x2000), map(0x4000)]);
        let mut b = CoverageLog::new(0);
        b.note_windows(map(0x6000));
        b.note_windows(map(0x2000));
        assert!(a.merge(&b));
        assert_eq!(a.windows.len(), 3);
        assert!(a.windows.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(WindowMap::page_of(0x7FFF), None);
        assert_eq!(WindowMap::page_of(0x8000), Some(0));
        assert_eq!(WindowMap::page_of(0xFFFF), Some(7));
    }

    #[test]
    fn merge_unions_and_rejects_size_mismatch() {
        let mut a = CoverageLog::new(4);
        let mut b = CoverageLog::new(4);
        a.mark_prg(0, OPCODE);
        b.mark_prg(0, OPERAND);
        b.mark_prg(3, OPCODE);
        b.note_interrupt(InterruptEntry {
            kind: InterruptKind::Reset,
            target: at(0x8000, 0),
        });
        a.instructions_recorded = 2;
        b.instructions_recorded = 3;
        assert!(a.merge(&b));
        assert_eq!(a.prg_flags, vec![OPCODE | OPERAND, 0, 0, OPCODE]);
        assert_eq!(a.interrupts.len(), 1);
        assert_eq!(a.instructions_recorded, 5);
        let before = a.clone();
        assert!(!a.merge(&CoverageLog::new(5)));
        assert_eq!(a, before);
    }
}
