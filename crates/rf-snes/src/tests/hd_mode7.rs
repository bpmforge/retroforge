//! HD Mode 7 sampler tests (ticket W30-02).
//!
//! Every comparison is against the hardware path itself
//! (`mode7::render_scanline`) or against a hand-computed projection, never
//! against another run of the HD sampler.

use crate::debug::{cgram_rgb, mode7_pixel};
use crate::ppu::hd_mode7::{render_hd_mode7, HdMode7Options};
use crate::ppu::{mode7, Ppu, WIDTH};

const LINES: u16 = 224;

fn opts(scale: u32, supersample: u32, perspective: bool) -> HdMode7Options {
    HdMode7Options {
        scale,
        supersample,
        perspective,
    }
}

/// A mode-7 PPU with a busy playfield: tile `n` of the map is `n * 7`
/// (mod 256) and character pixels vary with tile, x and y, with some
/// index-0 (transparent) texels. CGRAM entries are all distinct.
fn pattern_ppu() -> Ppu {
    let mut p = Ppu::new();
    p.forced_blank = false;
    p.brightness = 15;
    p.bg_mode = 7;
    p.bgs[0].enabled = true;
    for n in 0..(128 * 128) {
        p.vram[n * 2] = (n * 7) as u8;
    }
    for tile in 0..256usize {
        for px in 0..64usize {
            let v = (tile * 13 + (px % 8) * 5 + (px / 8) * 11) % 61;
            p.vram[(tile * 64 + px) * 2 + 1] = if v < 6 { 0 } else { (v * 4) as u8 };
        }
    }
    for (i, w) in p.cgram.iter_mut().enumerate() {
        *w = ((i as u16) * 129 + 17) & 0x7FFF;
    }
    p.mode7.screen_over = 0;
    p
}

/// Latch hardware lines `1..=LINES` (visible rows `0..LINES`).
fn latch_all(p: &mut Ppu) {
    for l in 1..=LINES {
        p.latch_line(l);
    }
}

fn set_matrix(p: &mut Ppu, a: i16, b: i16, c: i16, d: i16) {
    p.mode7.a = a;
    p.mode7.b = b;
    p.mode7.c = c;
    p.mode7.d = d;
}

fn cgram_bytes(p: &Ppu) -> Vec<u8> {
    p.cgram.iter().flat_map(|w| w.to_le_bytes()).collect()
}

fn px(rgba: &[u8], width: usize, x: usize, y: usize) -> [u8; 4] {
    let at = (y * width + x) * 4;
    [rgba[at], rgba[at + 1], rgba[at + 2], rgba[at + 3]]
}

/// The native frame's colour for a Mode 7 index (full brightness).
fn native_px(p: &Ppu, idx: Option<u8>) -> [u8; 4] {
    match idx {
        Some(i) => {
            let [r, g, b] = cgram_rgb(&cgram_bytes(p), i);
            [r, g, b, 255]
        }
        None => [0, 0, 0, 0],
    }
}

fn rotated() -> Ppu {
    let mut p = pattern_ppu();
    set_matrix(&mut p, 0x00E0, 0x0040, -0x0040, 0x00E0);
    p.mode7.x0 = 128;
    p.mode7.y0 = 112;
    p.mode7.hofs = 300;
    p.mode7.vofs = 200;
    latch_all(&mut p);
    p
}

/// (a) Scale 1 / supersample 1 / perspective off IS the hardware picture.
#[test]
fn scale_1_reproduces_the_native_path_pixel_for_pixel() {
    let p = rotated();
    let layer = render_hd_mode7(&p, opts(1, 1, false), WIDTH).expect("mode 7 frame");
    assert_eq!((layer.width, layer.height), (WIDTH, usize::from(LINES)));
    assert!(layer.mode7_lines.iter().all(|&m| m));
    let mut transparent = 0;
    for y in 0..LINES {
        // The native renderer is handed the HARDWARE line (row + 1).
        let native = mode7::render_scanline(&p, y + 1, 1, WIDTH);
        for (x, idx) in native.iter().enumerate() {
            let got = px(&layer.rgba, layer.width, x, usize::from(y));
            assert_eq!(got, native_px(&p, *idx), "pixel ({x},{y})");
            transparent += usize::from(idx.is_none());
        }
    }
    assert!(transparent > 0, "fixture must exercise transparent texels");
}

