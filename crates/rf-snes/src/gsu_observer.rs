//! Ticket W31-02: a read-only observer of the Super FX's polygon stream.
//!
//! Configured by a plain [`GsuPolyConfig`] (what a profile's
//! `[superfx_polygons]` says; `rf-snes` does not depend on `rf-profiles`),
//! it watches the GSU's RAM reads, `PLOT`s and `STOP`s and turns each render
//! job into a [`PolyJob`]. It never writes machine state (law 4: state
//! hashes are identical with it on or off), is not part of save states, and
//! costs one `Option` check per hook when unset.
//!
//! What it recognises (found by observation, `docs/research/
//! W31-01-starfox-polygon-map.md`):
//!
//! 1. A read of `count_addr` by the instruction at `pc_bank:count_read_pc`
//!    **starts a polygon** (and ends any previous polygon's fill; a polygon
//!    started but never plotted is dropped).
//! 2. RAM reads by instructions in `walker_pcs` (`lo..hi`, half-open, same
//!    bank) from `buffer_addr .. buffer_addr + 0x100` are the edge walker
//!    reading corners. Entries are 4 bytes (x word, y word); `base` is the
//!    entry holding the lowest address read and the corners are the entries
//!    from `base` through the one holding the highest address read.
//! 3. The **first `PLOT` after at least one such read closes the corner
//!    list**: the low bytes of each entry's x and y are snapshotted at that
//!    moment, together with `COLR`/`POR`. A polygon's fill ends at the next
//!    start (rule 1) or at `STOP`; its `PLOT`s are *not* recorded as ops
//!    (but are in [`PolyJob::plotted`]).
//! 4. Any `PLOT` while no polygon is open is a [`PolyOp::Plot`].
//! 5. `STOP` completes the job into a ring of the last [`RING`] jobs.

use std::collections::VecDeque;
use std::sync::Arc;

use rf_core_api::gsu_poly::{PolyJob, PolyOp};

/// Completed jobs kept for [`GsuPolyObserver::take_jobs`].
pub const RING: usize = 3;
/// How far past `buffer_addr` a read still counts as the corner buffer.
const BUFFER_SPAN: u16 = 0x100;

/// Where a game keeps its polygon stream. Mirrors `[superfx_polygons]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GsuPolyConfig {
    /// GSU program bank of the instruction that reads the vertex count.
    pub pc_bank: u8,
    /// Address (in `pc_bank`) of the instruction that reads the count.
    pub count_read_pc: u16,
    /// GSU RAM address of the vertex count.
    pub count_addr: u16,
    /// GSU RAM address of the corner work buffer (4-byte x,y entries).
    pub buffer_addr: u16,
    /// Half-open range of instruction addresses of the edge walker.
    pub walker_pcs: (u16, u16),
    /// The 3D viewport, `(width, height)`.
    pub viewport: (u16, u16),
    /// Also capture the GSU's own bitmap at `STOP` into
    /// [`PolyJob::bitmap`]. A proof/test hook; costs a viewport decode per job.
    pub capture_bitmap: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// No polygon open: `PLOT`s are single-pixel ops.
    Idle,
    /// Count read seen; `span` is the (lowest, highest) buffer entry index
    /// the walker has read so far.
    Collecting { span: Option<(u16, u16)> },
    /// Corners closed; `PLOT`s are this polygon's fill.
    Filling,
}

#[derive(Debug, Clone)]
pub struct GsuPolyObserver {
    cfg: GsuPolyConfig,
    phase: Phase,
    /// `(pbr, address)` of the instruction now executing.
    pc: (u8, u16),
    ops: Vec<PolyOp>,
    plotted: Vec<bool>,
    bpp: u8,
    done: VecDeque<Arc<PolyJob>>,
    completed: u64,
}

impl GsuPolyObserver {
    #[must_use]
    pub fn new(cfg: GsuPolyConfig) -> Self {
        Self {
            cfg,
            phase: Phase::Idle,
            pc: (0, 0),
            ops: Vec::new(),
            plotted: Vec::new(),
            bpp: 4,
            done: VecDeque::new(),
            completed: 0,
        }
    }

    #[must_use]
    pub fn config(&self) -> &GsuPolyConfig {
        &self.cfg
    }

    /// Jobs completed since this observer was created.
    #[must_use]
    pub fn completed(&self) -> u64 {
        self.completed
    }

