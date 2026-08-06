//! Ticket W2-03's ANALYTIC scanline-IRQ assertion (acceptance criterion 3):
//! "the MMC3 scanline IRQ fires on the EXACT scanline programmed, proven by
//! an ANALYTIC assertion... rather than a recorded golden frame" —
//! `docs/STATUS.md`'s W1-04b entry's law, restated in this ticket's own
//! `plan.json` note: "a golden captured by running the emulator once would
//! bake in whatever bug it had and look just as green".
//!
//! Nothing here is captured from a run of this emulator. The expected
//! scanline is derived from nesdev.org/wiki/MMC3's own stated rule ("the
//! exact number of scanlines between IRQs is N+1, where N is the value
//! written to $C000") and this crate's OWN A12-edge-cadence proof
//! (`ppu/sprites.rs`'s `Ppu::reset_sprite_output_units`/
//! `Ppu::run_sprite_fetch_dot` doc, independently confirmed by the real,
//! fetched `mmc3_test_2/2-details.s` test 8: "Counter should be clocked
//! 241 times in PPU frame" with sprites at `$1xxx`): one filtered A12
//! rising edge per rendered scanline (including the pre-render line),
//! landing at that scanline's own dot 257-320 sprite-fetch window.
//!
//! ## Deriving the expected scanline by hand
//!
//! [`NesBus::new`] leaves the PPU at `(scanline = 261 [pre-render], dot =
//! 0)` (undriven — `crate::ppu::Ppu::new`'s own doc). This test arms the
//! MMC3 IRQ latch/reload/enable registers BEFORE enabling rendering, so the
//! very first A12 edge is the pre-render line's own dot-257 fetch —
//! nesdev: "when the IRQ counter is zero (or a reload is requested through
//! $C001), this value will be copied to the IRQ counter at the NEXT rising
//! edge" — a **reload**, not a decrement, and (revision B, this crate's
//! default) fires only if the reloaded value is itself zero.
//!
//! With `$C000 = LATCH` (chosen `5`, below — see [`mod@self`]'s "Why 5"
//! section) and `LATCH != 0`, that first edge reloads to 5 and does NOT
//! fire. Every following scanline's own edge decrements by exactly one:
//!
//! | edge # | scanline that owns it | counter after | fires? |
//! |---|---|---|---|
//! | 1 | 261 (pre-render) | 5 (reload) | no |
//! | 2 | 0 | 4 | no |
//! | 3 | 1 | 3 | no |
//! | 4 | 2 | 2 | no |
//! | 5 | 3 | 1 | no |
//! | 6 | 4 | 0 | **yes** |
//!
//! So `irq_line()` must read `false` through the end of scanline 3 and
//! `true` from scanline 4 onward (it latches — `$E000` is the only thing
//! that clears it, never written here). Scanline 4 = `LATCH - 1`.
//!
//! **Checked assumption:** this table treats the very first pre-render line
//! as full-length (edge 1 lands at its own dot 257-320 window, not one dot
//! early). That's only true because `Ppu::new` documents `frame_is_odd:
//! false` at power-on (an "arbitrary but documented choice" — see that
//! struct's own doc) and `advance_counters` only skips pre-render's dot 340
//! when `frame_is_odd && rendering_enabled()`. If a future ticket flips that
//! power-on default, this table (and `EXPECTED_FIRE_SCANLINE`) must be
//! re-derived — it is not independent of it.
//!
//! ## Why `LATCH = 5`, not `1` (the off-by-one requirement)
//!
//! Acceptance requires "choose the latch value so an off-by-one changes
//! the expected scanline" (this ticket's `plan.json` note). With `LATCH =
//! 1`, an implementation that (incorrectly) fires on the reload edge
//! itself whenever the reload happens to be small, or one that (correctly)
//! fires on the true nesdev rule, could both coincidentally land on
//! scanline 0 in some broken variants — too little room to distinguish "N"
//! from "N+1" or "off by one edge". `LATCH = 5` (expected scanline 4) means
//! a "clock on N edges instead of N+1" bug fires at scanline 3, a "reload
//! edge itself decrements instead of just reloading" bug fires at scanline
//! 3 too, and a "start counting from scanline 0 instead of the pre-render
//! reload" bug fires at scanline 5 — every one of those plausible off-by-
//! one mistakes lands on a DIFFERENT, distinguishable scanline than the
//! correct answer (4), not coincidentally the same one.
use crate::cpu::CpuBus;
use crate::system::{NesBus, NesRom};

