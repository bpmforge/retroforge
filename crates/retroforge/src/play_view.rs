//! Where the emulated picture goes in the window (ticket W20-01;
//! `docs/design/UX_WAVE_20.md` §4, `docs/design/RENDERER.md` §2).
//!
//! Until W20-01 the play view drew every frame with
//! `egui::Image::shrink_to_fit`, whatever Settings › Video said: the
//! scale-mode radio was saved to `settings.toml` and read by nothing. This
//! module is the one function those radios now drive, kept pure (rects in,
//! rect out) so each mode is unit-tested without a window.
//!
//! ## Integer mode follows RENDERER.md §2 resolution (b)
//!
//! Integer scaling and the 8:7 TV pixel aspect pull against each other —
//! 8:7 is not a whole ratio. RENDERER.md §2 (ticket W3-01b) chose to lock
//! the **vertical** axis to a whole multiple and let the horizontal axis
//! carry the aspect correction (`y_scale * 8/7`), so every source scanline
//! lands on an exact number of output rows. This module applies that same
//! rule at the window level.
//!
//! ## Hires and interlace are folded back to the 256×240 grid
//!
//! The SNES can emit 512-wide (hires) and 448/478-tall (interlace) frames.
//! Those are twice as many samples over the **same** picture area, not a
//! twice-as-large picture, so [`DisplayGrid::for_frame`] halves them before
//! the aspect is applied. A decoded-widescreen frame (400 wide, W11-03) is
//! genuinely wider and keeps its width.

use eframe::egui;

use crate::settings::{PixelAspect, ScaleMode};

/// Horizontal stretch for one emulated pixel. 8:7 is the NTSC NES/SNES
/// pixel aspect (nesdev "Overscan"/"PPU rendering"; fullsnes "SNES
/// Timings") — RENDERER.md §2 names it as the default.
#[must_use]
pub fn pixel_aspect_ratio(aspect: PixelAspect) -> f32 {
    match aspect {
        PixelAspect::Tv => 8.0 / 7.0,
        PixelAspect::Square => 1.0,
    }
}

/// The picture's size in **logical** emulated pixels, with any hires/
/// interlace doubling folded back (module doc).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisplayGrid {
    pub columns: f32,
    pub lines: f32,
}

impl DisplayGrid {
    /// Logical grid for a console frame of `width`×`height` samples.
    #[must_use]
    pub fn for_frame(width: f32, height: f32) -> Self {
        // 512 is SNES hires (fullsnes "PPU Hires"); anything that wide is
        // two samples per pixel. Above 240 lines is interlace, two fields.
        let columns = if width >= 512.0 { width / 2.0 } else { width };
        let lines = if height > 240.0 { height / 2.0 } else { height };
        Self { columns, lines }
    }

    /// A non-console image (the diorama render) whose pixels are already
    /// display pixels: no folding.
    #[must_use]
    pub fn exact(width: f32, height: f32) -> Self {
        Self {
            columns: width,
            lines: height,
        }
    }
}

/// The rect, inside `available`, that the picture occupies under `mode`.
///
/// Always centred. `par` is the horizontal stretch per logical pixel
/// ([`pixel_aspect_ratio`]). A degenerate grid or area returns `available`
/// unchanged rather than a zero or NaN rect.
#[must_use]
pub fn play_rect(
    available: egui::Rect,
    grid: DisplayGrid,
    par: f32,
    mode: ScaleMode,
) -> egui::Rect {
    let (aw, ah) = (available.width(), available.height());
    if grid.columns <= 0.0 || grid.lines <= 0.0 || aw <= 0.0 || ah <= 0.0 {
        return available;
    }
    let display_w = grid.columns * par;
    let size = match mode {
        ScaleMode::Stretch => egui::vec2(aw, ah),
        ScaleMode::Fit => fit(display_w, grid.lines, aw, ah),
        ScaleMode::Integer => {
            // Largest whole multiple of the line count that fits BOTH
            // axes. When even 1x does not fit (a tiny window), fall back
            // to Fit rather than drawing a picture larger than the window.
            let by_height = (ah / grid.lines).floor();
            let by_width = (aw / display_w).floor();
            let k = by_height.min(by_width);
            if k >= 1.0 {
                egui::vec2(display_w * k, grid.lines * k)
            } else {
                fit(display_w, grid.lines, aw, ah)
            }
        }
    };
    egui::Rect::from_center_size(available.center(), size)
}

