//! Cross-thread frame handoff type (ticket W4-01; ARCHITECTURE §6's sketch
//! `FrameBundle { video, audio, events, state_snapshot_refs }`; RENDERER §1
//! "Consumes `FrameBundle` video (indexed pixels + metadata)"; FRONTEND_UI
//! §1 "Panels read from FrameBundle / debugger ring buffers").
//!
//! ## Placement ruling (ticket W4-01, conductor, 2026-08-07)
//!
//! `FrameBundle` lives here rather than in `rf-enhance` because both
//! `rf-renderer` (RENDERER §1) and `retroforge`'s panels (FRONTEND_UI §1)
//! need the type, and neither may depend on `rf-enhance` (ARCHITECTURE §3's
//! layering — Host and Enhance are siblings under the app shell, no edge
//! between them). `rf-core-api` is the shared contract crate everything
//! already depends on.
//!
//! ## What's here, and what's deliberately not (resisting speculative
//! fields, `docs/design/CONTRACTS.md`'s versioned-commitment warning)
//!
//! [`FrameBundle::video`] (indexed pixels + dimensions — RENDERER §1/§2's
//! "indexed pixels + metadata") and [`FrameBundle::events`] (what W4-01's
//! event bus/subscriptions, `rf_enhance::bus`, filters per consumer) are
//! both directly named by the docs above and directly consumed by this
//! ticket's own deliverables. [`FrameBundle::frame_count`] is a cheap
//! monotonic identifier the mode-invariant harness
//! (`retroforge::mode_invariant`) and any future debugger use to align
//! frames across runs/threads.
//!
//! `audio` and `state_snapshot_refs` — both named in ARCHITECTURE §6's
//! sketch tuple — are deliberately **not** here yet:
//! - `audio`: `rf-audio` is an empty crate stub with no consumer of
//!   per-frame samples wired up anywhere in the workspace today. A field
//!   nothing reads would be exactly the speculative addition the
//!   placement ruling warns against. `CoreSink::audio` samples are already
//!   reachable (any `CoreSink` implementor can collect them) the moment a
//!   real consumer needs them — adding the field then is additive, not a
//!   breaking change.
//! - `state_snapshot_refs`: would need `rf_core_api::StateView`, which is
//!   borrowed (`<'_>`, "valid only between frames" per its own doc) and
//!   cannot be stored in an owned, `'static`, triple-buffered value
//!   without `rf-nes` first implementing `EmulatorCore::state_view()` —
//!   still unimplemented (`retroforge::stepper`'s own module doc: "NES
//!   state serialization is ticket W2-04's job"). Out of this ticket's
//!   write scope to add (`rf-nes` is explicitly not touched — see the
//!   ticket's own escalation clause).
use crate::core::CoreSink;
use crate::event::CoreEvent;
use crate::video::{LineRegisters, PixelLayer, PpuPixel, SubLine, SubPixel};

/// One frame's worth of assembled [`CoreSink`] output, ready to cross a
/// thread boundary via [`crate::triple_buffer`] (ARCHITECTURE §6).
/// Assembled by the app/enhancement side from `CoreSink` calls — see
/// [`FrameBundleBuilder`] — never emitted by a core directly (a core only
/// ever sees `&mut dyn CoreSink`, never constructs one of these itself).
// Ticket W16-14: `Eq` dropped here — `CoreEvent::Mode7` now carries
// `Mode7Frame`, which is only `PartialEq` (its `lines: Option<Vec<..>>`
// field derives that way too), so `CoreEvent` itself is `PartialEq`-only
// and `FrameBundle` cannot derive `Eq` through it. Nothing in this crate
// relied on `FrameBundle: Eq` — every existing comparison already used
// `PartialEq`/`assert_eq!`.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameBundle {
    /// Frames completed since power-on (same counter `EmulatorCore`
    /// implementations track internally), for cross-run/cross-thread frame
    /// alignment.
    pub frame_count: u64,
    /// Video width in pixels (NES: 256; SNES: up to 512 — RENDERER §2).
    pub width: u16,
    /// Video height in pixels (NES: 240; SNES: up to 478 — RENDERER §2).
    pub height: u16,
    /// Row-major indexed pixels, `width * height` long — RENDERER §1's
    /// "indexed pixels + metadata", never RGB (project law, [`crate::video`]
    /// module doc). Always the accuracy-exact stream: assembled from
    /// [`CoreSink::video_scanline`] only, never
    /// [`CoreSink::overlay_scanline`] — see [`FrameBundleBuilder`]'s
    /// `CoreSink` impl for why.
    pub video: Vec<PpuPixel>,
    /// Every subscribed [`CoreEvent`] emitted during this frame, in
    /// emission order (preserves the cross-source ordering ticket W4-00
    /// deliberately kept in one shared FIFO). Empty whenever the core's
    /// own [`crate::EventMask`] had nothing subscribed
    /// ([`crate::CoreConfig`]'s default `EventMask::NONE`) — which is also
    /// why bus-side subscription filtering (`rf_enhance::bus`) costs
    /// nothing extra in that case: there is nothing in this `Vec` to
    /// filter.
    pub events: Vec<CoreEvent>,
    /// Ticket W33-01: per visible line, the sub-screen exactly as
    /// [`CoreSink::sub_scanline`] delivered it (the console's own dots
    /// only). `height` entries, `None` on a line with no colour math, or
    /// EMPTY when the builder was not asked to capture
    /// ([`FrameBundleBuilder::set_capture_wide`]): set for widescreen, the
    /// tile grids, and the compare/peek "original" (which blends with it).
    /// Not part of the accuracy-exact stream:
    /// [`Self::video`] (and any hash over it) is unaffected.
    pub sub: Vec<Option<SubLine>>,
    /// Ticket W33-01: per visible line, the registers it was drawn with
    /// ([`LineRegisters`]). Same shape and gating as [`Self::sub`].
    pub lines: Vec<Option<LineRegisters>>,
}

