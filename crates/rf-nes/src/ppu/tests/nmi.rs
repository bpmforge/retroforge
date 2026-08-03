//! Ticket W1-05b: [`Ppu::nmi_line`] (the `$2000`-bit-7-AND-`$2002`-bit-7
//! level) and the one-dot-early VBlank-read suppression race
//! (`suppress_vblank_this_frame`). Sources cited in `crate::ppu`'s module
//! doc "Scope fence" section and `scroll.rs`'s `read_status` doc; this file
//! doesn't re-cite them per test.
use super::test_ppu;

const STATUS_VBLANK: u8 = 0x80;
const VBLANK_START_SCANLINE: u16 = 241;
const PRERENDER_SCANLINE: u16 = 261;

#[test]
fn nmi_line_is_false_when_ctrl_bit7_is_clear_even_with_vblank_set() {
    let mut ppu = test_ppu();
    ppu.ctrl = 0x00; // NMI disabled
    ppu.status = STATUS_VBLANK;
    assert!(!ppu.nmi_line());
}

#[test]
fn nmi_line_is_false_when_vblank_is_clear_even_with_ctrl_bit7_set() {
    let mut ppu = test_ppu();
    ppu.ctrl = 0x80; // NMI enabled
    ppu.status = 0x00;
    assert!(!ppu.nmi_line());
}

#[test]
fn nmi_line_is_true_only_when_both_are_set() {
    let mut ppu = test_ppu();
    ppu.ctrl = 0x80;
    ppu.status = STATUS_VBLANK;
    assert!(ppu.nmi_line());
}

/// nesdev.org/wiki/NMI, verbatim: "By toggling `NMI_output` (`PPUCTRL.7`)
/// during vertical blank without reading `PPUSTATUS`, a program can cause
/// `/NMI` to be pulled low multiple times, causing multiple NMIs to be
/// generated." [`Ppu::nmi_line`] is a pure, side-effect-free level
/// (`crate::system::NesBus`'s `CpuBus::nmi_line` impl is a direct
/// passthrough, and the CPU's own edge-detector, W1-01b, re-samples it
/// every bus cycle) -- so proving this behavior at this layer is proving
/// the level toggles correctly on every `$2000` write while VBlank stays
/// set, with no bus-side latching to get in the way.
#[test]
fn toggling_nmi_enable_while_vblank_stays_set_retoggles_the_level_each_time() {
    let mut ppu = test_ppu();
    ppu.status = STATUS_VBLANK; // VBlank already set, held for this whole test
    ppu.write_register(0, 0x00); // $2000 = 0: NMI disabled
    assert!(!ppu.nmi_line());

    ppu.write_register(0, 0x80); // enable
    assert!(ppu.nmi_line(), "first enable while VBlank is set");

    ppu.write_register(0, 0x00); // disable
    assert!(!ppu.nmi_line());

    ppu.write_register(0, 0x80); // enable again -- a second rising edge
    assert!(
        ppu.nmi_line(),
        "re-enabling while VBlank is STILL set must re-assert the level"
    );
}

#[test]
fn frame_count_increments_exactly_once_per_frame() {
    let mut ppu = test_ppu();
    assert_eq!(ppu.frame_count(), 0);
    // `Ppu::new` starts on the pre-render line: this first wrap completes
    // only that partial "frame" (see `frame_timing.rs`'s own test on this).
    for _ in 0..341u32 {
        ppu.tick();
    }
    assert_eq!(ppu.frame_count(), 1);
    for _ in 0..(341u32 * 262) {
        ppu.tick();
    }
    assert_eq!(ppu.frame_count(), 2);
}

