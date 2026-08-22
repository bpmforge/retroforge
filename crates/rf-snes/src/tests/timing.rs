//! Frame timing, `$4210`/`$4211` race behaviour and auto-joypad
//! (ticket W6-02b, criterion 1).

use crate::bus::SnesBus;
use crate::cpu::CpuBus;
use crate::regs::NmiTimen;
use crate::timing::{Timing, LINES_PER_FRAME, MASTER_PER_LINE, VBLANK_START_LINE};
use rf_cart::SnesMapMode;

fn bus() -> SnesBus {
    SnesBus::new(vec![0; 32 * 1024], 0, SnesMapMode::LoRom)
}

fn advance(t: &mut Timing, cycles: u64) {
    t.advance(cycles, false, |_, _| false);
}

#[test]
fn a_frame_is_262_lines_of_1364_master_cycles() {
    let mut t = Timing::new();
    advance(&mut t, MASTER_PER_LINE);
    assert_eq!(t.line, 1);
    advance(&mut t, MASTER_PER_LINE * u64::from(LINES_PER_FRAME - 1));
    assert_eq!((t.line, t.frame), (0, 1), "one full frame wraps to line 0");
}

#[test]
fn vblank_begins_at_line_225_and_sets_the_nmi_flag() {
    let mut t = Timing::new();
    assert!(!t.in_vblank());
    advance(&mut t, MASTER_PER_LINE * u64::from(VBLANK_START_LINE) - 1);
    assert!(!t.in_vblank(), "still rendering one cycle before line 225");
    assert!(!t.nmi_flag);
    advance(&mut t, 2);
    assert!(t.in_vblank());
    assert!(t.nmi_flag, "the NMI flag latches when vblank starts");
}

/// **The `$4210` race.** Reading RDNMI clears the flag, and gilyon
/// cputest's `wait_for_vblank` depends on exactly that.
///
/// Without clear-on-read its first loop (`bit $4210 : bmi`) never exits
/// and the ROM hangs before running a single test — so this is
/// load-bearing for the whole suite, not an edge case.
#[test]
fn reading_4210_clears_the_nmi_flag() {
    let mut t = Timing::new();
    advance(&mut t, MASTER_PER_LINE * u64::from(VBLANK_START_LINE) + 1);
    assert!(t.nmi_flag);

    let first = t.read_rdnmi();
    assert_eq!(first & 0x80, 0x80, "the first read reports the flag");
    assert_eq!(first & 0x0F, 0x02, "low nibble is the CPU version");
    assert_eq!(t.read_rdnmi() & 0x80, 0x00, "...and cleared it");
    assert!(!t.nmi_flag);

    // It latches again next frame, so a poller keeps working.
    advance(&mut t, MASTER_PER_LINE * u64::from(LINES_PER_FRAME));
    assert!(t.nmi_flag, "the next vblank sets it again");
}

/// The flag persists until read, rather than lasting only an instant.
///
/// A game that polls slowly must still see the vblank it missed the
/// exact cycle of.
#[test]
fn the_nmi_flag_persists_until_something_reads_it() {
    let mut t = Timing::new();
    advance(&mut t, MASTER_PER_LINE * u64::from(VBLANK_START_LINE) + 1);
    advance(&mut t, MASTER_PER_LINE * 10);
    assert!(t.nmi_flag, "still set ten scanlines later");
}

/// `$4210` must go through `read`, never `peek` — a debugger that peeked
/// it would acknowledge a vblank the program had not seen.
#[test]
fn peeking_4210_does_not_acknowledge_the_vblank() {
    let mut b = bus();
    b.timing.nmi_flag = true;
    assert_eq!(b.peek(0x00_4210) & 0x80, 0x00, "peek must not report it");
    assert!(b.timing.nmi_flag, "and must not clear it");
    assert_eq!(b.read(0x00_4210) & 0x80, 0x80);
    assert!(!b.timing.nmi_flag);
}

