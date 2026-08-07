//! Ticket W4-00: the three bus-origin `CoreEvent` sites `crate::system::
//! NesBus` owns (`DmaStart`/`OamRewrite`/`MapperIrq`) —
//! `crate::ppu::tests::event_emission` covers the five PPU-origin ones with
//! bare `Ppu` fixtures; these three need a real `NesBus` (OAM DMA) and a
//! real MMC3 mapper (the IRQ counter), so this file drives a real `NesBus`
//! instead, reusing `mmc3_irq.rs`'s own synthetic-MMC3-cart fixture.
use super::bus_with_pattern_rom;
use super::mmc3_irq::mmc3_bus;
use crate::cpu::CpuBus;
use crate::system::NesBus;
use rf_core_api::{CoreEvent, CoreSink, EventMask, PpuPixel};

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

/// `$C000`/`$C001`/`$E001` latch=5 arm, then enable rendering — mirrors
/// `mmc3_irq.rs`'s own `LATCH = 5` (chosen there so an off-by-one changes
/// the expected scanline) and its "arm BEFORE enabling rendering" ordering
/// (that module's doc). Reused here only to guarantee at least one genuine
/// A12-driven IRQ fire within one frame's worth of ticks — this file never
/// re-derives `mmc3_irq.rs`'s exact-scanline table, only borrows its
/// already-proven fixture.
const LATCH: u8 = 5;

fn arm_mmc3_irq_and_enable_rendering(bus: &mut NesBus) {
    bus.write(0xC000, LATCH); // latch
    bus.write(0xC001, 0); // request reload on the next edge
    bus.write(0xE001, 0); // enable IRQs
    bus.write(0x2000, 0x08); // sprites at $1xxx, BG at $0xxx
    bus.write(0x2001, 0x18); // show BG + show sprites -> rendering_enabled()
}

/// Harmless open-bus reads (`$4020`, dropped — `crate::system` module
/// doc's memory map), the same technique `mmc3_irq.rs`'s own
/// `advance_to_next_scanline_and_read_irq` uses to advance time without
/// touching any register that would perturb this test's own assertions.
fn advance_cycles(bus: &mut NesBus, cycles: u32) {
    for _ in 0..cycles {
        bus.read(0x4020);
    }
}

/// One CPU/bus cycle ticks the PPU exactly 3 dots (`NesBus::tick_master`'s
/// doc) — `3 * 30_000 = 90_000` dots, comfortably more than one full
/// 89,342-dot frame (`262 * 341`), enough margin to guarantee at least one
/// full lap (and, with `arm_mmc3_irq_and_enable_rendering`'s `LATCH = 5`,
/// at least one MMC3 IRQ fire — `mmc3_irq.rs`'s own analytic table predicts
/// scanline 4) regardless of exactly which dot rendering was enabled on.
const MARGIN_CYCLES: u32 = 30_000;

#[test]
fn dma_start_and_oam_rewrite_fire_in_order_when_subscribed() {
    let mut bus = bus_with_pattern_rom(1, 1);
    bus.set_event_mask(EventMask::DMA_START.union(EventMask::OAM_REWRITE));

    bus.write(0x4014, 0x02); // trigger OAM DMA from page $02
    assert!(
        bus.last_oam_dma_stall().is_some(),
        "sanity: the DMA must have genuinely run"
    );

    let mut sink = RecordingSink::default();
    bus.drain_video(&mut sink);

    assert_eq!(
        sink.events,
        vec![CoreEvent::DmaStart { chan: 0 }, CoreEvent::OamRewrite],
        "DmaStart must precede OamRewrite -- it fires before the 256-byte copy runs, \
         OamRewrite only once the whole table has actually changed"
    );
}

#[test]
fn dma_events_are_silent_under_event_mask_none_despite_a_real_dma_running() {
    let mut bus = bus_with_pattern_rom(1, 1);
    bus.set_event_mask(EventMask::NONE);

    bus.write(0x4014, 0x02);
    assert!(
        bus.last_oam_dma_stall().is_some(),
        "sanity: the DMA must have genuinely run despite EventMask::NONE"
    );

    let mut sink = RecordingSink::default();
    bus.drain_video(&mut sink);
    assert!(
        sink.events.is_empty(),
        "EventMask::NONE must silence both DmaStart and OamRewrite: {:?}",
        sink.events
    );
}

