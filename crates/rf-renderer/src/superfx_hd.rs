//! Ticket W31-03: the Super FX "HD 3D" compositor.
//!
//! The GSU draws into its own bitmap; the SNES copies that into VRAM and the
//! PPU shows it as background tiles somewhere on the screen, through a CGRAM
//! palette, with sprites and a HUD possibly over it, and possibly a frame or
//! two after the job that drew it. So an HD picture cannot be placed or
//! coloured from the job alone. It is placed and coloured **from the screen
//! itself**, and it replaces only the screen pixels that really are the 3D
//! layer:
//!
//! 1. each candidate job (the last few) is drawn at 1x
//!    ([`crate::gsu_poly::rasterise_job_1x`], cached);
//! 2. for a screen offset, an index -> screen-RGB mapping is read off the
//!    native frame (majority colour per index) and the *score* is the share
//!    of indexed pixels whose screen colour equals that mapping;
//! 3. the best (job, offset) wins if its score is at least [`MIN_SCORE`];
//!    otherwise there is no HD this frame and the native picture stands;
//! 4. the mask is "inside the viewport, the 1x raster has an index there, and
//!    the screen colour equals the mapping" - a sprite or HUD over the 3D
//!    never matches, so it stays native;
//! 5. the job is re-drawn at `n`x and its colours replace the masked pixels'
//!    `n` by `n` blocks of the nearest-upscaled native frame. Two ways to
//!    redraw it: on the GPU as antialiased triangles
//!    ([`crate::superfx_gpu`], ticket W31-05, the default when a GPU and
//!    `n >= 2` are available -- the picture's alpha is the polygon coverage,
//!    blended over the background behind it) or, without a GPU, by the CPU
//!    scanline fill ([`crate::gsu_poly::rasterise_job_scaled`]). At `n == 1`
//!    it is always the CPU path, so the 1x proof stays byte-identical.
//!
//! Matching and masking are CPU-only. Presentation only (project laws 4 and 6): it reads the
//! frame the core already produced and never writes anything back.

use std::collections::VecDeque;
use std::sync::Arc;

use rf_core_api::gsu_poly::PolyJob;

use crate::gpu::GpuContext;
use crate::gsu_poly::{rasterise_job_1x, rasterise_job_scaled, NONE};
use crate::superfx_gpu::{GpuPolyImage, SuperFxGpu, SCALE_MAX};

/// The best (job, offset) is used only if this share of its indexed pixels
/// agree with the screen. Below it we show the original, never a guess.
pub const MIN_SCORE: f32 = 0.97;
/// A job that indexes fewer in-bounds pixels than this proves nothing (a
/// near-empty raster "agrees" with any flat patch of screen).
pub const MIN_INDEXED: usize = 64;
/// The screen offsets searched, inclusive: the viewport's top-left in the
/// native frame.
pub const SEARCH_X: std::ops::RangeInclusive<i32> = 0..=32;
pub const SEARCH_Y: std::ops::RangeInclusive<i32> = 0..=64;
/// How many completed jobs are kept as candidates.
pub const CANDIDATES: usize = 3;
/// Frames to wait after a failed full search before searching again (a
/// stale or non-3D screen would otherwise pay the search every frame).
const RETRY_AFTER: u8 = 15;
/// The offset search scores about this many sampled pixels per offset.
const SEARCH_SAMPLES: usize = 1000;
/// This many best sampled offsets are re-scored on every pixel.
const REFINE: usize = 4;

/// A native frame: `width * height * 4` RGBA bytes.
#[derive(Clone, Copy)]
pub struct Frame<'a> {
    pub rgba: &'a [u8],
    pub width: usize,
    pub height: usize,
}

impl Frame<'_> {
    /// `0xRRGGBB` at `(x, y)`, or `None` outside the frame.
    fn rgb(&self, x: i32, y: i32) -> Option<u32> {
        if x < 0 || y < 0 || x as usize >= self.width || y as usize >= self.height {
            return None;
        }
        let i = (y as usize * self.width + x as usize) * 4;
        let p = self.rgba.get(i..i + 3)?;
        Some(u32::from(p[0]) << 16 | u32::from(p[1]) << 8 | u32::from(p[2]))
    }
}

/// Index -> screen colour, read off the native frame.
#[derive(Clone)]
pub struct Mapping(Box<[Option<u32>; 256]>);

impl Mapping {
    fn get(&self, index: u8) -> Option<u32> {
        self.0[usize::from(index)]
    }
}