#[test]
fn hvbjoy_reports_vblank_hblank_and_auto_joypad_busy() {
    let mut t = Timing::new();
    assert_eq!(t.read_hvbjoy() & 0x80, 0, "not in vblank at line 0");
    advance(&mut t, MASTER_PER_LINE * u64::from(VBLANK_START_LINE) + 1);
    assert_eq!(t.read_hvbjoy() & 0x80, 0x80, "vblank flag");
}

/// Auto-joypad runs only when `$4200` bit 0 is set, occupies a window of
/// about three scanlines, and reports busy for its duration.
#[test]
fn auto_joypad_runs_only_when_enabled_and_reports_busy() {
    // Disabled: no window at all.
    let mut off = Timing::new();
    off.advance(
        MASTER_PER_LINE * u64::from(VBLANK_START_LINE) + 1,
        false,
        |_, _| false,
    );
    assert!(!off.auto_joypad_busy());
    assert_eq!(off.read_hvbjoy() & 0x01, 0);

    // Enabled: busy from the start of vblank for roughly three lines.
    let mut on = Timing::new();
    on.advance(
        MASTER_PER_LINE * u64::from(VBLANK_START_LINE) + 1,
        true,
        |_, _| false,
    );
    assert!(on.auto_joypad_busy(), "the window opens with vblank");
    assert_eq!(on.read_hvbjoy() & 0x01, 0x01, "$4212 bit 0 reports busy");

    on.advance(MASTER_PER_LINE * 4, true, |_, _| false);
    assert!(!on.auto_joypad_busy(), "and closes within four scanlines");
}

/// The controller is latched on the window's CLOSING edge, not
/// continuously.
///
/// Latching whenever idle would make `$4218` track the pad in real time
/// and hide any error in the window's timing — the port would look right
/// for the wrong reason.
#[test]
fn the_joypad_latches_on_the_closing_edge_of_the_window() {
    let mut t = Timing::new();
    let mut closes = 0;
    // One whole frame with auto-joypad on: exactly one closing edge.
    for _ in 0..u64::from(LINES_PER_FRAME) {
        let e = t.advance(MASTER_PER_LINE, true, |_, _| false);
        if e.auto_joypad_done {
            closes += 1;
        }
    }
    assert_eq!(closes, 1, "one latch per frame");
}

#[test]
fn the_latched_ports_are_what_4218_returns() {
    let mut b = bus();
    b.joypads.ports[0] = 0xABCD;
    // Before latching, the port still reads the old value.
    assert_eq!(b.read(0x00_4218), 0x00);
    b.joypads.latch();
    assert_eq!(b.read(0x00_4218), 0xCD, "JOY1L");
    assert_eq!(b.read(0x00_4219), 0xAB, "JOY1H");
}

/// An H/V IRQ must not be stepped over by a long instruction: the
/// comparison is asked once per dot crossed.
#[test]
fn hv_irq_is_checked_per_dot_not_per_instruction() {
    let mut t = Timing::new();
    let mut hits = 0;
    // Advance a whole scanline in ONE call, matching a single dot.
    t.advance(MASTER_PER_LINE, false, |dot, _| {
        if dot == 100 {
            hits += 1;
            true
        } else {
            false
        }
    });
    assert_eq!(hits, 1, "dot 100 must be seen even inside a big advance");
}

#[test]
fn nmitimen_gates_auto_joypad_through_the_bus() {
    let mut b = bus();
    b.write(0x00_4200, 0x01);
    assert!(b.nmitimen.auto_joypad());
    assert_eq!(b.nmitimen, NmiTimen(0x01));
    b.write(0x00_4200, 0x00);
    assert!(!b.nmitimen.auto_joypad());
}

// ---------------------------------------------------------------------
// Interrupt dispatch
// ---------------------------------------------------------------------

use crate::cpu::{flags, Cpu, FlatBus};

