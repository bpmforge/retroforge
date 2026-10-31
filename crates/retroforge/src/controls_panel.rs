//! The Controls screen's model and drawing (ticket W23-02).
//!
//! Brad, 2026-10-08: "what about settings for controller vs keyboard.
//! Keyboard should have defaults set and I can navigate". One screen, in
//! Settings and the Quick Menu: pick Keyboard or Controller and NES or
//! SNES, see a drawn controller with each button's key or pad button on
//! it, click a button to rebind it. The bindings themselves live in
//! `rf_input::Bindings` (NES) and `crate::snes_input` (SNES); this module
//! only names, draws and routes.

use eframe::egui;
use rf_input::{NesButton, SnesButton};

/// Which console's pad the screen shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Console {
    Nes,
    Snes,
}

/// Which device the screen binds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Device {
    Keyboard,
    Controller,
}

/// One emulated button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Nes(NesButton),
    Snes(SnesButton),
}

impl Button {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Button::Nes(b) => b.name(),
            Button::Snes(b) => b.name(),
        }
    }
}

/// Where each of a console's buttons sits on the drawn pad, in a 0..1
/// box: centre and half-size. Shoulder buttons are wide bars; the rest
/// squares or circles.
#[must_use]
pub fn layout(console: Console) -> Vec<(Button, egui::Pos2, egui::Vec2, Shape)> {
    let dpad = |up, down, left, right| {
        vec![
            (
                up,
                egui::pos2(0.22, 0.38),
                egui::vec2(0.045, 0.075),
                Shape::Square,
            ),
            (
                down,
                egui::pos2(0.22, 0.72),
                egui::vec2(0.045, 0.075),
                Shape::Square,
            ),
            (
                left,
                egui::pos2(0.15, 0.55),
                egui::vec2(0.045, 0.075),
                Shape::Square,
            ),
            (
                right,
                egui::pos2(0.29, 0.55),
                egui::vec2(0.045, 0.075),
                Shape::Square,
            ),
        ]
    };
    match console {
        Console::Nes => {
            use NesButton as N;
            let mut v = dpad(
                Button::Nes(N::Up),
                Button::Nes(N::Down),
                Button::Nes(N::Left),
                Button::Nes(N::Right),
            );
            v.extend([
                (
                    Button::Nes(N::Select),
                    egui::pos2(0.49, 0.62),
                    egui::vec2(0.05, 0.035),
                    Shape::Pill,
                ),
                (
                    Button::Nes(N::Start),
                    egui::pos2(0.61, 0.62),
                    egui::vec2(0.05, 0.035),
                    Shape::Pill,
                ),
                (
                    Button::Nes(N::B),
                    egui::pos2(0.73, 0.60),
                    egui::vec2(0.055, 0.09),
                    Shape::Circle,
                ),
                (
                    Button::Nes(N::A),
                    egui::pos2(0.86, 0.60),
                    egui::vec2(0.055, 0.09),
                    Shape::Circle,
                ),
            ]);
            v
        }
        Console::Snes => {
            use SnesButton as S;
            let mut v = dpad(
                Button::Snes(S::Up),
                Button::Snes(S::Down),
                Button::Snes(S::Left),
                Button::Snes(S::Right),
            );
            v.extend([
                (
                    Button::Snes(S::L),
                    egui::pos2(0.22, 0.10),
                    egui::vec2(0.12, 0.05),
                    Shape::Pill,
                ),
                (
                    Button::Snes(S::R),
                    egui::pos2(0.80, 0.10),
                    egui::vec2(0.12, 0.05),
                    Shape::Pill,
                ),
                (
                    Button::Snes(S::Select),
                    egui::pos2(0.48, 0.60),
                    egui::vec2(0.05, 0.035),
                    Shape::Pill,
                ),
                (
                    Button::Snes(S::Start),
                    egui::pos2(0.60, 0.60),
                    egui::vec2(0.05, 0.035),
                    Shape::Pill,
                ),
                (
                    Button::Snes(S::X),
                    egui::pos2(0.80, 0.36),
                    egui::vec2(0.045, 0.075),
                    Shape::Circle,
                ),
                (
                    Button::Snes(S::Y),
                    egui::pos2(0.71, 0.55),
                    egui::vec2(0.045, 0.075),
                    Shape::Circle,
                ),
                (
                    Button::Snes(S::A),
                    egui::pos2(0.89, 0.55),
                    egui::vec2(0.045, 0.075),
                    Shape::Circle,
                ),
                (
                    Button::Snes(S::B),
                    egui::pos2(0.80, 0.74),
                    egui::vec2(0.045, 0.075),
                    Shape::Circle,
                ),
            ]);
            v
        }
    }
}

