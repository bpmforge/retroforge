//! Core events and subscription filtering (FR-CORE-006).
use crate::video::{Mode7Registers, PixelLayer};

/// Events a core pushes through [`crate::CoreSink::event`] during
/// `run_frame`/`step` (ARCHITECTURE §5).
///
/// Construction is gated by [`EventMask`]: callers (concrete
/// `EmulatorCore` implementations) MUST check
/// `mask.is_subscribed(EventMask::SOME_BIT)` before building the matching
/// `CoreEvent` variant, so an unsubscribed sink pays no per-event
/// allocation/construction cost (FR-CORE-006). This enum intentionally has
/// no default/empty variant to keep that check meaningful — there is
/// nothing cheaper to construct than "don't construct at all".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreEvent {
    /// Emitted once at the start of `run_frame`, before the first scanline.
    FrameStart,
    /// Emitted once after the last scanline of `run_frame`.
    FrameEnd,
    /// PPU entered vertical blank.
    VblankStart,
    /// A scanline was produced; carries the scanline index (same value as
    /// the `y` passed to [`crate::CoreSink::video_scanline`]).
    Scanline(u16),
    /// OAM (sprite table) was rewritten outside the normal per-scanline
    /// path (e.g. mid-frame DMA), which can desync sprite-based tricks the
    /// enhancement side tracks.
    OamRewrite,
    /// A scroll register was written.
    ScrollWrite {
        /// New horizontal scroll value.
        x: u16,
        /// New vertical scroll value.
        y: u16,
        /// Layer the scroll write applies to.
        layer: PixelLayer,
    },
    /// A mapper-generated IRQ fired.
    MapperIrq,
    /// A DMA transfer started on the given channel.
    DmaStart {
        /// Core-defined DMA channel index.
        chan: u8,
    },
    /// A debugger memory watchpoint fired.
    MemWatch {
        /// Core-defined watchpoint id (assigned when the watch was set).
        id: u32,
    },
    /// The Mode 7 affine transform for this frame (ticket W16-09;
    /// `docs/design/ENHANCEMENT_WAVE_16.md` §9) — promoted from the SNES
    /// debug snapshot (`rf_snes::debug`) to this generic, cross-console
    /// path so `FrameBundle::events` (`rf-core-api::frame_bundle`) is the
    /// one place a renderer looks for it, rather than reaching past
    /// `CoreSink` into a console-specific debug API.
    ///
    /// Emitted **at most once per frame**, after the frame's video has
    /// been assembled (`rf_snes::core::SnesCore::emit_frame`), and only
    /// when BG mode 7 is active — a core in any other mode never
    /// constructs one, matching every other variant's pay-for-use rule.
    /// `rf-snes`'s settled-frame composition model (`rf_snes::core`'s own
    /// module doc: "runs the frame, then replays the settled picture")
    /// reads these registers post-hoc rather than live per scanline, so
    /// today's value is the registers' state at frame end for the whole
    /// frame — a real per-scanline HDMA ramp is not yet distinguishable
    /// from a static matrix at this layer; see that core's own doc for
    /// why, and `rf_snes::debug::mode7_registers`'s doc for the exact
    /// promotion.
    Mode7(Mode7Registers),
}

impl CoreEvent {
    /// The single [`EventMask`] bit this variant corresponds to
    /// ([`EventMask`]'s own doc: "Bits correspond 1:1 to `CoreEvent`
    /// variants"). Used by subscription filtering (`rf_enhance::bus`,
    /// ticket W4-01) to answer "does subscriber X want this event" without
    /// duplicating this match arm at every call site — and, being an
    /// exhaustive match, fails to compile (rather than silently missing a
    /// case) if a future variant is ever added without updating it here.
    #[must_use]
    pub const fn mask_bit(&self) -> EventMask {
        match self {
            CoreEvent::FrameStart => EventMask::FRAME_START,
            CoreEvent::FrameEnd => EventMask::FRAME_END,
            CoreEvent::VblankStart => EventMask::VBLANK_START,
            CoreEvent::Scanline(_) => EventMask::SCANLINE,
            CoreEvent::OamRewrite => EventMask::OAM_REWRITE,
            CoreEvent::ScrollWrite { .. } => EventMask::SCROLL_WRITE,
            CoreEvent::MapperIrq => EventMask::MAPPER_IRQ,
            CoreEvent::DmaStart { .. } => EventMask::DMA_START,
            CoreEvent::MemWatch { .. } => EventMask::MEM_WATCH,
            CoreEvent::Mode7(_) => EventMask::MODE7,
        }
    }
}

