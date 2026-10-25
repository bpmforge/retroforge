//! The atlas builder (ticket W36-02).
//!
//! ## Pipeline
//!
//! 1. **Instructions.** Every executed opcode offset is an instruction
//!    (`Observed`). From those, from interrupt targets and from recorded
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
//! ## Why it terminates (law 8)
//!
//! There is no hand-indexed loop. The instruction worklist pushes only when
//! it creates a new node and nodes are keyed by ROM offset in a visited
//! table, so it does at most `2 * prg_len` pushes; block chains place a
//! previously unplaced node on every step; function walks stamp each block
//! once. A step budget on the worklist and on the walks is a second line of
//! defence: if either trips, [`Stats::budget_hit`] is set (the SMB test
//! asserts it never is).

use std::collections::{BTreeMap, BTreeSet};

use rf_core_api::coverage::{CoverageLog, IndirectKind, OPCODE};

use crate::atlas::{
    entry, Atlas, Block, Edge, EdgeKind, Function, IndirectSite, InsnRec, Provenance, SiteKind,
    Stats, Target, VolatileRegion, Why,
};
use crate::isa::{DecodeMode, Flow, Isa};
use crate::superset::superset_decode;
use crate::windows::WindowIndex;

const NONE: u32 = u32::MAX;
/// Ceiling on function-walk steps over the whole build. Generous: a random
/// 32 KiB image needs a few million.
const WALK_STEP_BUDGET: u64 = 1 << 28;

struct Node {
    rom: u32,
    cpu: u32,
    len: u8,
    flow: Flow,
    prov: Provenance,
    succ: [Option<Edge>; 2],
}