/// Paint `texture` into the play area under `mode` and return the image's
/// response (its `rect` is where overlays such as a script's draw list
/// must be positioned).
pub fn show_frame(
    ui: &mut egui::Ui,
    texture: &egui::TextureHandle,
    grid: DisplayGrid,
    par: f32,
    mode: ScaleMode,
    surround: Surround,
) -> egui::Response {
    let area = ui.available_rect_before_wrap();
    let rect = play_rect(area, grid, par, mode);
    match surround {
        Surround::Black => {}
        Surround::Glow(edges) => paint_ambient_glow(ui.painter(), area, rect, edges),
        Surround::Bezel(accent) => paint_bezel(ui.painter(), area, rect, accent),
    }
    // `maintain_aspect_ratio(false)`: egui's `Image` keeps the TEXTURE's
    // aspect by default, which silently undid the 8:7 pixel shape — the
    // rect was 8:7 and the picture inside it 16:15 (found from a tour
    // photo, W20-12). The response reports the rect the image actually
    // fills (`calc_size`), so a test of the response measures what is
    // painted, not merely what was allocated.
    let image = egui::Image::from_texture(texture)
        .fit_to_exact_size(rect.size())
        .maintain_aspect_ratio(false);
    let drawn = image.calc_size(rect.size(), Some(texture.size_vec2()));
    let response = ui.put(rect, image);
    response.with_new_rect(egui::Rect::from_center_size(rect.center(), drawn))
}

fn fit(w: f32, h: f32, aw: f32, ah: f32) -> egui::Vec2 {
    let s = (aw / w).min(ah / h);
    egui::vec2(w * s, h * s)
}

/// The whole-number vertical multiple `rect` represents for `grid`, if
/// it is one (used by tests and by the shader chain's output-size choice).
#[must_use]
pub fn integer_multiple(rect: egui::Rect, grid: DisplayGrid) -> Option<u32> {
    let k = rect.height() / grid.lines;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    ((k - k.round()).abs() < 1e-3 && k >= 1.0).then(|| k.round() as u32)
}

/// Ticket W20-16: the frame to show while hold-to-peek is `amount` of the
/// way in — a left-to-right wipe from `enhanced` to `original`
/// (`rf_renderer::compose_split` puts `original` left of the divider).
///
/// When the two are not the same size (an HD pack or decoded widescreen
/// changed the geometry) there is no honest per-pixel wipe, so it is a cut
/// at the halfway point, returning `None` for "show the enhanced frame".
#[must_use]
pub fn peek_frame(
    original: &[u8],
    original_size: (usize, usize),
    enhanced: &[u8],
    enhanced_size: (usize, usize),
    amount: f32,
) -> Option<(Vec<u8>, (usize, usize))> {
    if amount <= 0.0 {
        return None;
    }
    if original_size == enhanced_size
        && original.len() == enhanced.len()
        && original.len() == original_size.0 * original_size.1 * 4
    {
        let (w, h) = original_size;
        let (Ok(w32), Ok(h32)) = (u32::try_from(w), u32::try_from(h)) else {
            return None;
        };
        return Some((
            rf_renderer::compose_split(original, enhanced, w32, h32, amount),
            original_size,
        ));
    }
    (amount >= 0.5).then(|| (original.to_vec(), original_size))
}

/// Ticket W20-19: the average colour along each edge of an RGBA frame —
/// `[left, right, top, bottom]` — for the ambient-glow letterbox, which
/// fills the bars around an integer-scaled picture with a darkened version
/// of what is next to them. A plain average (no blur pass): cheap enough
/// to recompute on every frame upload, and steady enough not to strobe.
///
/// Samples a 4-pixel-deep strip on each side. Returns black for a
/// malformed buffer rather than panicking.
#[must_use]
pub fn edge_colours(rgba: &[u8], width: usize, height: usize) -> [egui::Color32; 4] {
    const DEPTH: usize = 4;
    if width == 0 || height == 0 || rgba.len() != width * height * 4 {
        return [egui::Color32::BLACK; 4];
    }
    let avg = |pixels: &mut dyn Iterator<Item = (usize, usize)>| {
        let (mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64);
        for (x, y) in pixels {
            let i = (y * width + x) * 4;
            r += u64::from(rgba[i]);
            g += u64::from(rgba[i + 1]);
            b += u64::from(rgba[i + 2]);
            n += 1;
        }
        let n = n.max(1);
        #[allow(clippy::cast_possible_truncation)]
        egui::Color32::from_rgb((r / n) as u8, (g / n) as u8, (b / n) as u8)
    };
    let dx = DEPTH.min(width);
    let dy = DEPTH.min(height);
    [
        avg(&mut (0..height).flat_map(|y| (0..dx).map(move |x| (x, y)))),
        avg(&mut (0..height).flat_map(|y| (width - dx..width).map(move |x| (x, y)))),
        avg(&mut (0..dy).flat_map(|y| (0..width).map(move |x| (x, y)))),
        avg(&mut (height - dy..height).flat_map(|y| (0..width).map(move |x| (x, y)))),
    ]
}

