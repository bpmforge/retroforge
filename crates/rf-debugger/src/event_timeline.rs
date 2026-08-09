//! Event viewer data provider (FR-DBG-001, DEBUGGER.md §3 "Event viewer"
//! row: "Mesen-style frame timeline: dot/scanline scatter of register
//! writes, IRQ/NMI, DMA, sprite-0 hit").
//!
//! ## The real producer (do not invent a parallel one — ticket brief)
//!
//! W4-00 made `rf-nes` emit [`rf_core_api::CoreEvent`]s through a single
//! shared FIFO (deliberately one queue, not several, so cross-source order
//! — e.g. a `MapperIrq` and a `ScrollWrite` on the same scanline — is
//! preserved); W4-01 built [`rf_core_api::FrameBundle::events`], which
//! carries exactly that FIFO for one frame, in emission order. This module
//! consumes it as-is.
//!
//! ## Deriving a scanline for events that don't carry one
//!
//! Only [`rf_core_api::CoreEvent::Scanline`] carries a position;
//! `MapperIrq`, `ScrollWrite`, `OamRewrite`, `DmaStart`, `FrameStart`,
//! `FrameEnd`, `VblankStart`, `MemWatch` do not. [`build_timeline`] folds
//! over `events` in order, remembering the most recently seen `Scanline`
//! value and stamping every following positionless event with it — which
//! is exactly why the single-FIFO ordering above is load-bearing: an
//! implementation that reordered or grouped by event type first would
//! misplace every positionless event on the scatter.

use rf_core_api::CoreEvent;

/// One point on the frame timeline: the event, and the scanline it
/// occurred within (`None` only if the event arrived before any
/// `Scanline` event this frame — e.g. a `FrameStart` at the very top).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineEvent {
    pub scanline: Option<u16>,
    pub event: CoreEvent,
}

/// Fold `events` (one frame's worth, in emission order — module doc) into
/// a timeline, carrying the last-seen [`CoreEvent::Scanline`] forward onto
/// every event that has no position of its own. Output preserves input
/// order (never sorted/regrouped) — a scatter plot's X axis is this
/// module's `scanline`, not array position, so callers that want a
/// left-to-right timeline still need the FIFO's temporal order for events
/// that share a scanline.
#[must_use]
pub fn build_timeline(events: &[CoreEvent]) -> Vec<TimelineEvent> {
    let mut current_scanline: Option<u16> = None;
    events
        .iter()
        .map(|&event| {
            if let CoreEvent::Scanline(y) = event {
                current_scanline = Some(y);
            }
            TimelineEvent {
                scanline: current_scanline,
                event,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rf_core_api::PixelLayer;

    #[test]
    fn positionless_events_inherit_the_most_recently_seen_scanline() {
        // The exact discriminating case: an implementation that ignores
        // FIFO order (e.g. groups by event type, or sorts) would place
        // both MapperIrq and ScrollWrite at the same (wrong) scanline
        // instead of 10 and 20 respectively.
        let events = vec![
            CoreEvent::Scanline(10),
            CoreEvent::MapperIrq,
            CoreEvent::Scanline(20),
            CoreEvent::ScrollWrite {
                x: 1,
                y: 2,
                layer: PixelLayer::Background(0),
            },
        ];
        let timeline = build_timeline(&events);
        assert_eq!(timeline.len(), 4);
        assert_eq!(timeline[1].scanline, Some(10), "MapperIrq must land at 10");
        assert_eq!(
            timeline[1].event,
            CoreEvent::MapperIrq,
            "order must be preserved, not regrouped"
        );
        assert_eq!(
            timeline[3].scanline,
            Some(20),
            "ScrollWrite must land at 20, not 10 or 0"
        );
    }

    #[test]
    fn an_event_before_any_scanline_this_frame_carries_no_position() {
        let events = vec![CoreEvent::FrameStart, CoreEvent::Scanline(0)];
        let timeline = build_timeline(&events);
        assert_eq!(timeline[0].scanline, None);
        assert_eq!(timeline[1].scanline, Some(0));
    }

    #[test]
    fn multiple_positionless_events_on_the_same_scanline_all_share_it() {
        let events = vec![
            CoreEvent::Scanline(5),
            CoreEvent::DmaStart { chan: 0 },
            CoreEvent::OamRewrite,
            CoreEvent::MapperIrq,
        ];
        let timeline = build_timeline(&events);
        assert!(timeline[1..].iter().all(|p| p.scanline == Some(5)));
    }

    #[test]
    fn empty_events_produce_an_empty_timeline() {
        assert!(build_timeline(&[]).is_empty());
    }

    #[test]
    fn a_second_frames_scanline_does_not_leak_from_a_prior_call() {
        // Each call is independent (module doc: "one frame's worth"). A
        // fresh `build_timeline` call starting mid-scanline-numbering must
        // not remember a previous call's state.
        let _ = build_timeline(&[CoreEvent::Scanline(200)]);
        let second = build_timeline(&[CoreEvent::MapperIrq]);
        assert_eq!(second[0].scanline, None);
    }
}
