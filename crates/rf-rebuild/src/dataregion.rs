//! The data-region finder, observation half (ticket W38-01;
//! `docs/design/REBUILD_LAB.md` section 9): learn where a game keeps its ROM
//! tables and level data from **how its code reads ROM**, with no hints.
//!
//! The understand pass ([`crate::profile`]) looks at what a function does to
//! the machine and ignores every ROM access. This module is its counterpart:
//! it looks at nothing but the ROM *data* reads (never instruction fetches),
//! and records, per reading instruction (a **site**), which ROM bytes it read
//! and through which addressing: absolute, indexed by a register, or indexed
//! through a pointer in RAM.
//!
//! ## What the bus log gives, and how a read is classified
//!
//! A [`StepObserver`] sees the CPU-bus accesses of one instruction and no
//! registers, no opcode table and no program counter. They are rebuilt from
//! the access list, in order:
//!
//! 1. **Prefix.** The first access is the opcode fetch, so its address is the
//!    PC. The run of consecutive addresses that follows is the operand
//!    (capped at [`Platform`]'s longest instruction). The operand bytes are
//!    in the log with their values.
//! 2. **Data read.** The last ROM read after the prefix that is not inside
//!    the instruction's own bytes (a `JSR` fetches its second operand byte
//!    after the pushes; a taken branch reads a dummy byte). Earlier ROM reads
//!    are the 6502's page-cross dummy reads. Two ROM reads at consecutive
//!    addresses are one 16-bit read.
//! 3. **Addressing.** No RAM read before the data read: the operand is the
//!    base and `index = data address - base` (zero for absolute). RAM reads
//!    before it: the last two (65816 long pointers: three) are the pointer,
//!    and `index = data address - pointer`.
//!
//! An instruction that fits neither shape is counted in
//! [`ReadStats::unknown`] and recorded nowhere.
//!
//! ## Values: what flows where
//!
//! A value read from ROM that is stored to RAM a few steps later is followed
//! (a shadow of "which ROM read produced this RAM byte", checked against the
//! byte actually read back). When an indirect read finds its pointer bytes
//! produced by ROM reads, the three together are a **link**: a table entry
//! became a pointer and the pointer was used. That is what separates a
//! pointer table from any other table, and what ties it to the stream it
//! points at. A ROM value stored to a picture-unit data port marks the
//! reading site as feeding graphics. The value of a read is used for the
//! check and then dropped: nothing stored or printed contains a ROM byte
//! (law 5, D-021), only offsets, sizes, counts and index values.
//!
//! ## Limits (stated, measured by the callers)
//!
//! - The resolver (CPU address to ROM offset) is installed per frame by the
//!   caller. A bank switch in the middle of a frame is seen with the mapping
//!   the frame started with; neither game this was built on switches banks
//!   during play.
//! - Value matching is a heuristic: a store of a byte equal to a recent ROM
//!   read is taken as a copy of it. A link needs both pointer bytes to match
//!   *and* the table pair to keep one fixed distance across entries
//!   ([`crate::dataregion::infer`]), which small coincidental values do not.
//! - Table extents are what the game was seen reading: a lower bound.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use rf_core_api::{BusAccess, StepFacts, StepObserver};

use crate::hw::{classify, Hit, HwClass, Platform};
use crate::profile::RangeSet;
use crate::recipe::{DataRegion, Grade, Recipe};

/// CPU address to ROM offset (`None`: not ROM). Installed per frame.
pub type Resolver = Box<dyn Fn(u32) -> Option<u32>>;

/// Distinct index values kept per site.
const INDEX_CAP: usize = 8192;
/// Distinct pointer bases kept per site.
const SEGMENT_CAP: usize = 8192;
/// Entries kept per link.
const ENTRY_CAP: usize = 8192;
/// Recent ROM reads kept for value matching.
const RING: usize = 16;
/// A store matches a ROM read at most this many steps old.
const WINDOW: u64 = 8;
/// RAM reads remembered as candidate index sources.
const RAM_RECENT: usize = 8;
/// An index source is a RAM read at most this many steps before the read.
const INDEX_WINDOW: u64 = 6;

/// One read of ROM, as the finder remembers it for value flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Origin {
    /// ROM offset of the reading instruction.
    pub site: u32,
    /// ROM offset read.
    pub rom: u32,
    /// Index of the read (register value, or the Y added to a pointer).
    pub index: u32,
    value: u8,
}

/// Everything seen of one reading instruction.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Site {
    /// Atlas function the instruction belongs to, when known.
    pub function: Option<u32>,
    /// The instruction's CPU address, as first seen.
    pub cpu: u32,
    /// Reads go through a RAM pointer.
    pub indirect: bool,
    pub hits: u64,
    /// Widest read, bytes.
    pub width: u8,
    /// ROM bytes read.
    pub offsets: RangeSet,
    /// Distinct index values (see [`Self::index_overflow`]).
    pub indices: BTreeSet<u32>,
    pub index_overflow: bool,
    /// Per base (ROM offset the index counts from: the operand for a direct
    /// read, the pointer's target for an indirect one): the lowest and highest
    /// byte offset read from it, and the reads.
    pub segments: BTreeMap<u32, Segment>,
    pub segments_truncated: bool,
    /// Reads at an offset 1..=4 after the previous read of this site from the
    /// same base: the signature of walking through data.
    pub sequential: u64,
    last: Option<(u32, u32)>,
    /// RAM address of the pointer, by reads.
    pub pointer_ram: BTreeMap<u32, u64>,
    /// RAM addresses read a few steps before whose value was the index, by
    /// votes: the index's likely source.
    pub index_source: BTreeMap<u32, u64>,
    /// Reads whose value went to a picture-unit data port.
    pub feeds_ppu: u64,
}

/// Bytes read from one base: `[lo, hi)` as ROM offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub lo: u32,
    pub hi: u32,
    pub reads: u64,
}

/// Which pointer-table sites fed one pointer, and what was seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct LinkKey {
    pub lo: u32,
    pub hi: u32,
    pub bank: Option<u32>,
}

/// One table entry that became a pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkEntry {
    /// Index of the entry's low byte read.
    pub index: u32,
    /// ROM offsets the pointer's target base resolved to (the first byte the
    /// indirect read counts from).
    pub targets: BTreeSet<u32>,
    pub uses: u64,
}

/// What was seen of one (low site, high site[, bank site]) pointer feed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkStat {
    /// Pointer width in bytes (2, or 3 with a bank byte).
    pub ptr_bytes: u8,
    pub count: u64,
    /// `high read offset - low read offset`, by count.
    pub deltas: BTreeMap<i64, u64>,
    /// `bank read offset - low read offset`, by count.
    pub bank_deltas: BTreeMap<i64, u64>,
    /// Entries by the ROM offset of the low byte read.
    pub entries: BTreeMap<u32, LinkEntry>,
    /// The sites that used the pointer.
    pub readers: BTreeSet<u32>,
    pub pointer_ram: BTreeSet<u32>,
}

