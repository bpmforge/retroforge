//! The compatibility PPU catch-up scheduler (ticket W3-07b;
//! `docs/design/EMULATION_CORES.md` §5 row 1, "PPU stepping: dot-accurate
//! | catch-up").
//!
//! ## The vacuity trap this file is shaped around
//!
//! The scheduler's whole acceptance is "produces no divergence", and the
//! easiest way to satisfy that is to skip nothing. A test that only
//! compared the two configs' output would go green forever against a
//! scheduler that had quietly become a no-op — which is exactly the
//! failure mode `mode_diff`'s stale-declaration rule exists to catch one
//! level up. So the equivalence test below is paired with a test that the
//! scheduler is doing real work, and that one asserts a MEASURED fraction
//! of dots, not merely "more than zero".
//!
//! ## What the mutations proved, including where they did not
//!
//! Four deliberate breaks of `Ppu::inert_run_len`, run against this file
//! and `crate::ppu::tests::catch_up`:
//!
//! | mutation | caught by |
//! |---|---|
//! | swallow the vblank-set dot (241,1) | 3 tests, including the equivalence test below |
//! | swallow the pre-render clear (261,1) | 2 tests |
//! | call dot 0 of a **rendering** visible line inert | the predicate test only |
//! | call dot 300 of a rendering line inert | two predicate tests only |
//!
//! The last two rows are the honest part. Dot 0 of a rendering scanline
//! turns out to be behaviourally empty, so skipping it changes no state
//! and the equivalence test cannot see it; dot 300 is never even asked
//! about, because the prediction covers the whole scanline and the bus
//! does not re-ask mid-run. Both are still failures — `a_rendering_
//! scanline_has_no_inert_dot` catches them — and they are failures on
//! purpose: "no rendering dot is ever inert" is the STRUCTURAL rule the
//! A12 and `render_enable_pipe` safety arguments rest on, and a rule that
//! is only enforced where it currently happens to matter is not a rule.
//!
//! The equivalence test's workload was rewritten because of this
//! exercise. The first version polled `$2002` without branching on it,
//! and the vblank mutation left every byte of state identical — a
//! workload that never acts on what the PPU says cannot detect a PPU
//! scheduling bug.

use rf_core_api::{CoreSink, PpuPixel, StateError, StateWriter};

use crate::cpu::Cpu;
use crate::state::StateRegion;
use crate::system::NesBus;

struct NullSink;
impl CoreSink for NullSink {
    fn video_scanline(&mut self, _y: u16, _pixels: &[PpuPixel]) {}
    fn audio(&mut self, _samples: &[i16]) {}
    fn event(&mut self, _ev: rf_core_api::CoreEvent) {}
}

#[derive(Default)]
struct MemWriter(Vec<u8>);
impl StateWriter for MemWriter {
    fn write_all(&mut self, buf: &[u8]) -> Result<(), StateError> {
        self.0.extend_from_slice(buf);
        Ok(())
    }
}

/// A workload that keeps all three chips busy, with rendering under the
/// caller's control — the one variable the scheduler's behaviour depends
/// on.
fn workload_rom(rendering: bool) -> Vec<u8> {
    let mut rom = vec![0u8; 16 + 0x4000 + 0x2000];
    rom[0..4].copy_from_slice(b"NES\x1a");
    rom[4] = 1;
    rom[5] = 1;
    let prg = &mut rom[16..16 + 0x4000];
    // The loop OBSERVES the PPU, and that is the whole point of it.
    //
    // An earlier version of this workload polled `$2002` without ever
    // branching on the result, and it was worthless: skipping the one dot
    // that sets the vblank flag left every byte of machine state
    // identical, so the equivalence test below passed against a predicate
    // deliberately broken to swallow (241,1). A workload that never acts
    // on what the PPU says cannot detect a PPU scheduling bug. This one
    // spin-waits on the flag and counts frames, so the vblank dot, the
    // pre-render clear, and the timing of both are all load-bearing.
    let code: &[u8] = &[
        0xA9, 0x1E, // LDA #$1E    (patched to #$00 when rendering is off)
        0x8D, 0x01, 0x20, // STA $2001  ; rendering
        0xA9, 0x9F, 0x8D, 0x00, 0x40, // APU pulse 1: duty/volume
        0xA9, 0x40, 0x8D, 0x02, 0x40, // timer low
        0xA9, 0x08, 0x8D, 0x03, 0x40, // length load
        0xA9, 0x01, 0x8D, 0x15, 0x40, // enable pulse 1
        // wait_set: spin until vblank is flagged.
        0xAD, 0x02, 0x20, // LDA $2002
        0x10, 0xFB, // BPL wait_set
        0xEE, 0x00, 0x00, // INC $0000   ; frames seen
        // wait_clear: spin until it clears again, so a shifted
        // pre-render clear is observable too.
        0xAD, 0x02, 0x20, // LDA $2002
        0x30, 0xFB, // BMI wait_clear
        0xEE, 0x01, 0x00, // INC $0001
        // Sprite-0 hit, which only means anything with rendering on:
        // stores the live status byte so render-dot timing lands in WRAM.
        0xAD, 0x02, 0x20, // LDA $2002
        0x8D, 0x02, 0x00, // STA $0002
        0x4C, 0x18, 0x80, // JMP $8018   ; back to wait_set
    ];
    prg[..code.len()].copy_from_slice(code);
    if !rendering {
        prg[1] = 0x00; // the first LDA's operand
    }
    prg[0x3FFC] = 0x00;
    prg[0x3FFD] = 0x80;
    rom
}