/// Build a synthetic mapper-4 (MMC3) iNES image: `prg_banks` x 16 KiB PRG
/// (zeroed — this test drives the bus directly via [`CpuBus::read`]/
/// [`CpuBus::write`], the same no-`Cpu`-involved technique
/// `system::tests::integration::oam_dma_stolen_cycles_never_flow_through_any_cpu_api`
/// already establishes, so no runnable program is needed), `chr_banks` x
/// 8 KiB CHR (also zeroed — CHR *contents* are irrelevant to A12 timing,
/// only which *addresses* get touched).
fn build_mmc3_ines(prg_banks: u8, chr_banks: u8) -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(&rf_cart::nes::INES_MAGIC);
    data.push(prg_banks);
    data.push(chr_banks);
    data.push(0x40); // flags6: mapper low nibble = 4
    data.push(0x00); // flags7: mapper high nibble = 0
    data.extend_from_slice(&[0u8; 8]); // 8 reserved
    data.extend(vec![0u8; prg_banks as usize * 16 * 1024]);
    data.extend(vec![0u8; chr_banks as usize * 8 * 1024]);
    data
}

fn mmc3_bus() -> NesBus {
    let raw = build_mmc3_ines(2, 1); // 32 KiB PRG (>= MMC3's 4 x 8 KiB minimum), 8 KiB CHR
    let rom = NesRom::from_ines_bytes(&raw).expect("valid synthetic MMC3 image");
    NesBus::new(rom) // Mmc3Revision::B, this crate's default (see `mmc3.rs` module doc)
}

/// The programmed latch — see [`mod@self`]'s "Why 5" section.
const LATCH: u8 = 5;
/// Derived by hand above: `LATCH - 1`.
const EXPECTED_FIRE_SCANLINE: u16 = 4;

/// Drives `bus` forward via harmless open-bus reads (`$4020`, dropped —
/// `crate::system` module doc's memory map) until [`crate::ppu::Ppu`]'s own
/// `scanline` counter (visible here: `system::tests` is a descendant
/// module of `system`, and `Ppu::scanline` is `pub(super)` from `crate`'s
/// root — the same same-crate-only visibility this crate already uses
/// throughout) changes value, then returns the PPU's own `irq_line()`
/// reading taken at that exact wrap. By the time `scanline` wraps to N+1,
/// scanline N's entire dot range (0-340, including its own 257-320
/// sprite-fetch window) has already elapsed, so this reading is exactly
/// "did scanline N's own edge (plus everything before it) cause a fire".
fn advance_to_next_scanline_and_read_irq(bus: &mut NesBus) -> bool {
    let start = bus.ppu.scanline;
    loop {
        bus.read(0x4020);
        if bus.ppu.scanline != start {
            return bus.irq_line();
        }
    }
}