/// Counters of one observation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReadStats {
    pub steps: u64,
    pub data_reads: u64,
    pub direct: u64,
    pub indirect: u64,
    /// ROM data reads that fit neither shape (not recorded).
    pub unknown: u64,
    /// Direct reads with an index above 255 on a console whose index
    /// registers are 8 bits: a sign the classification is off.
    pub suspect_index: u64,
    pub links: u64,
}

/// What an observation learned.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DataFacts {
    pub platform: Option<Platform>,
    /// By ROM offset of the reading instruction.
    pub sites: BTreeMap<u32, Site>,
    pub links: BTreeMap<LinkKey, LinkStat>,
    pub stats: ReadStats,
}

impl DataFacts {
    /// Fold another run's facts in (offsets and index values are unions,
    /// counts add).
    pub fn merge(&mut self, other: &Self) {
        self.platform = self.platform.or(other.platform);
        let s = &mut self.stats;
        let o = &other.stats;
        s.steps += o.steps;
        s.data_reads += o.data_reads;
        s.direct += o.direct;
        s.indirect += o.indirect;
        s.unknown += o.unknown;
        s.suspect_index += o.suspect_index;
        s.links += o.links;
        for (&pc, theirs) in &other.sites {
            let mine = self.sites.entry(pc).or_default();
            mine.function = mine.function.or(theirs.function);
            if mine.cpu == 0 {
                mine.cpu = theirs.cpu;
            }
            mine.indirect |= theirs.indirect;
            mine.hits += theirs.hits;
            mine.width = mine.width.max(theirs.width);
            for &(a, b) in theirs.offsets.ranges() {
                for x in a..=b {
                    mine.offsets.insert(x);
                }
            }
            for &i in &theirs.indices {
                if mine.indices.len() >= INDEX_CAP {
                    mine.index_overflow = true;
                    break;
                }
                mine.indices.insert(i);
            }
            mine.index_overflow |= theirs.index_overflow;
            for (&base, seg) in &theirs.segments {
                let e = mine.segments.entry(base).or_insert(*seg);
                e.lo = e.lo.min(seg.lo);
                e.hi = e.hi.max(seg.hi);
                if !std::ptr::eq(e, seg) {
                    e.reads += seg.reads;
                }
            }
            mine.segments_truncated |= theirs.segments_truncated;
            mine.sequential += theirs.sequential;
            for (&a, &n) in &theirs.pointer_ram {
                *mine.pointer_ram.entry(a).or_default() += n;
            }
            for (&a, &n) in &theirs.index_source {
                *mine.index_source.entry(a).or_default() += n;
            }
            mine.feeds_ppu += theirs.feeds_ppu;
        }
        for (key, theirs) in &other.links {
            let mine = self.links.entry(*key).or_default();
            mine.ptr_bytes = mine.ptr_bytes.max(theirs.ptr_bytes);
            mine.count += theirs.count;
            for (&d, &n) in &theirs.deltas {
                *mine.deltas.entry(d).or_default() += n;
            }
            for (&d, &n) in &theirs.bank_deltas {
                *mine.bank_deltas.entry(d).or_default() += n;
            }
            for (&lo, e) in &theirs.entries {
                let m = mine.entries.entry(lo).or_insert_with(|| LinkEntry {
                    index: e.index,
                    targets: BTreeSet::new(),
                    uses: 0,
                });
                m.targets.extend(e.targets.iter().copied());
                m.uses += e.uses;
            }
            mine.readers.extend(theirs.readers.iter().copied());
            mine.pointer_ram.extend(theirs.pointer_ram.iter().copied());
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct RingEntry {
    step: u64,
    value: u8,
    origin: Origin,
}

/// The data read an instruction made, once classified.
#[derive(Debug, Clone, Copy)]
struct DataRead {
    /// Position in the access list of the first byte read.
    first: usize,
    /// Of the last byte read.
    last: usize,
    rom: u32,
    width: u8,
    index: u32,
    base: u32,
    pointer: Option<Pointer>,
}

#[derive(Debug, Clone, Copy)]
struct Pointer {
    /// Positions in the access list of the pointer bytes.
    at: [usize; 3],
    bytes: u8,
}

/// The observer: hand it to `run_frame_observed`, with the resolver set for
/// the frame, then [`Self::take_facts`].
pub struct DataReader {
    platform: Platform,
    resolver: Resolver,
    step: u64,
    ring: VecDeque<RingEntry>,
    ram_recent: VecDeque<(u64, u32, u8)>,
    taint: BTreeMap<u32, Origin>,
    facts: DataFacts,
}

impl DataReader {
    #[must_use]
    pub fn new(platform: Platform) -> Self {
        Self {
            platform,
            resolver: Box::new(|_| None),
            step: 0,
            ring: VecDeque::new(),
            ram_recent: VecDeque::new(),
            taint: BTreeMap::new(),
            facts: DataFacts {
                platform: Some(platform),
                ..DataFacts::default()
            },
        }
    }

    /// Install the CPU-address to ROM-offset map for the frames that follow.
    pub fn set_resolver(&mut self, resolver: Resolver) {
        self.resolver = resolver;
    }

    /// What has been learned so far; the reader keeps observing into a fresh
    /// set (its value-flow shadow stays, so a pointer loaded before the cut
    /// is still understood after it).
    pub fn take_facts(&mut self) -> DataFacts {
        std::mem::replace(
            &mut self.facts,
            DataFacts {
                platform: Some(self.platform),
                ..DataFacts::default()
            },
        )
    }

    /// The longest instruction the prefix scan accepts.
    fn max_prefix(&self) -> usize {
        match self.platform {
            Platform::Nes => 3,
            Platform::Snes => 4,
        }
    }

    fn ram_addr(&self, a: u32, write: bool) -> Option<u32> {
        match classify(self.platform, a, write) {
            Hit::Ram { addr } => Some(addr),
            _ => None,
        }
    }

    /// Find and classify the ROM data read of one instruction.
    fn classify_step(&self, acc: &[BusAccess]) -> Option<(u32, DataRead)> {
        let first = *acc.first()?;
        if first.write {
            return None;
        }
        let pc = first.addr;
        let cap = self.max_prefix();
        let mut prefix = 1;
        while prefix < cap
            && acc
                .get(prefix)
                .is_some_and(|a| !a.write && a.addr == pc.wrapping_add(prefix as u32))
        {
            prefix += 1;
        }
        let pc_rom = (self.resolver)(pc)?;
        // Candidate data reads: ROM reads after the prefix, outside the
        // instruction's own bytes.
        let is_data = |a: &BusAccess| {
            !a.write && a.addr.wrapping_sub(pc) > cap as u32 && (self.resolver)(a.addr).is_some()
        };
        let last = (prefix..acc.len()).rev().find(|&i| is_data(&acc[i]))?;
        let mut first_i = last;
        if last > prefix
            && is_data(&acc[last - 1])
            && acc[last - 1].addr.wrapping_add(1) == acc[last].addr
        {
            first_i = last - 1;
        }
        let rom = (self.resolver)(acc[first_i].addr)?;
        let width = (last - first_i + 1) as u8;
        let data = acc[first_i].addr;
        // RAM reads between the prefix and the data read.
        let rams: Vec<usize> = (prefix..first_i)
            .filter(|&i| !acc[i].write && self.ram_addr(acc[i].addr, false).is_some())
            .collect();
        let mut read = DataRead {
            first: first_i,
            last,
            rom,
            width,
            index: 0,
            base: rom,
            pointer: None,
        };
        if rams.is_empty() {
            if prefix < 3 {
                return None;
            }
            let operand = u32::from(acc[1].value) | (u32::from(acc[2].value) << 8);
            let base_cpu = if prefix >= 4 {
                operand | (u32::from(acc[3].value) << 16)
            } else {
                (data & !0xFFFF) | operand
            };
            read.index = (data & 0xFFFF).wrapping_sub(operand) & 0xFFFF;
            read.base = (self.resolver)(base_cpu).unwrap_or_else(|| rom.wrapping_sub(read.index));
            return Some((pc_rom, read));
        }
        // Through a pointer: its bytes are the last RAM reads before the data.
        let n = rams.len();
        if n < 2 {
            return None;
        }
        let (a, b) = (rams[n - 2], rams[n - 1]);
        if acc[b].addr != acc[a].addr.wrapping_add(1) {
            return None;
        }
        let ptr16 = u32::from(acc[a].value) | (u32::from(acc[b].value) << 8);
        let mut pointer = Pointer {
            at: [a, b, 0],
            bytes: 2,
        };
        let mut index = (data & 0xFFFF).wrapping_sub(ptr16) & 0xFFFF;
        if self.platform == Platform::Snes && n >= 3 {
            let (z, y, x) = (rams[n - 3], rams[n - 2], rams[n - 1]);
            let consecutive = acc[y].addr == acc[z].addr.wrapping_add(1)
                && acc[x].addr == acc[y].addr.wrapping_add(1);
            // A long pointer's bank byte is the bank the data came from.
            if consecutive && u32::from(acc[x].value) == (data >> 16) & 0xFF {
                let lo = u32::from(acc[z].value) | (u32::from(acc[y].value) << 8);
                pointer = Pointer {
                    at: [z, y, x],
                    bytes: 3,
                };
                index = (data & 0xFFFF).wrapping_sub(lo) & 0xFFFF;
            }
        }
        read.index = index;
        read.base = rom.wrapping_sub(index);
        read.pointer = Some(pointer);
        Some((pc_rom, read))
    }

    fn push_ring(&mut self, e: RingEntry) {
        if self.ring.len() == RING {
            self.ring.pop_front();
        }
        self.ring.push_back(e);
    }

    fn matching_origin(&self, value: u8) -> Option<Origin> {
        self.ring
            .iter()
            .rev()
            .find(|e| e.value == value && self.step - e.step <= WINDOW)
            .map(|e| e.origin)
    }

    fn record(&mut self, pc_rom: u32, function: Option<u32>, cpu: u32, r: &DataRead) {
        let st = &mut self.facts.stats;
        st.data_reads += 1;
        if r.pointer.is_some() {
            st.indirect += 1;
        } else {
            st.direct += 1;
            if self.platform == Platform::Nes && r.index > 0xFF {
                st.suspect_index += 1;
            }
        }
        let site = self.facts.sites.entry(pc_rom).or_default();
        site.function = site.function.or(function);
        if site.cpu == 0 {
            site.cpu = cpu;
        }
        site.indirect |= r.pointer.is_some();
        site.hits += 1;
        site.width = site.width.max(r.width);
        for k in 0..u32::from(r.width) {
            site.offsets.insert(r.rom + k);
        }
        if site.indices.len() < INDEX_CAP {
            site.indices.insert(r.index);
        } else if !site.indices.contains(&r.index) {
            site.index_overflow = true;
        }
        let hi = r.rom + u32::from(r.width);
        let nseg = site.segments.len();
        match site.segments.get_mut(&r.base) {
            Some(seg) => {
                seg.lo = seg.lo.min(r.rom);
                seg.hi = seg.hi.max(hi);
                seg.reads += 1;
            }
            None if nseg < SEGMENT_CAP => {
                site.segments.insert(
                    r.base,
                    Segment {
                        lo: r.rom,
                        hi,
                        reads: 1,
                    },
                );
            }
            None => site.segments_truncated = true,
        }
        if let Some((base, last)) = site.last {
            if base == r.base && r.rom > last && r.rom - last <= 4 {
                site.sequential += 1;
            }
        }
        site.last = Some((r.base, r.rom));
    }
}

impl StepObserver for DataReader {
    fn step(&mut self, facts: StepFacts, acc: &[BusAccess]) {
        self.step += 1;
        self.facts.stats.steps += 1;
        if facts.interrupt || acc.is_empty() {
            return;
        }
        let classified = self.classify_step(acc);
        let is_jump_ind = classified.is_none() && acc[0].value == 0x6C;
        let _ = is_jump_ind;
        let mut here: Option<(u32, DataRead)> = None;
        if let Some((pc_rom, r)) = classified {
            self.record(pc_rom, facts.function, acc[0].addr, &r);
            here = Some((pc_rom, r));
        } else if acc.iter().skip(1).any(|a| {
            !a.write && a.addr.wrapping_sub(acc[0].addr) > 4 && (self.resolver)(a.addr).is_some()
        }) {
            self.facts.stats.unknown += 1;
        }
        // The pointer link, if this read went through a pointer the ROM made.
        if let Some((pc_rom, r)) = here {
            if let Some(p) = r.pointer {
                self.note_pointer(pc_rom, &r, p, acc);
            } else {
                self.note_index_source(pc_rom, &r);
            }
        }
        // Value flow, in bus order.
        for (i, a) in acc.iter().enumerate() {
            if a.write {
                self.note_write(*a);
                continue;
            }
            match here {
                Some((pc_rom, r)) if (r.first..=r.last).contains(&i) => {
                    let origin = Origin {
                        site: pc_rom,
                        rom: r.rom + (i - r.first) as u32,
                        index: r.index,
                        value: a.value,
                    };
                    self.push_ring(RingEntry {
                        step: self.step,
                        value: a.value,
                        origin,
                    });
                }
                _ => {
                    if let Some(ram) = self.ram_addr(a.addr, false) {
                        if let Some(o) = self.taint.get(&ram).copied() {
                            if o.value == a.value {
                                self.push_ring(RingEntry {
                                    step: self.step,
                                    value: a.value,
                                    origin: o,
                                });
                            }
                        }
                        if self.ram_recent.len() == RAM_RECENT {
                            self.ram_recent.pop_front();
                        }
                        self.ram_recent.push_back((self.step, ram, a.value));
                    }
                }
            }
        }
    }
}

impl DataReader {
    fn note_write(&mut self, a: BusAccess) {
        match classify(self.platform, a.addr, true) {
            Hit::Ram { addr } => match self.matching_origin(a.value) {
                Some(o) => {
                    self.taint.insert(addr, o);
                }
                None => {
                    self.taint.remove(&addr);
                }
            },
            Hit::Reg {
                class: HwClass::Ppu,
                reg,
            } if self.is_graphics_port(reg) => {
                if let Some(o) = self.matching_origin(a.value) {
                    if let Some(s) = self.facts.sites.get_mut(&o.site) {
                        s.feeds_ppu += 1;
                    }
                }
            }
            _ => {}
        }
    }

    /// Picture-unit ports that take bulk data: NES `$2007` (VRAM data);
    /// SNES `$2118`/`$2119` (VRAM) and `$2122` (CGRAM).
    fn is_graphics_port(&self, reg: u16) -> bool {
        match self.platform {
            Platform::Nes => reg == 0x2007,
            Platform::Snes => matches!(reg, 0x2118 | 0x2119 | 0x2122),
        }
    }

    fn note_index_source(&mut self, pc_rom: u32, r: &DataRead) {
        if r.index == 0 {
            return;
        }
        let want = (r.index & 0xFF) as u8;
        let step = self.step;
        let hits: Vec<u32> = self
            .ram_recent
            .iter()
            .filter(|&&(s, _, v)| step - s <= INDEX_WINDOW && v == want)
            .map(|&(_, a, _)| a)
            .collect();
        if let Some(site) = self.facts.sites.get_mut(&pc_rom) {
            for a in hits {
                *site.index_source.entry(a).or_default() += 1;
            }
        }
    }

    fn note_pointer(&mut self, pc_rom: u32, r: &DataRead, p: Pointer, acc: &[BusAccess]) {
        let first = acc[p.at[0]];
        if let Some(ram) = self.ram_addr(first.addr, false) {
            if let Some(site) = self.facts.sites.get_mut(&pc_rom) {
                *site.pointer_ram.entry(ram).or_default() += 1;
            }
        }
        let origins: Vec<Option<Origin>> = (0..usize::from(p.bytes))
            .map(|k| {
                let a = acc[p.at[k]];
                self.ram_addr(a.addr, false)
                    .and_then(|ram| self.taint.get(&ram).copied())
                    .filter(|o| o.value == a.value)
            })
            .collect();
        let (Some(lo), Some(hi)) = (origins[0], origins[1]) else {
            return;
        };
        let bank = if p.bytes == 3 { origins[2] } else { None };
        let key = LinkKey {
            lo: lo.site,
            hi: hi.site,
            bank: bank.map(|o| o.site),
        };
        self.facts.stats.links += 1;
        let ram_first = self.ram_addr(first.addr, false);
        let link = self.facts.links.entry(key).or_default();
        link.ptr_bytes = link.ptr_bytes.max(p.bytes);
        link.count += 1;
        *link
            .deltas
            .entry(i64::from(hi.rom) - i64::from(lo.rom))
            .or_default() += 1;
        if let Some(b) = bank {
            *link
                .bank_deltas
                .entry(i64::from(b.rom) - i64::from(lo.rom))
                .or_default() += 1;
        }
        link.readers.insert(pc_rom);
        link.pointer_ram.extend(ram_first);
        if link.entries.len() < ENTRY_CAP || link.entries.contains_key(&lo.rom) {
            let e = link.entries.entry(lo.rom).or_insert_with(|| LinkEntry {
                index: lo.index,
                targets: BTreeSet::new(),
                uses: 0,
            });
            e.uses += 1;
            if e.targets.len() < 8 {
                e.targets.insert(r.base);
            }
        }
    }
}

// ------------------------------------------------------------ inference

/// What a region of ROM is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RegionKind {
    /// Entries that become pointers (to data, usually).
    PointerTable,
    /// A table read by index.
    ElementTable,
    /// Read in order through a moving index or pointer: level objects,
    /// scripts.
    Stream,
    /// Read to feed the picture unit: tiles, tilemaps, palettes.
    Graphics,
}

impl RegionKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PointerTable => "pointer_table",
            Self::ElementTable => "element_table",
            Self::Stream => "stream",
            Self::Graphics => "graphics",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pointer_table" => Self::PointerTable,
            "element_table" => Self::ElementTable,
            "stream" => Self::Stream,
            "graphics" => Self::Graphics,
            _ => return None,
        })
    }

    const fn short(self) -> &'static str {
        match self {
            Self::PointerTable => "ptrtab",
            Self::ElementTable => "table",
            Self::Stream => "stream",
            Self::Graphics => "gfx",
        }
    }
}

