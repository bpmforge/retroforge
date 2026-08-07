//! Ticket W4-00: the four PPU-origin `CoreEvent` sites this crate's
//! `crate::ppu` module owns directly (`FrameStart`/`FrameEnd`/
//! `VblankStart`/`Scanline`/`ScrollWrite` — five variants, four call
//! sites since `write_scroll` fires `ScrollWrite` from both its first and
//! second `$2005` write). `crate::system::tests::events` covers the three
//! bus-origin sites (`DmaStart`/`OamRewrite`/`MapperIrq`), which need a
//! real `NesBus` + mapper this file's bare `Ppu` fixtures don't have.
//!
//! Three groups, matching this ticket's own acceptance criteria:
//! - masking (criterion 2's correctness half, not the bench): `NONE`
//!   emits nothing despite every site genuinely firing; a single
//!   selectively-subscribed bit emits ONLY that variant.
//! - `one_full_frame_...`: exact event sequence/count over one deterministic
//!   frame, `EventMask::ALL`.
//! - `mode_invariant_...`: the STRONG (PPU-internal-field, not
//!   `retroforge::EmuStepper::state_hash`) non-perturbation proof this
//!   ticket's `plan.json` note requires — same technique
//!   `sprite_overlay.rs`'s own mode-invariant test uses (that file's doc
//!   explains why the hash alone would be vacuous here too).
use super::test_ppu;
use rf_core_api::{CoreEvent, CoreSink, EventMask, PixelLayer, PpuPixel};

const DOTS_PER_SCANLINE: u32 = 341;
const DOTS_PER_FRAME: u32 = 262 * DOTS_PER_SCANLINE;

#[derive(Default)]
struct RecordingSink {
    events: Vec<CoreEvent>,
}

impl CoreSink for RecordingSink {
    fn video_scanline(&mut self, _y: u16, _pixels: &[PpuPixel]) {}
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, ev: CoreEvent) {
        self.events.push(ev);
    }
}

/// Drives every PPU-origin site this file covers: two `$2005` writes
/// (`ScrollWrite` x2) before ticking, then one full frame's worth of dots
/// (`FrameEnd`+`FrameStart` at the pre-render wrap, `VblankStart` at
/// (241,1), `Scanline(0..=239)`).
fn drive_all_ppu_sites(ppu: &mut super::Ppu) {
    ppu.write_register(5, 0x11); // $2005 first write (X)
    ppu.write_register(5, 0x22); // $2005 second write (Y)
    for _ in 0..DOTS_PER_FRAME {
        ppu.tick();
    }
}

#[test]
fn event_mask_none_emits_nothing_despite_every_site_genuinely_firing() {
    let mut ppu = test_ppu();
    ppu.set_event_mask(EventMask::NONE);
    drive_all_ppu_sites(&mut ppu);

    let mut sink = RecordingSink::default();
    ppu.drain(&mut sink);

    assert!(
        sink.events.is_empty(),
        "EventMask::NONE must emit zero events even though this exact workload -- \
         verified by the ALL-mask test below -- genuinely crosses every one of \
         FrameStart/FrameEnd/VblankStart/Scanline/ScrollWrite's trigger conditions: {:?}",
        sink.events
    );
}

#[test]
fn a_single_subscribed_bit_emits_only_that_variant() {
    // Discriminates bit-level gating from an "any bit set -> emit
    // everything" mutation: subscribing to ONLY Scanline must never leak a
    // FrameStart/FrameEnd/VblankStart/ScrollWrite alongside it.
    let mut ppu = test_ppu();
    ppu.set_event_mask(EventMask::SCANLINE);
    drive_all_ppu_sites(&mut ppu);

    let mut sink = RecordingSink::default();
    ppu.drain(&mut sink);

    assert_eq!(
        sink.events.len(),
        240,
        "only the 240 visible-scanline Scanline events, nothing else"
    );
    for (i, ev) in sink.events.iter().enumerate() {
        assert_eq!(*ev, CoreEvent::Scanline(i as u16));
    }
}

