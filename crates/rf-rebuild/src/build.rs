//! The atlas builder (ticket W36-02).
//!
//! ## Pipeline
//!
//! 1. **Instructions.** Every executed opcode `(offset, mode)` is an
//!    instruction (`Observed`; the mode is the 65816 width state the
//!    recording saw it run in, always 0 on the 6502). From those, from interrupt targets and from recorded
//!    indirect targets, a worklist follows static control flow into bytes the
//!    recording never ran (`Inferred`). Targets are placed in the ROM image
//!    by [`WindowIndex`]; one that cannot be placed is an
//!    unresolved edge, not a guess.
//! 2. **Entries.** Interrupt targets, recorded indirect targets that are
//!    not plain RTS returns, and targets of decoded absolute calls.
//! 3. **Blocks.** Leaders are entries, branch/jump/call targets, and the
//!    instruction after a branch or call; blocks are cut at leaders in a
//!    second linear pass. A block is shared by every function that reaches it.
//! 4. **Functions.** An entry plus every block reachable without entering
//!    a callee. Executed code no entry reaches (typically entered from RAM
//!    code, whose jumps the recorder does not log) becomes an `ORPHAN`
//!    function so that every executed instruction belongs to one.
//!
//! ## Width modes (ticket W36-08)
//!
//! Nodes are keyed `(ROM offset, mode)`, so the same bytes decoded in two
//! widths are two instruction streams and two functions. The mode of a
//! successor is, in order: what the instruction says it becomes
//! ([`ModeAfter::Set`], a `REP`/`SEP` with its immediate), the current mode
//! ([`ModeAfter::Same`]), or, when the bytes cannot say
//! ([`ModeAfter::Unknown`], `XCE`/`PLP`), one node per mode the recording saw
//! the successor run in; with none observed the edge is
//! [`Why::WidthUnknown`], never a guess. Calls enter the callee in the
//! caller's mode and the instruction after a call is also entered in every
//! mode it was observed in, since a callee may change widths before it
//! returns. A static decode that did not execute is only accepted when it
//! cannot contradict what ran: if the offset ran in other modes, it must
//! decode to the same length as one of them; otherwise none of its bytes may
//! have executed.
//!
//! ## Why it terminates (law 8)
//!
//! There is no hand-indexed loop. The instruction worklist pushes only when
//! it creates a new node and nodes are keyed by ROM offset in a visited
//! table, so it does at most `2 * prg_len * modes` pushes; block chains place a
//! previously unplaced node on every step; function walks stamp each block
//! once. A step budget on the worklist and on the walks is a second line of
//! defence: if either trips, [`Stats::budget_hit`] is set (the SMB test
//! asserts it never is).

use std::collections::{BTreeMap, BTreeSet};

use rf_core_api::coverage::{
    mode_bit, CoverageLog, IndirectKind, MODE_BITS, MODE_COUNT, MODE_SHIFT, OPCODE,
};

use crate::atlas::{
    entry, Atlas, Block, Edge, EdgeKind, Function, IndirectSite, InsnRec, Provenance, SiteKind,
    Stats, Target, VolatileRegion, Why,
};
use crate::isa::{DecodeMode, Flow, Isa, ModeAfter};
use crate::superset::superset_decode;
use crate::windows::WindowIndex;

const NONE: u32 = u32::MAX;
/// Ceiling on function-walk steps over the whole build. Generous: a random
/// 32 KiB image needs a few million.
const WALK_STEP_BUDGET: u64 = 1 << 28;

struct Node {
    rom: u32,
    cpu: u32,
    mode: u8,
    len: u8,
    flow: Flow,
    prov: Provenance,
    succ: Vec<Edge>,
}

/// Placing CPU addresses in the ROM image: through the recorded bank windows
/// (NES) or the ISA's fixed cartridge mapping (SNES).
struct Resolver<'a> {
    isa: &'a dyn Isa,
    win: WindowIndex<'a>,
    n: u32,
    mask: u32,
    fixed: bool,
}