/// Which byte of a split pointer table a region holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Part {
    Lo,
    Hi,
    Bank,
}

impl Part {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Lo => "lo",
            Self::Hi => "hi",
            Self::Bank => "bank",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "lo" => Self::Lo,
            "hi" => Self::Hi,
            "bank" => Self::Bank,
            _ => return None,
        })
    }
}

/// A region the finder proposes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    pub name: String,
    pub kind: RegionKind,
    /// ROM offset (into the image the atlas indexes).
    pub start: u32,
    pub size: u32,
    /// Bytes between elements (1 for a byte table or a stream).
    pub stride: u32,
    /// For a split pointer table, the byte this region holds.
    pub part: Option<Part>,
    /// For a pointer table: pointer width in bytes.
    pub pointer_bytes: u8,
    /// RAM address the index was seen to come from.
    pub index_ram: Option<u32>,
    /// RAM address of the pointer a stream is read through.
    pub pointer_ram: Option<u32>,
    /// A stream: the pointer table whose entries point into it.
    pub via: Option<String>,
    /// Elements seen read (pointer-table entries, table indices).
    pub entries: u32,
    /// Reads behind the region.
    pub reads: u64,
    /// 0..=100.
    pub confidence: u8,
    /// Reading sites (ROM offsets of the instructions).
    pub sites: Vec<u32>,
    pub evidence: String,
}