impl FrameBundle {
    /// # Panics
    /// Panics if `video.len() != width * height` — a malformed bundle here
    /// is an assembly bug, the same "caller bug, not a runtime condition to
    /// degrade through" stance `rf_renderer::IndexedFrame::new` takes for
    /// the same shape of invariant.
    #[must_use]
    pub fn new(
        frame_count: u64,
        width: u16,
        height: u16,
        video: Vec<PpuPixel>,
        events: Vec<CoreEvent>,
    ) -> Self {
        assert_eq!(
            video.len(),
            width as usize * height as usize,
            "FrameBundle: {} pixels for a {width}x{height} frame (need {})",
            video.len(),
            width as usize * height as usize
        );
        FrameBundle {
            frame_count,
            width,
            height,
            video,
            events,
            sub: Vec::new(),
            lines: Vec::new(),
        }
    }

    /// A zero-sized placeholder — what a fresh [`crate::triple_buffer`]
    /// pair's 3 slots hold before the first real frame is published, so a
    /// reader that polls before the core thread's first frame gets a
    /// well-defined empty bundle rather than blocking or panicking.
    #[must_use]
    pub fn empty() -> Self {
        FrameBundle {
            frame_count: 0,
            width: 0,
            height: 0,
            video: Vec::new(),
            events: Vec::new(),
            sub: Vec::new(),
            lines: Vec::new(),
        }
    }
}

impl Default for FrameBundle {
    fn default() -> Self {
        Self::empty()
    }
}

const BLANK_PIXEL: PpuPixel = PpuPixel {
    palette_index: 0,
    layer: PixelLayer::Backdrop,
    sprite_id: None,
    priority: 0,
};

/// Accumulates one frame's worth of [`CoreSink`] output into a
/// [`FrameBundle`] — the concrete answer to "the `FrameBundle` is
/// assembled from `CoreSink` output by the app/enhancement side, not
/// emitted by the core" (ticket W4-01). A minimal `CoreSink` implementor:
/// unlike a fan-out sink that forwards to several downstream consumers,
/// this only accumulates, mirroring how `rf_renderer::FrameBuffer`/
/// `LayeredFrame` each independently accumulate their own view of the same
/// scanline stream.
pub struct FrameBundleBuilder {
    width: u16,
    height: u16,
    pixels: Vec<PpuPixel>,
    events: Vec<CoreEvent>,
    /// Ticket W33-01: keep the sub-screen and per-line registers (the
    /// widescreen stitcher's inputs). Off by default.
    capture_wide: bool,
    sub: Vec<Option<SubLine>>,
    lines: Vec<Option<LineRegisters>>,
}

impl FrameBundleBuilder {
    /// The frame width in dots (the console's own picture).
    #[must_use]
    pub fn width(&self) -> u16 {
        self.width
    }

    #[must_use]
    pub fn new(width: u16, height: u16) -> Self {
        FrameBundleBuilder {
            width,
            height,
            pixels: vec![BLANK_PIXEL; width as usize * height as usize],
            events: Vec::new(),
            capture_wide: false,
            sub: Vec::new(),
            lines: Vec::new(),
        }
    }

