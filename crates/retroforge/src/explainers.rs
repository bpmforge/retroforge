//! "What is this?" for each enhancement (ticket W22-05;
//! `docs/design/UX_WAVE_22.md`).
//!
//! A longer explanation than the card's one line, and a small diagram
//! painted from shapes. The diagrams are drawn here, not taken from any
//! game (law 5: no copyrighted art in the shell), and stay in the theme's
//! colours so they read in every palette.

use eframe::egui;

/// The explainer text for a feature, by its row id.
#[must_use]
pub fn long_text(id: &str) -> &'static str {
    match id {
        "sprite_overlay" => {
            "The console can only draw a few sprites on one line (8 on the NES, 32 on the \
             SNES). In a busy scene the extra ones vanish or flicker. This draws all of \
             them on top of the original picture. The game itself runs exactly the same."
        }
        "deflicker" => {
            "Some games draw half their sprites on one frame and the other half on the \
             next, which you see as flicker. This holds each sprite steady by remembering \
             where it was on the frame it was hidden."
        }
        "widescreen_decoded" => {
            "Games keep more of the level in memory than the screen shows. With a profile \
             that says where, RetroForge draws that scenery beyond the screen's edges. The \
             middle stays pixel-for-pixel what the console drew."
        }
        "full_level_view" => {
            "Builds a map of the whole level from the game's own level data as you play, \
             with a box for what you can see and a dot for you."
        }
        "loading_fast_forward" => {
            "Some games wait on loading or transition screens. A profile lists those waits, \
             and RetroForge runs through them at full speed. Gameplay is never sped up."
        }
        "atmosphere_fog" => {
            "Where a game draws fog or mist on its own layer, this adds gentle depth and \
             drift to it. It needs a profile that names the fog layer."
        }
        "diorama" => {
            "Raises the level's walls and floors into a tilted 3D scene you look across. \
             Your controls and the game stay exactly 2D."
        }
        "mode7_ground" => {
            "SNES games use Mode 7 for flat rotating floors such as race tracks and world \
             maps. This turns that floor into real 3D ground with a horizon. It appears \
             only while the game is using Mode 7."
        }
        _ => "An enhancement.",
    }
}