/// Arms `$C000`/`$C001`/`$E001`/`$2000`/`$2001` per this module's doc, then
/// advances scanline-by-scanline and asserts `irq_line()` matches the
/// hand-derived table exactly: `false` through scanline
/// `EXPECTED_FIRE_SCANLINE - 1` (and the pre-render line before it), `true`
/// from `EXPECTED_FIRE_SCANLINE` onward. `+2` iterations (not `+1`) is
/// deliberate: edge 1 is owned by the pre-render line (scanline 261, the
/// starting point, never itself a loop-produced `sl_before`... it IS the
/// first `sl_before`), so observing edges 1 through `LATCH + 1` (the firing
/// edge) needs `LATCH + 1` transitions, i.e. `EXPECTED_FIRE_SCANLINE + 2`
/// (`EXPECTED_FIRE_SCANLINE == LATCH - 1`) — verified by hand against the
/// table, not tuned to make a test pass (a `+1` version was tried first and
/// caught its own off-by-one: it stopped one transition short of the
/// firing edge, only ever observing the edge BEFORE it).
fn assert_analytic_irq_schedule(bus: &mut NesBus) {
    assert!(
        !bus.irq_line(),
        "must not be pending before any A12 edge has even happened"
    );

    let mut observed = Vec::new();
    for _ in 0..(EXPECTED_FIRE_SCANLINE as usize + 2) {
        let sl_before = bus.ppu.scanline;
        let irq_after = advance_to_next_scanline_and_read_irq(bus);
        observed.push((sl_before, irq_after));
    }

    for &(scanline, irq) in &observed {
        if scanline == EXPECTED_FIRE_SCANLINE {
            assert!(
                irq,
                "expected IRQ pending immediately after scanline {EXPECTED_FIRE_SCANLINE} \
                 (latch={LATCH}) -- observed sequence: {observed:?}"
            );
        } else {
            assert!(
                !irq,
                "expected IRQ NOT pending after scanline {scanline} (only scanline \
                 {EXPECTED_FIRE_SCANLINE} should fire for latch={LATCH}) -- observed \
                 sequence: {observed:?}"
            );
        }
    }
}

#[test]
fn irq_fires_on_the_exact_scanline_the_latch_value_analytically_predicts() {
    let mut bus = mmc3_bus();

    // Arm the counter BEFORE any rendering happens, so the very first A12
    // edge (the pre-render line's own dot 257) is a clean, known reload —
    // see this module's doc for why order matters here.
    bus.write(0xC000, LATCH); // latch
    bus.write(0xC001, 0); // request reload on the next edge, counter := 0
    bus.write(0xE001, 0); // enable IRQs

    // $2000: sprites at $1xxx (bit3), BG at $0xxx (bit4 clear), NMI
    // disabled (bit7 clear -- irrelevant here, avoids incidental NMI
    // machinery). $2001: show BG (bit3) + show sprites (bit4) -- both
    // required for `rendering_enabled()`, which gates the whole fetch
    // pipeline this counter depends on.
    bus.write(0x2000, 0x08);
    bus.write(0x2001, 0x18);

    assert_analytic_irq_schedule(&mut bus);
}

/// Same setup and analytic prediction as the test above, but for
/// [`crate::mappers::Mmc3Revision::A`] via the test-only
/// [`NesBus::new_forcing_mmc3_revision_a`] seam — revision A's fire rule
/// differs from B's only in whether a reload-to-zero fires (see
/// `mmc3.rs`'s module doc), which this test's `LATCH = 5` (never reloads
/// to zero) never exercises, so revision A must predict the IDENTICAL
/// scanline. This is a second, independent proof that the scanline-count
/// half of the counter (as opposed to the revision-specific fire-on-zero
/// half) is revision-agnostic, exactly as nesdev's "N+1" rule (which never
/// mentions revisions) implies.
#[test]
fn revision_a_predicts_the_identical_scanline_for_a_nonzero_latch() {
    let raw = build_mmc3_ines(2, 1);
    let rom = NesRom::from_ines_bytes(&raw).expect("valid synthetic MMC3 image");
    let mut bus = NesBus::new_forcing_mmc3_revision_a(rom);

    bus.write(0xC000, LATCH);
    bus.write(0xC001, 0);
    bus.write(0xE001, 0);
    bus.write(0x2000, 0x08);
    bus.write(0x2001, 0x18);

    assert_analytic_irq_schedule(&mut bus);
}