#[test]
fn mapper_irq_fires_once_on_the_rising_edge_and_never_again_while_still_pending() {
    let mut bus = mmc3_bus();
    bus.set_event_mask(EventMask::MAPPER_IRQ);
    arm_mmc3_irq_and_enable_rendering(&mut bus);

    advance_cycles(&mut bus, MARGIN_CYCLES);
    assert!(
        bus.irq_line(),
        "sanity: the IRQ must have genuinely fired by now (mmc3_irq.rs's own analytic \
         table: latch=5 fires on scanline 4, long before this margin runs out)"
    );

    let mut sink = RecordingSink::default();
    bus.drain_video(&mut sink);
    assert_eq!(
        sink.events,
        vec![CoreEvent::MapperIrq],
        "exactly ONE MapperIrq for the one genuine rising edge -- it must not re-fire on \
         every subsequent A12 edge while irq_pending() stays latched true (never acked here)"
    );
}

#[test]
fn mapper_irq_is_silent_under_event_mask_none_despite_a_real_irq_firing() {
    let mut bus = mmc3_bus();
    bus.set_event_mask(EventMask::NONE);
    arm_mmc3_irq_and_enable_rendering(&mut bus);
    advance_cycles(&mut bus, MARGIN_CYCLES);
    assert!(bus.irq_line(), "sanity: the IRQ must have genuinely fired");

    let mut sink = RecordingSink::default();
    bus.drain_video(&mut sink);
    assert!(
        sink.events.is_empty(),
        "EventMask::NONE must silence MapperIrq: {:?}",
        sink.events
    );
}