/// Run `frames` frames and return every state region's bytes, which is
/// the whole machine as this crate is able to describe it.
fn run_and_snapshot(rendering: bool, accuracy: bool, frames: u64) -> (Vec<u8>, u64, u64) {
    let rom = workload_rom(rendering);
    let mut bus = NesBus::from_ines_bytes(&rom).expect("workload rom loads");
    bus.set_accuracy_mode(accuracy);
    let mut cpu = Cpu::power_on(&mut bus);
    let target = bus.frame_count() + frames;
    while bus.frame_count() < target {
        cpu.step(&mut bus);
        bus.drain_video(&mut NullSink);
        bus.drain_audio(&mut NullSink);
    }

    // Neutralise the ONE thing the other §5 switch changes. Accuracy and
    // Compatibility differ in two ways at once, because both ride the
    // single `accuracy_mode` flag: this ticket's scheduler, and W3-07's
    // open-bus model (Accuracy ages the decay register once a frame,
    // Compatibility does not — `Ppu::accuracy_mode`'s doc: "The two paths
    // differ in exactly one thing: whether the decay register ages").
    // That declared divergence is `mode_diff`'s business, not this test's,
    // and leaving it in makes the assertion below fail for a reason that
    // has nothing to do with catch-up — as it did on the first run, at
    // state byte 86.
    //
    // Zeroing both halves of the decay state is the narrowest possible
    // exclusion, and it is deliberately done by NAME rather than by
    // skipping a byte range: if a future ticket gives the open-bus switch
    // a second effect, this stops compiling or starts failing, instead of
    // quietly widening the hole.
    bus.ppu.decay = 0;
    bus.ppu.decay_ttl = [0; 8];

    let mut out = MemWriter::default();
    for region in [
        StateRegion::Cpu,
        StateRegion::Ppu,
        StateRegion::Apu,
        StateRegion::Wram,
        StateRegion::Vram,
        StateRegion::Oam,
        StateRegion::Cgram,
        StateRegion::Mapper,
        StateRegion::Cart,
    ] {
        bus.save_region(&cpu, region, &mut out)
            .expect("every region saves");
    }
    // Dots actually ticked, so the "did it skip anything" test can talk
    // about a fraction rather than a bare count.
    let total_dots = bus.master_cycle() * 3;
    (out.0, bus.skipped_dots(), total_dots)
}

/// **Acceptance criterion 2, as a hermetic test.** `mode_diff` already
/// proves this over 52 fetched ROMs, but CI has no ROMs (NFR-006) and the
/// property is too important to be checked only where the artifacts
/// happen to exist.
///
/// The comparison is every save-state region after 12 frames — CPU
/// registers and bus latches, the full PPU (scanline, dot, `v`/`t`, the
/// decay register, sprite state), APU, WRAM, VRAM, OAM, palette, cart RAM
/// and mapper registers. Catch-up is the one §5 row whose risk is stated
/// as "none if catch-up correct", so a single differing byte anywhere is
/// a failure, not a declared divergence.
#[test]
fn compatibility_is_bit_identical_to_lock_step() {
    for rendering in [true, false] {
        let (accuracy, _, _) = run_and_snapshot(rendering, true, 12);
        let (compat, _, _) = run_and_snapshot(rendering, false, 12);
        assert_eq!(
            accuracy.len(),
            compat.len(),
            "rendering={rendering}: state encodings differ in length"
        );
        if let Some(i) = accuracy.iter().zip(&compat).position(|(a, c)| a != c) {
            panic!(
                "rendering={rendering}: the catch-up scheduler diverged from lock-step at state \
                 byte {i} ({:#04X} vs {:#04X}) — EMULATION_CORES §5 allows this switch NO \
                 divergence",
                accuracy[i], compat[i]
            );
        }
    }
}

/// **The anti-vacuity half.** The test above passes trivially against a
/// scheduler that skips nothing, so this one pins that it skips, and how
/// much.
///
/// The fractions are measured, not invented, and they are asserted as
/// bands rather than exact numbers because the workload's own register
/// traffic shifts them slightly. Rendering off, roughly a third of all
/// dots are inert (every visible line past dot 256, all of post-render,
/// nearly all of vblank, nearly all of the pre-render line); rendering
/// on, only post-render and vblank are, which is about 8% — and that gap
/// is the single most important fact about this scheduler, because
/// measuring only the rendering-off case (which is how most of the blargg
/// suite runs) would overstate what it buys a real game by four times.
#[test]
fn the_scheduler_actually_skips_dots_and_by_how_much() {
    let (_, skipped_off, total_off) = run_and_snapshot(false, false, 12);
    let (_, skipped_on, total_on) = run_and_snapshot(true, false, 12);

    #[allow(clippy::cast_precision_loss)]
    let frac = |s: u64, t: u64| 100.0 * s as f64 / t as f64;
    let (off, on) = (frac(skipped_off, total_off), frac(skipped_on, total_on));
    eprintln!("skipped: rendering-off {off:.1}%, rendering-on {on:.1}%");

    assert!(
        (25.0..=35.0).contains(&off),
        "rendering off, ~31% of dots should be skippable, got {off:.1}%"
    );
    assert!(
        (5.0..=11.0).contains(&on),
        "rendering on, ~8% of dots should be skippable, got {on:.1}%"
    );
    assert!(
        on < off / 2.0,
        "the rendering-on figure must stay far below the rendering-off one; collapsing them \
         would mean the predicate had stopped depending on rendering at all"
    );
}

/// Acceptance criterion 3's first half, mechanically: Accuracy does not
/// merely *agree* with lock-step, it *is* lock-step — it never enters the
/// scheduler at all.
#[test]
fn accuracy_mode_skips_nothing() {
    for rendering in [true, false] {
        let (_, skipped, _) = run_and_snapshot(rendering, true, 6);
        assert_eq!(
            skipped, 0,
            "rendering={rendering}: Accuracy is the reference path (project law 6) and must \
             process every dot"
        );
    }
}
