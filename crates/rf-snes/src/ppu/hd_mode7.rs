//! HD Mode 7 sampler (ticket W30-02).
//!
//! Re-runs the hardware's Mode 7 projection at a finer step than one
//! sample per screen pixel and hands back a plain RGBA layer. This is the
//! documented HD-Mode-7 approach (fullsnes "SNES PPU Mode 7": the output
//! is a pure function of the `$211A`-`$2120` matrix, the scroll registers
//! and VRAM): nothing is filtered, smoothed or invented — every sample is
//! a real evaluation of the transform at a sub-pixel screen position.
//!
//! ## Opt-in, and it touches nothing
//!
//! Law 6: Accuracy Mode is the reference. [`render_hd_mode7`] takes
//! `&Ppu`, mutates nothing, and is called only by new callers; the
//! hardware path (`mode7::render_scanline`) is unchanged and shares the
//! playfield fetch ([`super::mode7::sample_with`]) with this module.
//!
//! ## Which registers
//!
//! Per visible row `y` the registers are the ones the native renderer
//! composes that row from: hardware line `y + 1` (fullsnes: line 0 is
//! vblank, the picture starts on line 1; ticket W7-13), taken from the
//! just-completed frame's latched set when it has one, else the live
//! latched set, else the live registers (`Ppu::render_scanline_masked`'s
//! own order). That is what keeps scale 1 / supersample 1 bit-identical to
//! the hardware picture.
//!
//! ## Perspective correction
//!
//! A game that animates a floor with HDMA changes the matrix once per
//! scanline, so the native picture is stair-stepped vertically. With
//! `perspective` on, a sub-row `t` of the way from line `y` to line
//! `y + 1` uses the matrix linearly interpolated between the two lines'
//! registers — a finer evaluation of the ramp the ROM is drawing, not new
//! detail. Interpolation is skipped where the ramp is not continuous (see
//! [`CONTINUITY_TEXELS`]), so a split-screen seam stays a seam.

use super::mode7::{sample_with, Mode7};
use super::{Ppu, WIDTH};
use crate::debug::mode7_project;

/// Largest playfield jump, in texels, between line `y`'s and line `y + 1`'s
/// projection of the screen centre for which the two lines are still
/// treated as one continuous ramp. Beyond it (a two-player split-screen
/// seam, a hard camera cut) the lines are different views and are never
/// blended.
const CONTINUITY_TEXELS: i32 = 64;

/// Largest supported `scale`.
const MAX_SCALE: u32 = 4;

/// Options for [`render_hd_mode7`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HdMode7Options {
    /// Output pixels per native pixel, per axis (`1..=4`; clamped).
    pub scale: u32,
    /// `SxS` sub-samples averaged per output pixel (`1` or `2`; clamped).
    pub supersample: u32,
    /// Interpolate the matrix between consecutive lines (see module doc).
    pub perspective: bool,
}

/// The HD Mode 7 background layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HdMode7Layer {
    /// `out_width * scale`.
    pub width: usize,
    /// `visible_lines * scale`.
    pub height: usize,
    /// Row-major straight-alpha RGBA8. Alpha 0: no Mode 7 BG1 there
    /// (non-Mode-7 line, or a transparent texel).
    pub rgba: Vec<u8>,
    /// Per NATIVE visible line: this line is drawn in BG mode 7.
    pub mode7_lines: Vec<bool>,
}

/// Everything the sampler needs for one native visible row.
struct RowPlan {
    mode7: bool,
    /// This row's registers.
    m: Mode7,
    /// Line `y + 1`'s registers when sub-rows may interpolate toward them.
    next: Option<Mode7>,
    /// The row's CGRAM resolved to RGB at the row's brightness.
    rgb: Vec<[u8; 3]>,
}

/// One sub-row's matrix, every field scaled by `d` (= `scale * supersample`)
/// so interpolation weights stay integral.
struct SubRow {
    a: i64,
    b: i64,
    c: i64,
    d_: i64,
    x0: i64,
    y0: i64,
    /// `hofs - x0`, scaled by `d`.
    dx: i64,
    /// Screen y in 8.8 with `vofs - y0` folded in, scaled by `d`.
    cy: i64,
    screen_over: u8,
}

/// Registers, BG mode and forced-blank for visible row `y`, resolved the
/// way the native composition resolves them (module doc).
fn line_regs(ppu: &Ppu, y: u16) -> (Mode7, u8, bool) {
    let idx = usize::from(y) + 1;
    let state = if ppu.line_uses_completed(idx) {
        ppu.completed_line_state[idx]
    } else {
        ppu.line_state.get(idx).copied().flatten()
    };
    state.map_or((ppu.mode7, ppu.bg_mode, ppu.forced_blank), |s| {
        (s.mode7, s.bg_mode, s.forced_blank)
    })
}