/// (b) Scale 2: the top-left sample of every 2x2 block is the hardware's.
#[test]
fn scale_2_edge_sample_is_the_hardware_sample() {
    let p = rotated();
    let hd = render_hd_mode7(&p, opts(2, 1, false), WIDTH).unwrap();
    let native = render_hd_mode7(&p, opts(1, 1, false), WIDTH).unwrap();
    assert_eq!((hd.width, hd.height), (WIDTH * 2, usize::from(LINES) * 2));
    for y in 0..usize::from(LINES) {
        for x in 0..WIDTH {
            assert_eq!(
                px(&hd.rgba, hd.width, 2 * x, 2 * y),
                px(&native.rgba, native.width, x, y),
                "({x},{y})"
            );
        }
    }
}

/// Pixels are sampled in screen space, so flips mirror the sub-samples
/// exactly as `render_scanline` mirrors them.
#[test]
fn flipped_screen_still_matches_native_at_scale_1() {
    let mut p = rotated();
    p.mode7.flip_x = true;
    p.mode7.flip_y = true;
    latch_all(&mut p);
    let layer = render_hd_mode7(&p, opts(1, 1, false), WIDTH).unwrap();
    for y in 0..LINES {
        let native = mode7::render_scanline(&p, y + 1, 1, WIDTH);
        for (x, idx) in native.iter().enumerate() {
            assert_eq!(
                px(&layer.rgba, layer.width, x, usize::from(y)),
                native_px(&p, *idx)
            );
        }
    }
}

/// A ramp as an HDMA racing game writes it: A and D grow with the line.
fn ramp_ppu() -> Ppu {
    let mut p = pattern_ppu();
    p.mode7.x0 = 128;
    p.mode7.y0 = 112;
    p.mode7.hofs = 128;
    p.mode7.vofs = 100;
    for l in 1..=LINES {
        let a = 0x0100 + 4 * l as i16;
        set_matrix(&mut p, a, 0, 0, a);
        p.latch_line(l);
    }
    p
}

/// (c) Sub-row t = 0.5 uses the midpoint matrix. Hand-projected with
/// B = C = 0: `vx = a*cx/256 + x0*256`, `vy = d*cy/256 + y0*256`.
#[test]
fn perspective_sub_row_uses_the_midpoint_matrix() {
    let p = ramp_ppu();
    let hd = render_hd_mode7(&p, opts(2, 1, true), WIDTH).unwrap();
    let flat = render_hd_mode7(&p, opts(2, 1, false), WIDTH).unwrap();
    let row = 9usize; // hardware line 10: a = 0x0100 + 40, next +44
    let mid = 0x0100 + 42i64; // midpoint of the two lines' A (= D)
    let line = row as i64 + 1;
    let (x0, y0, hofs, vofs) = (128i64, 112i64, 128i64, 100i64);
    let mut differs = 0;
    for x in 0..WIDTH {
        let cx_fixed = x as i64 * 256 + (hofs - x0) * 256;
        let cy_fixed = line * 256 + 128 + (vofs - y0) * 256;
        let vx = (mid * cx_fixed) / 256 + x0 * 256;
        let vy = (mid * cy_fixed) / 256 + y0 * 256;
        let idx = mode7_pixel(&p.vram, (vx >> 8) as u16, (vy >> 8) as u16);
        let want = native_px(&p, (idx != 0).then_some(idx));
        let got = px(&hd.rgba, hd.width, 2 * x, 2 * row + 1);
        assert_eq!(got, want, "x={x}");
        differs += usize::from(got != px(&flat.rgba, flat.width, 2 * x, 2 * row + 1));
    }
    assert!(differs > 0, "midpoint must differ from the stepped matrix");
    // The t = 0 sub-row is the hardware's either way.
    for x in 0..WIDTH {
        assert_eq!(
            px(&hd.rgba, hd.width, 2 * x, 2 * row),
            px(&flat.rgba, flat.width, 2 * x, 2 * row)
        );
    }
}

/// (c, second half) A seam (huge scroll jump between two lines) is not
/// interpolated across.
#[test]
fn a_discontinuous_pair_is_not_interpolated() {
    let mut p = ramp_ppu();
    // Hardware line 21 (row 20) jumps 500 texels: a split-screen seam.
    p.mode7.hofs = 128;
    for l in 21..=LINES {
        let a = 0x0100 + 4 * l as i16;
        set_matrix(&mut p, a, 0, 0, a);
        p.mode7.hofs = 128 + 500;
        p.latch_line(l);
    }
    let on = render_hd_mode7(&p, opts(2, 1, true), WIDTH).unwrap();
    let off = render_hd_mode7(&p, opts(2, 1, false), WIDTH).unwrap();
    let w = on.width;
    let seam_row = 19usize; // line 20 -> 21
    let rows = |l: &crate::ppu::hd_mode7::HdMode7Layer| {
        l.rgba[(2 * seam_row + 1) * w * 4..(2 * seam_row + 2) * w * 4].to_vec()
    };
    assert_eq!(rows(&on), rows(&off), "no blend across the seam");
    // A neighbouring continuous row still interpolates.
    let row = 5usize;
    let r = |l: &crate::ppu::hd_mode7::HdMode7Layer| {
        l.rgba[(2 * row + 1) * w * 4..(2 * row + 2) * w * 4].to_vec()
    };
    assert_ne!(r(&on), r(&off));
}

