//! **The SNES core emits `CoreEvent`s** (ticket W13-02h).
//!
//! Until this ticket, `grep -rn CoreEvent crates/rf-snes/src` returned
//! **zero lines**: FR-CORE-006's whole subscription channel — the one the
//! enhancement runtime, the event timeline and W13-02e's watchpoints all
//! ride on — had no producer on SNES. Every consumer was NES-only whatever
//! its own code said.

use rf_core_api::{
    CartImage, CoreEvent, CoreSink, EmulatorCore, EventMask, InputFrame, MemWatch, OverlayPixel,
    PpuPixel, Step, WatchAccess, WatchSpace,
};
use rf_snes::core::SnesCore;

#[derive(Default)]
struct Collector {
    events: Vec<CoreEvent>,
}

impl CoreSink for Collector {
    fn video_scanline(&mut self, _y: u16, _pixels: &[PpuPixel]) {}
    fn overlay_scanline(&mut self, _y: u16, _pixels: &[OverlayPixel]) {}
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, ev: CoreEvent) {
        self.events.push(ev);
    }
}

/// A minimal LoROM image: enough header for `rf_cart` to accept it, and a
/// reset vector into code that loops. The events under test are produced
/// by the frame clock, which advances whatever the CPU is doing — so the
/// program does not need to be interesting, and a fixture ROM would only
/// make the test depend on something else being right.
fn tiny_lorom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    // `BRA *` at $8000 — two bytes, branches to itself forever.
    rom[0x0000] = 0x80;
    rom[0x0001] = 0xFE;
    // LoROM header at $7FC0.
    for (i, b) in b"RF EVENT TEST        ".iter().enumerate() {
        rom[0x7FC0 + i] = *b;
    }
    rom[0x7FD5] = 0x20; // LoROM, slow
    rom[0x7FD6] = 0x00; // ROM only
    rom[0x7FD7] = 0x08; // 256 KiB claim; the loader does not require truth here
                        // Reset vector -> $8000.
    rom[0x7FFC] = 0x00;
    rom[0x7FFD] = 0x80;
    rom
}

fn core() -> SnesCore {
    let rom = tiny_lorom();
    let mut core = SnesCore::load(&rom).expect("the test image must load");
    // Through the trait too, so the test exercises the same entry the
    // shell uses rather than only the constructor.
    core.load(CartImage::from_rom(&rom)).expect("load");
    core
}

#[test]
fn an_unsubscribed_core_emits_nothing_at_all() {
    let mut core = core();
    let mut sink = Collector::default();
    for _ in 0..2 {
        core.step(Step::Frame, &mut sink);
    }
    assert!(
        sink.events.is_empty(),
        "EventMask::NONE must build no CoreEvent at all (ARCHITECTURE §5 \
         pay-for-use), got {:?}",
        sink.events
    );
}

#[test]
fn a_subscribed_core_reports_the_frame_boundary_vblank_and_scanlines() {
    let mut core = core();
    core.config().event_mask = EventMask::ALL;
    let mut sink = Collector::default();
    for _ in 0..3 {
        core.step(Step::Frame, &mut sink);
    }

    assert!(
        sink.events.contains(&CoreEvent::FrameStart),
        "no FrameStart in {:?}",
        &sink.events[..sink.events.len().min(8)]
    );
    assert!(sink.events.contains(&CoreEvent::FrameEnd), "no FrameEnd");
    assert!(
        sink.events.contains(&CoreEvent::VblankStart),
        "no VblankStart"
    );
    let scanlines = sink
        .events
        .iter()
        .filter(|e| matches!(e, CoreEvent::Scanline(_)))
        .count();
    assert!(
        scanlines > 100,
        "a frame has ~224 visible lines; got {scanlines} Scanline events"
    );

    // FrameEnd precedes FrameStart at the boundary — the order both
    // variants' own docs specify, and the order rf-nes emits them in.
    let end = sink
        .events
        .iter()
        .position(|e| *e == CoreEvent::FrameEnd)
        .unwrap();
    let start = sink
        .events
        .iter()
        .position(|e| *e == CoreEvent::FrameStart)
        .unwrap();
    assert!(end < start, "FrameEnd must land before FrameStart");
}

/// W13-02e's facility, now reporting on SNES too.
#[test]
fn a_watchpoint_installed_through_coreconfig_reports_hits() {
    let mut core = core();
    core.config().event_mask = EventMask::ALL;
    // $8000 is the reset target this image loops at, so the CPU reads it
    // continuously — a hit is guaranteed without the test depending on
    // anything the program does deliberately.
    core.config().watches.set(&[MemWatch::unconditional(
        42,
        WatchSpace::Cpu,
        0x008000,
        WatchAccess::Read,
    )]);

    let mut sink = Collector::default();
    core.step(Step::Frame, &mut sink);
    assert!(
        sink.events.contains(&CoreEvent::MemWatch { id: 42 }),
        "an armed read watch on the executing address must fire"
    );

    // Disarming stops it, and the empty table is the fast path again.
    core.config().watches.clear();
    let mut after = Collector::default();
    core.step(Step::Frame, &mut after);
    assert!(
        !after
            .events
            .iter()
            .any(|e| matches!(e, CoreEvent::MemWatch { .. })),
        "a cleared table must report nothing"
    );
}

/// Events are observation. A core that ran differently when subscribed
/// would be a core whose debugger changed the game.
#[test]
fn subscribing_does_not_change_what_the_machine_does() {
    let mut quiet = core();
    let mut loud = core();
    loud.config().event_mask = EventMask::ALL;
    loud.config().watches.set(&[MemWatch::unconditional(
        1,
        WatchSpace::Cpu,
        0x008000,
        WatchAccess::Any,
    )]);

    let mut a = Collector::default();
    let mut b = Collector::default();
    let input = InputFrame::default();
    for _ in 0..3 {
        quiet.run_frame(&input, &mut a);
        loud.run_frame(&input, &mut b);
    }
    assert_eq!(
        quiet.state_view().wram,
        loud.state_view().wram,
        "subscription must not perturb machine state"
    );
    assert!(a.events.is_empty());
    assert!(!b.events.is_empty());
}