/// A hardware interrupt pushes `B` CLEAR; `BRK` pushes it set.
///
/// That single bit is how a handler tells the two apart, and in emulation
/// mode it shares `X`'s position — which is forced set — so pushing `P`
/// unmodified would report every IRQ as a `BRK`.
#[test]
fn a_hardware_interrupt_pushes_b_clear_while_brk_pushes_it_set() {
    // Hardware interrupt, emulation mode.
    let mut cpu = Cpu::new();
    cpu.set_emulation(true);
    cpu.sp = 0x01FF;
    cpu.pc = 0x1234;
    let mut bus = FlatBus::new();
    bus.mem[0xFFFE] = 0x00;
    bus.mem[0xFFFF] = 0x90;
    cpu.interrupt(&mut bus, false);
    let pushed_irq = bus.mem[0x01FD];
    assert_eq!(pushed_irq & flags::X, 0, "IRQ must push B clear");
    assert_eq!(cpu.pc, 0x9000, "and vector through $FFFE in emulation mode");
    assert!(cpu.flag(flags::I), "interrupts masked on entry");
    assert!(!cpu.flag(flags::D), "decimal cleared on entry");

    // BRK, same mode.
    let mut cpu2 = Cpu::new();
    cpu2.set_emulation(true);
    cpu2.sp = 0x01FF;
    cpu2.pc = 0x1234;
    let mut bus2 = FlatBus::new();
    bus2.load(0x1234, &[0x00, 0x00]);
    bus2.mem[0xFFFE] = 0x00;
    bus2.mem[0xFFFF] = 0x90;
    cpu2.step(&mut bus2).expect("implemented");
    assert_ne!(bus2.mem[0x01FD] & flags::X, 0, "BRK must push B set");
}

/// The four interrupt vectors, one per (kind, mode).
#[test]
fn interrupts_take_the_right_vector_for_the_mode() {
    for (nmi, emulation, vector, name) in [
        (true, true, 0xFFFAusize, "NMI emulation"),
        (true, false, 0xFFEA, "NMI native"),
        (false, true, 0xFFFE, "IRQ emulation"),
        (false, false, 0xFFEE, "IRQ native"),
    ] {
        let mut cpu = Cpu::new();
        cpu.set_emulation(emulation);
        cpu.sp = 0x01FF;
        cpu.pc = 0x1000;
        let mut bus = FlatBus::new();
        bus.mem[vector] = 0xCD;
        bus.mem[vector + 1] = 0xAB;
        cpu.interrupt(&mut bus, nmi);
        assert_eq!(cpu.pc, 0xABCD, "{name} should vector through {vector:#06X}");
        assert_eq!(cpu.pbr, 0, "{name} clears the program bank");
    }
}

/// An interrupt resumes at the preempted instruction — it does not skip a
/// byte the way `BRK` skips its signature.
#[test]
fn an_interrupt_pushes_the_unadvanced_pc() {
    let mut cpu = Cpu::new();
    cpu.set_emulation(false);
    cpu.sp = 0x01FF;
    cpu.pc = 0x4321;
    cpu.pbr = 0x7E;
    let mut bus = FlatBus::new();
    cpu.interrupt(&mut bus, false);
    assert_eq!(bus.mem[0x01FF], 0x7E, "native pushes PBR first");
    assert_eq!(bus.mem[0x01FE], 0x43, "then PC high");
    assert_eq!(bus.mem[0x01FD], 0x21, "then PC low — unadvanced");
}

/// `WAI` waits for an interrupt, so an interrupt must wake it.
#[test]
fn an_interrupt_wakes_a_cpu_halted_by_wai() {
    let mut cpu = Cpu::new();
    cpu.set_emulation(true);
    cpu.sp = 0x01FF;
    let mut bus = FlatBus::new();
    bus.load(0, &[0xCB]); // WAI
    cpu.step(&mut bus).expect("implemented");
    assert!(cpu.stopped);
    cpu.interrupt(&mut bus, false);
    assert!(!cpu.stopped, "an interrupt is exactly what WAI waits for");
}