    /// Remove and return the completed jobs still in the ring, oldest first.
    pub fn take_jobs(&mut self) -> Vec<Arc<PolyJob>> {
        self.done.drain(..).collect()
    }

    /// The instruction about to execute.
    pub(crate) fn set_pc(&mut self, pbr: u8, pc: u16) {
        self.pc = (pbr, pc);
    }

    /// The running instruction read GSU RAM at `addr` (a word read covers
    /// the aligned pair, as the core does).
    pub(crate) fn on_ram_read(&mut self, addr: u16, word: bool) {
        if self.pc.0 != self.cfg.pc_bank {
            return;
        }
        let lo = if word { addr & !1 } else { addr };
        let hi = if word { lo.wrapping_add(1) } else { lo };
        let pc = self.pc.1;
        if pc == self.cfg.count_read_pc && (lo..=hi).contains(&self.cfg.count_addr) {
            self.phase = Phase::Collecting { span: None };
            return;
        }
        let (w_lo, w_hi) = self.cfg.walker_pcs;
        if !(w_lo..w_hi).contains(&pc) {
            return;
        }
        let Some(off) = lo.checked_sub(self.cfg.buffer_addr) else {
            return;
        };
        if off >= BUFFER_SPAN {
            return;
        }
        if let Phase::Collecting { span } = &mut self.phase {
            let slot = off / 4;
            *span = Some(span.map_or((slot, slot), |(a, b)| (a.min(slot), b.max(slot))));
        }
    }

    /// Where the corner snapshot must be read from if a `PLOT` happened now:
    /// `(base address, entry count)`.
    pub(crate) fn corner_span(&self) -> Option<(u16, u16)> {
        match self.phase {
            Phase::Collecting { span: Some((a, b)) } => {
                Some((self.cfg.buffer_addr + 4 * a, b - a + 1))
            }
            _ => None,
        }
    }

    /// A `PLOT` at `(x, y)` with the current `COLR`/`POR`/bpp; `drawn` is
    /// whether it changed a pixel (not transparent). `corners` is the
    /// snapshot for [`Self::corner_span`], supplied by the caller.
    pub(crate) fn on_plot(
        &mut self,
        (x, y): (u8, u8),
        (colr, por, bpp): (u8, u8, u8),
        drawn: bool,
        corners: Option<Vec<(u8, u8)>>,
    ) {
        self.bpp = bpp;
        let (w, h) = self.cfg.viewport;
        if drawn && u16::from(x) < w && u16::from(y) < h {
            if self.plotted.is_empty() {
                self.plotted = vec![false; usize::from(w) * usize::from(h)];
            }
            self.plotted[usize::from(y) * usize::from(w) + usize::from(x)] = true;
        }
        match (self.phase, corners) {
            (Phase::Filling, _) => {}
            (Phase::Collecting { span: Some(_) }, Some(corners)) => {
                self.ops.push(PolyOp::Polygon { corners, colr, por });
                self.phase = Phase::Filling;
            }
            _ => self.ops.push(PolyOp::Plot { x, y, colr, por }),
        }
    }

