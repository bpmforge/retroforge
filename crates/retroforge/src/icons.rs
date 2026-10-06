//! The shell's symbol vocabulary (ticket W20-05; `docs/design/UX_WAVE_20.md`
//! §2 principle 10).
//!
//! Every symbol the UI draws comes from the Phosphor icon font
//! (`egui-phosphor` 0.13, MIT), registered by [`crate::theme::install_fonts`]
//! as a fallback after the body face — never from whatever Unicode the body
//! face happens to cover. That habit shipped five tofu boxes (`◆`, `◇`,
//! `●`, `▦`, `≡`) and a honesty badge whose `⚡` was itself a box.
//! `tests/glyphs_render.rs` now scans every string literal in `src/` and
//! fails on any character the installed fonts lack.
//!
//! Names here are **roles** ("favourite", "has save states"), not shapes,
//! so swapping an icon is one edit and a reader of a call site knows what
//! it means rather than what it looks like.
//!
//! Icon-only buttons go through [`icon_button`], which gives them a real
//! accessible name: a private-use codepoint read aloud is noise, and that
//! is what a screen reader got for the bare `☆` favourite button.

use eframe::egui;
use egui_phosphor::regular as ph;

/// `FontFamily` name the Phosphor **Fill** weight is registered under,
/// for the few icons whose on/off state is filled vs outline.
pub const FILL_FAMILY: &str = "phosphor-fill";

pub const GRID_VIEW: &str = ph::GRID_FOUR;
pub const LIST_VIEW: &str = ph::LIST;
/// Favourite star; drawn filled ([`filled`]) when on, outline when off.
pub const FAVOURITE: &str = ph::STAR;
pub const PROFILE: &str = ph::BOOKMARK_SIMPLE;
pub const HAS_STATES: &str = ph::FLOPPY_DISK;
pub const ENHANCED_SET: &str = ph::SPARKLE;
/// The honesty badge's enhancement mark (`NES · Enhanced ⚡(2)` before
/// W20-05, when the `⚡` rendered as a tofu box).
pub const ENHANCED_BADGE: &str = ph::LIGHTNING;
pub const WARNING: &str = ph::WARNING;
pub const INFO: &str = ph::INFO;
pub const SUCCESS: &str = ph::CHECK_CIRCLE;
pub const ARROW: &str = ph::ARROW_RIGHT;
pub const BULLET: &str = ph::DOT_OUTLINE;

/// `icon` in the Fill weight.
#[must_use]
pub fn filled(icon: &str) -> egui::RichText {
    egui::RichText::new(icon).family(egui::FontFamily::Name(FILL_FAMILY.into()))
}

/// An icon-only button whose accessible name and tooltip are `label`.
pub fn icon_button(
    ui: &mut egui::Ui,
    icon: impl Into<egui::WidgetText>,
    label: &str,
) -> egui::Response {
    let response = ui.button(icon).on_hover_text(label);
    let enabled = response.enabled();
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label));
    response
}

/// An icon-only selectable toggle (e.g. Grid/List) with an accessible name.
pub fn icon_toggle(ui: &mut egui::Ui, selected: bool, icon: &str, label: &str) -> egui::Response {
    let response = ui.selectable_label(selected, icon).on_hover_text(label);
    let enabled = response.enabled();
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, enabled, selected, label)
    });
    response
}

/// A small status icon with an accessible name (badges on library rows).
pub fn icon_label(ui: &mut egui::Ui, icon: &str, label: &str) -> egui::Response {
    let response = ui.label(icon).on_hover_text(label);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, label));
    response
}