#[test]
fn one_full_frame_emits_the_exact_expected_event_sequence() {
    let mut ppu = test_ppu();
    ppu.set_event_mask(EventMask::ALL);
    drive_all_ppu_sites(&mut ppu);

    let mut sink = RecordingSink::default();
    ppu.drain(&mut sink);

    // Two ScrollWrite (from the two $2005 writes in drive_all_ppu_sites),
    // queued before any tick() runs, so they lead the sequence. $2005=0x11
    // first write: coarse_x=0x11>>3=2, fine_x=0x11&7=1 -> x=2*8+1=17; t's
    // Y-related bits are still 0 at that point (only the FIRST write has
    // landed) -> y=0.
    assert_eq!(
        sink.events[0],
        CoreEvent::ScrollWrite {
            x: 17,
            y: 0,
            layer: PixelLayer::Background(0)
        },
        "first $2005 write (X=0x11): coarse_x=2,fine_x=1 -> x=17, y=0",
    );
    // $2005=0x22 second write sets coarse_y=0x22>>3=4, fine_y=0x22&7=2 ->
    // y=4*8+2=34; x is whatever the first write already set (t's X bits
    // untouched by the second write) -> x=17 still.
    assert_eq!(
        sink.events[1],
        CoreEvent::ScrollWrite {
            x: 17,
            y: 34,
            layer: PixelLayer::Background(0)
        },
        "second $2005 write (Y=0x22): coarse_y=4,fine_y=2 -> y=34; x unchanged at 17",
    );

    // Then, within the one-full-frame tick loop: FrameEnd, FrameStart (the
    // pre-render-line wrap happens almost immediately -- Ppu::new starts at
    // (261,0), and the module doc's math: this loop ticks exactly one lap),
    // then Scanline(0..=239), then VblankStart (scanline 241, dot 1).
    assert_eq!(sink.events[2], CoreEvent::FrameEnd);
    assert_eq!(sink.events[3], CoreEvent::FrameStart);
    for y in 0u16..240 {
        assert_eq!(sink.events[4 + y as usize], CoreEvent::Scanline(y));
    }
    assert_eq!(sink.events[4 + 240], CoreEvent::VblankStart);
    assert_eq!(
        sink.events.len(),
        4 + 240 + 1,
        "2 ScrollWrite + FrameEnd + FrameStart + 240 Scanline + 1 VblankStart, nothing more"
    );
}

/// Ticket W4-00 acceptance criterion 3, the STRONG check: two otherwise-
/// identical `Ppu`s (mask ALL vs NONE) driven through the identical
/// workload, then compared PPU-INTERNAL-FIELD-BY-FIELD -- not
/// `retroforge::EmuStepper::state_hash()`, which W3-05a proved excludes PPU
/// internals entirely (`sprite_overlay.rs`'s own doc makes the same
/// argument for its ticket). Includes the A12 filter trio
/// (`dot_clock`/`a12_low_since`/`pending_a12_edges`) specifically because
/// that is exactly what a `mem_read`-based (instead of register-state-based)
/// emission would perturb, per this ticket's hazard note -- `status`/
/// `line_buffer` alone would not catch a stray bus read that never changed
/// rendered pixels.
#[test]
fn event_emission_does_not_perturb_ppu_internal_state() {
    let mut ppu_off = test_ppu();
    ppu_off.set_event_mask(EventMask::NONE);
    drive_all_ppu_sites(&mut ppu_off);

    let mut ppu_on = test_ppu();
    ppu_on.set_event_mask(EventMask::ALL);
    drive_all_ppu_sites(&mut ppu_on);

    assert_eq!(
        ppu_off.status, ppu_on.status,
        "$2002 status must be identical"
    );
    assert_eq!(ppu_off.v, ppu_on.v, "loopy v must be identical");
    assert_eq!(ppu_off.t, ppu_on.t, "loopy t must be identical");
    assert_eq!(ppu_off.x, ppu_on.x, "fine X must be identical");
    assert_eq!(ppu_off.w, ppu_on.w, "write-toggle latch must be identical");
    assert_eq!(
        ppu_off.scanline, ppu_on.scanline,
        "scanline counter must be identical"
    );
    assert_eq!(ppu_off.dot, ppu_on.dot, "dot counter must be identical");
    assert_eq!(
        ppu_off.frame_is_odd, ppu_on.frame_is_odd,
        "frame parity must be identical"
    );
    assert_eq!(
        ppu_off.frame_count, ppu_on.frame_count,
        "frame_count must be identical"
    );
    assert_eq!(
        ppu_off.line_buffer, ppu_on.line_buffer,
        "the accuracy-exact rendered framebuffer must be byte-identical"
    );
    assert_eq!(
        ppu_off.dot_clock, ppu_on.dot_clock,
        "the A12 filter's own PPU-dot time base must be identical"
    );
    assert_eq!(
        ppu_off.a12_low_since, ppu_on.a12_low_since,
        "the A12 filter's low-period timestamp must be identical"
    );
    assert_eq!(
        ppu_off.pending_a12_edges, ppu_on.pending_a12_edges,
        "undrained A12 edge count must be identical -- a stray mem_read-based emission \
         would desync exactly this counter, invisible to status/line_buffer alone"
    );
}
