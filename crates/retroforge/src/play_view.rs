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
) -> egui::Response {
    let rect = play_rect(ui.available_rect_before_wrap(), grid, par, mode);
    ui.put(
        rect,
        egui::Image::from_texture(texture).fit_to_exact_size(rect.size()),
    )
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
    fn degenerate_inputs_return_the_area() {
        let a = area(100.0, 100.0);
        assert_eq!(
            play_rect(a, DisplayGrid::exact(0.0, 240.0), TV, ScaleMode::Fit),
            a
        );
        let zero = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(0.0, 10.0));
        assert_eq!(play_rect(zero, NES, TV, ScaleMode::Integer), zero);
    }
}