impl Resolver<'_> {
    fn placed(&self, cpu: u32) -> Result<u32, Why> {
        match self.isa.rom_offset(cpu) {
            Some(r) if r < self.n => Ok(r),
            Some(_) => Err(Why::OutOfImage),
            None => Err(Why::NotRom),
        }
    }

    /// The address a static `Branch`/`Jump`/`Call` operand names: it keeps
    /// the site's bits above the ISA's address mask (the program bank).
    fn in_bank(&self, site_cpu: u32, target: u32) -> u32 {
        (site_cpu & !self.mask) | (target & self.mask)
    }

    /// ROM offset of static target `target` for a site at (`cpu`, `rom`).
    fn static_target(&mut self, cpu: u32, rom: u32, target: u32) -> Result<u32, Why> {
        if self.fixed {
            return self.placed(target);
        }
        if is_prg_cpu(target) && target >> 12 == cpu >> 12 {
            let r = i64::from(rom) + i64::from(target) - i64::from(cpu);
            return u32::try_from(r)
                .ok()
                .filter(|&r| r < self.n)
                .ok_or(Why::OutOfImage);
        }
        self.win.resolve(cpu, rom, target, self.n)
    }

    /// The address and ROM offset of the instruction after the one at
    /// (`cpu`, `rom`). The program bank never changes by falling through
    /// (a fetch at `$xx:FFFF` is followed by `$xx:0000`). Inside a 4 KiB
    /// window page that is simply the next ROM byte; across a page boundary
    /// the next page may map elsewhere, so it is resolved like any address.
    fn fall(&mut self, cpu: u32, rom: u32, len: u32) -> (u32, Result<u32, Why>) {
        let next = (cpu & !self.mask) | ((cpu + len) & self.mask);
        if self.fixed {
            return (next, self.placed(next));
        }
        if next > cpu && next >> 12 == cpu >> 12 {
            let r = if rom + len < self.n {
                Ok(rom + len)
            } else {
                Err(Why::OutOfImage)
            };
            return (next, r);
        }
        (next, self.win.resolve(cpu, rom, next, self.n))
    }
}