fn gcd(mut a: u32, mut b: u32) -> u32 {
    // `b` strictly decreases (it becomes `a % b < b`), so this ends.
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// Gap between sorted distinct values: the gcd of consecutive differences
/// (0 when fewer than two).
fn stride_of(values: &BTreeSet<u32>) -> u32 {
    values
        .iter()
        .zip(values.iter().skip(1))
        .fold(0, |g, (&a, &b)| gcd(g, b - a))
}

/// Overlapping (not merely touching) merge of half-open ranges.
fn merge_overlapping(mut v: Vec<(u32, u32)>) -> Vec<(u32, u32)> {
    v.sort_unstable();
    let mut out: Vec<(u32, u32)> = Vec::new();
    for (lo, hi) in v {
        match out.last_mut() {
            Some(last) if lo < last.1 => last.1 = last.1.max(hi),
            _ => out.push((lo, hi)),
        }
    }
    out
}

fn top_ram(m: &BTreeMap<u32, u64>, hits: u64, min_share: f64) -> Option<u32> {
    let (&addr, &n) = m.iter().max_by_key(|(&a, &n)| (n, std::cmp::Reverse(a)))?;
    (n as f64 >= min_share * hits as f64 && n >= 2).then_some(addr)
}

fn clamp_conf(x: f64) -> u8 {
    x.clamp(0.0, 95.0) as u8
}

/// Propose regions from what was observed. Pure: the same facts give the
/// same regions in the same order (ascending start).
#[must_use]
pub fn infer(facts: &DataFacts) -> Vec<Region> {
    let mut out: Vec<Region> = Vec::new();
    let mut claimed: BTreeSet<u32> = BTreeSet::new();

    // 1. Pointer tables, from links.
    let mut ptr_regions: Vec<(LinkKey, Vec<Region>)> = Vec::new();
    for (key, link) in &facts.links {
        let regions = pointer_regions(facts, key, link);
        if regions.is_empty() {
            continue;
        }
        claimed.insert(key.lo);
        claimed.insert(key.hi);
        claimed.extend(key.bank);
        ptr_regions.push((*key, regions));
    }
    let table_name = |target: u32| -> Option<String> {
        ptr_regions.iter().find_map(|(k, rs)| {
            let link = &facts.links[k];
            link.entries
                .values()
                .any(|e| e.targets.contains(&target))
                .then(|| rs[0].name.clone())
        })
    };

    // 2. Everything else, by site.
    let mut gfx: Vec<(u32, &Site)> = Vec::new();
    for (&pc, site) in &facts.sites {
        if claimed.contains(&pc) || site.hits < 2 {
            continue;
        }
        let segs: Vec<(u32, u32)> = site.segments.values().map(|s| (s.lo, s.hi)).collect();
        if site.feeds_ppu >= 4 && site.feeds_ppu * 4 >= site.hits {
            gfx.push((pc, site));
            continue;
        }
        let walks = site.sequential * 10 >= site.hits * 4 && site.hits >= 4;
        if site.indirect {
            let ranges = merge_overlapping(segs);
            let kind = if walks {
                RegionKind::Stream
            } else {
                RegionKind::ElementTable
            };
            for (lo, hi) in ranges {
                if hi - lo < 2 {
                    continue;
                }
                let mut r = region(kind, lo, hi - lo, 1);
                r.reads = site.hits;
                r.sites = vec![pc];
                r.pointer_ram = top_ram(&site.pointer_ram, site.hits, 0.5);
                r.via = site
                    .segments
                    .keys()
                    .filter(|&&b| (lo..hi).contains(&b))
                    .find_map(|&b| table_name(b));
                r.confidence = clamp_conf(
                    35.0 + (site.sequential as f64 / site.hits as f64) * 30.0
                        + (f64::from(hi - lo)).log2() * 2.0
                        + if r.via.is_some() { 15.0 } else { 0.0 },
                );
                r.evidence = format!(
                    "{} reads through a RAM pointer, {} sequential",
                    site.hits, site.sequential
                );
                out.push(r);
            }
        } else if site.indices.len() >= 2 {
            let stride = stride_of(&site.indices).max(1);
            let kind = if walks && stride == 1 {
                RegionKind::Stream
            } else {
                RegionKind::ElementTable
            };
            for (lo, hi) in merge_overlapping(segs) {
                let mut r = region(kind, lo, hi - lo, stride);
                r.entries = site.indices.len() as u32;
                r.reads = site.hits;
                r.sites = vec![pc];
                r.index_ram = top_ram(&site.index_source, site.hits, 0.6);
                r.confidence = clamp_conf(
                    30.0 + (site.indices.len() as f64).log2() * 6.0
                        + if r.index_ram.is_some() { 15.0 } else { 0.0 },
                );
                r.evidence = format!(
                    "{} reads indexed by a register, {} distinct indices",
                    site.hits,
                    site.indices.len()
                );
                out.push(r);
            }
        }
    }
    for (pc, site) in gfx {
        let hits = site.hits;
        let segs: Vec<(u32, u32)> = site.segments.values().map(|s| (s.lo, s.hi)).collect();
        for (lo, hi) in merge_overlapping(segs) {
            let mut r = region(RegionKind::Graphics, lo, hi - lo, 1);
            r.reads = hits;
            r.sites = vec![pc];
            r.confidence = clamp_conf(40.0 + 50.0 * site.feeds_ppu as f64 / hits as f64);
            r.evidence = format!(
                "{} of {} reads went to a picture-unit data port",
                site.feeds_ppu, hits
            );
            out.push(r);
        }
    }
    for (_, rs) in &ptr_regions {
        out.extend(rs.iter().cloned());
    }

    // Streams reached from a pointer table name it.
    for r in &mut out {
        if r.kind == RegionKind::Stream && r.via.is_none() {
            r.via = table_name(r.start);
        }
    }
    out = merge_same_region(out);
    out.sort_by(|a, b| (a.start, a.kind, a.part).cmp(&(b.start, b.kind, b.part)));
    out
}

fn region(kind: RegionKind, start: u32, size: u32, stride: u32) -> Region {
    Region {
        name: format!("{}_{start:06x}", kind.short()),
        kind,
        start,
        size,
        stride,
        part: None,
        pointer_bytes: 0,
        index_ram: None,
        pointer_ram: None,
        via: None,
        entries: 0,
        reads: 0,
        confidence: 0,
        sites: Vec::new(),
        evidence: String::new(),
    }
}

/// Rows of the same kind, extent and part from different sites are one
/// region read by several sites.
fn merge_same_region(rows: Vec<Region>) -> Vec<Region> {
    let mut out: Vec<Region> = Vec::new();
    for r in rows {
        match out.iter_mut().find(|o| {
            o.kind == r.kind && o.start == r.start && o.size == r.size && o.part == r.part
        }) {
            Some(o) => {
                o.reads += r.reads;
                o.sites.extend(r.sites);
                o.sites.sort_unstable();
                o.sites.dedup();
                o.confidence = o.confidence.max(r.confidence);
                o.index_ram = o.index_ram.or(r.index_ram);
                o.pointer_ram = o.pointer_ram.or(r.pointer_ram);
                o.via = o.via.take().or(r.via);
            }
            None => out.push(r),
        }
    }
    out
}

/// The most common delta and its share of `count`.
fn mode_delta(m: &BTreeMap<i64, u64>, count: u64) -> Option<(i64, f64)> {
    let (&d, &n) = m.iter().max_by_key(|(&d, &n)| (n, std::cmp::Reverse(d)))?;
    Some((d, n as f64 / count.max(1) as f64))
}

/// The pointer-table regions of one link: one region when the pointer's bytes
/// are interleaved, one per byte when they are split.
fn pointer_regions(facts: &DataFacts, key: &LinkKey, link: &LinkStat) -> Vec<Region> {
    let Some((delta, share)) = mode_delta(&link.deltas, link.count) else {
        return Vec::new();
    };
    // The two halves of a table keep one distance across entries; coincidental
    // matches of small values do not.
    if share < 0.8 || link.entries.len() < 2 || delta == 0 {
        return Vec::new();
    }
    let bank_delta = if link.ptr_bytes == 3 && key.bank.is_some() {
        match mode_delta(&link.bank_deltas, link.count) {
            Some((d, s)) if s >= 0.8 => Some(d),
            _ => return Vec::new(),
        }
    } else {
        None
    };
    let lo_set: BTreeSet<u32> = link.entries.keys().copied().collect();
    let idx_set: BTreeSet<u32> = link.entries.values().map(|e| e.index).collect();
    let lo_site = facts.sites.get(&key.lo);
    let entries = lo_set.len() as u32;
    let stride_idx = stride_of(&idx_set).max(1);
    let mut conf = 40.0
        + (f64::from(entries)).log2() * 5.0
        + share * 15.0
        + if link.entries.values().all(|e| !e.targets.is_empty()) {
            5.0
        } else {
            0.0
        };
    let ptr_bytes = link.ptr_bytes;
    let evidence = format!(
        "{} pointers built from table reads, {} entries, high byte {delta:+} from low",
        link.count, entries
    );
    let (&lo_min, &lo_max) = (
        lo_set.iter().next().unwrap_or(&0),
        lo_set.iter().last().unwrap_or(&0),
    );
    let sites: Vec<u32> = [Some(key.lo), Some(key.hi), key.bank]
        .into_iter()
        .flatten()
        .collect();
    let readers = link.readers.len();
    conf += (readers as f64).min(3.0);
    let mut base = region(RegionKind::PointerTable, lo_min, 1, 1);
    base.pointer_bytes = ptr_bytes;
    base.entries = entries;
    base.reads = link.count;
    base.confidence = clamp_conf(conf);
    base.evidence = evidence;
    base.index_ram = lo_site.and_then(|s| top_ram(&s.index_source, s.hits, 0.6));
    base.pointer_ram = link.pointer_ram.iter().next().copied();
    let interleaved = delta == 1 && bank_delta.is_none_or(|d| d == 2);
    if interleaved {
        // One table: low, high (, bank) bytes next to each other.
        let width = u32::from(ptr_bytes);
        let stride = if entries >= 2 {
            // Entries are `lo` offsets: their distance is the stride.
            stride_of(&lo_set).max(width)
        } else {
            width
        };
        let mut r = base;
        r.size = lo_max - lo_min + width;
        r.stride = stride;
        r.sites = sites;
        return vec![r];
    }
    let part_regions = |set: &BTreeSet<u32>, part: Part, off: i64, sites: &[u32]| {
        let shifted: BTreeSet<u32> = set
            .iter()
            .map(|&x| (i64::from(x) + off).max(0) as u32)
            .collect();
        let (&a, &b) = (
            shifted.iter().next().unwrap_or(&0),
            shifted.iter().last().unwrap_or(&0),
        );
        let mut r = base.clone();
        r.name = format!(
            "{}_{}",
            region(RegionKind::PointerTable, a, 0, 1).name,
            part.as_str()
        );
        r.start = a;
        r.size = b - a + 1;
        r.stride = stride_of(&shifted).max(1).max(stride_idx.min(1));
        r.part = Some(part);
        r.sites = sites.to_vec();
        r
    };
    let mut v = vec![
        part_regions(&lo_set, Part::Lo, 0, &[key.lo]),
        part_regions(&lo_set, Part::Hi, delta, &[key.hi]),
    ];
    if let (Some(d), Some(b)) = (bank_delta, key.bank) {
        v.push(part_regions(&lo_set, Part::Bank, d, &[b]));
    }
    // Name the halves after the low one so they read as a group.
    let group = v[0].name.trim_end_matches("_lo").to_string();
    for r in &mut v {
        if let Some(p) = r.part {
            r.name = format!("{group}_{}", p.as_str());
        }
    }
    v
}

/// The recipe row for a region, with the grade the caller's experiment
/// earned and a `finder:` source.
#[must_use]
pub fn to_row(r: &Region, grade: Grade, experiment: &str) -> DataRegion {
    DataRegion {
        name: r.name.clone(),
        kind: r.kind,
        rom: r.start,
        size: r.size,
        stride: r.stride.max(1),
        part: r.part,
        pointer_bytes: (r.pointer_bytes != 0).then_some(r.pointer_bytes),
        index_ram: r.index_ram,
        pointer_ram: r.pointer_ram,
        via: r.via.clone(),
        confidence: Some(r.confidence),
        source: format!("finder: {}{}", r.evidence, experiment),
        grade,
    }
}

/// Add finder rows to a recipe. A row with the same name replaces a previous
/// finder row and never a hand-written one (source not starting `finder:`).
pub fn merge_rows(recipe: &mut Recipe, rows: Vec<DataRegion>) {
    for row in rows {
        match recipe.data_regions.iter_mut().find(|d| d.name == row.name) {
            Some(old) if old.source.starts_with("finder:") => *old = row,
            Some(_) => {}
            None => recipe.data_regions.push(row),
        }
    }
    recipe
        .data_regions
        .sort_by(|a, b| (a.rom, &a.name).cmp(&(b.rom, &b.name)));
}

// ------------------------------------------------------- level association

/// What one run showed of one pointer feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunEntry {
    /// Run key (level number).
    pub key: u32,
    /// Entry the pointer was loaded from (ROM offset of its low byte), when
    /// the run used an entry the other runs did not.
    pub entry: Option<u32>,
    /// Index of that entry (the register value at the table read).
    pub index: Option<u32>,
    /// Where the pointer pointed.
    pub target: Option<u32>,
    /// The stream read from that target in the run: `(start, size)`.
    pub stream: Option<(u32, u32)>,
}