/// How a button is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Square,
    Circle,
    Pill,
}

/// The SNES face buttons' own colours (the Super Famicom's), drawn only
/// as a thin ring so the binding text stays readable.
fn face_colour(b: Button) -> Option<egui::Color32> {
    match b {
        Button::Snes(SnesButton::A) => Some(egui::Color32::from_rgb(0xD8, 0x3A, 0x3A)),
        Button::Snes(SnesButton::B) => Some(egui::Color32::from_rgb(0xE8, 0xC3, 0x4A)),
        Button::Snes(SnesButton::X) => Some(egui::Color32::from_rgb(0x5A, 0x86, 0xE2)),
        Button::Snes(SnesButton::Y) => Some(egui::Color32::from_rgb(0x4C, 0xAE, 0x6A)),
        Button::Nes(NesButton::A | NesButton::B) => Some(egui::Color32::from_rgb(0xC8, 0x43, 0x3A)),
        _ => None,
    }
}

/// What [`draw_laid_out`] put on screen, for tests: the pad's whole area,
/// each button's rect and each binding label's rect.
pub struct Drawn {
    pub clicked: Option<Button>,
    pub area: egui::Rect,
    pub buttons: Vec<(Button, egui::Rect)>,
    pub labels: Vec<(Button, egui::Rect)>,
}

/// A controller button as a player says it (W42-09): "Up", "L", "South",
/// not the saved config name ("DpadUp", "LeftShoulder"). The saved name
/// (`PadButton::name`) is unchanged.
#[must_use]
pub fn pad_label(p: rf_input::PadButton) -> &'static str {
    use rf_input::PadButton as P;
    match p {
        P::DpadUp => "Up",
        P::DpadDown => "Down",
        P::DpadLeft => "Left",
        P::DpadRight => "Right",
        P::LeftShoulder => "L",
        P::RightShoulder => "R",
        P::LeftStickUp => "Stick Up",
        P::LeftStickDown => "Stick Down",
        P::LeftStickLeft => "Stick Left",
        P::LeftStickRight => "Stick Right",
        other => other.name(),
    }
}

/// Where a button's binding label is anchored, relative to its rect `r`.
/// D-pad labels sit outward from the cross, shoulder labels inward along
/// the top edge, everything else underneath.
fn label_anchor(button: Button, r: egui::Rect) -> (egui::Pos2, egui::Align2) {
    match short_name(button) {
        x if x == egui_phosphor::regular::CARET_UP => (
            egui::pos2(r.center().x, r.top() - 3.0),
            egui::Align2::CENTER_BOTTOM,
        ),
        x if x == egui_phosphor::regular::CARET_LEFT => (
            egui::pos2(r.left() - 5.0, r.center().y),
            egui::Align2::RIGHT_CENTER,
        ),
        x if x == egui_phosphor::regular::CARET_RIGHT => (
            egui::pos2(r.right() + 5.0, r.center().y),
            egui::Align2::LEFT_CENTER,
        ),
        _ if button == Button::Snes(SnesButton::L) => (
            egui::pos2(r.right() + 6.0, r.center().y),
            egui::Align2::LEFT_CENTER,
        ),
        _ if button == Button::Snes(SnesButton::R) => (
            egui::pos2(r.left() - 6.0, r.center().y),
            egui::Align2::RIGHT_CENTER,
        ),
        _ => (
            egui::pos2(r.center().x, r.bottom() + 3.0),
            egui::Align2::CENTER_TOP,
        ),
    }
}