/// Build the atlas for `prg` (the cartridge's PRG ROM) from a recording.
#[must_use]
pub fn build_atlas(isa: &dyn Isa, prg: &[u8], log: &CoverageLog) -> Atlas {
    let k = isa.mode_count().clamp(1, MODE_COUNT);
    let Some(n) = u32::try_from(prg.len())
        .ok()
        .filter(|&n| n < NONE / k as u32)
    else {
        return Atlas {
            prg_len: 0,
            mode_count: k as u8,
            blocks: Vec::new(),
            functions: Vec::new(),
            indirect_sites: Vec::new(),
            volatile: Vec::new(),
            stats: Stats::default(),
        };
    };
    let key = |rom: u32, mode: u8| rom as usize * k + usize::from(mode);
    let flag = |rom: u32| log.prg_flags.get(rom as usize).copied().unwrap_or(0);
    // Bit `m` set: the opcode at `rom` ran in mode `m`. A 6502 recording
    // carries no mode bits and means mode 0.
    let obs_mask = |rom: u32| -> u8 {
        let f = flag(rom);
        if f & OPCODE == 0 {
            0
        } else if f & MODE_BITS == 0 {
            1
        } else {
            (f & MODE_BITS) >> MODE_SHIFT
        }
    };
    let observed = |rom: u32, mode: u8| obs_mask(rom) >> mode & 1 != 0;
    let mut stats = Stats::default();
    let mut res = Resolver {
        isa,
        win: WindowIndex::new(&log.windows),
        n,
        mask: isa.addr_mask(),
        fixed: isa.fixed_mapping(),
    };
    let max_len = isa.max_len().max(1);
    let decode_at = |rom: u32, cpu: u32, mode: u8| {
        let start = rom as usize;
        let end = (start + max_len).min(prg.len());
        isa.decode(&prg[start..end], cpu, DecodeMode(mode))
    };
    let cpu_of = |res: &Resolver<'_>, rom: u32| {
        if res.fixed {
            isa.cpu_for_rom(rom)
        } else {
            res.win.cpu_for_rom(rom).unwrap_or(0x8000 + (rom & 0x7FFF))
        }
    };

    // ---- 1. seeds --------------------------------------------------------
    let mut entry_flags: BTreeMap<(u32, u8), u8> = BTreeMap::new();
    let mut entry_seeds: Vec<(u32, u32, u8)> = Vec::new();
    let mut rti_targets: BTreeSet<(u32, u8)> = BTreeSet::new();
    let cpu24 = |a: &rf_core_api::CodeAddr| (u32::from(a.pbr) << 16) | u32::from(a.cpu);
    let usable = |a: &rf_core_api::CodeAddr| {
        a.rom
            .filter(|&r| r < n)
            .filter(|_| usize::from(a.mode) < k)
            .map(|r| (r, a.mode))
    };
    for i in &log.interrupts {
        if let Some((rom, mode)) = usable(&i.target) {
            let which = match i.kind {
                rf_core_api::coverage::InterruptKind::Reset => entry::RESET,
                rf_core_api::coverage::InterruptKind::Nmi => entry::NMI,
                rf_core_api::coverage::InterruptKind::Irq => entry::IRQ,
            };
            *entry_flags.entry((rom, mode)).or_insert(0) |= entry::INTERRUPT | which;
            entry_seeds.push((rom, cpu24(&i.target), mode));
        }
    }
    // A plain return lands right after the call that really ran: 3 bytes
    // after a `JSR abs`/`JSR (abs,X)` for RTS, 4 after a `JSL` for RTL.
    // Those are mid-function; a trick target (pushed address - 1) has no such
    // call before it and stays an entry.
    let plain_return = |rom: u32, long: bool| {
        let back = if long { 4 } else { 3 };
        rom >= back && {
            let at = rom - back;
            let mask = obs_mask(at);
            (0..k as u8).filter(|&m| mask >> m & 1 != 0).any(|m| {
                matches!(
                    decode_at(at, 0, m),
                    Some(d) if u32::from(d.len) == back
                        && match d.flow {
                            Flow::Call { .. } | Flow::IndirectCall => !long,
                            Flow::CallLong { .. } => long,
                            _ => false,
                        }
                )
            })
        }
    };
    for e in &log.indirect {
        let Some((rom, mode)) = usable(&e.target) else {
            continue;
        };
        entry_seeds.push((rom, cpu24(&e.target), mode));
        let flags = entry_flags.entry((rom, mode)).or_insert(0);
        match e.kind {
            IndirectKind::JmpIndirect | IndirectKind::JsrIndirect => *flags |= entry::INDIRECT,
            IndirectKind::Brk => *flags |= entry::INTERRUPT | entry::BRK,
            IndirectKind::Cop => *flags |= entry::INTERRUPT,
            IndirectKind::Rts | IndirectKind::Rtl => {
                if !plain_return(rom, e.kind == IndirectKind::Rtl) {
                    *flags |= entry::INDIRECT;
                }
            }
            // An RTI resumes the code an interrupt cut into: a point in
            // the middle of some function, not the start of one. It is
            // still a decode seed (above) and a block leader, never an
            // entry.
            IndirectKind::Rti => {
                rti_targets.insert((rom, mode));
            }
        }
        // `or_insert(0)` above created an empty entry for kinds that are
        // not entries; those are dropped here.
        if *flags == 0 {
            entry_flags.remove(&(rom, mode));
        }
    }

    // ---- 2. instruction closure -------------------------------------------
    let mut idx = vec![NONE; n as usize * k];
    let mut nodes: Vec<Node> = Vec::new();
    // Low priority first (popped last): every executed opcode in every mode
    // it ran in, at the CPU address the windows (or the fixed mapping) say.
    let mut stack: Vec<(u32, u32, u8)> = Vec::new();
    for r in (0..n).filter(|&r| flag(r) & OPCODE != 0) {
        let cpu = cpu_of(&res, r);
        let mask = obs_mask(r);
        stack.extend(
            (0..k as u8)
                .filter(|&m| mask >> m & 1 != 0)
                .map(|m| (r, cpu, m)),
        );
    }
    stack.extend(entry_seeds.iter().copied());
    let budget = 16 * u64::from(n) * k as u64 + 4096;
    let mut steps = 0u64;
    while let Some((rom, cpu, mode)) = stack.pop() {
        steps += 1;
        if steps > budget {
            stats.budget_hit = true;
            break;
        }
        if rom >= n || idx[key(rom, mode)] != NONE {
            continue;
        }
        let obs = observed(rom, mode);
        let Some(d) = decode_at(rom, cpu, mode) else {
            if obs {
                stats.undecodable_observed += 1;
            }
            continue;
        };
        if !obs {
            // Static decode must not contradict what actually ran.
            let compatible = if flag(rom) & OPCODE != 0 {
                // Ran in other modes: acceptable only with the same extent.
                let ran = obs_mask(rom);
                (0..k as u8).any(|m| {
                    ran >> m & 1 != 0 && decode_at(rom, cpu, m).is_some_and(|o| o.len == d.len)
                })
            } else {
                !(rom..rom + u32::from(d.len)).any(|r| flag(r) != 0)
            };
            if !compatible {
                stats.overlap_rejected += 1;
                continue;
            }
        }
        let len = u32::from(d.len);
        let after = match d.after {
            ModeAfter::Same => Some(mode),
            ModeAfter::Set(m) => Some(m.0).filter(|&m| usize::from(m) < k),
            ModeAfter::Unknown => None,
        };
        let succ = successors(&mut res, &obs_mask, (rom, cpu, mode, len), d.flow, after, k);
        for t in &succ {
            if let Target::Rom { cpu, rom, mode } = t.to {
                stack.push((rom, cpu, mode));
            }
        }
        idx[key(rom, mode)] = nodes.len() as u32;
        nodes.push(Node {
            rom,
            cpu,
            mode,
            len: d.len,
            flow: d.flow,
            prov: if obs {
                Provenance::Observed
            } else {
                Provenance::Inferred
            },
            succ,
        });
    }
    // Targets the closure never turned into an instruction (rejected,
    // truncated, or cut off by the budget) are not edges into the graph.
    for nd in &mut nodes {
        for e in &mut nd.succ {
            if let Target::Rom { cpu, rom, mode } = e.to {
                if idx[key(rom, mode)] == NONE {
                    e.to = Target::Unresolved {
                        cpu,
                        why: Why::Undecodable,
                    };
                }
            }
        }
    }
    let node_of = |t: &Target| t.node().map(|(r, m)| idx[key(r, m)] as usize);

    // ---- 3. entries ----------------------------------------------------------
    for nd in &nodes {
        if matches!(nd.flow, Flow::Call { .. } | Flow::CallLong { .. }) {
            if let Some(e) = nd.succ.iter().find(|e| e.kind == EdgeKind::Call) {
                if let Some(node) = e.to.node() {
                    *entry_flags.entry(node).or_insert(0) |= entry::CALL;
                }
            }
        }
    }
    entry_flags.retain(|&(rom, mode), _| idx[key(rom, mode)] != NONE);
    stats.rti_entries = rti_targets
        .iter()
        .filter(|r| entry_flags.contains_key(r))
        .count() as u32;

    // ---- 4. leaders and blocks --------------------------------------------------
    let mut leader = vec![false; nodes.len()];
    let mut plain_in = vec![false; nodes.len()];
    // RTI targets cut blocks (control arrives there from outside) without
    // being entries; see the indirect-edge loop above.
    for &(rom, mode) in &rti_targets {
        if let Some(&i) = idx.get(key(rom, mode)).filter(|&&i| i != NONE) {
            leader[i as usize] = true;
        }
    }
    for &(rom, mode) in entry_flags.keys() {
        leader[idx[key(rom, mode)] as usize] = true;
    }
    for nd in &nodes {
        for e in &nd.succ {
            if let Some(j) = node_of(&e.to) {
                if e.kind == EdgeKind::Fall && nd.flow == Flow::Fall && nd.succ.len() == 1 {
                    plain_in[j] = true;
                } else {
                    leader[j] = true;
                }
            }
        }
    }
    for (l, &p) in leader.iter_mut().zip(&plain_in) {
        *l |= !p;
    }
    let mut order: Vec<usize> = (0..nodes.len()).collect();
    order.sort_by_key(|&i| (nodes[i].rom, nodes[i].mode));
    let mut placed = vec![false; nodes.len()];
    let mut blocks: Vec<Block> = Vec::new();
    let finish = |e: Edge, src: Provenance| -> Edge {
        let dst = node_of(&e.to).map(|j| nodes[j].prov);
        Edge {
            prov: if src == Provenance::Observed && dst == Some(Provenance::Observed) {
                Provenance::Observed
            } else {
                Provenance::Inferred
            },
            ..e
        }
    };
    // Pass 0 starts blocks at leaders; pass 1 sweeps up nodes only a
    // leaderless fall-through cycle could leave behind.
    for pass in 0..2 {
        for &start in &order {
            if placed[start] || (pass == 0 && !leader[start]) {
                continue;
            }
            leader[start] = true;
            let mut b = Block {
                start_rom: nodes[start].rom,
                start_cpu: nodes[start].cpu,
                start_mode: nodes[start].mode,
                insns: Vec::new(),
                edges: Vec::new(),
            };
            let mut cur = start;
            loop {
                // Every pass through here places a node nobody placed
                // before (the `continue` below checks), so this ends.
                placed[cur] = true;
                let nd = &nodes[cur];
                b.insns.push(InsnRec {
                    rom: nd.rom,
                    cpu: nd.cpu,
                    len: nd.len,
                    flow: nd.flow,
                    prov: nd.prov,
                    mode: nd.mode,
                });
                if nd.flow != Flow::Fall || nd.succ.len() != 1 {
                    b.edges.extend(nd.succ.iter().map(|&e| finish(e, nd.prov)));
                    break;
                }
                let e = nd.succ[0];
                match node_of(&e.to) {
                    Some(j) => {
                        if leader[j] || placed[j] {
                            b.edges.push(finish(e, nd.prov));
                            break;
                        }
                        cur = j;
                    }
                    None => {
                        b.edges.push(finish(e, nd.prov));
                        break;
                    }
                }
            }
            blocks.push(b);
        }
    }
    blocks.sort_by_key(|b| (b.start_rom, b.start_mode));
    let mut block_at = vec![NONE; n as usize * k];
    for (i, b) in blocks.iter().enumerate() {
        block_at[key(b.start_rom, b.start_mode)] = i as u32;
    }

    // ---- 5. functions -----------------------------------------------------------------
    let mut stamp = vec![NONE; blocks.len()];
    let mut covered = vec![false; blocks.len()];
    let mut walk_steps = 0u64;
    let mut functions: Vec<Function> = Vec::new();
    let mut walk = |entry_block: u32,
                    kinds: u8,
                    fid: u32,
                    covered: &mut [bool],
                    stats: &mut Stats|
     -> Function {
        let mut out: Vec<u32> = Vec::new();
        let mut work = vec![entry_block];
        stamp[entry_block as usize] = fid;
        while let Some(b) = work.pop() {
            walk_steps += 1;
            if walk_steps > WALK_STEP_BUDGET {
                stats.budget_hit = true;
                break;
            }
            out.push(b);
            covered[b as usize] = true;
            for e in &blocks[b as usize].edges {
                if !e.kind.stays_in_function() {
                    continue;
                }
                let Some((r, m)) = e.to.node() else { continue };
                let t = block_at[key(r, m)];
                if t == NONE {
                    stats.dangling_edges += 1;
                } else if stamp[t as usize] != fid {
                    stamp[t as usize] = fid;
                    work.push(t);
                }
            }
        }
        out.sort_unstable();
        let eb = &blocks[entry_block as usize];
        let mut bytes: Vec<(u32, u32, u8, u8)> = out
            .iter()
            .flat_map(|&b| blocks[b as usize].insns.iter())
            .map(|i| (i.cpu, i.rom, i.len, i.mode))
            .collect();
        bytes.sort_unstable();
        let mut hash = FNV_OFFSET;
        let mut shape = FNV_OFFSET;
        let mut byte_len = 0u32;
        for &(_, rom, len, mode) in &bytes {
            let s = rom as usize;
            let insn = &prg[s..(s + usize::from(len)).min(prg.len())];
            let masked = isa.masked_operand(insn, DecodeMode(mode));
            for (i, &b) in insn.iter().enumerate() {
                hash = (hash ^ u64::from(b)).wrapping_mul(FNV_PRIME);
                let kept = if masked.contains(&i) { 0 } else { b };
                shape = (shape ^ u64::from(kept)).wrapping_mul(FNV_PRIME);
            }
            byte_len += u32::from(len);
        }
        Function {
            entry_rom: eb.start_rom,
            entry_cpu: eb.start_cpu,
            entry_mode: eb.start_mode,
            entry_kinds: kinds,
            blocks: out,
            insn_count: bytes.len() as u32,
            byte_len,
            byte_hash: hash,
            shape_hash: shape,
        }
    };
    for (&(rom, mode), &kinds) in &entry_flags {
        let fid = functions.len() as u32;
        functions.push(walk(
            block_at[key(rom, mode)],
            kinds,
            fid,
            &mut covered,
            &mut stats,
        ));
    }
    let mut has_pred = vec![false; blocks.len()];
    for b in &blocks {
        for e in b.edges.iter().filter(|e| e.kind.stays_in_function()) {
            if let Some((r, m)) = e.to.node() {
                if block_at[key(r, m)] != NONE {
                    has_pred[block_at[key(r, m)] as usize] = true;
                }
            }
        }
    }
    // Blocks nothing covers, roots (no predecessor) first so a chain is one
    // function rather than one per block.
    for pass in 0..2 {
        for b in 0..blocks.len() {
            if covered[b] || (pass == 0 && has_pred[b]) {
                continue;
            }
            let fid = functions.len() as u32;
            functions.push(walk(b as u32, entry::ORPHAN, fid, &mut covered, &mut stats));
            covered[b] = true;
            stats.orphan_entries += 1;
        }
    }
    functions.sort_by_key(|f| (f.entry_rom, f.entry_mode, f.entry_kinds));

    // ---- 6. sites, volatile regions, stats --------------------------------------------------
    let mut sites: BTreeMap<(u32, SiteKind), (u32, BTreeSet<Target>)> = BTreeMap::new();
    let mut ram_sites: BTreeSet<(u32, SiteKind)> = BTreeSet::new();
    for e in &log.indirect {
        let kind = match e.kind {
            IndirectKind::JmpIndirect => SiteKind::JmpIndirect,
            IndirectKind::Rts => SiteKind::Rts,
            IndirectKind::Rti => SiteKind::Rti,
            IndirectKind::Brk => SiteKind::Brk,
            IndirectKind::JsrIndirect => SiteKind::JsrIndirect,
            IndirectKind::Rtl => SiteKind::Rtl,
            IndirectKind::Cop => SiteKind::Cop,
        };
        let Some(site) = e.site.rom.filter(|&r| r < n) else {
            ram_sites.insert((cpu24(&e.site), kind));
            continue;
        };
        let target = match usable(&e.target) {
            Some((rom, mode)) => Target::Rom {
                cpu: cpu24(&e.target),
                rom,
                mode,
            },
            None => Target::Unresolved {
                cpu: cpu24(&e.target),
                why: Why::NotRom,
            },
        };
        sites
            .entry((site, kind))
            .or_insert_with(|| (cpu24(&e.site), BTreeSet::new()))
            .1
            .insert(target);
    }
    stats.ram_indirect_sites = ram_sites.len() as u32;
    let indirect_sites = sites
        .into_iter()
        .map(|((site_rom, kind), (site_cpu, targets))| IndirectSite {
            site_rom,
            site_cpu,
            kind,
            targets: targets.into_iter().collect(),
        })
        .collect();

    let mut volatile = Vec::new();
    let mut run: Option<u32> = None;
    for (i, &f) in log.ram_exec.iter().enumerate() {
        match (f != 0, run) {
            (true, None) => run = Some(i as u32),
            (false, Some(s)) => {
                volatile.push(VolatileRegion {
                    start: s,
                    len: i as u32 - s,
                });
                run = None;
            }
            _ => {}
        }
    }
    if let Some(s) = run {
        volatile.push(VolatileRegion {
            start: s,
            len: log.ram_exec.len() as u32 - s,
        });
    }

    for nd in &nodes {
        match nd.prov {
            Provenance::Observed => stats.observed_insns += 1,
            Provenance::Inferred => stats.inferred_insns += 1,
        }
        for e in &nd.succ {
            if let Target::Unresolved { why, .. } = e.to {
                stats.unresolved_static += 1;
                stats.ambiguous_static += u32::from(why == Why::Ambiguous);
            }
        }
    }
    let mut start_in_fn = vec![false; n as usize * k];
    for (b, blk) in blocks.iter().enumerate() {
        if covered[b] {
            for i in &blk.insns {
                start_in_fn[key(i.rom, i.mode)] = true;
            }
        }
    }
    stats.observed_uncovered = (0..n)
        .flat_map(|r| {
            (0..k as u8)
                .filter(move |&m| observed_bit(log, r, m))
                .map(move |m| (r, m))
        })
        .filter(|&(r, m)| !start_in_fn[key(r, m)])
        .count() as u32;
    let sup = superset_decode(isa, prg, log);
    stats.superset_candidates = sup.candidates() as u32;
    stats.superset_in_atlas = sup
        .len
        .iter()
        .zip(&idx)
        .filter(|(&l, &i)| l != 0 && i != NONE)
        .count() as u32;

    Atlas {
        prg_len: n,
        mode_count: k as u8,
        blocks,
        functions,
        indirect_sites,
        volatile,
        stats,
    }
}

