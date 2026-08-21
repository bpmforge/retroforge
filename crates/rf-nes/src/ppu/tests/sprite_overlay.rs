//! Ticket W3-05a: the sprite-limit-bypass overlay's own tests. Two groups,
//! matching `sprite_evaluation.rs`'s own split:
//! - [`more_than_8_sprites_on_one_scanline_satisfies_all_four_acceptance_criteria_at_once`]
//!   is the ticket's own acceptance criterion 4, direct-call (same
//!   convention `sprite_evaluation.rs` uses throughout): accuracy still
//!   drops the 9th sprite onward, the overlay contains it, the overflow
//!   flag still sets, and the sprite-pipeline state two otherwise-identical
//!   `Ppu`s reach (overlay on vs off) is bit-identical.
//! - [`overlay_recording_does_not_perturb_a12_edge_timing`] is dot-driven
//!   (real [`crate::ppu::Ppu::tick`]) and exists because none of the six
//!   project canaries run with the overlay enabled at all — it is the only
//!   thing that would catch `record_overlay_sprites` regressing from
//!   `Ppu::chr_peek` back to `Ppu::mem_read` (see `mem.rs`'s `chr_peek` doc
//!   for why that distinction matters: `mem_read`'s A12-bus-observation
//!   side effect feeds MMC3's scanline-IRQ counter).
//!
//! Per `sprite_evaluation.rs`'s own trap warning (echoed here since this
//! file follows the same convention): every fixture starts from a
//! non-empty, non-zero `oam`, and the >8-sprite fixtures use a CONTIGUOUS
//! same-Y layout deliberately — a non-contiguous one can legitimately (and
//! correctly, per the buggy diagonal scan `sprites.rs`'s module doc
//! documents) fail to set the overflow flag, which would make this file's
//! assertions flaky against a hardware-correct core for the wrong reason.
use super::{test_ppu, Ppu};
use rf_core_api::PixelLayer;

const STATUS_SPRITE_OVERFLOW: u8 = 0x20;

/// Same "solid fill" discipline `sprite_evaluation.rs::fill_solid_tiles`
/// uses, duplicated here (small, test-only, not worth threading a shared
/// helper across files for — matches this test tree's existing convention
/// of each file owning its own fixtures).
fn fill_solid_tiles(chr: &mut [u8]) {
    for tile in 1u16..=12 {
        let base = (tile * 16) as usize;
        for row in 0..8 {
            chr[base + row] = 0xFF; // lo plane: every bit set -> pattern 1
            chr[base + 8 + row] = 0x00; // hi plane: pattern stays 01
        }
    }
}

fn poke_sprite(oam: &mut [u8; 256], n: u8, y: u8, tile: u8, attr: u8, x: u8) {
    let base = n as usize * 4;
    oam[base] = y;
    oam[base + 1] = tile;
    oam[base + 2] = attr;
    oam[base + 3] = x;
}

/// Screen X for sprite `i` in this file's fixtures: width 8, 1px gap, never
/// overlaps (`sprite_evaluation.rs`'s own convention).
fn x_of(i: u8) -> u16 {
    8 + i as u16 * 9
}

/// 10 sprites (0-9), CONTIGUOUS OAM indices, all at the SAME in-range Y —
/// advisor guidance: this reliably trips the buggy overflow scan's
/// FIRST check (phase 2 starts at n=8, m=0, reads sprite 8's own real Y,
/// which is genuinely in range), so the overflow flag sets regardless of
/// the diagonal bug's exact drift, unlike a non-contiguous layout which
/// could legitimately miss it.
fn overflow_ppu(overlay_enabled: bool) -> Ppu {
    let mut ppu = test_ppu();
    fill_solid_tiles(&mut ppu.chr);
    ppu.write_register(1, 0x14); // show sprites + show in left 8px
    ppu.scanline = 50;
    for i in 0..10u8 {
        poke_sprite(&mut ppu.oam, i, 50, i + 1, 0, x_of(i) as u8);
    }
    ppu.set_sprite_overlay_enabled(overlay_enabled);
    ppu
}

