//! Non-blocking status toasts (ticket W15-04; `docs/design/UX_WAVE_15.md`
//! §5).
//!
//! §5's table sends four things here: ROM folder added, library
//! rescanned, state saved, and script error — the last of these replacing
//! a plain status string nobody reading `crate::app`'s `status` field
//! would ever be certain to see (the gap the review calls out against
//! user story E7-S1). None of these are decisions; a toast never blocks
//! input, per FRONTEND_UI §1 principle 3 ("the UI never blocks
//! emulation") — that's what separates this from `egui::Modal`, which
//! W15-04 also introduces for the three dialogs that DO need to block.
//!
//! ## Why this is hand-rolled instead of `egui-notify`
//!
//! `docs/design/UX_WAVE_15.md` §5 names `egui-notify` (MIT) as the
//! intended dependency. Checked against the actual published crate
//! (project law 2 — verify every external API against source, not
//! memory) rather than assumed: `egui-notify` 0.22.0 depends on
//! `egui = "0.34"` and 0.23.0 depends on `egui = "0.36"` — verified by
//! reading each version's `[dependencies.egui]` table directly out of
//! the fetched crate sources in `~/.cargo/registry/src`. No published
//! release targets this workspace's `egui = "0.35"` pin
//! (`docs/TECH_STACK.md`), and Cargo's caret matching means neither
//! resolves against it. Rather than force a mismatched egui into the
//! graph (two copies of egui in one binary do not talk to the same
//! `Context`) or bump the whole workspace's egui/eframe/egui_kittest pin
//! as a side effect of a two-point ticket, this ticket ships a minimal
//! in-crate stack instead, per its own brief's fallback instruction.
//!
//! ## Ticks on `Context::time`, not the wall clock
//!
//! Every other timer in this crate (`fps_window_start`, the core
//! thread's pacer) uses `std::time::Instant`. Toasts deliberately do not:
//! `egui_kittest`'s harness never sets `RawInput::time`, so
//! `egui::Context` accumulates its own clock as
//! `self.time + predicted_dt` each step (verified in
//! `egui-0.35.0/src/input_state/mod.rs`) — which means `ctx.time()`
//! advances deterministically with `Harness::run_steps`, and a test can
//! expire a toast by stepping the harness instead of sleeping the real
//! test process for real seconds. `Instant::now()` would not move at all
//! across a headless test unless the test slept for wall-clock time,
//! which is exactly the flake-prone pattern law 8's spirit warns against
//! elsewhere in this crate.

use eframe::egui;

/// How long a toast stays up before it starts fading, in seconds of
/// `egui::Context::time`.
pub const DEFAULT_DURATION: f64 = 4.0;
/// How long the fade-out takes. Kept short and separate from
/// `DEFAULT_DURATION` so the "one moving element" rule
/// (`docs/design/UX_WAVE_15.md` §5 styling note) has exactly one thing to
/// point at: the toast's own opacity, nothing else on screen animates.
const FADE_SECS: f64 = 0.3;

/// What a toast is reporting, per FRONTEND_UI's honesty principle: the
/// state is in the WORD (`av_sync_indicator`'s own rule, `crate::app`),
/// not carried by colour alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Error,
    /// Ticket W20-14: a recording in progress — its own icon, so the card
    /// does not carry an "info" glyph beside the record dot.
    Recording,
}

impl ToastKind {
    fn glyph(self) -> &'static str {
        match self {
            ToastKind::Info => crate::icons::INFO,
            ToastKind::Success => crate::icons::SUCCESS,
            ToastKind::Error => crate::icons::WARNING,
            ToastKind::Recording => egui_phosphor::regular::RECORD,
        }
    }
}

struct Toast {
    text: String,
    kind: ToastKind,
    /// `ctx.time()` when this toast was pushed.
    created_at: f64,
    /// Ticket W20-12: an OSD card's picture (the frame saved, the slot
    /// loaded, the screenshot taken).
    thumb: Option<egui::TextureHandle>,
    /// Ticket W20-12: a card with a key replaces any live card with the
    /// same key instead of stacking — "Fast-forward" refreshed every frame
    /// it is held is ONE card, not sixty.
    key: Option<&'static str>,
}

/// A bottom-right stack of auto-expiring, non-blocking notifications.
///
/// Owned by `RetroForgeApp` and drawn once per frame from
/// `impl eframe::App::ui` — see that call site's comment for why it runs
/// after every window/modal, not before.
pub struct ToastStack {
    toasts: Vec<Toast>,
    /// How long a toast stays fully visible before it fades.
    duration: f64,
    /// `egui::Area` id — the toast stack and the OSD stack must differ.
    id: &'static str,
}