/// Build the atlas for `prg` (the cartridge's PRG ROM) from a recording.
#[must_use]
pub fn build_atlas(isa: &dyn Isa, prg: &[u8], log: &CoverageLog) -> Atlas {
    let Some(n) = u32::try_from(prg.len()).ok().filter(|&n| n < NONE) else {
        return Atlas {
            prg_len: 0,
            blocks: Vec::new(),
            functions: Vec::new(),
            indirect_sites: Vec::new(),
            volatile: Vec::new(),
            stats: Stats::default(),
        };
    };
    let flag = |rom: u32| log.prg_flags.get(rom as usize).copied().unwrap_or(0);
    let mut stats = Stats::default();
    let mut win = WindowIndex::new(&log.windows);
    let mask = isa.addr_mask();
    let max_len = isa.max_len().max(1);
    let decode_at = |rom: u32, cpu: u32| {
        let start = rom as usize;
        let end = (start + max_len).min(prg.len());
        isa.decode(&prg[start..end], cpu, DecodeMode::default())
    };

    // ---- 1. seeds --------------------------------------------------------
    let mut entry_flags: BTreeMap<u32, u8> = BTreeMap::new();
    let mut entry_seeds: Vec<(u32, u32)> = Vec::new();
    let mut rti_targets: BTreeSet<u32> = BTreeSet::new();
    for i in &log.interrupts {
        if let Some(rom) = i.target.rom.filter(|&r| r < n) {
            *entry_flags.entry(rom).or_insert(0) |= entry::INTERRUPT;
            entry_seeds.push((rom, u32::from(i.target.cpu)));
        }
    }
    // A plain RTS return lands 3 bytes after a JSR that really ran. Those
    // are mid-function; an RTS-trick target (pushed address - 1) has no such
    // JSR before it and stays an entry.
    let plain_return = |rom: u32| {
        rom >= 3
            && flag(rom - 3) & OPCODE != 0
            && matches!(
                decode_at(rom - 3, 0),
                Some(d) if d.len == 3 && matches!(d.flow, Flow::Call { .. })
            )
    };
    for e in &log.indirect {
        let Some(rom) = e.target.rom.filter(|&r| r < n) else {
            continue;
        };
        entry_seeds.push((rom, u32::from(e.target.cpu)));
        match e.kind {
            IndirectKind::JmpIndirect => *entry_flags.entry(rom).or_insert(0) |= entry::INDIRECT,
            IndirectKind::Brk => *entry_flags.entry(rom).or_insert(0) |= entry::INTERRUPT,
            IndirectKind::Rts => {
                if !plain_return(rom) {
                    *entry_flags.entry(rom).or_insert(0) |= entry::INDIRECT;
                }
            }
            // An RTI resumes the code an interrupt cut into: a point in
            // the middle of some function, not the start of one. It is
            // still a decode seed (above) and a block leader, never an
            // entry.
            IndirectKind::Rti => {
                rti_targets.insert(rom);
            }
        }
    }

    // ---- 2. instruction closure -------------------------------------------
    let mut idx = vec![NONE; n as usize];
    let mut nodes: Vec<Node> = Vec::new();
    // Low priority first (popped last): every executed opcode, at the CPU
    // address the windows say it was mapped at.
    let mut stack: Vec<(u32, u32)> = (0..n)
        .filter(|&r| flag(r) & OPCODE != 0)
        .map(|r| (r, win.cpu_for_rom(r).unwrap_or(0x8000 + (r & 0x7FFF))))
        .collect();
    stack.extend(entry_seeds.iter().copied());
    let budget = 16 * u64::from(n) + 4096;
    let mut steps = 0u64;
    while let Some((rom, cpu)) = stack.pop() {
        steps += 1;
        if steps > budget {
            stats.budget_hit = true;
            break;
        }
        if rom >= n || idx[rom as usize] != NONE {
            continue;
        }
        let observed = flag(rom) & OPCODE != 0;
        let Some(d) = decode_at(rom, cpu) else {
            if observed {
                stats.undecodable_observed += 1;
            }
            continue;
        };
        if !observed && (rom..rom + u32::from(d.len)).any(|r| flag(r) != 0) {
            // Static decode must not contradict what actually ran.
            stats.overlap_rejected += 1;
            continue;
        }
        let len = u32::from(d.len);
        let fall = |win: &mut WindowIndex<'_>, kind| fall_edge(win, mask, n, rom, cpu, len, kind);
        let stat = |win: &mut WindowIndex<'_>, kind, t| static_edge(win, n, rom, cpu, t, kind);
        let succ = match d.flow {
            Flow::Fall => [Some(fall(&mut win, EdgeKind::Fall)), None],
            Flow::Branch { target } => [
                Some(stat(&mut win, EdgeKind::Taken, target)),
                Some(fall(&mut win, EdgeKind::Fall)),
            ],
            Flow::Jump { target } => [Some(stat(&mut win, EdgeKind::Jump, target)), None],
            Flow::Call { target } => [
                Some(stat(&mut win, EdgeKind::Call, target)),
                Some(fall(&mut win, EdgeKind::AfterCall)),
            ],
            Flow::IndirectJump
            | Flow::Return
            | Flow::ReturnInterrupt
            | Flow::Break
            | Flow::Halt => [None, None],
        };
        for t in succ.iter().flatten() {
            if let Target::Rom { cpu, rom } = t.to {
                stack.push((rom, cpu));
            }
        }
        idx[rom as usize] = nodes.len() as u32;
        nodes.push(Node {
            rom,
            cpu,
            len: d.len,
            flow: d.flow,
            prov: if observed {
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
        for e in nd.succ.iter_mut().flatten() {
            if let Target::Rom { cpu, rom } = e.to {
                if idx[rom as usize] == NONE {
                    e.to = Target::Unresolved {
                        cpu,
                        why: Why::Undecodable,
                    };
                }
            }
        }
    }

    // ---- 3. entries ----------------------------------------------------------
    for nd in &nodes {
        if let (Flow::Call { .. }, Some(e)) = (nd.flow, nd.succ[0]) {
            if let Some(rom) = e.to.rom() {
                *entry_flags.entry(rom).or_insert(0) |= entry::CALL;
            }
        }
    }
    entry_flags.retain(|&rom, _| idx[rom as usize] != NONE);
    stats.rti_entries = rti_targets
        .iter()
        .filter(|r| entry_flags.contains_key(r))
        .count() as u32;

    // ---- 4. leaders and blocks --------------------------------------------------
    let mut leader = vec![false; nodes.len()];
    let mut plain_in = vec![false; nodes.len()];
    // RTI targets cut blocks (control arrives there from outside) without
    // being entries; see the indirect-edge loop above.
    for &rom in &rti_targets {
        if let Some(&i) = idx.get(rom as usize).filter(|&&i| i != NONE) {
            leader[i as usize] = true;
        }
    }
    for &rom in entry_flags.keys() {
        leader[idx[rom as usize] as usize] = true;
    }
    for nd in &nodes {
        for e in nd.succ.iter().flatten() {
            if let Some(rom) = e.to.rom() {
                let j = idx[rom as usize] as usize;
                if e.kind == EdgeKind::Fall && nd.flow == Flow::Fall {
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
    order.sort_by_key(|&i| nodes[i].rom);
    let mut placed = vec![false; nodes.len()];
    let mut blocks: Vec<Block> = Vec::new();
    let finish = |e: Edge, src: Provenance| -> Edge {
        let dst = e.to.rom().map(|r| nodes[idx[r as usize] as usize].prov);
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
                });
                if nd.flow != Flow::Fall {
                    b.edges
                        .extend(nd.succ.iter().flatten().map(|&e| finish(e, nd.prov)));
                    break;
                }
                let Some(e) = nd.succ[0] else { break };
                match e.to.rom() {
                    Some(r) => {
                        let j = idx[r as usize] as usize;
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
    blocks.sort_by_key(|b| b.start_rom);
    let mut block_at = vec![NONE; n as usize];
    for (i, b) in blocks.iter().enumerate() {
        block_at[b.start_rom as usize] = i as u32;
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
                let Some(r) = e.to.rom() else { continue };
                let t = block_at[r as usize];
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
        let mut bytes: Vec<(u32, u32, u8)> = out
            .iter()
            .flat_map(|&b| blocks[b as usize].insns.iter())
            .map(|i| (i.cpu, i.rom, i.len))
            .collect();
        bytes.sort_unstable();
        let mut hash = FNV_OFFSET;
        let mut byte_len = 0u32;
        for &(_, rom, len) in &bytes {
            let s = rom as usize;
            for &b in &prg[s..(s + usize::from(len)).min(prg.len())] {
                hash = (hash ^ u64::from(b)).wrapping_mul(FNV_PRIME);
            }
            byte_len += u32::from(len);
        }
        Function {
            entry_rom: eb.start_rom,
            entry_cpu: eb.start_cpu,
            entry_kinds: kinds,
            blocks: out,
            insn_count: bytes.len() as u32,
            byte_len,
            byte_hash: hash,
        }
    };
    for (&rom, &kinds) in &entry_flags {
        let fid = functions.len() as u32;
        functions.push(walk(
            block_at[rom as usize],
            kinds,
            fid,
            &mut covered,
            &mut stats,
        ));
    }
    let mut has_pred = vec![false; blocks.len()];
    for b in &blocks {
        for e in b.edges.iter().filter(|e| e.kind.stays_in_function()) {
            if let Some(r) = e.to.rom() {
                if block_at[r as usize] != NONE {
                    has_pred[block_at[r as usize] as usize] = true;
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
    functions.sort_by_key(|f| (f.entry_rom, f.entry_kinds));

    // ---- 6. sites, volatile regions, stats --------------------------------------------------
    let mut sites: BTreeMap<(u32, SiteKind), (u32, BTreeSet<Target>)> = BTreeMap::new();
    let mut ram_sites: BTreeSet<(u16, SiteKind)> = BTreeSet::new();
    for e in &log.indirect {
        let kind = match e.kind {
            IndirectKind::JmpIndirect => SiteKind::JmpIndirect,
            IndirectKind::Rts => SiteKind::Rts,
            IndirectKind::Rti => SiteKind::Rti,
            IndirectKind::Brk => SiteKind::Brk,
        };
        let Some(site) = e.site.rom.filter(|&r| r < n) else {
            ram_sites.insert((e.site.cpu, kind));
            continue;
        };
        let target = match e.target.rom.filter(|&r| r < n) {
            Some(rom) => Target::Rom {
                cpu: u32::from(e.target.cpu),
                rom,
            },
            None => Target::Unresolved {
                cpu: u32::from(e.target.cpu),
                why: Why::NotRom,
            },
        };
        sites
            .entry((site, kind))
            .or_insert_with(|| (u32::from(e.site.cpu), BTreeSet::new()))
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
        for e in nd.succ.iter().flatten() {
            if let Target::Unresolved { why, .. } = e.to {
                stats.unresolved_static += 1;
                stats.ambiguous_static += u32::from(why == Why::Ambiguous);
            }
        }
    }
    let mut start_in_fn = vec![false; n as usize];
    for (b, blk) in blocks.iter().enumerate() {
        if covered[b] {
            for i in &blk.insns {
                start_in_fn[i.rom as usize] = true;
            }
        }
    }
    stats.observed_uncovered = (0..n)
        .filter(|&r| flag(r) & OPCODE != 0 && !start_in_fn[r as usize])
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
        blocks,
        functions,
        indirect_sites,
        volatile,
        stats,
    }
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn resolve(win: &mut WindowIndex<'_>, cpu: u32, rom: u32, target: u32, n: u32) -> Target {
    match win.resolve(cpu, rom, target, n) {
        Ok(r) => Target::Rom {
            cpu: target,
            rom: r,
        },
        Err(why) => Target::Unresolved { cpu: target, why },
    }
}

fn is_prg_cpu(addr: u32) -> bool {
    u16::try_from(addr)
        .ok()
        .and_then(rf_core_api::coverage::WindowMap::page_of)
        .is_some()
}

/// The edge to the instruction after the one at (`cpu`, `rom`). Inside a
/// 4 KiB page that is simply the next ROM byte; across a page boundary the
/// next page may map elsewhere, so it is resolved like any other address.
fn fall_edge(
    win: &mut WindowIndex<'_>,
    mask: u32,
    n: u32,
    rom: u32,
    cpu: u32,
    len: u32,
    kind: EdgeKind,
) -> Edge {
    let next = (cpu + len) & mask;
    let to = if next > cpu && next >> 12 == cpu >> 12 {
        if rom + len < n {
            Target::Rom {
                cpu: next,
                rom: rom + len,
            }
        } else {
            Target::Unresolved {
                cpu: next,
                why: Why::OutOfImage,
            }
        }
    } else {
        resolve(win, cpu, rom, next, n)
    };
    Edge {
        kind,
        to,
        prov: Provenance::Inferred,
    }
}

fn static_edge(
    win: &mut WindowIndex<'_>,
    n: u32,
    rom: u32,
    cpu: u32,
    target: u32,
    kind: EdgeKind,
) -> Edge {
    let to = if is_prg_cpu(target) && target >> 12 == cpu >> 12 {
        let r = i64::from(rom) + i64::from(target) - i64::from(cpu);
        match u32::try_from(r).ok().filter(|&r| r < n) {
            Some(r) => Target::Rom {
                cpu: target,
                rom: r,
            },
            None => Target::Unresolved {
                cpu: target,
                why: Why::OutOfImage,
            },
        }
    } else {
        resolve(win, cpu, rom, target, n)
    };
    Edge {
        kind,
        to,
        prov: Provenance::Inferred,
    }
}