/// How well a job at an offset agrees with the screen.
#[derive(Clone)]
pub struct Scored {
    /// Agreeing / indexed pixels, `0.0..=1.0`.
    pub score: f32,
    /// Indexed pixels that fall inside the frame.
    pub indexed: usize,
    pub mapping: Mapping,
}

/// A few most-frequent colours of one index (Misra-Gries, so a sprite that
/// covers some of an index's pixels cannot become its colour).
#[derive(Clone, Copy, Default)]
struct Tally([(u32, u32); 4]);

impl Tally {
    fn add(&mut self, rgb: u32) {
        if let Some(slot) = self.0.iter_mut().find(|s| s.1 > 0 && s.0 == rgb) {
            slot.1 += 1;
        } else if let Some(slot) = self.0.iter_mut().find(|s| s.1 == 0) {
            *slot = (rgb, 1);
        } else {
            for slot in &mut self.0 {
                slot.1 -= 1;
            }
        }
    }
    fn best(&self) -> Option<u32> {
        self.0
            .iter()
            .filter(|s| s.1 > 0)
            .max_by_key(|s| s.1)
            .map(|s| s.0)
    }
}

/// Visit the in-frame indexed pixels of `raster` placed at `off`:
/// `(raster pixel position, screen rgb, index)`, every `stride`-th indexed
/// pixel.
fn for_each_pixel(
    raster: &[u8],
    vp: (u16, u16),
    frame: &Frame,
    off: (i32, i32),
    stride: usize,
    mut f: impl FnMut(u8, u32),
) {
    let w = usize::from(vp.0);
    let mut seen = 0usize;
    for (i, &index) in raster.iter().enumerate() {
        if index == NONE {
            continue;
        }
        seen += 1;
        if !(seen - 1).is_multiple_of(stride) {
            continue;
        }
        let (x, y) = ((i % w) as i32 + off.0, (i / w) as i32 + off.1);
        if let Some(rgb) = frame.rgb(x, y) {
            f(index, rgb);
        }
    }
}

/// Score `raster` (a 1x raster of viewport `vp`) placed at `off` on `frame`,
/// reading every `stride`-th indexed pixel. `None` if too few land in the
/// frame ([`MIN_INDEXED`], scaled by the stride).
#[must_use]
pub fn score_at(
    raster: &[u8],
    vp: (u16, u16),
    frame: &Frame,
    off: (i32, i32),
    stride: usize,
) -> Option<Scored> {
    let stride = stride.max(1);
    let mut tallies = vec![Tally::default(); 256];
    let mut indexed = 0usize;
    for_each_pixel(raster, vp, frame, off, stride, |index, rgb| {
        tallies[usize::from(index)].add(rgb);
        indexed += 1;
    });
    if indexed * stride < MIN_INDEXED {
        return None;
    }
    let mut map = Box::new([None; 256]);
    for (slot, t) in map.iter_mut().zip(&tallies) {
        *slot = t.best();
    }
    let mapping = Mapping(map);
    let mut agree = 0usize;
    for_each_pixel(raster, vp, frame, off, stride, |index, rgb| {
        agree += usize::from(mapping.get(index) == Some(rgb));
    });
    Some(Scored {
        score: agree as f32 / indexed as f32,
        indexed: indexed * stride,
        mapping,
    })
}

/// The placement chosen for a frame.
#[derive(Clone)]
pub struct Match {
    /// 0 = the newest candidate job.
    pub job_age: usize,
    pub offset: (i32, i32),
    pub scored: Scored,
}

/// Best (job, offset) among `rasters` (newest first) by full-resolution
/// score at one offset.
fn best_at(rasters: &[&[u8]], vp: (u16, u16), frame: &Frame, off: (i32, i32)) -> Option<Match> {
    let mut best: Option<Match> = None;
    for (age, raster) in rasters.iter().enumerate() {
        let Some(scored) = score_at(raster, vp, frame, off, 1) else {
            continue;
        };
        // Strictly better only, so on a tie the newer job stays.
        if best.as_ref().is_none_or(|b| scored.score > b.scored.score) {
            best = Some(Match {
                job_age: age,
                offset: off,
                scored,
            });
        }
    }
    best
}