/// BGR555 to RGB888 at a master brightness. The same arithmetic as
/// `rf_renderer::palette::bgr555_to_rgb` (rf-snes cannot depend on the
/// renderer; fullsnes "SNES Color Palette" / "2100h - INIDISP": each
/// channel `*255/31`, brightness `N` in `1..=15` scales by `(N+1)/16`, 0 is
/// black) so the HD layer matches the native frame's colours.
fn bgr555_to_rgb(word: u16, brightness: u8) -> [u8; 3] {
    if brightness == 0 {
        return [0, 0, 0];
    }
    let scale = u32::from(brightness.min(15)) + 1;
    let ch = |shift: u16| -> u8 {
        let full = u32::from((word >> shift) & 0x1F) * 255 / 31;
        (full * scale / 16) as u8
    };
    [ch(0), ch(5), ch(10)]
}

/// Do lines `y` and `y + 1` form one continuous ramp (module doc)?
fn continuous(m0: &Mode7, m1: &Mode7, line: u16) -> bool {
    let centre = |m: &Mode7| {
        let cy = i32::from(line) + i32::from(m.vofs) - i32::from(m.y0);
        let cx = 128 * 256 + (i32::from(m.hofs) - i32::from(m.x0)) * 256;
        let (vx, vy) = mode7_project(m, cx, cy);
        (vx >> 8, vy >> 8)
    };
    let (x0, y0) = centre(m0);
    let (x1, y1) = centre(m1);
    (x0 - x1).abs() <= CONTINUITY_TEXELS && (y0 - y1).abs() <= CONTINUITY_TEXELS
}

fn plan_rows(ppu: &Ppu, opts: HdMode7Options) -> Vec<RowPlan> {
    let visible = ppu.setini.visible_lines();
    let regs: Vec<(Mode7, u8, bool)> = (0..visible).map(|y| line_regs(ppu, y)).collect();
    let is_m7 = |r: &(Mode7, u8, bool)| r.1 == 7 && !r.2;
    (0..visible)
        .map(|y| {
            let i = usize::from(y);
            let (m, ..) = regs[i];
            let mode7 = is_m7(&regs[i]);
            let next = (mode7 && opts.perspective)
                .then(|| regs.get(i + 1))
                .flatten()
                .filter(|r| is_m7(r))
                .map(|r| r.0)
                .filter(|n| n.flip_x == m.flip_x && n.flip_y == m.flip_y)
                .filter(|n| continuous(&m, n, y + 1));
            let rgb = if mode7 {
                let (cgram, brightness) = ppu.line_palette(y);
                cgram
                    .iter()
                    .map(|&w| bgr555_to_rgb(w, brightness))
                    .collect()
            } else {
                Vec::new()
            };
            RowPlan {
                mode7,
                m,
                next,
                rgb,
            }
        })
        .collect()
}

/// Sub-row `k` (of `d`) of native row `row`.
fn sub_row(plan: &RowPlan, row: usize, k: i64, d: i64) -> SubRow {
    let m = &plan.m;
    // Interpolation weight toward line y+1: none when not interpolating.
    let (w0, w1, n) = match plan.next {
        Some(n) => (d - k, k, n),
        None => (d, 0, *m),
    };
    let lerp = |f0: i16, f1: i16| i64::from(f0) * w0 + i64::from(f1) * w1;
    let line = row as i64 + 1; // hardware line (module doc)
                               // Screen y in 8.8: the line's own integer y plus k/d of a line,
                               // mirrored for flip_y (`render_scanline`'s `255 - y`).
    let step = k * 256 / d;
    let sy_fixed = if m.flip_y {
        (255 - line) * 256 - step
    } else {
        line * 256 + step
    };
    let (vofs, y0) = (lerp(m.vofs, n.vofs), lerp(m.y0, n.y0));
    SubRow {
        a: lerp(m.a, n.a),
        b: lerp(m.b, n.b),
        c: lerp(m.c, n.c),
        d_: lerp(m.d, n.d),
        x0: lerp(m.x0, n.x0),
        y0,
        dx: lerp(m.hofs, n.hofs) - lerp(m.x0, n.x0),
        cy: sy_fixed * d + (vofs - y0) * 256,
        screen_over: m.screen_over,
    }
}