/// Direct-call sprite pipeline, matching `sprite_evaluation.rs`'s own
/// convention: evaluate, load the output units, then render every visible
/// x. `evaluate_sprites` is what actually calls
/// `Ppu::record_overlay_sprites` internally, gated on the flag `overflow_ppu`
/// already set.
fn run_sprite_pipeline(ppu: &mut Ppu) {
    ppu.evaluate_sprites();
    ppu.load_sprite_units();
    for x in 0..256u16 {
        ppu.output_pixel(x);
    }
}

/// Ticket W3-05a acceptance criterion 4, verified as ONE test per its own
/// wording ("asserts all four things at once"). Two independently
/// constructed, otherwise-identical `Ppu`s — one overlay-on, one
/// overlay-off — driven through the identical direct-call sprite pipeline.
///
/// The 4th assertion (mode invariant) compares PPU-INTERNAL state directly
/// (`status`, `secondary_oam_count`, `active_sprite_count`, the full
/// `line_buffer`), not `retroforge::EmuStepper::state_hash()`. That hash
/// (used separately, at the `retroforge` crate level, as FR-MODE-002's
/// formal/weaker check — see `crates/retroforge/tests/
/// sprite_overlay_mode_invariant.rs`'s module doc) covers WRAM/OAM/PRG-RAM/
/// CPU/`master_cycle`/`frame_count` — NONE of which a sprite-limit-bypass
/// mutation (e.g. "quietly raise the 8-sprite cap") would ever touch: that
/// mutation only changes `secondary_oam`/`active_sprites`/`line_buffer`/
/// `self.status`, all PPU-internal and all outside the hash's coverage. A
/// mutation-tested claim that only checked the hash would be vacuous here;
/// this test is the one that actually catches it.
#[test]
fn more_than_8_sprites_on_one_scanline_satisfies_all_four_acceptance_criteria_at_once() {
    let mut ppu_off = overflow_ppu(false);
    let mut ppu_on = overflow_ppu(true);
    run_sprite_pipeline(&mut ppu_off);
    run_sprite_pipeline(&mut ppu_on);

    // (1) the accuracy path still drops the 9th sprite onward -- checked on
    // BOTH ppus, so a "quietly disabled/raised the limit" mutation can't
    // hide behind only checking the overlay-enabled one.
    for ppu in [&ppu_off, &ppu_on] {
        for i in 0..8u8 {
            let px = ppu.line_buffer[x_of(i) as usize];
            assert_eq!(
                px.layer,
                PixelLayer::Sprite,
                "sprite {i} must still render normally"
            );
            assert_eq!(px.sprite_id, Some(i));
        }
        for i in 8..10u8 {
            let px = ppu.line_buffer[x_of(i) as usize];
            assert_eq!(
                px.layer,
                PixelLayer::Backdrop,
                "sprite {i} must still be dropped by the accuracy path -- \
                 the sink stays accuracy-exact whether or not the overlay is on"
            );
            assert_eq!(px.sprite_id, None);
        }
    }

    // (2) the overflow flag still sets -- on BOTH ppus.
    for ppu in [&ppu_off, &ppu_on] {
        assert_eq!(
            ppu.status & STATUS_SPRITE_OVERFLOW,
            STATUS_SPRITE_OVERFLOW,
            "overflow flag must still set regardless of the overlay"
        );
    }

    // (3) the overlay contains the dropped sprites -- ONLY when enabled.
    for i in 8..10u8 {
        assert!(
            !ppu_off.overlay_line_buffer[x_of(i) as usize].opaque,
            "overlay off: nothing must be recorded for dropped sprite {i}"
        );
        assert!(
            ppu_on.overlay_line_buffer[x_of(i) as usize].opaque,
            "overlay on: dropped sprite {i} must be present in the overlay"
        );
    }
    // Sprites 0-7 must NOT double-draw through the overlay -- the real
    // sprite pixel (lower OAM index) already claims that x.
    for i in 0..8u8 {
        assert!(
            !ppu_on.overlay_line_buffer[x_of(i) as usize].opaque,
            "sprite {i} already rendered for real; the overlay must not draw it a second time"
        );
    }

    // (4) mode invariant, mechanically, not asserted: every PPU-internal
    // field the accuracy simulation depends on is bit-identical between
    // overlay-on and overlay-off. A bypass that quietly disabled the limit
    // in the core (the tempting shortcut the ticket explicitly forbids)
    // would change `secondary_oam_count`/`active_sprite_count`/
    // `line_buffer`/`status` here and fail this block -- see this test's
    // own doc for why `EmuStepper::state_hash()` alone could not catch it.
    assert_eq!(
        ppu_off.status, ppu_on.status,
        "status ($2002) must be identical"
    );
    assert_eq!(
        ppu_off.secondary_oam_count, ppu_on.secondary_oam_count,
        "secondary OAM count must be identical"
    );
    assert_eq!(
        ppu_off.active_sprite_count, ppu_on.active_sprite_count,
        "active sprite count must be identical"
    );
    assert_eq!(
        ppu_off.line_buffer, ppu_on.line_buffer,
        "the accuracy-exact rendered framebuffer must be byte-identical"
    );
}