/// Ticket W20-19: paint the letterbox bars around `picture` (inside
/// `area`) with each edge's colour, darkened so the picture stays the
/// brightest thing on screen.
/// Ticket W21-08: how many rows of a `height`-row host frame the console
/// actually drew, from the last scanline it emitted. The host buffer is
/// 240 rows, but an SNES frame without overscan draws 224 (fullsnes
/// "PPU Ports", `$2133` bit 2) — showing all 240 put a 16-line black band
/// under the picture. Only ever SHRINKS a non-interlaced frame; unknown
/// input leaves it as is.
#[must_use]
pub fn visible_rows(height: usize, last_scanline: Option<u16>) -> usize {
    match last_scanline {
        Some(y) if height <= 240 => (usize::from(y) + 1).clamp(1, height),
        _ => height,
    }
}

/// The first `rows` rows of an RGBA frame `width` pixels wide.
#[must_use]
pub fn crop_rows(rgba: &[u8], width: usize, rows: usize) -> &[u8] {
    &rgba[..(width * rows * 4).min(rgba.len())]
}

/// Ticket W21-12: the window inner size that makes the play area exactly
/// the picture — the window shrinks (or grows) by however much empty
/// space surrounds the picture now, so the scale already chosen is kept.
/// `None` when it is already within a point of fitting, or for a
/// degenerate input.
#[must_use]
pub fn fitted_window(
    inner: egui::Vec2,
    play_area: egui::Vec2,
    picture: egui::Vec2,
) -> Option<egui::Vec2> {
    if !(picture.x >= 1.0 && picture.y >= 1.0 && inner.x > 0.0 && inner.y > 0.0) {
        return None;
    }
    let spare = play_area - picture;
    if spare.x.abs() < 1.0 && spare.y.abs() < 1.0 {
        return None;
    }
    let want = (inner - spare).ceil();
    (want.x >= 1.0 && want.y >= 1.0).then_some(want)
}

/// Ticket W21-04: what is drawn around the picture, resolved for one
/// frame (the settings' `crate::settings::Surround` plus what it needs).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Surround {
    Black,
    /// The four edge colours (left, right, top, bottom).
    Glow([egui::Color32; 4]),
    /// The console's identity colour, for the bezel's light.
    Bezel(egui::Color32),
}

/// How much frame the bezel takes, at most, on each side.
pub const BEZEL_MAX: f32 = 28.0;

/// Ticket W21-04: the bezel rect around `picture` — up to [`BEZEL_MAX`] on
/// each side, never past `area` (with Fit or Stretch there may be no
/// room, and then there is no bezel). `None` when it would be thinner
/// than 4 px.
#[must_use]
pub fn bezel_rect(area: egui::Rect, picture: egui::Rect) -> Option<egui::Rect> {
    let room = (picture.left() - area.left())
        .min(area.right() - picture.right())
        .min(picture.top() - area.top())
        .min(area.bottom() - picture.bottom())
        .min(BEZEL_MAX);
    (room >= 4.0).then(|| picture.expand(room))
}

/// Ticket W21-04: a plain procedural bezel — a dark rounded frame with a
/// lighter top edge, an inset shadow line at the picture, and a small
/// light in the console's colour under the picture. No artwork: nothing
/// here imitates a real product's case.
pub fn paint_bezel(
    painter: &egui::Painter,
    area: egui::Rect,
    picture: egui::Rect,
    accent: egui::Color32,
) {
    let Some(outer) = bezel_rect(area, picture) else {
        return;
    };
    let radius =
        egui::CornerRadius::same(((outer.width() - picture.width()) / 2.0).min(14.0) as u8);
    painter.rect_filled(outer, radius, egui::Color32::from_rgb(0x1C, 0x1E, 0x23));
    painter.rect_stroke(
        outer,
        radius,
        egui::Stroke::new(1.0, egui::Color32::from_rgb(0x34, 0x37, 0x3F)),
        egui::StrokeKind::Inside,
    );
    painter.rect_stroke(
        picture.expand(1.0),
        egui::CornerRadius::same(2),
        egui::Stroke::new(2.0, egui::Color32::from_rgb(0x08, 0x09, 0x0B)),
        egui::StrokeKind::Outside,
    );
    let gap = outer.bottom() - picture.bottom();
    if gap >= 10.0 {
        let led = egui::pos2(picture.left() + 18.0, picture.bottom() + gap / 2.0);
        painter.circle_filled(led, 2.5, accent);
    }
}