/// Project screen x (8.8, `sx_fixed`) through one sub-row; `d` as in
/// [`SubRow`]. At `d`-weights of exactly one line this reduces to
/// `debug::mode7_project` term for term (same truncating divides).
fn project(s: &SubRow, sx_fixed: i64, d: i64) -> (i32, i32) {
    let cx = sx_fixed * d + s.dx * 256;
    let den = 256 * d * d;
    let vx = (s.a * cx) / den + (s.b * s.cy) / den + s.x0 * 256 / d;
    let vy = (s.c * cx) / den + (s.d_ * s.cy) / den + s.y0 * 256 / d;
    ((vx >> 8) as i32, (vy >> 8) as i32)
}

/// Fill output rows `first_row..` held in `chunk` (whole rows).
fn render_rows(
    ppu: &Ppu,
    plans: &[RowPlan],
    opts: HdMode7Options,
    out_width: usize,
    first_row: usize,
    chunk: &mut [u8],
) {
    let scale = opts.scale as usize;
    let ss = opts.supersample as usize;
    let d = (scale * ss) as i64;
    let width = out_width * scale;
    let pad = (out_width.saturating_sub(WIDTH) / 2) as i64;
    let total = (out_width * scale * ss) as i64;
    let vram = &ppu.vram;

    for (r, out_row) in chunk.chunks_exact_mut(width * 4).enumerate() {
        let j = first_row + r;
        let (row, kk) = (j / scale, j % scale);
        let plan = &plans[row];
        if !plan.mode7 {
            continue;
        }
        let flip_x = plan.m.flip_x;
        let subs: Vec<SubRow> = (0..ss)
            .map(|s| sub_row(plan, row, (kk * ss + s) as i64, d))
            .collect();
        for (i, px) in out_row.chunks_exact_mut(4).enumerate() {
            let (mut sum, mut opaque) = ([0u32; 3], 0u32);
            for sx in 0..ss {
                let n = (i * ss + sx) as i64;
                let sx_num = if flip_x { total - 1 - n } else { n };
                let sx_fixed = sx_num * 256 / d - pad * 256;
                for sub in &subs {
                    let (vx, vy) = project(sub, sx_fixed, d);
                    if let Some(idx) = sample_with(vram, sub.screen_over, vx, vy) {
                        let c = plan.rgb[usize::from(idx)];
                        sum[0] += u32::from(c[0]);
                        sum[1] += u32::from(c[1]);
                        sum[2] += u32::from(c[2]);
                        opaque += 1;
                    }
                }
            }
            if opaque > 0 {
                px[0] = (sum[0] / opaque) as u8;
                px[1] = (sum[1] / opaque) as u8;
                px[2] = (sum[2] / opaque) as u8;
                px[3] = (opaque * 255 / (ss * ss) as u32) as u8;
            }
        }
    }
}

/// Render the Mode 7 BG1 layer at `opts.scale`x the native resolution.
///
/// `out_width` is the native width in pixels (256, or wider for the
/// widescreen path, centred exactly as `mode7::render_scanline` pads it).
/// Returns `None` when no visible line is in BG mode 7. Each output pixel
/// sample `n` of `scale * supersample` along an axis sits `n / (scale *
/// supersample)` of a native pixel from the native pixel's left/top edge,
/// so at scale 1 / supersample 1 the sample IS the hardware's.
///
/// Work is split by output row across `std::thread::available_parallelism`
/// scoped threads; the result does not depend on the thread count.
#[must_use]
pub fn render_hd_mode7(ppu: &Ppu, opts: HdMode7Options, out_width: usize) -> Option<HdMode7Layer> {
    let opts = HdMode7Options {
        scale: opts.scale.clamp(1, MAX_SCALE),
        supersample: opts.supersample.clamp(1, 2),
        ..opts
    };
    let plans = plan_rows(ppu, opts);
    if out_width == 0 || !plans.iter().any(|p| p.mode7) {
        return None;
    }
    let scale = opts.scale as usize;
    let width = out_width * scale;
    let height = plans.len() * scale;
    let mut rgba = vec![0u8; width * height * 4];

    let threads = std::thread::available_parallelism().map_or(1, usize::from);
    let rows_per = height.div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        // `chunks_mut` over a non-empty buffer with a non-zero chunk size
        // visits every row exactly once: it is the progress proof.
        for (n, chunk) in rgba.chunks_mut(rows_per * width * 4).enumerate() {
            let plans = &plans;
            scope.spawn(move || render_rows(ppu, plans, opts, out_width, n * rows_per, chunk));
        }
    });

    Some(HdMode7Layer {
        width,
        height,
        rgba,
        mode7_lines: plans.iter().map(|p| p.mode7).collect(),
    })
}
