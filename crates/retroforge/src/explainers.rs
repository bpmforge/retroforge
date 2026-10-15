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
        "superfx_overclock" => {
            "Cartridges with a Super FX chip draw their 3D scenes in software on that chip, \
             which is why they run at low frame rates. This gives the chip more speed in the \
             same way as the CPU option: Governed (the default) only while the game is \
             running slow, Fixed always, which makes some games run faster than intended. \
             It changes the simulation, so it is off until you turn it on."
        }
        "cpu_overclock" => {
            "Some SNES games slow down when too much happens at once, because the main \
             processor runs out of time in a frame. Governed (the default) gives it more \
             speed only while the game is running slow, and takes it back the moment the \
             game is back at its normal pace, so nothing runs faster than it was meant to. \
             The slider is the most it may use. Fixed always runs at the slider's speed, \
             which makes some games run faster than intended. It changes the simulation, \
             so it is off until you turn it on."
        }
        "sa1_overclock" => {
            "Cartridges with an SA-1 chip (such as Kirby Super Star or Super Mario RPG) do \
             their heavy work on that second, faster processor, and can still slow down. \
             This gives the chip more speed in the same way as the CPU option: Governed \
             only while the game is running slow, Fixed always. It changes the simulation, \
             so it is off until you turn it on."
        }
        "nes_extra_lines" => {
            "NES games slow down or flicker when their code does not finish before the \
             picture needs it. This gives the processor extra time between frames while the \
             picture and sound wait, so the game finishes its work. Some games may glitch; \
             turn it down or off if one does. It changes the simulation, so it is off until \
             you turn it on."
        }
        "run_ahead" => {
            "Many games take a frame or two to react to a button press. This quietly plays \
             the game one or two frames further ahead, with your buttons held as they are, and \
             shows you that picture, then rewinds those frames so the real game carries on \
             exactly as before. Sound and saves come from the real game. It costs extra \
             processor time every frame, and it is not used while you are debugging."
        }
        "snes_interpolation" => {
            "The SNES sound chip plays each recorded sound back at the pitch it needs by \
             blending neighbouring samples with a soft filter, which is part of how the \
             console sounds. This swaps that filter, for what you hear only, for Cubic \
             (smoothest), Linear, or None (raw, crunchy steps). Echo and everything the \
             game can read stay exactly as the console has them, and it is off until you \
             turn it on."
        }
        "hd_mode7" => {
            "Mode 7 floors are drawn one sample per screen pixel, so racetracks and world \
             maps look blocky up close. This re-runs the console's own Mode 7 maths at a \
             finer step, so the floor is sharper. Perspective smoothing blends between \
             scanlines; supersampling smooths edges at some CPU cost."
        }
        "hd2d_lens" => {
            "A set of picture effects laid over the finished frame, each on its own switch \
             and strength: a glow around bright spots, a miniature-model blur above and below \
             a sharp band, soft drop shadows under sprites, a day-to-night tint you can set \
             or let follow the clock, and rain or snow. The game itself is untouched; it \
             needs a graphics card."
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
        "superfx_overclock" | "cpu_overclock" | "sa1_overclock" => {
            // Two clock traces: the original period and a faster one.
            for (i, ticks) in [(0, 4), (1, 8)] {
                let y = r.top() + r.height() * (0.35 + 0.35 * i as f32);
                let w = r.width() / ticks as f32;
                for t in 0..ticks {
                    let x = r.left() + w * t as f32;
                    painter.rect_filled(
                        egui::Rect::from_min_size(
                            egui::pos2(x + w * 0.1, y - 6.0),
                            egui::vec2(w * 0.5, 12.0),
                        ),
                        egui::CornerRadius::same(1),
                        if i == 0 { faint } else { accent },
                    );
                }
            }
        }
        "nes_extra_lines" => {
            // A frame's picture lines, then extra lines before the next
            // frame in the accent colour.
            let n = 8;
            let h = (r.height() - 8.0) / n as f32;
            for i in 0..n {
                painter.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(r.left() + 4.0, r.top() + 4.0 + h * i as f32 + 1.0),
                        egui::vec2(r.width() - 8.0, h - 2.0),
                    ),
                    egui::CornerRadius::same(1),
                    if i < 5 { faint } else { accent },
                );
            }
        }
        "hd2d_lens" => {
            // A sharp middle band between soft, fading bands, and a glow.
            for i in 0..3 {
                let h = r.height() * 0.14;
                for (y, fade) in [
                    (r.top() + 2.0 + h * i as f32, 0.10 + 0.07 * (2 - i) as f32),
                    (
                        r.bottom() - 2.0 - h * (i + 1) as f32,
                        0.10 + 0.07 * (2 - i) as f32,
                    ),
                ] {
                    painter.rect_filled(
                        egui::Rect::from_min_size(
                            egui::pos2(r.left() + 4.0, y),
                            egui::vec2(r.width() - 8.0, h),
                        ),
                        egui::CornerRadius::ZERO,
                        ink.gamma_multiply(fade),
                    );
                }
            }
            let band = egui::Rect::from_center_size(
                r.center(),
                egui::vec2(r.width() - 8.0, r.height() * 0.28),
            );
            painter.rect_filled(band, egui::CornerRadius::same(2), faint);
            painter.circle_filled(band.center(), 5.0, accent);
            painter.circle_filled(band.center(), 10.0, accent.gamma_multiply(0.25));
        }
        "run_ahead" => {
            // A row of frames: the real ones in the faint colour, and the
            // picture shown reaching one frame past the latest real one.
            let n = 5;
            let w = r.width() / n as f32;
            let y = r.center().y;
            for i in 0..n {
                let cell = egui::Rect::from_min_size(
                    egui::pos2(r.left() + w * i as f32 + 3.0, y - 9.0),
                    egui::vec2(w - 6.0, 18.0),
                );
                let shown = i == n - 1;
                painter.rect_filled(
                    cell,
                    egui::CornerRadius::same(2),
                    if shown { accent } else { faint },
                );
            }
            painter.hline(r.left()..=r.right(), y + 18.0, thin);
        }
        "snes_interpolation" => {
            // A stepped waveform (the raw samples) and a smooth curve.
            let n = 16;
            let w = r.width() / n as f32;
            let mid = r.center().y;
            let amp = r.height() * 0.3;
            let level = |i: usize| ((i as f32 / n as f32) * std::f32::consts::TAU).sin() * amp;
            for i in 0..n {
                let x = r.left() + w * i as f32;
                painter.hline(x..=x + w, mid - level(i / 4 * 4), thin);
            }
            let pts: Vec<egui::Pos2> = (0..=n)
                .map(|i| egui::pos2(r.left() + w * i as f32, mid - level(i)))
                .collect();
            painter.add(egui::Shape::line(pts, egui::Stroke::new(2.0, accent)));
            let _ = faint;
        }
        "hd_mode7" => {
            // The same floor sampled coarsely (left) and finely (right).
            let half = r.width() / 2.0;
            let grid = (half - 8.0).min(r.height() - 8.0);
            for (side, cells) in [(0, 3), (1, 6)] {
                let cell = grid / cells as f32;
                let left = r.left() + half * side as f32 + 4.0;
                for cy in 0..cells {
                    for cx in 0..cells {
                        if (cx + cy) % 2 == 0 {
                            painter.rect_filled(
                                egui::Rect::from_min_size(
                                    egui::pos2(
                                        left + cell * cx as f32,
                                        r.center().y - grid / 2.0 + cell * cy as f32,
                                    ),
                                    egui::vec2(cell, cell),
                                ),
                                egui::CornerRadius::ZERO,
                                if side == 0 { faint } else { accent },
                            );
                        }
                    }
                }
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