/// Search every offset in [`SEARCH_X`] x [`SEARCH_Y`] for every candidate on
/// a sample of its pixels, then re-score the best few on all of them.
fn search(rasters: &[&[u8]], vp: (u16, u16), frame: &Frame) -> Option<Match> {
    let mut coarse: Vec<(f32, usize, (i32, i32))> = Vec::new();
    for (age, raster) in rasters.iter().enumerate() {
        let total = raster.iter().filter(|&&c| c != NONE).count();
        let stride = (total / SEARCH_SAMPLES).max(1);
        let mut best: Vec<(f32, (i32, i32))> = Vec::new();
        for oy in SEARCH_Y {
            for ox in SEARCH_X {
                if let Some(s) = score_at(raster, vp, frame, (ox, oy), stride) {
                    best.push((s.score, (ox, oy)));
                }
            }
        }
        // Highest score first; a stable sort keeps scan order on a tie.
        best.sort_by(|a, b| b.0.total_cmp(&a.0));
        coarse.extend(best.into_iter().take(REFINE).map(|(s, o)| (s, age, o)));
    }
    coarse.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut best: Option<Match> = None;
    for &(_, age, off) in coarse.iter().take(REFINE) {
        let Some(scored) = score_at(rasters[age], vp, frame, off, 1) else {
            continue;
        };
        if best.as_ref().is_none_or(|b| scored.score > b.scored.score) {
            best = Some(Match {
                job_age: age,
                offset: off,
                scored,
            });
        }
    }
    best
}

/// What a composite did, for the proof and the debug readout.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct HdStats {
    /// The best candidate's score (even when it was not accepted).
    pub score: f32,
    pub offset: (i32, i32),
    /// 0 = the newest job.
    pub job_age: usize,
    /// Native pixels replaced.
    pub masked: usize,
    /// The viewport's pixel count.
    pub viewport_pixels: usize,
    /// The chosen job's indexed (in-frame) pixel count.
    pub indexed: usize,
    /// Whether the score cleared the threshold and an HD picture was made.
    pub accepted: bool,
    /// Whether the HD picture was drawn on the GPU (else the CPU fill).
    pub gpu: bool,
    /// The multisample count the GPU drew with (0 when `gpu` is false).
    pub samples: u32,
}

/// The GPU side of a composite: the drawing pass, its device and the
/// requested multisample count (lowered to what the device supports).
#[derive(Clone, Copy)]
pub struct GpuPath<'a> {
    pub renderer: &'a SuperFxGpu,
    pub ctx: &'a GpuContext,
    pub msaa: u32,
}

/// Where the `n`x picture's pixels come from.
enum HdSource {
    /// Palette indices ([`NONE`] = nothing) through the screen mapping.
    Cpu(Vec<u8>),
    /// Straight-alpha RGBA, alpha = coverage.
    Gpu(GpuPolyImage),
}

/// `base` blended toward `rgb` by `alpha / 255`.
fn blend(base: [u8; 3], rgb: [u8; 3], alpha: u8) -> [u8; 3] {
    let a = u32::from(alpha);
    let mix = |b: u8, c: u8| ((u32::from(b) * (255 - a) + u32::from(c) * a + 127) / 255) as u8;
    [
        mix(base[0], rgb[0]),
        mix(base[1], rgb[1]),
        mix(base[2], rgb[2]),
    ]
}

fn unpack(c: u32) -> [u8; 3] {
    [(c >> 16) as u8, (c >> 8) as u8, c as u8]
}

/// [`composite_with`] without a GPU: the CPU scanline fill.
#[must_use]
pub fn composite(
    frame: &Frame,
    m: &Match,
    job: &PolyJob,
    raster_1x: &[u8],
    n: u32,
) -> (Vec<u8>, usize) {
    let (out, masked, _) = composite_with(frame, m, job, raster_1x, n, None);
    (out, masked)
}