/// **Overscan moves the vblank boundary, it does not add lines** (ticket
/// W7-06).
///
/// The frame is 262 lines either way; overscan takes 15 of them from
/// vblank and gives them to the display. That is worth pinning because
/// the consequence is counter-intuitive: turning overscan ON makes vblank
/// SHORTER, so a game whose vblank DMA routine was comfortable at 224
/// lines starts writing VRAM during active display at 239.
#[test]
fn overscan_shortens_vblank_rather_than_lengthening_the_frame() {
    let mut t = Timing::new();
    assert_eq!(t.vblank_start, 225);
    t.line = 230;
    assert!(t.in_vblank(), "line 230 is vblank in 224-line mode");

    t.set_overscan(true);
    assert_eq!(t.vblank_start, 240);
    assert!(
        !t.in_vblank(),
        "line 230 is VISIBLE with overscan on - treating it as vblank is \
         what crops the bottom of an overscan game"
    );
    assert_eq!(
        LINES_PER_FRAME, 262,
        "overscan must not change the frame length"
    );

    t.set_overscan(false);
    assert!(t.in_vblank(), "and back again");
}

// ---------------------------------------------------------------------
// Region: NTSC vs PAL (ticket W7-10)
// ---------------------------------------------------------------------

/// The three region figures, checked against the sources rather than
/// recalled.
///
/// fullsnes gives the V counter as "0 to 261 in NTSC mode" and "0 to 311
/// in PAL mode"; the PAL master clock is 21.281370 MHz (4.8x chroma, a
/// 17.734475 MHz crystal multiplied by 6/5) against NTSC's 21.477270 MHz.
/// A scanline is 1364 master cycles in BOTH regions — that one is easy to
/// assume moves and it does not.
#[test]
fn the_region_constants_are_the_documented_ones() {
    use crate::timing::Region;
    assert_eq!(Region::Ntsc.master_clock_hz(), 21_477_270);
    assert_eq!(Region::Pal.master_clock_hz(), 21_281_370);
    assert_eq!(Region::Ntsc.lines_per_frame(), 262);
    assert_eq!(Region::Pal.lines_per_frame(), 312);
    assert_eq!(
        MASTER_PER_LINE, 1364,
        "a scanline is 1364 master cycles in both regions"
    );
}

/// The frame rate is DERIVED, and both regions land where they should.
///
/// 60.1 Hz and 50.0 Hz are consequences of the clock and the line count,
/// never a second constant — writing them down separately is how the two
/// drift apart, which is the failure this project has hit three times.
#[test]
fn both_regions_produce_their_documented_frame_rate() {
    use crate::timing::Region;
    let ntsc = Region::Ntsc.frame_rate();
    let pal = Region::Pal.frame_rate();
    assert!(
        (ntsc - 60.0988).abs() < 0.001,
        "NTSC should be ~60.0988 Hz, got {ntsc}"
    );
    assert!(
        (pal - 50.0070).abs() < 0.001,
        "PAL should be ~50.0070 Hz, got {pal}"
    );
    assert!(pal < ntsc, "PAL is the slower frame rate");
}

/// A PAL frame really is 312 lines long — the clock wraps there, not at
/// 262.
#[test]
fn a_pal_frame_wraps_at_312_lines_not_262() {
    use crate::timing::Region;
    let mut t = Timing::with_region(Region::Pal);
    assert_eq!(t.lines_per_frame(), 312);

    // One line short of an NTSC frame: a PAL clock must still be on
    // frame 0, which is the whole difference.
    advance(&mut t, MASTER_PER_LINE * 262);
    assert_eq!(t.frame, 0, "262 lines is NOT a frame in PAL");
    assert_eq!(t.line, 262);

    advance(&mut t, MASTER_PER_LINE * 50);
    assert_eq!(t.frame, 1, "312 lines IS");
    assert_eq!(t.line, 0);
}