impl Default for ToastStack {
    fn default() -> Self {
        Self {
            toasts: Vec::new(),
            duration: DEFAULT_DURATION,
            id: "rf_toast_stack",
        }
    }
}

/// Ticket W20-12 (`docs/design/UX_WAVE_20.md` §5): how long an
/// on-screen-display card stays before fading — shorter than a toast; it
/// reports something the player just did, over the picture they are
/// looking at.
pub const OSD_DURATION: f64 = 2.0;
/// Where a stack is drawn: anchored to a screen corner (toasts) or at a
/// fixed point (OSD cards, inside the game picture).
enum Placement {
    Anchor(egui::Align2, egui::Vec2),
    At(egui::Pos2),
}

/// Thumbnail height on an OSD card.
const OSD_THUMB_HEIGHT: f32 = 54.0;

impl ToastStack {
    /// Queue a new toast. Newest renders at the bottom of the stack, so a
    /// rapid sequence (folder added, then the rescan it triggers
    /// finishing) reads top-to-bottom in the order it happened.
    pub fn push(&mut self, kind: ToastKind, text: impl Into<String>, ctx: &egui::Context) {
        self.push_card(kind, text, None, None, ctx);
    }

    /// Ticket W20-12: the on-screen-display stack — the same mechanism as
    /// the toasts (one fading, non-interactive, `Context::time`-driven
    /// stack), drawn over the game picture's corner by [`Self::show_at`]
    /// with a shorter life.
    #[must_use]
    pub fn osd() -> Self {
        Self {
            toasts: Vec::new(),
            duration: OSD_DURATION,
            id: "rf_osd_stack",
        }
    }

    /// Push a card with an optional picture, replacing any live card that
    /// carries the same `key`.
    pub fn push_card(
        &mut self,
        kind: ToastKind,
        text: impl Into<String>,
        thumb: Option<egui::TextureHandle>,
        key: Option<&'static str>,
        ctx: &egui::Context,
    ) {
        if let Some(k) = key {
            self.toasts.retain(|t| t.key != Some(k));
        }
        self.toasts.push(Toast {
            text: text.into(),
            kind,
            created_at: ctx.time(),
            thumb,
            key,
        });
    }

    /// Drop every expired toast without drawing (ticket W20-12): a stack
    /// that is hidden for a while — the OSD under the Quick Menu or a
    /// window — must still expire on time, not reappear stale.
    pub fn prune(&mut self, ctx: &egui::Context) {
        let now = ctx.time();
        let life = self.duration + FADE_SECS;
        self.toasts.retain(|t| now - t.created_at < life);
    }

    /// The texts currently live, oldest first (tests).
    #[doc(hidden)]
    #[must_use]
    pub fn texts(&self) -> Vec<String> {
        self.toasts.iter().map(|t| t.text.clone()).collect()
    }