/// Compose the `n`x picture for `m`: the nearest-upscaled native frame with
/// the masked pixels' blocks taken from the job drawn at `n`x. Returns the
/// `(width * n) x (height * n)` RGBA frame, the masked pixel count and
/// whether the GPU drew the job (`gpu` given, `n >= 2` and it succeeded).
///
/// On the GPU path a subpixel's coverage `alpha` blends the polygon colour
/// over what is behind it (the neighbouring background, else the native
/// colour): that is what makes the edges antialiased. The CPU path is the
/// `alpha = 255 / 0` special case.
#[must_use]
pub fn composite_with(
    frame: &Frame,
    m: &Match,
    job: &PolyJob,
    raster_1x: &[u8],
    n: u32,
    gpu: Option<GpuPath>,
) -> (Vec<u8>, usize, bool) {
    let n = n.clamp(1, SCALE_MAX) as usize;
    let vp = job.viewport;
    let (vw, vh) = (usize::from(vp.0), usize::from(vp.1));
    let (out_w, out_h) = (frame.width * n, frame.height * n);
    let mut out = vec![0u8; out_w * out_h * 4];
    // Nearest upscale: build one output row per native row, copy it n times.
    for (y, native_row) in frame.rgba.chunks_exact(frame.width * 4).enumerate() {
        if y >= frame.height {
            break;
        }
        let first = y * n * out_w * 4;
        for (x, px) in native_row.chunks_exact(4).enumerate() {
            for dx in 0..n {
                let o = first + (x * n + dx) * 4;
                out[o..o + 3].copy_from_slice(&px[..3]);
                out[o + 3] = 255;
            }
        }
        for dy in 1..n {
            let (head, tail) = out.split_at_mut(first + dy * out_w * 4);
            tail[..out_w * 4].copy_from_slice(&head[first..first + out_w * 4]);
        }
    }
    let mapping = &m.scored.mapping;
    let gpu_image = gpu.filter(|_| n > 1).and_then(|g| {
        g.renderer
            .render(g.ctx, job, vp, n as u32, g.msaa, &|i| {
                mapping.get(i).map(unpack)
            })
            .ok()
    });
    let used_gpu = gpu_image.is_some();
    let hd = match gpu_image {
        Some(img) => HdSource::Gpu(img),
        None if n == 1 => HdSource::Cpu(raster_1x.to_vec()),
        None => HdSource::Cpu(rasterise_job_scaled(job, vp, n as u32)),
    };
    let hd_w = vw * n;
    // The HD picture's subpixel `(sx, sy)`: its colour and coverage.
    let hd_px = |sx: usize, sy: usize| -> Option<([u8; 3], u8)> {
        match &hd {
            HdSource::Cpu(r) => {
                let h = r[sy * hd_w + sx];
                (h != NONE)
                    .then(|| mapping.get(h))
                    .flatten()
                    .map(|c| (unpack(c), 255))
            }
            HdSource::Gpu(img) => {
                let (rgb, a) = img.pixel(sx, sy);
                (a > 0).then_some((rgb, a))
            }
        }
    };
    let put = |out: &mut [u8], o: usize, base: Option<[u8; 3]>, px: ([u8; 3], u8)| {
        let c = match (px.1, base) {
            (255, _) => px.0,
            (a, Some(b)) => blend(b, px.0, a),
            (a, None) => blend([out[o], out[o + 1], out[o + 2]], px.0, a),
        };
        out[o..o + 3].copy_from_slice(&c);
    };
    let mut masked = 0usize;
    for (i, &index) in raster_1x.iter().enumerate() {
        if index == NONE || i >= vw * vh {
            continue;
        }
        let (x, y) = ((i % vw) as i32 + m.offset.0, (i / vw) as i32 + m.offset.1);
        let (Some(rgb), Some(want)) = (frame.rgb(x, y), mapping.get(index)) else {
            continue;
        };
        if rgb != want {
            continue; // a sprite, the HUD, anything that is not the 3D layer
        }
        masked += 1;
        // Where the HD polygon leaves part of this pixel uncovered, the
        // background behind the 3D shows there, not the polygon's own colour
        // (that kept edges staircased): take it from an un-indexed
        // 4-neighbour. None found -> the native colour. At n = 1 nothing is
        // uncovered, so the 1x composite stays identical.
        let (xi, yi) = ((i % vw) as i32, (i / vw) as i32);
        let behind = [(1, 0), (-1, 0), (0, 1), (0, -1)]
            .iter()
            .find_map(|&(dx, dy)| {
                let (nx, ny) = (xi + dx, yi + dy);
                (nx >= 0
                    && ny >= 0
                    && (nx as usize) < vw
                    && (ny as usize) < vh
                    && raster_1x[ny as usize * vw + nx as usize] == NONE)
                    .then(|| frame.rgb(nx + m.offset.0, ny + m.offset.1))
                    .flatten()
            })
            .map(unpack);
        let (hx, hy) = ((i % vw) * n, (i / vw) * n);
        for dy in 0..n {
            for dx in 0..n {
                let o = ((y as usize * n + dy) * out_w + x as usize * n + dx) * 4;
                match (hd_px(hx + dx, hy + dy), behind) {
                    (Some(px), base) => put(&mut out, o, base, px),
                    (None, Some(b)) => out[o..o + 3].copy_from_slice(&b),
                    (None, None) => {} // nothing drawn here at n x: the native colour
                }
            }
        }
    }
    // Silhouettes: a native pixel just OUTSIDE the 3D (no index at 1x) next
    // to a masked one may be partly covered by the polygon at n x. Let the
    // HD raster paint into it when it shows plain background: its screen
    // colour matches one of its own un-indexed 4-neighbours (so a sprite or
    // HUD pixel, which rarely does, keeps its colour). At n = 1 the HD
    // raster has no index there, so the 1x composite stays identical.
    if n > 1 {
        let indexed_and_masked = |xi: i32, yi: i32| -> bool {
            if xi < 0 || yi < 0 || xi as usize >= vw || yi as usize >= vh {
                return false;
            }
            let idx = raster_1x[yi as usize * vw + xi as usize];
            idx != NONE
                && frame
                    .rgb(xi + m.offset.0, yi + m.offset.1)
                    .is_some_and(|rgb| mapping.get(idx) == Some(rgb))
        };
        let plain_neighbour = |xi: i32, yi: i32, rgb: u32| -> bool {
            [(1, 0), (-1, 0), (0, 1), (0, -1)].iter().any(|&(dx, dy)| {
                let (nx, ny) = (xi + dx, yi + dy);
                nx >= 0
                    && ny >= 0
                    && (nx as usize) < vw
                    && (ny as usize) < vh
                    && raster_1x[ny as usize * vw + nx as usize] == NONE
                    && frame.rgb(nx + m.offset.0, ny + m.offset.1) == Some(rgb)
            })
        };
        for (i, &idx) in raster_1x.iter().enumerate().take(vw * vh) {
            if idx != NONE {
                continue;
            }
            let (xi, yi) = ((i % vw) as i32, (i / vw) as i32);
            let near_3d = [(1, 0), (-1, 0), (0, 1), (0, -1)]
                .iter()
                .any(|&(dx, dy)| indexed_and_masked(xi + dx, yi + dy));
            if !near_3d {
                continue;
            }
            let (x, y) = (xi + m.offset.0, yi + m.offset.1);
            let Some(rgb) = frame.rgb(x, y) else { continue };
            if !plain_neighbour(xi, yi, rgb) {
                continue;
            }
            let (hx, hy) = ((i % vw) * n, (i / vw) * n);
            for dy in 0..n {
                for dx in 0..n {
                    let Some(px) = hd_px(hx + dx, hy + dy) else {
                        continue;
                    };
                    let o = ((y as usize * n + dy) * out_w + x as usize * n + dx) * 4;
                    put(&mut out, o, None, px);
                }
            }
        }
    }
    (out, masked, used_gpu)
}