/// The reachable half of the read-vs-set race (`scroll.rs`'s `read_status`
/// doc): a `$2002` read landing exactly at (scanline 241, dot 0) — one dot
/// before the VBlank-set dot — must suppress the VBlank flag from EVER
/// setting that frame, not just read it as clear once.
#[test]
fn reading_2002_one_dot_before_the_set_suppresses_vblank_for_the_whole_frame() {
    let mut ppu = test_ppu();
    ppu.scanline = VBLANK_START_SCANLINE;
    ppu.dot = 0;

    let result = ppu.read_register(2, 0);
    assert_eq!(
        result & STATUS_VBLANK,
        0,
        "the read itself sees VBlank still clear (correct regardless of suppression)"
    );

    // `tick()` processes whatever `self.dot` CURRENTLY holds, then
    // advances -- so from dot 0, the FIRST tick() processes dot 0 (a
    // no-op for this scanline) and only ADVANCES to dot 1; it does NOT yet
    // process dot 1's own (suppressed) set attempt. A premature assertion
    // here would pass vacuously regardless of whether suppression works —
    // exactly the trap this ticket's own notes warn about. The SECOND
    // tick() is what actually processes dot 1.
    ppu.tick(); // dot 0: no-op, advances to dot 1
    ppu.tick(); // dot 1: the set attempt -- must be suppressed
    assert_eq!(
        ppu.status & STATUS_VBLANK,
        0,
        "VBlank must never set this frame -- nesdev: \"never sets the flag... for that frame\""
    );

    // Keep checking through the rest of the vblank window: it must STAY
    // suppressed, not just miss the exact set dot.
    for _ in 0..30u32 {
        ppu.tick();
        assert_eq!(ppu.status & STATUS_VBLANK, 0);
    }
}

#[test]
fn reading_2002_well_before_the_set_dot_does_not_suppress_it() {
    let mut ppu = test_ppu();
    // A read at dot 0 is the ONLY dot that suppresses (see the test above).
    // Reading earlier in the frame (e.g. mid-visible-scanline) must have no
    // effect on this frame's own VBlank set.
    ppu.scanline = 100;
    ppu.dot = 50;
    let _ = ppu.read_register(2, 0);

    // Loop while `dot != 1` reads a false positive one tick early: the
    // condition already reads (VBLANK_START_SCANLINE, 1) as soon as the
    // tick that processed dot 0 (and advanced to dot 1) has run, but dot
    // 1's OWN set logic hasn't executed yet at that point. One more
    // `tick()` after the loop is what actually runs it.
    while !(ppu.scanline == VBLANK_START_SCANLINE && ppu.dot == 1) {
        ppu.tick();
    }
    ppu.tick();
    assert_eq!(
        ppu.status & STATUS_VBLANK,
        STATUS_VBLANK,
        "a read far from the set dot must not suppress it"
    );
}

#[test]
fn reading_2002_after_the_set_dot_does_not_suppress_it() {
    let mut ppu = test_ppu();
    // Start AT dot 1 itself (not dot 0 + one tick -- `tick()` processes
    // whatever dot `self.dot` already holds, then advances, so starting at
    // dot 0 and ticking once only ADVANCES to dot 1 without processing it).
    ppu.scanline = VBLANK_START_SCANLINE;
    ppu.dot = 1;
    ppu.tick(); // processes dot 1: sets VBlank
    assert_eq!(ppu.status & STATUS_VBLANK, STATUS_VBLANK);

    // A read now (dot 2) sees it set and clears it (ordinary $2002 read
    // behavior) -- unrelated to the one-dot-early suppression window,
    // which only ever applies to a read BEFORE the set.
    let result = ppu.read_register(2, 0);
    assert_eq!(result & STATUS_VBLANK, STATUS_VBLANK);
    assert_eq!(
        ppu.status & STATUS_VBLANK,
        0,
        "ordinary read-clears-VBlank behavior, not the suppression path"
    );
}

/// The suppression latch must not leak into the NEXT frame's own VBlank
/// window -- `process_dot`'s pre-render dot-1 arm resets it alongside the
/// other per-frame status-bit clears.
#[test]
fn suppression_does_not_carry_over_to_the_next_frame() {
    let mut ppu = test_ppu();
    ppu.scanline = VBLANK_START_SCANLINE;
    ppu.dot = 0;
    let _ = ppu.read_register(2, 0); // suppress THIS frame
    ppu.tick(); // dot 1: confirms suppressed
    assert_eq!(ppu.status & STATUS_VBLANK, 0);

    // Run all the way to the NEXT frame's (241, 1) without touching $2002
    // again -- pre-render dot 1 (261, 1) must have reset the latch along
    // the way.
    while !(ppu.scanline == PRERENDER_SCANLINE && ppu.dot == 1) {
        ppu.tick();
    }
    ppu.tick(); // process pre-render dot 1 (resets the suppression latch)
    while !(ppu.scanline == VBLANK_START_SCANLINE && ppu.dot == 1) {
        ppu.tick();
    }
    ppu.tick(); // process the next frame's own dot 1 (the actual VBlank set)
    assert_eq!(
        ppu.status & STATUS_VBLANK,
        STATUS_VBLANK,
        "the NEXT frame's VBlank set must be unaffected by the earlier frame's suppression"
    );
}
