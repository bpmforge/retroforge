//! The enhancement side's event bus: subscribe to a triple-buffered
//! `FrameBundle` stream and filter to just the `CoreEvent`s a consumer
//! declared interest in (ticket W4-01; `docs/design/ENHANCEMENT_RUNTIME.md`
//! §1's diagram — `FrameBundle -> EnhancementRuntime::on_frame`, fanning
//! out to `SpriteHistorian`/`ScrollTracker`/etc, none of which exist yet).
//!
//! ## Why no `SpriteHistorian`/`ScrollTracker`/`EnhancementRuntime` here
//!
//! Same scope fence ticket W3-03 drew around `SceneGraph`: those consumer
//! types are each their own ticket (W3-05, W4-03a, ...), and building a
//! placeholder now — with no real behavior to validate its shape against —
//! would be the "unvalidated scaffolding" that ticket declined. What's
//! genuinely this ticket's job is the bus/subscription *mechanism* below,
//! generic enough that those tickets plug in by constructing their own
//! [`FrameSubscriber`] with their own [`EventMask`], no redesign needed.
use std::sync::Arc;

use rf_core_api::{CoreEvent, EventMask, FrameBundle, TripleBufferReader};

/// One consumer's view onto the shared `FrameBundle` stream: a cloned
/// [`TripleBufferReader`] (cheap — see that type's doc) plus the
/// [`EventMask`] this consumer declared interest in.
pub struct FrameSubscriber {
    reader: TripleBufferReader<FrameBundle>,
    mask: EventMask,
}

impl FrameSubscriber {
    #[must_use]
    pub fn new(reader: TripleBufferReader<FrameBundle>, mask: EventMask) -> Self {
        FrameSubscriber { reader, mask }
    }

    /// The declared subscription mask.
    #[must_use]
    pub fn mask(&self) -> EventMask {
        self.mask
    }

    /// The latest published bundle, unfiltered — for a consumer that needs
    /// `video`/`frame_count`, not just events (e.g. a future scene
    /// composer).
    #[must_use]
    pub fn latest(&self) -> Arc<FrameBundle> {
        self.reader.latest()
    }

    /// Just the events from the latest bundle this subscriber declared
    /// interest in, in emission order.
    ///
    /// Costs nothing beyond iterating whatever the core already emitted
    /// ([`FrameBundle::events`]'s own doc): when the core-side `EventMask`
    /// was `NONE`, that `Vec` is empty and this returns immediately.
    /// W4-00's own bench (`crates/rf-nes/benches/event_emission.rs`,
    /// `[-0.97%, -0.15%, +0.67%] p=0.73`, "no change detected") already
    /// covers the cost of getting to that empty `Vec` in the first place —
    /// no separate bus-side bench is needed for the "nothing subscribed"
    /// case (ticket W4-01's own notes make this call explicitly).
    #[must_use]
    pub fn subscribed_events(&self) -> Vec<CoreEvent> {
        let bundle = self.reader.latest();
        bundle
            .events
            .iter()
            .filter(|ev| self.mask.is_subscribed(ev.mask_bit()))
            .copied()
            .collect()
    }

    /// A fresh subscriber sharing the same underlying stream but its own
    /// (possibly different) [`EventMask`] — how a second consumer plugs in
    /// without redesign (module doc).
    #[must_use]
    pub fn resubscribe(&self, mask: EventMask) -> Self {
        FrameSubscriber {
            reader: self.reader.clone(),
            mask,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rf_core_api::{triple_buffer, PixelLayer};

    fn events_bundle(events: Vec<CoreEvent>) -> FrameBundle {
        FrameBundle::new(1, 0, 0, Vec::new(), events)
    }

    #[test]
    fn subscriber_only_sees_events_matching_its_mask() {
        let (mut writer, reader) = triple_buffer(FrameBundle::empty());
        writer.publish(events_bundle(vec![
            CoreEvent::MapperIrq,
            CoreEvent::ScrollWrite {
                x: 1,
                y: 2,
                layer: PixelLayer::Background(0),
            },
            CoreEvent::OamRewrite,
        ]));

        let scroll_only = FrameSubscriber::new(reader.clone(), EventMask::SCROLL_WRITE);
        assert_eq!(
            scroll_only.subscribed_events(),
            vec![CoreEvent::ScrollWrite {
                x: 1,
                y: 2,
                layer: PixelLayer::Background(0),
            }]
        );

        let irq_and_oam =
            FrameSubscriber::new(reader, EventMask::MAPPER_IRQ.union(EventMask::OAM_REWRITE));
        assert_eq!(
            irq_and_oam.subscribed_events(),
            vec![CoreEvent::MapperIrq, CoreEvent::OamRewrite]
        );
    }

    #[test]
    fn subscriber_with_none_mask_sees_no_events_even_when_the_bundle_has_some() {
        let (mut writer, reader) = triple_buffer(FrameBundle::empty());
        writer.publish(events_bundle(vec![
            CoreEvent::FrameStart,
            CoreEvent::FrameEnd,
        ]));
        let sub = FrameSubscriber::new(reader, EventMask::NONE);
        assert!(sub.subscribed_events().is_empty());
    }

    #[test]
    fn empty_events_bundle_yields_no_events_regardless_of_mask() {
        let (_writer, reader) = triple_buffer(FrameBundle::empty());
        let sub = FrameSubscriber::new(reader, EventMask::ALL);
        assert!(
            sub.subscribed_events().is_empty(),
            "a core-side EventMask::NONE run leaves the bundle's events Vec empty; \
             ALL downstream masks must see nothing to filter"
        );
    }

    #[test]
    fn resubscribe_shares_the_stream_but_can_change_the_mask() {
        let (mut writer, reader) = triple_buffer(FrameBundle::empty());
        let sub_a = FrameSubscriber::new(reader, EventMask::MAPPER_IRQ);
        let sub_b = sub_a.resubscribe(EventMask::OAM_REWRITE);

        writer.publish(events_bundle(vec![
            CoreEvent::MapperIrq,
            CoreEvent::OamRewrite,
        ]));

        assert_eq!(sub_a.subscribed_events(), vec![CoreEvent::MapperIrq]);
        assert_eq!(sub_b.subscribed_events(), vec![CoreEvent::OamRewrite]);
    }

    #[test]
    fn latest_exposes_the_unfiltered_bundle() {
        let one_pixel = rf_core_api::PpuPixel {
            palette_index: 0,
            layer: PixelLayer::Backdrop,
            sprite_id: None,
            priority: 0,
            dropped_by_limit: false,
        };
        let (mut writer, reader) = triple_buffer(FrameBundle::empty());
        writer.publish(FrameBundle::new(5, 1, 1, vec![one_pixel], Vec::new()));
        let sub = FrameSubscriber::new(reader, EventMask::NONE);
        assert_eq!(sub.latest().frame_count, 5);
    }
}