/// Cheap, allocation-free bitmask of which [`CoreEvent`] variants a sink is
/// subscribed to (FR-CORE-006). Bits correspond 1:1 to `CoreEvent`
/// variants.
///
/// A core checks `mask.is_subscribed(EventMask::X)` and only constructs
/// `CoreEvent::X` when it returns `true`. In Accuracy mode with no
/// subscribers ([`EventMask::NONE`]) every check is a single `u32` AND
/// against a `const`, and no `CoreEvent` value is ever built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EventMask(u32);

impl EventMask {
    /// Bit for [`CoreEvent::FrameStart`].
    pub const FRAME_START: EventMask = EventMask(1 << 0);
    /// Bit for [`CoreEvent::FrameEnd`].
    pub const FRAME_END: EventMask = EventMask(1 << 1);
    /// Bit for [`CoreEvent::VblankStart`].
    pub const VBLANK_START: EventMask = EventMask(1 << 2);
    /// Bit for [`CoreEvent::Scanline`].
    pub const SCANLINE: EventMask = EventMask(1 << 3);
    /// Bit for [`CoreEvent::OamRewrite`].
    pub const OAM_REWRITE: EventMask = EventMask(1 << 4);
    /// Bit for [`CoreEvent::ScrollWrite`].
    pub const SCROLL_WRITE: EventMask = EventMask(1 << 5);
    /// Bit for [`CoreEvent::MapperIrq`].
    pub const MAPPER_IRQ: EventMask = EventMask(1 << 6);
    /// Bit for [`CoreEvent::DmaStart`].
    pub const DMA_START: EventMask = EventMask(1 << 7);
    /// Bit for [`CoreEvent::MemWatch`].
    pub const MEM_WATCH: EventMask = EventMask(1 << 8);
    /// Bit for [`CoreEvent::Mode7`].
    pub const MODE7: EventMask = EventMask(1 << 9);

    /// No events subscribed (Accuracy mode default: zero per-event cost).
    pub const NONE: EventMask = EventMask(0);
    /// Every event subscribed.
    pub const ALL: EventMask = EventMask(
        Self::FRAME_START.0
            | Self::FRAME_END.0
            | Self::VBLANK_START.0
            | Self::SCANLINE.0
            | Self::OAM_REWRITE.0
            | Self::SCROLL_WRITE.0
            | Self::MAPPER_IRQ.0
            | Self::DMA_START.0
            | Self::MEM_WATCH.0
            | Self::MODE7.0,
    );

    /// The empty mask. Same as [`EventMask::NONE`]; provided for
    /// bitflags-style naming at call sites that build a mask up from
    /// nothing.
    #[must_use]
    pub const fn empty() -> Self {
        EventMask(0)
    }

    /// Combine two masks (set union).
    #[must_use]
    pub const fn union(self, other: EventMask) -> Self {
        EventMask(self.0 | other.0)
    }

    /// Add `other`'s bits to `self`.
    pub const fn insert(&mut self, other: EventMask) {
        self.0 |= other.0;
    }

    /// Remove `other`'s bits from `self`.
    pub const fn remove(&mut self, other: EventMask) {
        self.0 &= !other.0;
    }

    /// `true` if every bit set in `other` is also set in `self`.
    #[must_use]
    pub const fn contains(self, other: EventMask) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Semantic alias for [`EventMask::contains`] at the single-bit
    /// call sites cores use to gate `CoreEvent` construction
    /// (`if mask.is_subscribed(EventMask::MAPPER_IRQ) { ... }`).
    #[must_use]
    pub const fn is_subscribed(self, event_bit: EventMask) -> bool {
        self.contains(event_bit)
    }
}