/// Ticket W4-00 acceptance criterion 3's coverage requirement (conductor
/// guidance): one workload that genuinely crosses all EIGHT `CoreEvent`
/// trigger conditions in a single run -- the five PPU-origin ones via
/// ordinary ticking/`$2005` writes, the three bus-origin ones via `$4014`/
/// the MMC3 IRQ -- so the mutation-test protocol (this ticket's own
/// instructions: inject a perturbing read, confirm the criterion-3 test
/// fails) can't be defeated by a workload the mutated site was never
/// reached by.
#[test]
fn one_workload_exercises_all_eight_core_event_variants() {
    let mut bus = mmc3_bus();
    bus.set_event_mask(EventMask::ALL);
    bus.write(0x2005, 0x11); // ScrollWrite x2
    bus.write(0x2005, 0x22);
    bus.write(0x4014, 0x02); // DmaStart, OamRewrite
    arm_mmc3_irq_and_enable_rendering(&mut bus); // (eventually) MapperIrq
    advance_cycles(&mut bus, MARGIN_CYCLES); // FrameStart/FrameEnd/VblankStart/Scanline

    let mut sink = RecordingSink::default();
    bus.drain_video(&mut sink);

    type Check = (&'static str, fn(&CoreEvent) -> bool);
    let checks: [Check; 8] = [
        ("FrameStart", |e| matches!(e, CoreEvent::FrameStart)),
        ("FrameEnd", |e| matches!(e, CoreEvent::FrameEnd)),
        ("VblankStart", |e| matches!(e, CoreEvent::VblankStart)),
        ("Scanline", |e| matches!(e, CoreEvent::Scanline(_))),
        ("ScrollWrite", |e| {
            matches!(e, CoreEvent::ScrollWrite { .. })
        }),
        ("OamRewrite", |e| matches!(e, CoreEvent::OamRewrite)),
        ("DmaStart", |e| matches!(e, CoreEvent::DmaStart { .. })),
        ("MapperIrq", |e| matches!(e, CoreEvent::MapperIrq)),
    ];
    for (label, check) in checks {
        assert!(
            sink.events.iter().any(check),
            "workload must exercise {label} at least once -- got {:?}",
            sink.events
        );
    }
}

/// Ticket W4-00 acceptance criterion 3, the STRONG check for the
/// bus-level half (`crate::ppu::tests::event_emission`'s own
/// `event_emission_does_not_perturb_ppu_internal_state` covers the
/// PPU-only half): two otherwise-identical `NesBus`es (mask ALL vs NONE),
/// driven through the SAME all-eight-sites workload above, compared
/// field-by-field on both bus-level state (never exposed by
/// `retroforge::EmuStepper::state_hash`'s CPU/WRAM/OAM/PRG-RAM/
/// master_cycle/frame_count coverage in a way that would catch a
/// mapper-side or PPU-internal perturbation) and PPU-internal state.
#[test]
fn event_emission_does_not_perturb_bus_or_ppu_internal_state() {
    fn drive(bus: &mut NesBus, mask: EventMask) {
        bus.set_event_mask(mask);
        bus.write(0x2005, 0x11);
        bus.write(0x2005, 0x22);
        bus.write(0x4014, 0x02);
        arm_mmc3_irq_and_enable_rendering(bus);
        advance_cycles(bus, MARGIN_CYCLES);
    }

    let mut bus_off = mmc3_bus();
    drive(&mut bus_off, EventMask::NONE);
    let mut bus_on = mmc3_bus();
    drive(&mut bus_on, EventMask::ALL);

    // Bus-level state (private fields directly -- `system::tests::events`
    // is a descendant of `crate::system`, same visibility `oam_dma.rs`/
    // `mmc3_irq.rs` already rely on).
    assert_eq!(
        bus_off.master_cycle(),
        bus_on.master_cycle(),
        "master_cycle must be identical"
    );
    assert_eq!(bus_off.ram, bus_on.ram, "internal RAM must be identical");
    assert_eq!(
        bus_off.open_bus, bus_on.open_bus,
        "open_bus latch must be identical"
    );
    assert_eq!(
        *bus_off.prg_ram(),
        *bus_on.prg_ram(),
        "PRG-RAM must be identical"
    );
    assert_eq!(
        bus_off.last_oam_dma_stall(),
        bus_on.last_oam_dma_stall(),
        "OAM DMA stall accounting must be identical"
    );
    assert_eq!(
        bus_off.nmi_level_latch, bus_on.nmi_level_latch,
        "the one-PPU-dot-delayed NMI latch must be identical"
    );
    assert_eq!(
        bus_off.irq_line(),
        bus_on.irq_line(),
        "the mapper's own IRQ line must be identical"
    );
    assert_eq!(
        *bus_off.oam(),
        *bus_on.oam(),
        "primary OAM must be identical"
    );

    // PPU-internal state, reachable here because these specific fields are
    // `pub(super)` from `crate::ppu` (visible crate-wide, unlike
    // `line_buffer`/`dot_clock`/etc., which `event_emission.rs`'s own test
    // covers instead since only `crate::ppu`'s descendants can see them).
    assert_eq!(
        bus_off.ppu.status, bus_on.ppu.status,
        "$2002 status must be identical"
    );
    assert_eq!(bus_off.ppu.v, bus_on.ppu.v, "loopy v must be identical");
    assert_eq!(bus_off.ppu.t, bus_on.ppu.t, "loopy t must be identical");
    assert_eq!(bus_off.ppu.x, bus_on.ppu.x, "fine X must be identical");
    assert_eq!(
        bus_off.ppu.scanline, bus_on.ppu.scanline,
        "scanline counter must be identical"
    );
    assert_eq!(
        bus_off.ppu.dot, bus_on.ppu.dot,
        "dot counter must be identical"
    );
    assert_eq!(
        bus_off.frame_count(),
        bus_on.frame_count(),
        "frame_count must be identical"
    );

    // The A12 filter's own drained edge count -- the meaningful proxy for
    // `dot_clock`/`a12_low_since` (private to `crate::ppu`, unreachable
    // here): if either timing field had desynced, this count would too,
    // since it's derived entirely from them (`ppu/mem.rs`'s own doc).
    assert_eq!(
        bus_off.ppu.take_a12_edges(),
        bus_on.ppu.take_a12_edges(),
        "undrained filtered A12 edge count must be identical -- a stray mem_read-based \
         MapperIrq emission would desync exactly this, invisible to status/master_cycle alone"
    );
}