/// Ticket W3-05a: dedicated regression test for the one thing NONE of the
/// six project canaries protect (see this file's module doc) --
/// `record_overlay_sprites` fetching a dropped sprite's pattern bytes
/// through `Ppu::mem_read` instead of the side-effect-free `Ppu::chr_peek`
/// would silently corrupt MMC3's A12-edge-based scanline-IRQ timing.
/// Dot-driven (real [`Ppu::tick`], not a direct call) because A12 edges are
/// a property of the real per-dot bus-access sequence -- a direct call
/// can't exercise it (module doc).
///
/// 8x16 sprites (ctrl bit 5) so each sprite's own tile-byte bit 0 selects
/// its pattern bank independently: the 8 "real" sprites use bank $0000
/// (even tile numbers), the 2 dropped ones use bank $1000 (odd tile
/// numbers) -- deliberately DIFFERENT from the bank everything else on
/// this scanline touches (BG pattern table also pinned to $0000, ctrl bit4
/// clear), so a `mem_read`-based overlay fetch would be the scanline's
/// FIRST bit12=1 bus sample, landing at dot 65 (`evaluate_sprites`'s own
/// dot) rather than any real hardware-timed fetch dot -- exactly the kind
/// of out-of-band bus observation the A12 filter (`mem.rs` module doc) is
/// sensitive to. Correctly implemented (via `chr_peek`), this scanline's
/// bus never sees bit12=1 at all, so both runs' edge counts should be
/// equal (and, incidentally, zero); a regression to `mem_read` diverges
/// them.
#[test]
fn overlay_recording_does_not_perturb_a12_edge_timing() {
    let mut ppu_off = a12_overflow_ppu(false);
    let mut ppu_on = a12_overflow_ppu(true);
    run_full_scanline(&mut ppu_off);
    run_full_scanline(&mut ppu_on);

    assert_eq!(
        ppu_off.status & STATUS_SPRITE_OVERFLOW,
        STATUS_SPRITE_OVERFLOW,
        "sanity: this fixture must genuinely trip the overflow flag (same \
         contiguous-Y layout as the four-criteria test above)"
    );
    assert_eq!(
        ppu_off.status, ppu_on.status,
        "overlay on/off must not change $2002 status bits"
    );
    assert_eq!(
        ppu_off.dot_clock, ppu_on.dot_clock,
        "overlay on/off must not change the PPU dot clock"
    );
    assert_eq!(
        ppu_off.take_a12_edges(),
        ppu_on.take_a12_edges(),
        "overlay on/off must produce IDENTICAL filtered A12 rising-edge counts -- a \
         divergence here means the overlay's CHR fetch used Ppu::mem_read (with its \
         A12-bus-observation side effect) instead of the side-effect-free Ppu::chr_peek, \
         which would silently corrupt MMC3 scanline-IRQ timing on a real cartridge"
    );
}

fn a12_overflow_ppu(overlay_enabled: bool) -> Ppu {
    let mut ppu = test_ppu();
    ppu.ctrl = 0x20; // 8x16 sprites; bit4 clear -> BG pattern table bank $0000
    ppu.write_register(1, 0x10); // show sprites (BG left off; still gates rendering_enabled())
    ppu.scanline = 50;
    ppu.dot = 0;
    for i in 0..8u8 {
        // "real" (accuracy-rendered): even tile -> bank $0000.
        poke_sprite(&mut ppu.oam, i, 48, i * 2, 0, x_of(i) as u8);
    }
    for i in 8..10u8 {
        // dropped: odd tile -> bank $1000, deliberately different from
        // every other bus access this scanline makes.
        poke_sprite(&mut ppu.oam, i, 48, i * 2 + 1, 0, x_of(i) as u8);
    }
    ppu.set_sprite_overlay_enabled(overlay_enabled);
    ppu
}