/// (d) A static frame (every line the same matrix): interpolation is a
/// no-op.
#[test]
fn perspective_is_a_no_op_on_a_static_frame() {
    let p = rotated();
    for (scale, ss) in [(2, 1), (3, 2)] {
        let on = render_hd_mode7(&p, opts(scale, ss, true), WIDTH).unwrap();
        let off = render_hd_mode7(&p, opts(scale, ss, false), WIDTH).unwrap();
        assert_eq!(on, off, "scale {scale} ss {ss}");
    }
}

/// (e) No mode-7 line, no layer.
#[test]
fn none_when_no_line_is_mode_7() {
    let mut p = pattern_ppu();
    p.bg_mode = 1;
    latch_all(&mut p);
    assert!(render_hd_mode7(&p, opts(2, 2, true), WIDTH).is_none());
}

/// Lines in another BG mode stay alpha 0 and are reported as such.
#[test]
fn non_mode_7_lines_are_transparent_and_flagged() {
    let mut p = rotated();
    p.bg_mode = 1;
    for l in 51..=60 {
        p.latch_line(l);
    }
    let layer = render_hd_mode7(&p, opts(2, 1, true), WIDTH).unwrap();
    for row in 0..usize::from(LINES) {
        let m7 = !(50..60).contains(&row);
        assert_eq!(layer.mode7_lines[row], m7, "row {row}");
        if !m7 {
            for j in 2 * row..2 * row + 2 {
                assert!((0..layer.width).all(|x| px(&layer.rgba, layer.width, x, j)[3] == 0));
            }
        }
    }
}

/// Supersampling a uniform playfield changes nothing; at a transparent
/// edge alpha is the opaque fraction and RGB is the opaque samples' mean.
#[test]
fn supersample_averages_opaque_samples_only() {
    let mut p = pattern_ppu();
    // Uniform: tile 1 everywhere, every texel index 9.
    for n in 0..(128 * 128) {
        p.vram[n * 2] = 1;
    }
    for i in 0..64 {
        p.vram[(64 + i) * 2 + 1] = 9;
    }
    set_matrix(&mut p, 0x0100, 0, 0, 0x0100);
    latch_all(&mut p);
    let one = render_hd_mode7(&p, opts(1, 1, false), WIDTH).unwrap();
    let two = render_hd_mode7(&p, opts(1, 2, false), WIDTH).unwrap();
    assert_eq!(one.rgba, two.rgba);
    assert_eq!(px(&two.rgba, two.width, 5, 5), native_px(&p, Some(9)));

    // Screen-over transparent with A = 2.0, X0 = 1: playfield x = 2*(sx-1)+1,
    // so screen x 0 is outside (-1) and x 0.5 is inside (0). Output pixel 0
    // averages one transparent and one opaque column: alpha is the opaque
    // fraction (2 of 4 samples) and RGB the opaque samples' colour.
    p.mode7.screen_over = 2;
    p.mode7.a = 0x0200;
    p.mode7.x0 = 1;
    latch_all(&mut p);
    let edge = render_hd_mode7(&p, opts(1, 2, false), WIDTH).unwrap();
    let [r, g, b, a] = px(&edge.rgba, edge.width, 0, 50);
    let [nr, ng, nb, _] = native_px(&p, Some(9));
    assert_eq!([r, g, b, a], [nr, ng, nb, (2u32 * 255 / 4) as u8]);
    assert_eq!(px(&edge.rgba, edge.width, 1, 50)[3], 255);
}

/// Brightness is the line's own, applied as `(N+1)/16`.
#[test]
fn brightness_scales_the_palette_like_the_native_frame() {
    let mut p = rotated();
    p.brightness = 7;
    latch_all(&mut p);
    let layer = render_hd_mode7(&p, opts(1, 1, false), WIDTH).unwrap();
    let native = mode7::render_scanline(&p, 1, 1, WIDTH);
    let (x, idx) = native
        .iter()
        .enumerate()
        .find_map(|(x, i)| i.map(|i| (x, i)))
        .unwrap();
    let full = cgram_rgb(&cgram_bytes(&p), idx);
    let got = px(&layer.rgba, layer.width, x, 0);
    for c in 0..3 {
        assert_eq!(u32::from(got[c]), u32::from(full[c]) * 8 / 16);
    }
}