/// A pointer feed examined across runs of different levels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LevelLink {
    pub link: LinkKey,
    pub runs: u32,
    /// Runs that used an entry no other run used.
    pub own_entry: u32,
    pub distinct_entries: u32,
    pub distinct_targets: u32,
    /// Runs whose target was read as a stream in that run.
    pub stream_found: u32,
    pub per_run: Vec<RunEntry>,
}

impl LevelLink {
    /// The entry index follows the level: nearly every run used an entry of
    /// its own, the entries differ, and each led to a stream.
    #[must_use]
    pub fn tracks_level(&self) -> bool {
        self.runs >= 2
            && u64::from(self.own_entry) * 10 >= u64::from(self.runs) * 9
            && self.distinct_entries >= self.own_entry
            && self.stream_found >= self.own_entry
    }
}

/// Compare the same pointer feed over runs that forced different levels
/// (`key` = the level number, one facts set per run, each holding only the
/// level's load).
///
/// An entry used by every run is not the level's (attract mode, a shared
/// table lookup), so the entries are compared after those are set aside. Runs
/// that share an entry are reported as such, never merged away.
#[must_use]
pub fn associate(runs: &[(u32, &DataFacts)]) -> Vec<LevelLink> {
    let keys: BTreeSet<LinkKey> = runs
        .iter()
        .flat_map(|(_, f)| f.links.keys().copied())
        .collect();
    let mut out = Vec::new();
    for key in keys {
        let used: Vec<(u32, BTreeSet<u32>, &LinkStat, &DataFacts)> = runs
            .iter()
            .map(|&(k, f)| {
                let entries = f
                    .links
                    .get(&key)
                    .map(|l| l.entries.keys().copied().collect())
                    .unwrap_or_default();
                (k, entries, f.links.get(&key).unwrap_or(&EMPTY_LINK), f)
            })
            .collect();
        if used.iter().filter(|u| !u.1.is_empty()).count() < 2 {
            continue;
        }
        let common: BTreeSet<u32> = used
            .iter()
            .map(|u| u.1.clone())
            .reduce(|a, b| a.intersection(&b).copied().collect())
            .unwrap_or_default();
        let mut per_run = Vec::new();
        let mut entries_seen = BTreeSet::new();
        let mut targets_seen = BTreeSet::new();
        let (mut own, mut found) = (0u32, 0u32);
        for (k, set, link, f) in &used {
            let mine: Vec<u32> = set.difference(&common).copied().collect();
            let entry = mine.first().copied();
            let e = entry.and_then(|e| link.entries.get(&e));
            let target = e.and_then(|e| e.targets.iter().next().copied());
            let stream = target.and_then(|t| stream_from(f, t));
            if entry.is_some() {
                own += 1;
                entries_seen.insert(entry);
            }
            if stream.is_some() {
                found += 1;
            }
            targets_seen.extend(target);
            per_run.push(RunEntry {
                key: *k,
                entry,
                index: e.map(|e| e.index),
                target,
                stream,
            });
        }
        out.push(LevelLink {
            link: key,
            runs: used.len() as u32,
            own_entry: own,
            distinct_entries: entries_seen.len() as u32,
            distinct_targets: targets_seen.len() as u32,
            stream_found: found,
            per_run,
        });
    }
    out
}