pub fn paint_ambient_glow(
    painter: &egui::Painter,
    area: egui::Rect,
    picture: egui::Rect,
    edges: [egui::Color32; 4],
) {
    let dim = |c: egui::Color32| c.gamma_multiply(0.45);
    let [left, right, top, bottom] = edges;
    painter.rect_filled(
        egui::Rect::from_min_max(area.left_top(), egui::pos2(picture.left(), area.bottom())),
        0.0,
        dim(left),
    );
    painter.rect_filled(
        egui::Rect::from_min_max(egui::pos2(picture.right(), area.top()), area.right_bottom()),
        0.0,
        dim(right),
    );
    painter.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(picture.left(), area.top()),
            egui::pos2(picture.right(), picture.top()),
        ),
        0.0,
        dim(top),
    );
    painter.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(picture.left(), picture.bottom()),
            egui::pos2(picture.right(), area.bottom()),
        ),
        0.0,
        dim(bottom),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(w: f32, h: f32) -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(w, h))
    }

    const NES: DisplayGrid = DisplayGrid {
        columns: 256.0,
        lines: 240.0,
    };
    const TV: f32 = 8.0 / 7.0;

    #[test]
    fn integer_locks_the_vertical_axis_and_corrects_the_horizontal() {
        // Three window sizes: exactly 2x, between 2x and 3x, and 4x tall.
        for (h, k) in [(480.0, 2.0), (700.0, 2.0), (960.0, 4.0)] {
            let r = play_rect(area(2000.0, h), NES, TV, ScaleMode::Integer);
            assert!((r.height() - 240.0 * k).abs() < 1e-3, "{h}: {r:?}");
            assert!((r.width() - 256.0 * TV * k).abs() < 1e-3, "{h}: {r:?}");
            assert_eq!(integer_multiple(r, NES), Some(k as u32));
            assert_eq!(r.center(), area(2000.0, h).center());
        }
    }

    #[test]
    fn integer_is_limited_by_width_too() {
        // 3x tall fits (720 <= 800) but 3x wide (877.7) does not fit 800.
        let r = play_rect(area(800.0, 800.0), NES, TV, ScaleMode::Integer);
        assert_eq!(integer_multiple(r, NES), Some(2));
    }

    #[test]
    fn integer_falls_back_to_fit_when_even_1x_does_not_fit() {
        let r = play_rect(area(200.0, 150.0), NES, TV, ScaleMode::Integer);
        assert!(r.width() <= 200.0 + 1e-3 && r.height() <= 150.0 + 1e-3);
        assert!(r.height() > 0.0);
    }

    #[test]
    fn fit_preserves_aspect_and_touches_one_edge() {
        for (w, h) in [(768.0, 600.0), (1920.0, 1080.0), (500.0, 900.0)] {
            let r = play_rect(area(w, h), NES, TV, ScaleMode::Fit);
            let aspect = r.width() / r.height();
            assert!((aspect - 256.0 * TV / 240.0).abs() < 1e-3);
            assert!(
                (r.width() - w).abs() < 1e-3 || (r.height() - h).abs() < 1e-3,
                "{w}x{h}: {r:?}"
            );
        }
    }

    #[test]
    fn stretch_fills_the_area() {
        let a = area(1234.0, 567.0);
        assert_eq!(play_rect(a, NES, TV, ScaleMode::Stretch), a);
    }

    #[test]
    fn square_aspect_is_unstretched() {
        let r = play_rect(area(2000.0, 480.0), NES, 1.0, ScaleMode::Integer);
        assert_eq!(r.size(), egui::vec2(512.0, 480.0));
    }

    #[test]
    fn hires_and_interlace_fold_back_to_the_same_picture() {
        assert_eq!(
            DisplayGrid::for_frame(512.0, 448.0),
            DisplayGrid::for_frame(256.0, 224.0)
        );
        // Decoded widescreen is genuinely wider and keeps its width.
        assert_eq!(DisplayGrid::for_frame(400.0, 224.0).columns, 400.0);
    }

    #[test]
    fn peek_wipes_left_to_right_and_cuts_when_sizes_differ() {
        let o = vec![1u8; 4 * 4 * 4];
        let e = vec![9u8; 4 * 4 * 4];
        assert!(
            peek_frame(&o, (4, 4), &e, (4, 4), 0.0).is_none(),
            "no peek: enhanced"
        );
        let (half, _) = peek_frame(&o, (4, 4), &e, (4, 4), 0.5).unwrap();
        assert_eq!(&half[0..8], &[1u8; 8], "left half is the original");
        assert_eq!(&half[8..16], &[9u8; 8], "right half still enhanced");
        let (full, _) = peek_frame(&o, (4, 4), &e, (4, 4), 1.0).unwrap();
        assert_eq!(full, o, "fully in: the original, byte for byte");
        let big = vec![9u8; 8 * 8 * 4];
        assert!(peek_frame(&o, (4, 4), &big, (8, 8), 0.3).is_none());
        assert_eq!(peek_frame(&o, (4, 4), &big, (8, 8), 0.6).unwrap().1, (4, 4));
    }

    #[test]
    fn edge_colours_average_each_side() {
        // 8x8: left half red, right half blue, top row overwritten green.
        let (w, h) = (8usize, 8usize);
        let mut px = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                let c = if y == 0 {
                    [0, 200, 0]
                } else if x < 4 {
                    [200, 0, 0]
                } else {
                    [0, 0, 200]
                };
                px[i..i + 3].copy_from_slice(&c);
                px[i + 3] = 255;
            }
        }
        let [left, right, top, bottom] = edge_colours(&px, w, h);
        assert!(left.r() > left.b() && left.r() > 100, "{left:?}");
        assert!(right.b() > right.r() && right.b() > 100, "{right:?}");
        assert!(
            top.g() > 40,
            "the top strip includes the green row: {top:?}"
        );
        assert_eq!(bottom.g(), 0);
        assert_eq!(edge_colours(&px[..10], w, h), [egui::Color32::BLACK; 4]);
    }

    #[test]
    fn degenerate_inputs_return_the_area() {
        let a = area(100.0, 100.0);
        assert_eq!(
            play_rect(a, DisplayGrid::exact(0.0, 240.0), TV, ScaleMode::Fit),
            a
        );
        let zero = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(0.0, 10.0));
        assert_eq!(play_rect(zero, NES, TV, ScaleMode::Integer), zero);
    }

    /// Ticket W21-04: the bezel takes what room the letterbox has, up to
    /// its maximum, and none when the picture fills the area.
    #[test]
    fn bezel_fits_the_letterbox() {
        let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let pic = egui::Rect::from_center_size(area.center(), egui::vec2(512.0, 480.0));
        let b = bezel_rect(area, pic).unwrap();
        assert_eq!(b, pic.expand(BEZEL_MAX));
        let tight = egui::Rect::from_center_size(area.center(), egui::vec2(512.0, 590.0));
        assert_eq!(bezel_rect(area, tight).unwrap(), tight.expand(5.0));
        assert!(bezel_rect(area, area).is_none());
    }

    /// Ticket W21-08: a 224-line SNES frame shows 224 rows; NES (all 240
    /// drawn), overscan (239) and interlaced frames are not shrunk wrongly.
    #[test]
    fn visible_rows_follow_the_last_scanline() {
        assert_eq!(visible_rows(240, Some(223)), 224);
        assert_eq!(visible_rows(240, Some(239)), 240);
        assert_eq!(visible_rows(240, Some(238)), 239);
        assert_eq!(visible_rows(240, None), 240);
        assert_eq!(visible_rows(478, Some(238)), 478, "interlace untouched");
        assert_eq!(visible_rows(240, Some(400)), 240, "never grows");
        let rgba = vec![7u8; 4 * 2 * 3];
        assert_eq!(crop_rows(&rgba, 2, 2).len(), 16);
        assert_eq!(crop_rows(&rgba, 2, 9).len(), 24);
    }

    /// Ticket W21-12: the window loses exactly the empty space around the
    /// picture; an already-fitting window is left alone.
    #[test]
    fn fitted_window_removes_the_borders() {
        let inner = egui::vec2(892.0, 902.0);
        // Menu + status bars take 66 px; the play area is the rest.
        let area = egui::vec2(892.0, 836.0);
        let snes_3x = egui::vec2(877.7, 672.0);
        assert_eq!(
            fitted_window(inner, area, snes_3x),
            Some(egui::vec2(878.0, 738.0))
        );
        assert_eq!(fitted_window(inner, snes_3x, snes_3x), None);
        assert_eq!(fitted_window(inner, area, egui::Vec2::ZERO), None);
    }
}