/// Paint `id`'s diagram into `rect`.
pub fn paint(painter: &egui::Painter, rect: egui::Rect, id: &str, tokens: &crate::theme::Tokens) {
    let ink = tokens.ink;
    let accent = tokens.accent;
    let faint = tokens.line;
    let warn = tokens.warn;
    painter.rect_filled(rect, egui::CornerRadius::same(6), tokens.bg);
    let r = rect.shrink(10.0);
    let thin = egui::Stroke::new(1.0, faint);
    match id {
        "sprite_overlay" => {
            // A scanline with six sprites; past the limit they are dashed
            // on the left half and solid on the right.
            let y = r.center().y;
            for half in 0..2 {
                let x0 = r.left() + r.width() * 0.5 * half as f32;
                painter.hline(x0..=x0 + r.width() * 0.46, y + 12.0, thin);
                for i in 0..6 {
                    let s = egui::Rect::from_min_size(
                        egui::pos2(x0 + 6.0 + i as f32 * 16.0, y - 6.0),
                        egui::vec2(12.0, 12.0),
                    );
                    if i >= 4 && half == 0 {
                        painter.rect_stroke(
                            s,
                            egui::CornerRadius::same(2),
                            egui::Stroke::new(1.0, faint),
                            egui::StrokeKind::Inside,
                        );
                    } else {
                        painter.rect_filled(
                            s,
                            egui::CornerRadius::same(2),
                            if i >= 4 { accent } else { ink },
                        );
                    }
                }
            }
        }
        "deflicker" => {
            for (i, alpha) in [(0, 0.35_f32), (1, 1.0)] {
                let c = egui::pos2(r.left() + r.width() * (0.3 + 0.4 * i as f32), r.center().y);
                painter.circle_filled(c, 12.0, ink.gamma_multiply(alpha));
            }
            painter.arrow(
                egui::pos2(r.center().x - 14.0, r.center().y),
                egui::vec2(28.0, 0.0),
                egui::Stroke::new(1.5, accent),
            );
        }
        "widescreen_decoded" => {
            painter.rect_filled(r, egui::CornerRadius::same(3), faint);
            let mid =
                egui::Rect::from_center_size(r.center(), egui::vec2(r.width() * 0.45, r.height()));
            painter.rect_filled(mid, egui::CornerRadius::same(2), accent.gamma_multiply(0.6));
            painter.rect_stroke(
                mid,
                egui::CornerRadius::same(2),
                egui::Stroke::new(1.5, ink),
                egui::StrokeKind::Inside,
            );
        }
        "full_level_view" => {
            let strip =
                egui::Rect::from_center_size(r.center(), egui::vec2(r.width(), r.height() * 0.5));
            painter.rect_filled(strip, egui::CornerRadius::same(2), faint);
            let view = egui::Rect::from_min_size(
                egui::pos2(strip.left() + strip.width() * 0.2, strip.top()),
                egui::vec2(strip.width() * 0.18, strip.height()),
            );
            painter.rect_stroke(
                view,
                egui::CornerRadius::ZERO,
                egui::Stroke::new(1.5, ink),
                egui::StrokeKind::Inside,
            );
            painter.circle_filled(view.center(), 4.0, warn);
        }
        "loading_fast_forward" => {
            let bar = egui::Rect::from_center_size(r.center(), egui::vec2(r.width() * 0.8, 8.0));
            painter.rect_filled(bar, egui::CornerRadius::same(4), faint);
            let fill =
                egui::Rect::from_min_size(bar.min, egui::vec2(bar.width() * 0.8, bar.height()));
            painter.rect_filled(fill, egui::CornerRadius::same(4), accent);
        }
        "atmosphere_fog" => {
            for i in 0..4 {
                let band = egui::Rect::from_min_size(
                    egui::pos2(r.left(), r.top() + r.height() * (0.2 + 0.18 * i as f32)),
                    egui::vec2(r.width(), r.height() * 0.12),
                );
                painter.rect_filled(
                    band,
                    egui::CornerRadius::same(4),
                    ink.gamma_multiply(0.08 + 0.06 * i as f32),
                );
            }
        }
        "diorama" | "mode7_ground" => {
            // A floor seen at an angle: a trapezoid with receding lines.
            let top = r.top() + r.height() * 0.3;
            let pts = vec![
                egui::pos2(r.left() + r.width() * 0.3, top),
                egui::pos2(r.right() - r.width() * 0.3, top),
                egui::pos2(r.right(), r.bottom()),
                egui::pos2(r.left(), r.bottom()),
            ];
            painter.add(egui::Shape::convex_polygon(
                pts,
                faint,
                egui::Stroke::new(1.0, ink),
            ));
            for i in 1..4 {
                let t = i as f32 / 4.0;
                let y = top + (r.bottom() - top) * t * t;
                let inset = r.width() * 0.3 * (1.0 - (y - top) / (r.bottom() - top));
                painter.hline(
                    r.left() + inset..=r.right() - inset,
                    y,
                    egui::Stroke::new(1.0, ink.gamma_multiply(0.5)),
                );
            }
            if id == "diorama" {
                let wall = egui::Rect::from_min_size(
                    egui::pos2(r.center().x - 12.0, top + 4.0),
                    egui::vec2(24.0, 20.0),
                );
                painter.rect_filled(wall, egui::CornerRadius::same(2), accent);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every feature the panel shows has its own explainer.
    #[test]
    fn every_feature_is_explained() {
        let s = crate::game_settings::GameSettings::default();
        let rows = crate::enhance_ui::feature_rows(
            &s,
            &crate::enhance_ui::GameFacts::new(true, true, true),
        );
        for row in rows {
            let text = long_text(row.id);
            assert_ne!(text, "An enhancement.", "{} has no explainer", row.id);
            assert!(text.len() > 80, "{} explainer is too thin", row.id);
        }
    }
}