    /// Ticket W33-01: start or stop keeping the sub-screen and per-line
    /// registers in the bundles [`Self::take`] returns (what the widescreen
    /// stitcher needs; the app turns it on only while widescreen is on).
    /// Off, [`FrameBundle::sub`]/[`FrameBundle::lines`] are empty and the
    /// core is told this sink does not want line registers.
    pub fn set_capture_wide(&mut self, on: bool) {
        if on == self.capture_wide {
            return;
        }
        self.capture_wide = on;
        self.sub = if on { self.blank_rows() } else { Vec::new() };
        self.lines = if on { self.blank_rows() } else { Vec::new() };
    }

    /// Whether the sub-screen and per-line registers are being kept.
    #[must_use]
    pub fn capture_wide(&self) -> bool {
        self.capture_wide
    }

    fn blank_rows<T: Clone>(&self) -> Vec<Option<T>> {
        vec![None; usize::from(self.height)]
    }

    /// Snapshot the accumulated frame into an owned [`FrameBundle`] and
    /// clear the per-frame event log. The pixel buffer is **not** reset —
    /// it persists between calls the same way `rf_renderer::FrameBuffer`
    /// does, so a partial update (e.g. debugger scanline-stepping) layers
    /// over the previous frame's content rather than flashing to blank; a
    /// steady-state running frame overwrites every row regardless.
    #[must_use]
    pub fn take(&mut self, frame_count: u64) -> FrameBundle {
        let mut bundle = FrameBundle::new(
            frame_count,
            self.width,
            self.height,
            self.pixels.clone(),
            std::mem::take(&mut self.events),
        );
        if self.capture_wide {
            // Moved, not cloned, and reset: unlike the pixels, a line whose
            // colour math switched off must not keep last frame's sub-screen.
            let (fresh_sub, fresh_lines) = (self.blank_rows(), self.blank_rows());
            bundle.sub = std::mem::replace(&mut self.sub, fresh_sub);
            bundle.lines = std::mem::replace(&mut self.lines, fresh_lines);
        }
        bundle
    }
}

impl CoreSink for FrameBundleBuilder {
    fn video_scanline(&mut self, y: u16, pixels: &[PpuPixel]) {
        let row = y as usize;
        if row >= self.height as usize {
            return; // degrade, never crash -- same stance as FrameBuffer/LayeredFrame.
        }
        let row_start = row * self.width as usize;
        let n = pixels.len().min(self.width as usize);
        self.pixels[row_start..row_start + n].copy_from_slice(&pixels[..n]);
    }

    /// Deliberately a no-op, **not inherited silently** — spelled out here
    /// so a `overlay_scanline` grep looking for the "wrapper silently
    /// discards the overlay" bug class (`retroforge::stepper::CountingSink`'s
    /// own doc warns about exactly this) finds a documented, intentional
    /// choice rather than an oversight: [`FrameBundle::video`] must stay
    /// accuracy-exact regardless of any enhancement overlay, matching
    /// [`crate::video::PpuPixel::dropped_by_limit`]'s doc ("The sink is
    /// therefore accuracy-exact"). This is also what
    /// `retroforge::mode_invariant`'s strong check relies on: hashing
    /// [`FrameBundle::video`] captures the accuracy-exact stream whether or
    /// not an enhancement's overlay is active.
    fn overlay_scanline(&mut self, _y: u16, _pixels: &[crate::video::OverlayPixel]) {}

    fn audio(&mut self, _samples: &[i16]) {
        // No `audio` field on FrameBundle yet -- see module doc.
    }

    fn wants_line_registers(&self) -> bool {
        self.capture_wide
    }

    fn line_registers(&mut self, y: u16, regs: &LineRegisters) {
        if let Some(slot) = self.lines.get_mut(usize::from(y)) {
            *slot = Some(*regs);
        }
    }

    /// Ticket W33-01: keep the line's sub-screen when capturing. A widened
    /// line's sub-screen is `width + 2 * pad` dots long, the same shape as
    /// the main line ([`Self::video_scanline_widened`] takes its middle), so
    /// the console's own dots are the middle `width` here too.
    fn sub_scanline(&mut self, y: u16, pixels: &[SubPixel], fixed_color: u16) {
        if !self.capture_wide {
            return;
        }
        let width = usize::from(self.width);
        let pad = pixels.len().saturating_sub(width) / 2;
        let middle = pixels.get(pad..pad + width).unwrap_or(pixels);
        if let Some(slot) = self.sub.get_mut(usize::from(y)) {
            *slot = Some(SubLine {
                pixels: middle.to_vec(),
                fixed_color,
            });
        }
    }

    fn event(&mut self, ev: CoreEvent) {
        self.events.push(ev);
    }
}