/// The stateful side: the last jobs with their cached 1x rasters, and the
/// remembered placement.
pub struct SuperFxHd {
    /// Newest first, at most [`CANDIDATES`].
    jobs: VecDeque<(Arc<PolyJob>, Vec<u8>)>,
    hint: Option<(i32, i32)>,
    wait: u8,
    /// The threshold a candidate must clear (default [`MIN_SCORE`]; a proof
    /// can lower it to exercise the mask on weak matches).
    pub min_score: f32,
    last: HdStats,
}

impl Default for SuperFxHd {
    fn default() -> Self {
        Self {
            jobs: VecDeque::new(),
            hint: None,
            wait: 0,
            min_score: MIN_SCORE,
            last: HdStats::default(),
        }
    }
}

impl SuperFxHd {
    /// Remember a completed job (and draw its 1x raster once).
    pub fn push_job(&mut self, job: Arc<PolyJob>) {
        let raster = rasterise_job_1x(&job, job.viewport);
        self.jobs.push_front((job, raster));
        self.jobs.truncate(CANDIDATES);
    }

    /// Forget everything (a new game, the feature turned off).
    pub fn clear(&mut self) {
        self.jobs.clear();
        self.hint = None;
        self.wait = 0;
        self.last = HdStats::default();
    }

    /// The stats of the latest [`Self::compose`].
    #[must_use]
    pub fn last_stats(&self) -> HdStats {
        self.last
    }

    /// Whether any job is available to try.
    #[must_use]
    pub fn has_jobs(&self) -> bool {
        !self.jobs.is_empty()
    }

    /// The `n`x picture for `frame`, or `None` (show the native frame) when
    /// there is no job or no candidate clears the threshold. The stats are
    /// available from [`Self::last_stats`] either way.
    pub fn compose(&mut self, frame: &Frame, n: u32) -> Option<Vec<u8>> {
        self.compose_with(frame, n, None)
    }