    /// Whether at least one toast is currently visible (unexpired).
    /// Test-only: production code never needs to ask this, since
    /// `show` both draws and prunes in one pass.
    #[doc(hidden)]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.toasts.is_empty()
    }

    /// Draw every live toast bottom-right, and drop any whose fade has
    /// finished. Called every frame regardless of whether any toast is
    /// queued — the common case is an empty `Vec` and this is a cheap
    /// no-op then.
    ///
    /// Ticket W15-07: takes `&Tokens` rather than `&Palette` — `bg`/`ink`/
    /// `accent` are the same colours `Palette::raised`/`text`/`accent`
    /// were, now read through the token set every other W15 surface uses,
    /// so this is a signature change with no visual change.
    pub fn show(&mut self, ctx: &egui::Context, tokens: &crate::theme::Tokens) {
        self.show_placed(
            ctx,
            tokens,
            Placement::Anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -16.0)),
        );
    }

    /// Ticket W20-12: draw with the stack's top-left at `pos` — the OSD
    /// cards sit just inside the game picture's top-left corner.
    pub fn show_at(&mut self, ctx: &egui::Context, tokens: &crate::theme::Tokens, pos: egui::Pos2) {
        self.show_placed(ctx, tokens, Placement::At(pos));
    }

    fn show_placed(
        &mut self,
        ctx: &egui::Context,
        tokens: &crate::theme::Tokens,
        placement: Placement,
    ) {
        let now = ctx.time();
        let duration = self.duration;
        self.toasts
            .retain(|t| now - t.created_at < duration + FADE_SECS);
        if self.toasts.is_empty() {
            return;
        }
        // A live toast means the screen changes again shortly (the fade,
        // or the eventual expiry) even if nothing else is happening —
        // egui only repaints on demand otherwise.
        ctx.request_repaint();

        let bg = tokens.surface;
        let text_colour = tokens.ink;
        let accent = tokens.accent;

        let area = egui::Area::new(egui::Id::new(self.id));
        let area = match placement {
            Placement::Anchor(align, offset) => area.anchor(align, offset),
            Placement::At(pos) => area.fixed_pos(pos),
        };
        area.order(egui::Order::Tooltip)
            .interactable(false)
            .show(ctx, |ui| {
                ui.vertical(|ui| {
                    for toast in &self.toasts {
                        let age = now - toast.created_at;
                        // Fade over the last FADE_SECS; opaque before that.
                        // This is the ONE moving element a toast owns —
                        // nothing else about the stack animates.
                        let remaining = duration + FADE_SECS - age;
                        let alpha = (remaining / FADE_SECS).clamp(0.0, 1.0) as f32;
                        egui::Frame::popup(ui.style())
                            .fill(bg.gamma_multiply(alpha))
                            .stroke(egui::Stroke::new(1.0, accent.gamma_multiply(alpha)))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    if let Some(tex) = &toast.thumb {
                                        let [w, h] = tex.size();
                                        #[allow(clippy::cast_precision_loss)]
                                        let width = OSD_THUMB_HEIGHT * w as f32 / h.max(1) as f32;
                                        ui.add(
                                            egui::Image::from_texture(tex)
                                                .fit_to_exact_size(egui::vec2(
                                                    width,
                                                    OSD_THUMB_HEIGHT,
                                                ))
                                                .tint(egui::Color32::WHITE.gamma_multiply(alpha)),
                                        );
                                    }
                                    ui.label(
                                        egui::RichText::new(toast.kind.glyph())
                                            .color(accent.gamma_multiply(alpha)),
                                    );
                                    ui.label(
                                        egui::RichText::new(&toast.text)
                                            .color(text_colour.gamma_multiply(alpha)),
                                    );
                                });
                            });
                        ui.add_space(4.0);
                    }
                });
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Context::default()` with no viewport plumbing at all still has a
    /// clock — `ctx.time()` starts at 0.0 and only ever advances via
    /// `run` / `run_steps`'s `predicted_dt`, never on its own, so a stack
    /// that never gets `show`n never expires. This is the pure half of
    /// that guarantee — the harness half lives in
    /// `tests/toasts_and_modals.rs`.
    /// Ticket W20-12: a keyed card replaces its predecessor; unkeyed
    /// cards stack.
    #[test]
    fn keyed_cards_replace_and_plain_cards_stack() {
        let ctx = egui::Context::default();
        let mut osd = ToastStack::osd();
        for _ in 0..60 {
            osd.push_card(ToastKind::Info, "Fast-forward", None, Some("ff"), &ctx);
        }
        osd.push(ToastKind::Success, "Saved to Slot 1", &ctx);
        osd.push(ToastKind::Success, "Screenshot saved", &ctx);
        assert_eq!(
            osd.texts(),
            vec!["Fast-forward", "Saved to Slot 1", "Screenshot saved"]
        );
    }

    #[test]
    fn a_pushed_toast_survives_until_its_duration_elapses() {
        let ctx = egui::Context::default();
        let mut stack = ToastStack::default();
        stack.push(ToastKind::Success, "Saved Slot 1", &ctx);
        assert!(!stack.is_empty());

        // Drive the context's own clock forward without a full `run()` —
        // `begin_pass` is what other egui-internal tests use to advance
        // `Context::time` without a Window or a Painter.
        let advance = |seconds: f64| {
            let input = egui::RawInput {
                predicted_dt: seconds as f32,
                ..egui::RawInput::default()
            };
            ctx.begin_pass(input);
            let _ = ctx.end_pass();
        };

        advance(1.0);
        // Not yet expired: `show` draws (needs a pass in progress) and
        // prunes in the same call, so drive it inside a pass like the
        // real `impl eframe::App::ui` does.
        let tokens = crate::theme::Tokens::dark();
        ctx.begin_pass(egui::RawInput::default());
        stack.show(&ctx, &tokens);
        let _ = ctx.end_pass();
        assert!(!stack.is_empty());

        // Push it past DEFAULT_DURATION + FADE_SECS.
        advance(DEFAULT_DURATION + FADE_SECS + 1.0);
        ctx.begin_pass(egui::RawInput::default());
        stack.show(&ctx, &tokens);
        let _ = ctx.end_pass();
        assert!(
            stack.is_empty(),
            "a toast must expire once its duration elapses"
        );
    }
}