/// **Region does not move the visible window.** PAL renders the same
/// 224 lines (239 with overscan); its extra 50 scanlines are all vblank.
///
/// Getting this wrong is the tempting bug: "PAL has more lines, so it
/// must show more picture" is false, and a model that believed it would
/// letterbox every PAL game differently from hardware.
#[test]
fn pal_shows_the_same_visible_lines_and_spends_the_rest_in_vblank() {
    use crate::timing::Region;
    let ntsc = Timing::with_region(Region::Ntsc);
    let pal = Timing::with_region(Region::Pal);
    assert_eq!(pal.vblank_start, ntsc.vblank_start);
    assert_eq!(pal.vblank_start, VBLANK_START_LINE);

    let ntsc_vblank = ntsc.lines_per_frame() - ntsc.vblank_start;
    let pal_vblank = pal.lines_per_frame() - pal.vblank_start;
    assert_eq!(ntsc_vblank, 37);
    assert_eq!(pal_vblank, 87, "every extra PAL line is vblank");
}

/// Overscan and region are independent axes and must compose.
#[test]
fn overscan_and_region_compose() {
    use crate::timing::Region;
    let mut t = Timing::with_region(Region::Pal);
    t.set_overscan(true);
    assert_eq!(t.vblank_start, 240);
    assert_eq!(
        t.lines_per_frame(),
        312,
        "overscan must not change the frame length in PAL either"
    );
}

/// A fresh system is NTSC, per `EMULATION_CORES.md` §3's NTSC-first rule.
#[test]
fn the_default_region_is_ntsc() {
    use crate::timing::Region;
    assert_eq!(Timing::new().region, Region::Ntsc);
    assert_eq!(crate::timing::Region::default(), Region::Ntsc);
}

/// **Criterion 3, the half that needs no artifact.** The same ROM run for
/// the same number of master cycles reaches FEWER frames in PAL, in the
/// exact ratio the line counts predict.
///
/// This is the "differs in the expected way rather than by accident"
/// check: 262/312 = 0.8397, so PAL should reach ~84% of NTSC's frame
/// count. A region switch that changed the frame rate by any other factor
/// — or by none — is caught here rather than being noticed later as
/// "PAL feels wrong".
#[test]
fn pal_reaches_fewer_frames_than_ntsc_in_the_same_time() {
    use crate::system::SnesSystem;
    use crate::timing::Region;
    use rf_cart::SnesMapMode;

    let frames_after = |region: Region| -> u64 {
        let mut rom = vec![0xEAu8; 32 * 1024]; // NOP sled
        rom[0x7FFC] = 0x00;
        rom[0x7FFD] = 0x80;
        let mut s = SnesSystem::from_rom(rom, SnesMapMode::LoRom, 0);
        s.set_region(region);
        for _ in 0..3_000_000 {
            s.step().expect("NOP is implemented");
        }
        s.bus.timing.frame
    };

    let ntsc = frames_after(Region::Ntsc);
    let pal = frames_after(Region::Pal);
    assert!(
        ntsc > 0 && pal > 0,
        "both regions must advance the frame clock"
    );

    // Every instruction costs the same master cycles in both regions —
    // the CPU does not change — so the frame counts differ purely by
    // frame LENGTH, and frames are INVERSELY proportional to it: the
    // region with MORE lines per frame completes FEWER frames.
    //
    // (Written the other way up first, and this assertion caught it.)
    let expected = f64::from(u32::from(Region::Pal.lines_per_frame()))
        / f64::from(u32::from(Region::Ntsc.lines_per_frame()));
    let actual = ntsc as f64 / pal as f64;
    assert!(
        (actual - expected).abs() < 0.01,
        "NTSC/PAL frame ratio should be {expected:.4} (312/262); got \
         {actual:.4} from {ntsc} vs {pal} frames"
    );
}