    /// [`Self::compose`] drawing the job on the GPU when `gpu` is given and
    /// `n >= 2` (falling back to the CPU fill if the GPU draw fails).
    pub fn compose_with(&mut self, frame: &Frame, n: u32, gpu: Option<GpuPath>) -> Option<Vec<u8>> {
        let Some((newest, _)) = self.jobs.front() else {
            self.last = HdStats::default();
            return None;
        };
        let vp = newest.viewport;
        let rasters: Vec<&[u8]> = self.jobs.iter().map(|(_, r)| r.as_slice()).collect();
        let mut best = self.hint.and_then(|h| best_at(&rasters, vp, frame, h));
        let accepted =
            |m: &Option<Match>, min: f32| m.as_ref().is_some_and(|m| m.scored.score >= min);
        if !accepted(&best, self.min_score) {
            if self.wait > 0 {
                self.wait -= 1;
            } else {
                let found = search(&rasters, vp, frame);
                if found.as_ref().map(|m| m.scored.score) > best.as_ref().map(|m| m.scored.score) {
                    best = found;
                }
                if !accepted(&best, self.min_score) {
                    self.wait = RETRY_AFTER;
                }
            }
        }
        let Some(m) = best else {
            self.hint = None;
            self.last = HdStats::default();
            return None;
        };
        let viewport_pixels = usize::from(vp.0) * usize::from(vp.1);
        let ok = m.scored.score >= self.min_score;
        self.last = HdStats {
            score: m.scored.score,
            offset: m.offset,
            job_age: m.job_age,
            masked: 0,
            viewport_pixels,
            indexed: m.scored.indexed,
            accepted: ok,
            gpu: false,
            samples: 0,
        };
        if !ok {
            self.hint = None;
            return None;
        }
        self.hint = Some(m.offset);
        let (job, raster) = &self.jobs[m.job_age];
        let (out, masked, used_gpu) = composite_with(frame, &m, job, raster, n, gpu);
        self.last.masked = masked;
        self.last.gpu = used_gpu;
        self.last.samples = gpu
            .filter(|_| used_gpu)
            .map_or(0, |g| g.renderer.effective_samples(g.msaa));
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rf_core_api::gsu_poly::PolyOp;

    const VP: (u16, u16) = (40, 30);

    fn poly(corners: &[(u8, u8)], colr: u8) -> PolyOp {
        PolyOp::Polygon {
            corners: corners.to_vec(),
            colr,
            por: 0,
        }
    }

    /// Three overlapping polygons of different colours plus a loose plot.
    fn scene() -> PolyJob {
        PolyJob {
            ops: vec![
                poly(&[(2, 2), (30, 2), (30, 26), (2, 26)], 3),
                poly(&[(10, 4), (24, 20), (6, 22)], 5),
                poly(&[(18, 6), (36, 6), (36, 14), (18, 14)], 9),
                PolyOp::Plot {
                    x: 1,
                    y: 28,
                    colr: 7,
                    por: 0,
                },
            ],
            bpp: 4,
            viewport: VP,
            plotted: Vec::new(),
            bitmap: None,
        }
    }

    fn colour_of(index: u8) -> [u8; 3] {
        [index * 16 + 7, 200 - index * 9, index * 5 + 30]
    }

    /// A 64x56 frame of noisy background with `raster` drawn at `off`.
    fn frame_with(raster: &[u8], off: (usize, usize)) -> Vec<u8> {
        let (w, h) = (64usize, 56usize);
        let mut rgba = vec![0u8; w * h * 4];
        for (i, px) in rgba.chunks_exact_mut(4).enumerate() {
            // a background that is not any palette colour
            px.copy_from_slice(&[(i * 7 % 13) as u8, 250, (i % 5) as u8, 255]);
        }
        for (i, &idx) in raster.iter().enumerate() {
            if idx == NONE {
                continue;
            }
            let (x, y) = (i % usize::from(VP.0) + off.0, i / usize::from(VP.0) + off.1);
            let c = colour_of(idx);
            let o = (y * w + x) * 4;
            rgba[o..o + 3].copy_from_slice(&c);
        }
        rgba
    }

    fn frame(rgba: &[u8]) -> Frame<'_> {
        Frame {
            rgba,
            width: 64,
            height: 56,
        }
    }