/// Did the opcode at `rom` run in mode `mode`? (A recording without mode bits
/// means mode 0.)
fn observed_bit(log: &CoverageLog, rom: u32, mode: u8) -> bool {
    let f = log.prg_flags.get(rom as usize).copied().unwrap_or(0);
    f & OPCODE != 0
        && if f & MODE_BITS == 0 {
            mode == 0
        } else {
            f & mode_bit(mode) != 0
        }
}

/// The successor edges of one decoded instruction at (`rom`, `cpu`, `mode`,
/// `len`); `after` is the mode in force after it, `None` when unknown.
fn successors(
    res: &mut Resolver<'_>,
    obs_mask: &dyn Fn(u32) -> u8,
    (rom, cpu, mode, len): (u32, u32, u8, u32),
    flow: Flow,
    after: Option<u8>,
    k: usize,
) -> Vec<Edge> {
    let mut out = Vec::new();
    // Edges to `target_rom` in each mode it may be entered in.
    let push = |out: &mut Vec<Edge>,
                kind: EdgeKind,
                cpu_t: u32,
                r: Result<u32, Why>,
                assumed: Option<u8>,
                also_observed: bool| {
        let rom_t = match r {
            Ok(r) => r,
            Err(why) => {
                out.push(Edge {
                    kind,
                    to: Target::Unresolved { cpu: cpu_t, why },
                    prov: Provenance::Inferred,
                });
                return;
            }
        };
        let mut mask: u8 = 0;
        match assumed {
            Some(m) => mask |= 1 << m,
            None => mask |= obs_mask(rom_t),
        }
        if also_observed {
            mask |= obs_mask(rom_t);
        }
        mask &= (1u8 << k) - 1;
        if mask == 0 {
            out.push(Edge {
                kind,
                to: Target::Unresolved {
                    cpu: cpu_t,
                    why: Why::WidthUnknown,
                },
                prov: Provenance::Inferred,
            });
            return;
        }
        for m in (0..k as u8).filter(|&m| mask >> m & 1 != 0) {
            out.push(Edge {
                kind,
                to: Target::Rom {
                    cpu: cpu_t,
                    rom: rom_t,
                    mode: m,
                },
                prov: Provenance::Inferred,
            });
        }
    };
    let fall = |res: &mut Resolver<'_>, out: &mut Vec<Edge>, kind, assumed, also| {
        let (next, r) = res.fall(cpu, rom, len);
        push(out, kind, next, r, assumed, also);
    };
    let stat = |res: &mut Resolver<'_>, out: &mut Vec<Edge>, kind, target: u32, long: bool| {
        let t = if long {
            target
        } else {
            res.in_bank(cpu, target)
        };
        let r = res.static_target(cpu, rom, t);
        push(out, kind, t, r, Some(mode), false);
    };
    match flow {
        Flow::Fall => fall(res, &mut out, EdgeKind::Fall, after, false),
        Flow::Branch { target } => {
            stat(res, &mut out, EdgeKind::Taken, target, false);
            fall(res, &mut out, EdgeKind::Fall, after, false);
        }
        Flow::Jump { target } => stat(res, &mut out, EdgeKind::Jump, target, false),
        Flow::JumpLong { target } => stat(res, &mut out, EdgeKind::Jump, target, true),
        Flow::Call { target } => {
            stat(res, &mut out, EdgeKind::Call, target, false);
            fall(res, &mut out, EdgeKind::AfterCall, after, true);
        }
        Flow::CallLong { target } => {
            stat(res, &mut out, EdgeKind::Call, target, true);
            fall(res, &mut out, EdgeKind::AfterCall, after, true);
        }
        Flow::IndirectCall => fall(res, &mut out, EdgeKind::AfterCall, after, true),
        Flow::IndirectJump
        | Flow::Return
        | Flow::ReturnLong
        | Flow::ReturnInterrupt
        | Flow::Break
        | Flow::Halt => {}
    }
    out
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn is_prg_cpu(addr: u32) -> bool {
    u16::try_from(addr)
        .ok()
        .and_then(rf_core_api::coverage::WindowMap::page_of)
        .is_some()
}