/// Draw the pad for `console`. `binding` names what each button is bound
/// to on the chosen device ("Z", "South", or "—"); `waiting` is the button
/// waiting for a key or pad press. Returns the button clicked, if any.
/// Each button is a Button to accessibility, named "<button>: <binding>".
/// The diagram is sized to the available width and every label is kept
/// inside it.
pub fn draw(
    ui: &mut egui::Ui,
    tokens: &crate::theme::Tokens,
    console: Console,
    binding: &dyn Fn(Button) -> String,
    waiting: Option<Button>,
) -> Option<Button> {
    draw_laid_out(ui, tokens, console, binding, waiting).clicked
}

/// [`draw`], also reporting where everything landed.
pub fn draw_laid_out(
    ui: &mut egui::Ui,
    tokens: &crate::theme::Tokens,
    console: Console,
    binding: &dyn Fn(Button) -> String,
    waiting: Option<Button>,
) -> Drawn {
    let w = ui.available_width().min(520.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, w * 0.46), egui::Sense::hover());
    let body = rect.shrink2(egui::vec2(4.0, rect.height() * 0.16));
    let painter = ui.painter_at(rect);
    painter.rect_filled(body, egui::CornerRadius::same(28), tokens.surface);
    painter.rect_stroke(
        body,
        egui::CornerRadius::same(28),
        egui::Stroke::new(1.0, tokens.line),
        egui::StrokeKind::Inside,
    );
    let mut drawn = Drawn {
        clicked: None,
        area: rect,
        buttons: Vec::new(),
        labels: Vec::new(),
    };
    for (button, c, half, shape) in layout(console) {
        let centre = rect.min + egui::vec2(c.x * rect.width(), c.y * rect.height());
        let size = egui::vec2(half.x * rect.width() * 2.0, half.y * rect.height() * 2.0);
        let r = egui::Rect::from_center_size(centre, size);
        drawn.buttons.push((button, r));
        let id = ui.id().with(("ctl", button.name()));
        let response = ui.interact(r, id, egui::Sense::click());
        let bound = binding(button);
        let spoken = format!("{}: {}", button.name(), bound);
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &spoken));
        let is_waiting = waiting == Some(button);
        let fill = if is_waiting {
            tokens.accent
        } else if response.hovered() || response.has_focus() {
            tokens.accent_soft
        } else {
            tokens.bg
        };
        let radius = match shape {
            Shape::Square => egui::CornerRadius::same(4),
            Shape::Circle | Shape::Pill => egui::CornerRadius::same(u8::MAX),
        };
        painter.rect_filled(r, radius, fill);
        let ring = face_colour(button).unwrap_or(tokens.line);
        painter.rect_stroke(
            r,
            radius,
            egui::Stroke::new(
                if face_colour(button).is_some() {
                    2.0
                } else {
                    1.0
                },
                ring,
            ),
            egui::StrokeKind::Inside,
        );
        if response.has_focus() {
            painter.rect_stroke(
                r.expand(3.0),
                radius,
                egui::Stroke::new(2.0, tokens.accent_strong),
                egui::StrokeKind::Outside,
            );
        }
        // The button's own name inside, its binding next to it.
        painter.text(
            r.center(),
            egui::Align2::CENTER_CENTER,
            short_name(button),
            egui::FontId::proportional(11.0),
            if is_waiting { tokens.bg } else { tokens.muted },
        );
        let label = if is_waiting {
            "press\u{2026}".to_owned()
        } else {
            bound
        };
        let colour = if is_waiting {
            tokens.accent
        } else {
            tokens.ink
        };
        let galley = painter.layout_no_wrap(label, crate::theme::numeric(12.0), colour);
        let (at, align) = label_anchor(button, r);
        let mut lr = align.anchor_size(at, galley.size());
        // Keep the label inside the diagram, whatever its length.
        let bounds = rect.shrink(2.0);
        lr = lr.translate(egui::vec2(
            (bounds.left() - lr.left()).max(0.0) + (bounds.right() - lr.right()).min(0.0),
            (bounds.top() - lr.top()).max(0.0) + (bounds.bottom() - lr.bottom()).min(0.0),
        ));
        painter.galley(lr.min, galley, colour);
        drawn.labels.push((button, lr));
        if response.clicked() {
            drawn.clicked = Some(button);
        }
    }
    drawn
}