    /// `STOP`: the job is complete. `bitmap` is the GSU's bitmap if the
    /// config asked for it.
    pub(crate) fn on_stop(&mut self, bitmap: Option<Vec<u8>>) {
        self.phase = Phase::Idle;
        if self.ops.is_empty() {
            self.plotted.clear();
            return;
        }
        let job = PolyJob {
            ops: std::mem::take(&mut self.ops),
            bpp: self.bpp,
            viewport: self.cfg.viewport,
            plotted: std::mem::take(&mut self.plotted),
            bitmap,
        };
        if self.done.len() == RING {
            self.done.pop_front();
        }
        self.done.push_back(Arc::new(job));
        self.completed += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CFG: GsuPolyConfig = GsuPolyConfig {
        pc_bank: 1,
        count_read_pc: 0x8DE0,
        count_addr: 0x0134,
        buffer_addr: 0x0982,
        walker_pcs: (0xA6C0, 0xA740),
        viewport: (224, 170),
        capture_bitmap: false,
    };

    fn read(o: &mut GsuPolyObserver, bank: u8, pc: u16, addr: u16, word: bool) {
        o.set_pc(bank, pc);
        o.on_ram_read(addr, word);
    }

    fn plot(o: &mut GsuPolyObserver, x: u8, y: u8) {
        let corners = o.corner_span().map(|(_, n)| vec![(1, 7); usize::from(n)]);
        o.on_plot((x, y), (3, 0, 4), true, corners);
    }

    #[test]
    fn polygon_then_fill_then_loose_plot() {
        let mut o = GsuPolyObserver::new(CFG);
        plot(&mut o, 1, 1); // before any polygon: a loose plot
        read(&mut o, 1, 0x8DE0, 0x0134, true);
        for slot in [2u16, 0, 1, 3] {
            read(&mut o, 1, 0xA6C8, 0x0982 + 4 * slot, true);
            read(&mut o, 1, 0xA6CA, 0x0982 + 4 * slot + 2, true);
        }
        plot(&mut o, 10, 10);
        plot(&mut o, 11, 10);
        plot(&mut o, 12, 10);
        o.on_stop(None);
        let jobs = o.take_jobs();
        assert_eq!(jobs.len(), 1);
        let job = &jobs[0];
        assert_eq!(job.ops.len(), 2, "fill plots are not ops: {:?}", job.ops);
        assert!(matches!(job.ops[0], PolyOp::Plot { x: 1, y: 1, .. }));
        match &job.ops[1] {
            PolyOp::Polygon { corners, colr, .. } => {
                assert_eq!(corners.len(), 4);
                assert_eq!(*colr, 3);
            }
            other => panic!("{other:?}"),
        }
        let w = 224usize;
        assert!(job.plotted[w + 1] && job.plotted[10 * w + 10] && job.plotted[10 * w + 12]);
        assert_eq!(job.plotted.iter().filter(|b| **b).count(), 4);
    }

    #[test]
    fn next_count_read_ends_the_fill_and_a_plotless_polygon_is_dropped() {
        let mut o = GsuPolyObserver::new(CFG);
        read(&mut o, 1, 0x8DE0, 0x0134, true);
        read(&mut o, 1, 0xA6C8, 0x0982, true);
        plot(&mut o, 5, 5); // polygon A
        read(&mut o, 1, 0x8DE0, 0x0134, false); // B starts (byte read of the count)
        read(&mut o, 1, 0xA6C8, 0x0982, true);
        read(&mut o, 1, 0x8DE0, 0x0134, true); // C starts: B never plotted
        read(&mut o, 1, 0xA6C8, 0x0982, true);
        read(&mut o, 1, 0xA6C8, 0x0986, true);
        plot(&mut o, 6, 6);
        o.on_stop(None);
        let job = o.take_jobs().remove(0);
        let polys: Vec<_> = job
            .ops
            .iter()
            .filter_map(|op| match op {
                PolyOp::Polygon { corners, .. } => Some(corners.len()),
                PolyOp::Plot { .. } => None,
            })
            .collect();
        assert_eq!(polys, vec![1, 2]);
    }

    #[test]
    fn reads_from_the_wrong_place_are_ignored() {
        let mut o = GsuPolyObserver::new(CFG);
        read(&mut o, 0, 0x8DE0, 0x0134, true); // wrong bank
        read(&mut o, 1, 0x8DE2, 0x0134, true); // wrong pc
        read(&mut o, 1, 0x8DE0, 0x0136, true); // wrong address
        assert_eq!(o.phase, Phase::Idle);
        read(&mut o, 1, 0x8DE0, 0x0134, true);
        read(&mut o, 1, 0xA740, 0x0982, true); // pc range is half-open
        read(&mut o, 1, 0xA6C0, 0x0A82, true); // past the buffer
        read(&mut o, 1, 0xA6C0, 0x0900, true); // before the buffer
        assert_eq!(o.corner_span(), None);
        plot(&mut o, 0, 0);
        o.on_stop(None);
        assert!(matches!(
            o.take_jobs()[0].ops[..],
            [PolyOp::Plot { x: 0, y: 0, .. }]
        ));
    }

    #[test]
    fn ring_keeps_the_last_three_jobs_and_empty_jobs_are_skipped() {
        let mut o = GsuPolyObserver::new(CFG);
        o.on_stop(None); // empty: not a job
        for i in 0..5u8 {
            plot(&mut o, i, 0);
            o.on_stop(None);
        }
        assert_eq!(o.completed(), 5);
        let jobs = o.take_jobs();
        let xs: Vec<u8> = jobs
            .iter()
            .map(|j| match j.ops[0] {
                PolyOp::Plot { x, .. } => x,
                PolyOp::Polygon { .. } => 255,
            })
            .collect();
        assert_eq!(xs, vec![2, 3, 4]);
        assert!(o.take_jobs().is_empty());
    }
}