    #[test]
    fn placement_search_finds_a_synthetic_offset() {
        let job = scene();
        let raster = rasterise_job_1x(&job, VP);
        let rgba = frame_with(&raster, (17, 9));
        let m = search(&[&raster], VP, &frame(&rgba)).expect("a match");
        assert_eq!(m.offset, (17, 9));
        assert!(m.scored.score > 0.999, "{}", m.scored.score);
        assert_eq!(m.job_age, 0);
    }

    #[test]
    fn the_right_job_among_candidates_is_picked() {
        let job = scene();
        let raster = rasterise_job_1x(&job, VP);
        let mut other = scene();
        other.ops.truncate(1);
        let other_raster = rasterise_job_1x(&other, VP);
        let rgba = frame_with(&raster, (3, 5));
        // The matching job is the OLDER one.
        let m = search(&[&other_raster, &raster], VP, &frame(&rgba)).expect("a match");
        assert_eq!((m.job_age, m.offset), (1, (3, 5)));
    }

    #[test]
    fn a_screen_that_is_not_the_3d_layer_gives_no_hd() {
        let job = scene();
        let raster = rasterise_job_1x(&job, VP);
        // The screen shows the same shapes in colours that are a different
        // function of the index each time: no consistent mapping.
        let mut rgba = frame_with(&raster, (4, 4));
        for (i, px) in rgba.chunks_exact_mut(4).enumerate() {
            px[0] = px[0].wrapping_add((i * 31 % 251) as u8);
        }
        let mut hd = SuperFxHd::default();
        hd.push_job(Arc::new(job));
        assert!(hd.compose(&frame(&rgba), 3).is_none());
        let s = hd.last_stats();
        assert!(!s.accepted && s.score < MIN_SCORE, "{s:?}");
        assert_eq!(s.masked, 0);
        // No job at all is also "no HD".
        let mut empty = SuperFxHd::default();
        assert!(empty.compose(&frame(&rgba), 3).is_none());
    }

    #[test]
    fn the_mask_excludes_a_pixel_whose_screen_colour_differs() {
        let job = scene();
        let raster = rasterise_job_1x(&job, VP);
        let mut rgba = frame_with(&raster, (10, 10));
        // A "sprite" over the middle of the big polygon (index 3 -> 5 here).
        let (sx, sy) = (10 + 8, 10 + 18);
        let o = (sy * 64 + sx) * 4;
        rgba[o..o + 3].copy_from_slice(&[250, 0, 250]);
        let mut hd = SuperFxHd::default();
        hd.push_job(Arc::new(job));
        let out = hd.compose(&frame(&rgba), 3).expect("accepted");
        let s = hd.last_stats();
        assert!(s.accepted && s.score >= MIN_SCORE, "{s:?}");
        assert_eq!(s.offset, (10, 10));
        // The sprite's 3x3 block is untouched native colour.
        for dy in 0..3 {
            for dx in 0..3 {
                let p = ((sy * 3 + dy) * 192 + sx * 3 + dx) * 4;
                assert_eq!(&out[p..p + 3], &[250, 0, 250]);
            }
        }
        // ... and the pixel is not counted as masked.
        let indexed = raster.iter().filter(|&&c| c != NONE).count();
        assert_eq!(s.masked, indexed - 1);
    }

    #[test]
    fn at_1x_the_composite_is_the_native_frame_byte_for_byte() {
        let job = scene();
        let raster = rasterise_job_1x(&job, VP);
        let mut rgba = frame_with(&raster, (12, 6));
        // include a sprite and a stray pixel so the mask has work to do
        for o in [(30 * 64 + 20) * 4, (8 * 64 + 13) * 4] {
            rgba[o..o + 3].copy_from_slice(&[1, 2, 3]);
        }
        let mut hd = SuperFxHd::default();
        hd.push_job(Arc::new(job));
        let out = hd.compose(&frame(&rgba), 1).expect("accepted");
        assert_eq!(out, rgba);
        assert!(hd.last_stats().masked > 0);
    }

    #[test]
    fn at_3x_non_masked_pixels_are_the_nearest_upscale() {
        let job = scene();
        let raster = rasterise_job_1x(&job, VP);
        let rgba = frame_with(&raster, (12, 6));
        let mut hd = SuperFxHd::default();
        hd.push_job(Arc::new(job));
        let out = hd.compose(&frame(&rgba), 3).expect("accepted");
        assert_eq!(out.len(), 64 * 3 * 56 * 3 * 4);
        // The very top-left of the frame is background: plain upscale.
        for dy in 0..3 {
            for dx in 0..3 {
                assert_eq!(&out[(dy * 192 + dx) * 4..][..3], &rgba[..3]);
            }
        }
        // The big polygon's interior (index 3) is drawn in its mapped colour.
        let (sx, sy) = (12 + 3, 6 + 3);
        let p = ((sy * 3 + 1) * 192 + sx * 3 + 1) * 4;
        assert_eq!(&out[p..p + 3], &colour_of(3));
    }