/// A key as the Controls screen shows it: arrows as arrows, the long
/// modifier names shortened. The saved name (`Key::name`) is unchanged.
#[must_use]
pub fn key_label(key: rf_input::Key) -> &'static str {
    use rf_input::Key as K;
    match key {
        K::ArrowUp => egui_phosphor::regular::ARROW_UP,
        K::ArrowDown => egui_phosphor::regular::ARROW_DOWN,
        K::ArrowLeft => egui_phosphor::regular::ARROW_LEFT,
        K::ArrowRight => egui_phosphor::regular::ARROW_RIGHT,
        K::RightShift => "R Shift",
        K::LeftShift => "L Shift",
        K::RightCtrl => "R Ctrl",
        K::LeftCtrl => "L Ctrl",
        K::RightAlt => "R Alt",
        K::LeftAlt => "L Alt",
        K::Backspace => "Bksp",
        other => other.name(),
    }
}

/// The left stick's directions: an extra D-pad, never shown as "the"
/// binding of a button.
#[must_use]
pub fn is_stick(p: rf_input::PadButton) -> bool {
    use rf_input::PadButton as P;
    matches!(
        p,
        P::LeftStickUp | P::LeftStickDown | P::LeftStickLeft | P::LeftStickRight
    )
}

/// The glyph drawn inside a button: arrows for the D-pad, the letter or
/// word otherwise.
fn short_name(b: Button) -> &'static str {
    match b {
        Button::Nes(NesButton::Up) | Button::Snes(SnesButton::Up) => {
            egui_phosphor::regular::CARET_UP
        }
        Button::Nes(NesButton::Down) | Button::Snes(SnesButton::Down) => {
            egui_phosphor::regular::CARET_DOWN
        }
        Button::Nes(NesButton::Left) | Button::Snes(SnesButton::Left) => {
            egui_phosphor::regular::CARET_LEFT
        }
        Button::Nes(NesButton::Right) | Button::Snes(SnesButton::Right) => {
            egui_phosphor::regular::CARET_RIGHT
        }
        Button::Nes(NesButton::Select) | Button::Snes(SnesButton::Select) => "SELECT",
        Button::Nes(NesButton::Start) | Button::Snes(SnesButton::Start) => "START",
        other => other.name(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every button of each console is on its drawn pad, once.
    #[test]
    fn every_button_is_drawn_once() {
        let nes: Vec<_> = layout(Console::Nes).into_iter().map(|(b, ..)| b).collect();
        assert_eq!(nes.len(), NesButton::ALL.len());
        for b in NesButton::ALL {
            assert!(nes.contains(&Button::Nes(b)), "{b:?}");
        }
        let snes: Vec<_> = layout(Console::Snes).into_iter().map(|(b, ..)| b).collect();
        assert_eq!(snes.len(), SnesButton::ALL.len());
        for b in SnesButton::ALL {
            assert!(snes.contains(&Button::Snes(b)), "{b:?}");
        }
    }

    /// W42-09: at any sheet width the labels stay inside the diagram and
    /// never sit on another control or label (worst-case label text).
    #[test]
    fn labels_stay_inside_and_clear_of_controls() {
        let tokens = crate::theme::Tokens::dark();
        for console in [Console::Nes, Console::Snes] {
            for width in [380.0_f32, 420.0, 520.0] {
                let ctx = egui::Context::default();
                let mut drawn = None;
                let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                    ui.set_max_width(width);
                    let longest = |_b: Button| "Right".to_owned();
                    drawn = Some(draw_laid_out(ui, &tokens, console, &longest, None));
                });
                let d = drawn.expect("drawn");
                assert!(d.area.width() <= width + 0.5);
                for (b, l) in &d.labels {
                    assert!(d.area.contains_rect(*l), "{console:?}@{width} {b:?} {l:?}");
                    for (ob, r) in &d.buttons {
                        assert!(
                            !l.intersects(*r),
                            "{console:?}@{width} label {b:?} on {ob:?}"
                        );
                    }
                    for (ob, o) in &d.labels {
                        assert!(
                            ob == b || !l.intersects(*o),
                            "{console:?}@{width} {b:?}/{ob:?}"
                        );
                    }
                }
            }
        }
    }
}