static EMPTY_LINK: std::sync::LazyLock<LinkStat> = std::sync::LazyLock::new(LinkStat::default);

/// The stream a run read from `target`: the extent, over the run's indirect
/// sites, of the segment whose base is `target`.
fn stream_from(f: &DataFacts, target: u32) -> Option<(u32, u32)> {
    f.sites
        .values()
        .filter(|s| s.indirect)
        .filter_map(|s| s.segments.get(&target))
        .map(|seg| (seg.lo, seg.hi - seg.lo))
        .max_by_key(|&(_, n)| n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rd(addr: u32, value: u8) -> BusAccess {
        BusAccess {
            addr,
            value,
            write: false,
        }
    }

    fn wr(addr: u32, value: u8) -> BusAccess {
        BusAccess {
            addr,
            value,
            write: true,
        }
    }

    fn nes_reader() -> DataReader {
        let mut r = DataReader::new(Platform::Nes);
        r.set_resolver(Box::new(|a| (a >= 0x8000).then(|| a - 0x8000)));
        r
    }

    fn step(r: &mut DataReader, acc: &[BusAccess]) {
        r.step(StepFacts::default(), acc);
    }

    /// `LDA abs,X` at `pc` reading ROM address `base + x` (value `v`).
    fn lda_abs_x(pc: u32, base: u16, x: u16, v: u8) -> Vec<BusAccess> {
        vec![
            rd(pc, 0xBD),
            rd(pc + 1, (base & 0xFF) as u8),
            rd(pc + 2, (base >> 8) as u8),
            rd(u32::from(base) + u32::from(x), v),
        ]
    }

    /// `LDA (zp),Y` at `pc`, pointer at `zp`, reading ROM `ptr + y`.
    fn lda_ind_y(pc: u32, zp: u8, ptr: u16, y: u16, v: u8) -> Vec<BusAccess> {
        vec![
            rd(pc, 0xB1),
            rd(pc + 1, zp),
            rd(u32::from(zp), (ptr & 0xFF) as u8),
            rd(u32::from(zp) + 1, (ptr >> 8) as u8),
            rd(u32::from(ptr) + u32::from(y), v),
        ]
    }

    #[test]
    fn an_indexed_read_is_classified_with_its_base_and_index() {
        let mut r = nes_reader();
        for x in [0u16, 2, 4, 6] {
            step(&mut r, &lda_abs_x(0x9000, 0x8100, x, 0x11));
        }
        let f = r.take_facts();
        let s = &f.sites[&0x1000];
        assert!(!s.indirect);
        assert_eq!(s.hits, 4);
        assert_eq!(s.indices.iter().copied().collect::<Vec<_>>(), [0, 2, 4, 6]);
        assert_eq!(s.segments[&0x100].lo, 0x100);
        assert_eq!(s.segments[&0x100].hi, 0x107);
        assert_eq!(f.stats.suspect_index, 0);
        let regions = infer(&f);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].kind, RegionKind::ElementTable);
        assert_eq!(
            (regions[0].start, regions[0].size, regions[0].stride),
            (0x100, 7, 2)
        );
    }

    #[test]
    fn a_page_cross_dummy_read_does_not_become_the_data_read() {
        let mut r = nes_reader();
        let mut acc = lda_abs_x(0x9000, 0x80F0, 0x20, 0x33);
        // The dummy read of the partially-added address comes first.
        acc.insert(3, rd(0x8010, 0x00));
        step(&mut r, &acc);
        let f = r.take_facts();
        assert_eq!(f.sites[&0x1000].segments[&0xF0].lo, 0x110);
    }

    #[test]
    fn a_jsr_operand_fetched_after_the_pushes_is_not_a_data_read() {
        let mut r = nes_reader();
        step(
            &mut r,
            &[
                rd(0x8000, 0x20),
                rd(0x8001, 0x00),
                rd(0x01FD, 0),
                wr(0x01FD, 0x80),
                wr(0x01FC, 0x02),
                rd(0x8002, 0x90),
            ],
        );
        assert!(r.take_facts().sites.is_empty());
    }

    #[test]
    fn a_stream_read_through_a_pointer_is_recorded_per_base() {
        let mut r = nes_reader();
        for y in 0u16..12 {
            step(&mut r, &lda_ind_y(0x9000, 0x10, 0x8400, y, 0x55));
        }
        let f = r.take_facts();
        let s = &f.sites[&0x1000];
        assert!(s.indirect);
        assert_eq!(s.sequential, 11);
        assert_eq!(s.segments[&0x400].hi, 0x400 + 12);
        assert_eq!(top_ram(&s.pointer_ram, s.hits, 0.5), Some(0x10));
        let regions = infer(&f);
        assert_eq!(regions[0].kind, RegionKind::Stream);
        assert_eq!((regions[0].start, regions[0].size), (0x400, 12));
        assert_eq!(regions[0].pointer_ram, Some(0x10));
    }

    /// `LDA lo,Y; STA $10; LDA hi,Y; STA $11; LDA ($10),Y` for entry `i`.
    fn load_pointer(r: &mut DataReader, i: u16, lo_tab: u16, hi_tab: u16, ptr: u16) {
        let mut a = lda_abs_x(0x9000, lo_tab, i, (ptr & 0xFF) as u8);
        a.push(wr(0x10, (ptr & 0xFF) as u8));
        step(r, &a);
        let mut b = lda_abs_x(0x9010, hi_tab, i, (ptr >> 8) as u8);
        b.push(wr(0x11, (ptr >> 8) as u8));
        step(r, &b);
        for y in 0u16..6 {
            step(r, &lda_ind_y(0x9020, 0x10, ptr, y, 0x40 + y as u8));
        }
    }

    #[test]
    fn split_pointer_tables_are_linked_to_the_streams_they_point_at() {
        let mut r = nes_reader();
        for (i, ptr) in [0x8800u16, 0x8900, 0x8A40].into_iter().enumerate() {
            load_pointer(&mut r, i as u16, 0x8200, 0x8210, ptr);
        }
        let f = r.take_facts();
        assert_eq!(f.stats.links, 18, "one per pointer use");
        let regions = infer(&f);
        let kinds: Vec<_> = regions
            .iter()
            .map(|r| (r.kind, r.part, r.start, r.size))
            .collect();
        assert!(
            kinds.contains(&(RegionKind::PointerTable, Some(Part::Lo), 0x200, 3)),
            "{kinds:?}"
        );
        assert!(
            kinds.contains(&(RegionKind::PointerTable, Some(Part::Hi), 0x210, 3)),
            "{kinds:?}"
        );
        let streams: Vec<_> = regions
            .iter()
            .filter(|r| r.kind == RegionKind::Stream)
            .collect();
        assert_eq!(streams.len(), 3);
        assert!(streams
            .iter()
            .all(|s| s.via.as_deref() == Some("ptrtab_000200_lo")));
    }

    #[test]
    fn interleaved_pointer_tables_are_one_region() {
        let mut r = nes_reader();
        for (i, ptr) in [0x8800u16, 0x8900, 0x8A40].into_iter().enumerate() {
            // Word table: entry i at 0x8200 + 2i, indexed by 2i.
            load_pointer(&mut r, 2 * i as u16, 0x8200, 0x8201, ptr);
        }
        let regions = infer(&r.take_facts());
        let t = regions
            .iter()
            .find(|r| r.kind == RegionKind::PointerTable)
            .expect("table");
        assert_eq!((t.start, t.size, t.stride, t.part), (0x200, 6, 2, None));
        assert_eq!(t.pointer_bytes, 2);
    }

    #[test]
    fn a_coincidental_value_match_is_not_a_pointer_table() {
        // A byte read from a table and a different, unrelated table one byte
        // later whose distance changes from entry to entry.
        let mut r = nes_reader();
        for (i, (lo_tab, hi_tab)) in [(0x8200u16, 0x8210u16), (0x8200, 0x8230), (0x8200, 0x8250)]
            .into_iter()
            .enumerate()
        {
            load_pointer(&mut r, i as u16, lo_tab, hi_tab, 0x8800 + 0x100 * i as u16);
        }
        let regions = infer(&r.take_facts());
        assert!(regions.iter().all(|r| r.kind != RegionKind::PointerTable));
    }

    #[test]
    fn a_rom_value_written_to_the_ppu_data_port_marks_graphics() {
        let mut r = nes_reader();
        for x in 0u16..16 {
            let mut a = lda_abs_x(0x9000, 0x8300, x, 0x20 + x as u8);
            a.push(wr(0x2007, 0x20 + x as u8));
            step(&mut r, &a);
        }
        let regions = infer(&r.take_facts());
        assert_eq!(regions[0].kind, RegionKind::Graphics);
        assert_eq!((regions[0].start, regions[0].size), (0x300, 16));
    }

    #[test]
    fn the_index_source_is_the_ram_byte_read_just_before() {
        let mut r = nes_reader();
        for i in 0u16..6 {
            // LDX $0300 then LDA tab,X
            step(
                &mut r,
                &[
                    rd(0x9000, 0xAE),
                    rd(0x9001, 0),
                    rd(0x9002, 3),
                    rd(0x0300, (i * 2) as u8),
                ],
            );
            step(&mut r, &lda_abs_x(0x9003, 0x8100, i * 2, 7));
        }
        let regions = infer(&r.take_facts());
        let t = regions
            .iter()
            .find(|r| r.kind == RegionKind::ElementTable)
            .expect("table");
        assert_eq!(t.index_ram, Some(0x0300 & 0x07FF));
    }

    #[test]
    fn merged_facts_union_offsets_and_add_counts() {
        let mut a = nes_reader();
        step(&mut a, &lda_abs_x(0x9000, 0x8100, 0, 1));
        let mut b = nes_reader();
        step(&mut b, &lda_abs_x(0x9000, 0x8100, 4, 1));
        let mut fa = a.take_facts();
        fa.merge(&b.take_facts());
        let s = &fa.sites[&0x1000];
        assert_eq!(s.hits, 2);
        assert_eq!(s.indices.len(), 2);
        assert_eq!(s.segments[&0x100].reads, 2);
        assert_eq!(fa.stats.data_reads, 2);
    }

    #[test]
    fn levels_that_use_different_entries_are_told_apart() {
        let run = |entry: u16, ptr: u16| {
            let mut r = nes_reader();
            // A shared lookup every run does, and the level's own entry.
            load_pointer(&mut r, 0, 0x8200, 0x8210, 0x8700);
            load_pointer(&mut r, entry, 0x8200, 0x8210, ptr);
            r.take_facts()
        };
        let a = run(1, 0x8800);
        let b = run(2, 0x8900);
        let c = run(3, 0x8A00);
        let links = associate(&[(10, &a), (11, &b), (12, &c)]);
        assert_eq!(links.len(), 1);
        let l = &links[0];
        assert_eq!(
            (l.runs, l.own_entry, l.distinct_entries, l.distinct_targets),
            (3, 3, 3, 3)
        );
        assert_eq!(l.stream_found, 3);
        assert!(l.tracks_level());
        assert_eq!(l.per_run[1].index, Some(2));
        assert_eq!(
            l.per_run[1].target,
            Some(0x800 + 0x100 - 0x100 + 0x100 - 0x100 + 0x100)
        );
    }

    #[test]
    fn levels_that_share_an_entry_do_not_track() {
        let run = |ptr: u16| {
            let mut r = nes_reader();
            load_pointer(&mut r, 1, 0x8200, 0x8210, ptr);
            r.take_facts()
        };
        let a = run(0x8800);
        let b = run(0x8800);
        let links = associate(&[(1, &a), (2, &b)]);
        assert!(links.iter().all(|l| !l.tracks_level()));
    }

    #[test]
    fn gcd_and_stride() {
        assert_eq!(gcd(12, 18), 6);
        assert_eq!(gcd(0, 5), 5);
        let s: BTreeSet<u32> = [0, 3, 9, 12].into_iter().collect();
        assert_eq!(stride_of(&s), 3);
        assert_eq!(stride_of(&BTreeSet::from([4])), 0);
    }
}