    #[test]
    fn at_3x_on_the_gpu_edges_are_antialiased_and_the_interior_is_the_mapped_colour() {
        let Ok(ctx) = GpuContext::request_headless() else {
            println!("SKIP: no wgpu adapter");
            return;
        };
        let renderer = SuperFxGpu::new(&ctx);
        let path = GpuPath {
            renderer: &renderer,
            ctx: &ctx,
            msaa: 4,
        };
        let job = scene();
        let raster = rasterise_job_1x(&job, VP);
        let rgba = frame_with(&raster, (12, 6));
        let mut cpu = SuperFxHd::default();
        cpu.push_job(Arc::new(job.clone()));
        let cpu_out = cpu.compose(&frame(&rgba), 3).expect("accepted");
        assert!(!cpu.last_stats().gpu);
        let mut hd = SuperFxHd::default();
        hd.push_job(Arc::new(job));
        let out = hd
            .compose_with(&frame(&rgba), 3, Some(path))
            .expect("accepted");
        let s = hd.last_stats();
        assert!(s.gpu && s.samples >= 1 && s.accepted, "{s:?}");
        assert_eq!(out.len(), cpu_out.len());
        // Interior of the big polygon: the mapped colour, as on the CPU.
        let (sx, sy) = (12 + 3, 6 + 3);
        let p = ((sy * 3 + 1) * 192 + sx * 3 + 1) * 4;
        assert_eq!(&out[p..p + 3], &colour_of(3));
        // The slanted triangle's edge yields in-between colours (blends of
        // two palette colours / the background) that the CPU fill never has.
        let palette: Vec<[u8; 3]> = (0..16).map(colour_of).collect();
        let is_known = |px: &[u8]| {
            palette.iter().any(|c| c == &px[..3]) || px[..3] == rgba[..3] || px[1] == 250
        };
        let blended = out.chunks_exact(4).filter(|px| !is_known(px)).count();
        let cpu_blended = cpu_out.chunks_exact(4).filter(|px| !is_known(px)).count();
        assert_eq!(cpu_blended, 0, "the CPU fill has no blended pixels");
        assert!(blended > 20, "blended edge pixels: {blended}");
        // A scale the GPU is not asked for falls back to the CPU fill.
        let mut hd1 = SuperFxHd::default();
        hd1.push_job(Arc::new(scene()));
        let one = hd1
            .compose_with(&frame(&rgba), 1, Some(path))
            .expect("accepted");
        assert_eq!(one, rgba, "1x is the CPU path and stays identical");
        assert!(!hd1.last_stats().gpu);
    }

    #[test]
    fn the_remembered_offset_is_reused_and_a_collapse_researches() {
        let job = scene();
        let raster = rasterise_job_1x(&job, VP);
        let mut hd = SuperFxHd::default();
        hd.push_job(Arc::new(job));
        let a = frame_with(&raster, (5, 5));
        assert!(hd.compose(&frame(&a), 2).is_some());
        assert_eq!(hd.last_stats().offset, (5, 5));
        // Same offset again: the hint path (still accepted).
        assert!(hd.compose(&frame(&a), 2).is_some());
        // The picture moves: the hint collapses and the search finds it.
        let b = frame_with(&raster, (20, 22));
        assert!(hd.compose(&frame(&b), 2).is_some());
        assert_eq!(hd.last_stats().offset, (20, 22));
    }

    #[test]
    fn only_the_last_three_jobs_are_kept() {
        let mut hd = SuperFxHd::default();
        for _ in 0..5 {
            hd.push_job(Arc::new(scene()));
        }
        assert_eq!(hd.jobs.len(), CANDIDATES);
        hd.clear();
        assert!(!hd.has_jobs());
    }

    #[test]
    fn tally_keeps_the_majority_colour() {
        let mut t = Tally::default();
        for rgb in [9, 9, 4, 9, 7, 9, 5, 9, 9, 1] {
            t.add(rgb);
        }
        assert_eq!(t.best(), Some(9));
    }
}