fn run_full_scanline(ppu: &mut Ppu) {
    for _ in 0..341u32 {
        ppu.tick();
    }
}

/// CONDUCTOR CAUGHT (2026-08-06, W3-05a final review): the direct-call
/// tests above (`run_sprite_pipeline`: evaluate, load, THEN render every
/// x) put `record_overlay_sprites`'s write before every `output_pixel`
/// call, structurally unable to observe a real per-dot ordering bug. This
/// is the dot-driven test that catches it: `Ppu::evaluate_sprites` runs at
/// dot 65, mid-way through the SAME scanline's own dot 1-256 pixel loop.
/// Reading `overlay_sprites` directly from `Ppu::overlay_pixel` (the
/// pre-fix shape) would tear at exactly that dot: x < 64 (dots 1-64,
/// before dot 65's write) see whatever the PREVIOUS scanline's evaluation
/// left behind; x >= 64 (dots 65-256, after) see THIS scanline's own,
/// freshly written value — different content whenever the two scanlines'
/// evaluations disagree, which happens at exactly the scanline a sprite
/// enters or leaves range (this test's ENTRY case; `sprites.rs`'s
/// `reset_sprite_output_units` doc has the SAME argument in prose).
///
/// Fixture: 10 sprites (0-9), contiguous, Y=50 (renders starting screen
/// row 51, one-scanline delay) — sprites 0-7 elsewhere, 8-9 (dropped) at
/// X=8 (dot 9, BEFORE dot 65) and X=200 (dot 201, AFTER dot 65), same tile.
/// Ticked from scanline 49 (where Y=50 is NOT yet in range — matches
/// `Ppu::new`'s already-empty `overlay_sprites`, so no separate priming
/// tick is needed) through the end of scanline 50 — the exact scanline
/// whose OWN dot 65 evaluation is the FIRST to find the two dropped
/// sprites in range. Pre-fix, x=8's stale-empty read and x=200's
/// freshly-populated read disagree; post-fix (`overlay_active_sprites`,
/// latched at dot 257 of the PRECEDING scanline) they agree, because
/// scanline 50's own render reads only what dot 257 of scanline 49 latched
/// — which is empty either way (Y=50 wasn't in range at scanline 49's dot
/// 65 either), matching the correct one-scanline-delayed accuracy answer:
/// neither x=8 nor x=200 shows the dropped sprite yet on scanline 50.
#[test]
fn overlay_recording_stays_in_phase_across_a_scanline_tear_boundary() {
    let mut ppu = tear_boundary_ppu();
    ppu.scanline = 49;
    ppu.dot = 0;
    for _ in 0..(341u32 * 2) {
        ppu.tick();
    }
    assert_eq!(
        ppu.overlay_line_buffer[8].opaque, ppu.overlay_line_buffer[200].opaque,
        "the overlay must not tear mid-scanline: x=8 (before this scanline's own dot-65 \
         sprite evaluation) and x=200 (after it) must agree on whether the SAME dropped \
         sprite is visible on this scanline -- disagreement means overlay_pixel is reading \
         the evaluation-time buffer instead of a dot-257-latched render-time copy"
    );
}

fn tear_boundary_ppu() -> Ppu {
    let mut ppu = test_ppu();
    fill_solid_tiles(&mut ppu.chr);
    ppu.write_register(1, 0x14); // show sprites + show in left 8px
    for i in 0..8u8 {
        // "real": positioned well away from x=8/200.
        poke_sprite(&mut ppu.oam, i, 50, i + 1, 0, 20 + i * 10);
    }
    // dropped: same tile/attr, only X differs -- so a correct
    // implementation's opacity answer at x=8 and x=200 must always agree.
    poke_sprite(&mut ppu.oam, 8, 50, 1, 0, 8);
    poke_sprite(&mut ppu.oam, 9, 50, 1, 0, 200);
    ppu.set_sprite_overlay_enabled(true);
    ppu
}
