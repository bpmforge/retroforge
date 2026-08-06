//! Sprite evaluation (secondary OAM, the 8-sprite-per-scanline limit, and
//! the buggy overflow-flag scan) plus BG/sprite pixel compositing —
//! ticket W1-05a.
//!
//! ## One-scanline pipeline delay
//!
//! nesdev.org/wiki/PPU_OAM, OAM byte 0 ("Y position"): "Sprite data is
//! delayed by one scanline; you must subtract 1 from the sprite's Y
//! coordinate before writing it here." And nesdev.org/wiki/PPU_sprite_evaluation:
//! "During all visible scanlines, the PPU scans through OAM to determine
//! which sprites to render on the next scanline", and its Notes section:
//! "Sprite evaluation does not happen on the pre-render scanline. Because
//! evaluation applies to the next line's sprite rendering, no sprites will
//! be rendered on the first scanline, and this is why there is a 1 line
//! offset on a sprite's Y coordinate."
//!
//! This module models that as two buffers:
//! - [`Ppu::evaluate_sprites`] fills `secondary_oam` during a VISIBLE
//!   scanline N's dots 65-256, using scanline N itself (`self.scanline`) as
//!   the in-range comparison value — a sprite whose stored OAM Y equals N
//!   is "found" during N's own evaluation.
//! - [`Ppu::load_sprite_units`] (dot 257, both visible AND pre-render lines
//!   — nesdev's shared "sprite tile loading interval", also where `OAMADDR`
//!   resets to 0) fetches each found sprite's CHR pattern bytes and
//!   latches them into `active_sprites`, which [`Ppu::output_pixel`]'s
//!   compositing reads while drawing the scanline that immediately
//!   follows.
//!
//! Latching pattern bytes at dot 257 (rather than re-deriving a row from
//! `self.scanline` at arbitrary render time) is deliberate: the row
//! computed here is only valid because `self.scanline` at latch time is
//! *exactly* the same scanline `evaluate_sprites` used moments earlier as
//! its in-range comparison (`sprite.y <= self.scanline < sprite.y +
//! height`), which already makes `self.scanline - sprite.y` the correct
//! 0-indexed row. Recomputing this later from whatever `self.scanline`
//! happens to be at pixel-output time would only be correct if rendering
//! had stayed continuously enabled since the preceding scanline — a
//! fragile invariant with no oracle to catch a violation. Real hardware
//! doesn't recompute either: the 8 sprite output units latch already-
//! fetched pattern bytes during dots 257-320.
//!
//! The pre-render line never calls `evaluate_sprites` (per the "does not
//! happen on the pre-render scanline" quote above) but its dot 1
//! unconditionally clears both `secondary_oam` and `active_sprites`
//! ([`Ppu::process_render_dot`] in `background.rs`, NOT gated on
//! `rendering_enabled` — a mid-frame rendering toggle must never leave a
//! stale sprite set behind for scanline 0), and dot 257 still runs
//! `load_sprite_units` (the shared load window). Together these guarantee
//! scanline 0 always loads an empty active set, reproducing "no sprites
//! will be rendered on the first scanline" without a special-cased
//! scanline-0 branch.
//!
//! ## The buggy overflow-flag scan
//!
//! nesdev.org/wiki/PPU_sprite_evaluation's own numbered algorithm (wording
//! normalized for this doc comment, retrieved 2026-08-03 — treat this as a
//! faithful paraphrase, not a byte-exact quote):
//!
//! ```text
//! 1. Starting at n = 0, read a sprite's Y-coordinate (OAM[n][0]), copying
//!    it to the next open slot in secondary OAM (unless 8 sprites have
//!    already been found, in which case the write is ignored).
//!    1a. If the Y-coordinate is in range, also copy OAM[n][1..=3] into
//!        secondary OAM.
//! 2. Increment n.
//!    2a. If n has overflowed back to zero (all 64 sprites evaluated), stop.
//!    2b. If fewer than 8 sprites have been found, go to 1.
//!    2c. If exactly 8 have been found, secondary OAM is now full and
//!        further sprites drop out — continue to 3.
//! 3. Starting at m = 0, evaluate OAM[n][m] as a Y-coordinate.
//!    3a. If it's in range, set the sprite-overflow flag in $2002.
//!    3b. If it's NOT in range, increment BOTH n and m (m without carry
//!        into n beyond the normal n += 1), and repeat 3 until n overflows
//!        back to zero.
//! ```
//!
//! Step 3b is the load-bearing bug: once 8 sprites are already found, a
//! miss during the overflow search increments `n` (the sprite index) AND
//! `m` (the byte-within-sprite index, wrapping mod 4, never reset to 0) —
//! so the *next* comparison reads `OAM[n][m]` where `m` is no longer 0,
//! i.e. a tile index, attribute byte, or X position, as if it were a
//! Y-coordinate. That produces both false positives (a non-Y byte that
//! numerically looks like an in-range Y sets the flag even with no real
//! 9th sprite) and false negatives (a genuine 9th in-range sprite's real Y
//! byte gets skipped because by the time `n` reaches it, `m` isn't 0
//! anymore). [`Ppu::evaluate_sprites`] implements exactly this — it does
//! NOT special-case "a 9th sprite exists", which is the common wrong
//! shortcut this ticket explicitly warns against.
//!
//! Cross-checked against a second, independent source:
//! [nesalizer](https://github.com/ulfalizer/nesalizer)'s
//! `do_sprite_evaluation` implements step 3b's "increment n and m without
//! carry" as `oam_addr = ((oam_addr + 4) & 0xFC) | ((oam_addr + 1) & 3)` —
//! the same rule expressed as one 8-bit `n*4+m` pointer (`(oam_addr+4)&0xFC`
//! is `(n+1)*4`; `(oam_addr+1)&3` is `(m+1) mod 4`) — confirming both `n`
//! and `m` advance on a miss, with `m` wrapping rather than resetting.
//!
//! Once the 8-sprite cap is hit, later steps in the algorithm (continuing
//! to read 3 more bytes after a hit, OAM-write side effects during the
//! scan, etc.) have no effect this ticket's scope can observe: secondary
//! OAM stays full/write-disabled either way, and nothing here models
//! cycle-exact `$2004` snooping mid-evaluation. [`Ppu::evaluate_sprites`]
//! therefore stops as soon as the flag is set rather than continuing to
//! re-set an already-set flag.
//!
//! ## `OAMADDR` reset and what this module deliberately does NOT implement
//!
//! nesdev.org/wiki/PPU_registers: "OAMADDR is set to 0 during each of ticks
//! 257-320 (the sprite tile loading interval) of the pre-render and
//! visible scanlines" — implemented in `background.rs`'s
//! `process_render_dot`. In the ordinary case (rendering continuously
//! enabled) this guarantees `evaluate_sprites` always starts scanning from
//! `n = 0` on the very next scanline, matching the algorithm's own
//! "Starting at n = 0". A SEPARATE, chip-revision-specific quirk — nesdev:
//! "if the sprite address (OAMADDR, $2003) is not zero, the process of
//! starting sprite evaluation triggers an OAM hardware refresh bug" that
//! corrupts the first 8 OAM bytes on 2C02G/H — is NOT implemented here (see
//! `crate::ppu`'s module doc scope fence); it is a distinct effect from the
//! reset above and this ticket's acceptance criteria don't cover it.
//!
//! ## `dropped_by_limit`
//!
//! Always `false` — see [`rf_core_api::video::PpuPixel::dropped_by_limit`]'s
//! doc for the full ruling (Brad, 2026-08-03). A sprite dropped by the
//! 8-sprite cap is simply never copied into `secondary_oam`/`active_sprites`
//! in the first place, so it never reaches [`Ppu::output_pixel`] to be
//! flagged at all. Do not add code that sets this flag on a `Sprite`-layer
//! pixel; the sink is accuracy-exact by design (see the ruling).
//!
//! ## Sprite-0 hit (ticket W1-05b)
//!
//! nesdev.org/wiki/PPU_OAM's "Sprite zero hits" section, condensed to its
//! defining conditions (verified live 2026-08-03, quoted where load-bearing):
//! "When an opaque pixel of sprite 0 overlaps an opaque pixel of the
//! background, this is a sprite 0 hit" — set starting at that pixel's own
//! dot — and does NOT occur:
//! - "If background or sprite rendering is disabled in PPUMASK" — already
//!   guaranteed by reusing [`Ppu::background_pixel`]/[`Ppu::sprite_pixel`]
//!   below, both of which already return "transparent" whenever their own
//!   `$2001` show-bit is off.
//! - "At x=0 to x=7 if the left-side clipping window is enabled (if bit 2
//!   or bit 1 of PPUMASK is 0)" — also already covered by reusing those
//!   same two resolvers: each already forces its own layer transparent in
//!   x<8 when ITS OWN left-8 mask bit is clear, so "either bit clear
//!   suppresses the hit" falls out of the AND automatically, without a
//!   separate x<8 branch here.
//! - "At x=255, for an obscure reason related to the pixel pipeline" —
//!   handled by an explicit `x != 255` check, since neither resolver above
//!   has any other reason to treat x=255 specially.
//! - "At any pixel where the background or sprite pixel is transparent" —
//!   same reuse as the rendering-disabled case.
//! - Sprite priority, pixel colors, and palette contents do NOT gate it —
//!   sprite 0 can hit "from behind" the background. [`Ppu::sprite_pixel`]
//!   never consults `behind_background` while searching for an opaque
//!   match, so checking its result's `oam_index` below is priority-blind
//!   by construction, matching this rule for free.
//!
//! [`Ppu::sprite_pixel`]'s OAM-order search means sprite 0 (`oam_index ==
//! 0`, the lowest possible index) is always the first candidate checked
//! whenever it is present and opaque at a given x — so `sprite.oam_index
//! == 0` on its `Some` result is exactly "sprite 0's own opaque pixel is
//! the one that would be drawn here", the same "opaque sprite-0 pixel"
//! nesdev's condition names.
use super::{EvaluatedSprite, Ppu, SpriteUnit, EMPTY_EVALUATED_SPRITE, EMPTY_SPRITE_UNIT};
use crate::ppu::{STATUS_SPRITE0_HIT, STATUS_SPRITE_OVERFLOW};
use rf_core_api::{PixelLayer, PpuPixel};

/// The resolved sprite-layer contribution at one screen x, from
/// [`Ppu::sprite_pixel`] — the winning (highest-priority, first opaque)
/// active sprite, if any. `Copy`: [`Ppu::output_pixel`] (ticket W1-05b)
/// needs the same value both for the sprite-0-hit check and the
/// BG/sprite priority mux, and re-deriving it twice would risk the two
/// call sites silently disagreeing about which sprite "won" a pixel.
#[derive(Clone, Copy)]
struct SpritePixel {
    /// 0x10-0x1F: a sprite-palette-group address, ready for
    /// [`Ppu::palette_read`].
    palette_addr: u8,
    oam_index: u8,
    /// OAM attribute bit 5 ("Priority (0: in front of background; 1:
    /// behind background)" — nesdev.org/wiki/PPU_OAM).
    behind_background: bool,
}

impl Ppu {
    /// PPUCTRL ($2000) bit 5: sprite height, 8 or 16 pixels
    /// (nesdev.org/wiki/PPU_registers: "Sprite size (0: 8x8 pixels; 1: 8x16
    /// pixels)").
    fn sprite_height(&self) -> u8 {
        if self.ctrl & 0x20 != 0 {
            16
        } else {
            8
        }
    }

    /// PPUCTRL bit 3: sprite pattern table base for 8x8-mode sprites only
    /// (nesdev.org/wiki/PPU_registers: "...ignored in 8x16 mode", where the
    /// bank instead comes from OAM byte 1's own bit 0 — see
    /// [`Ppu::load_sprite_units`]).
    fn sprite_pattern_table_base(&self) -> u16 {
        if self.ctrl & 0x08 != 0 {
            0x1000
        } else {
            0x0000
        }
    }

    /// Whether OAM byte `y` (as a candidate Y-coordinate) covers the
    /// scanline currently being evaluated (`self.scanline`) — called both
    /// from the normal phase (real Y bytes) and the buggy overflow-search
    /// phase (arbitrary OAM bytes misread as Y), which is exactly why this
    /// takes a raw `u8` rather than an already-typed sprite.
    fn sprite_in_range(&self, y: u8) -> bool {
        let scanline = self.scanline;
        let y = y as u16;
        let height = self.sprite_height() as u16;
        scanline >= y && scanline < y + height
    }

    /// Dots 65-256 of a VISIBLE scanline only (module doc: "Sprite
    /// evaluation does not happen on the pre-render scanline") — the
    /// two-phase primary-OAM scan into `secondary_oam`, the 8-sprite limit,
    /// and the buggy diagonal overflow-flag scan (module doc's numbered
    /// algorithm). Always starts at OAM index 0 (module doc's "OAMADDR
    /// reset" section explains why that's the common-case behavior of the
    /// real register too).
    pub(super) fn evaluate_sprites(&mut self) {
        self.secondary_oam = [EMPTY_EVALUATED_SPRITE; 8];
        self.secondary_oam_count = 0;

        // Phase 1 (steps 1/1a/2/2a/2b/2c): n = 0..64, copying every
        // in-range sprite's 4 bytes into the next free secondary-OAM slot,
        // until either all 64 primary sprites are consumed or exactly 8
        // have been found.
        let mut n: u16 = 0;
        while n < 64 {
            let base = (n as usize) * 4;
            let y = self.oam[base];
            if self.sprite_in_range(y) {
                let slot = self.secondary_oam_count as usize;
                self.secondary_oam[slot] = EvaluatedSprite {
                    y,
                    tile: self.oam[base + 1],
                    attr: self.oam[base + 2],
                    x: self.oam[base + 3],
                    oam_index: n as u8,
                };
                self.secondary_oam_count += 1;
            }
            n += 1;
            if self.secondary_oam_count == 8 {
                break;
            }
        }

        // Phase 2 (steps 3/3a/3b): only entered once exactly 8 sprites have
        // been found. Continues from the SAME `n` phase 1 left off at, but
        // now reads OAM[n][m] (m starting at 0) as a candidate Y without
        // writing anything (secondary OAM is full). A miss increments BOTH
        // n and m (module doc's "buggy overflow-flag scan") so `m` drifts
        // off 0 and subsequent checks misread tile/attribute/X bytes as Y.
        if self.secondary_oam_count == 8 {
            let mut m: u16 = 0;
            while n < 64 {
                let candidate = self.oam[(n as usize) * 4 + m as usize];
                if self.sprite_in_range(candidate) {
                    self.status |= STATUS_SPRITE_OVERFLOW;
                    // Nothing after the flag is set is externally
                    // observable in this ticket's scope — module doc.
                    break;
                }
                n += 1;
                m = (m + 1) & 3;
            }
        }
    }

    /// Pre-render dot 1 (unconditional — see module doc): reset both sprite
    /// buffers to their empty state, guaranteeing scanline 0 never inherits
    /// a stale active set from the previous frame's scanline 239 or from a
    /// mid-frame rendering toggle.
    pub(super) fn clear_sprite_units(&mut self) {
        self.secondary_oam = [EMPTY_EVALUATED_SPRITE; 8];
        self.secondary_oam_count = 0;
        self.active_sprites = [EMPTY_SPRITE_UNIT; 8];
        self.active_sprite_count = 0;
    }

    /// A garbage sprite standing in for an unused output-unit slot (ticket
    /// W2-03) — see [`Ppu::load_sprite_units`]'s doc for why every slot
    /// fetches, not just the occupied ones. `tile: 0xFF` matches real
    /// hardware's own `$FF`-initialized secondary OAM
    /// (nesdev.org/wiki/PPU_sprite_evaluation); `y`/`attr`/`x` are never
    /// read for a slot this stands in for (only `tile`, via the address
    /// formula below, and even then only for its bank-select bit in 8x16
    /// mode).
    const DUMMY_SPRITE: EvaluatedSprite = EvaluatedSprite {
        y: 0xFF,
        tile: 0xFF,
        attr: 0,
        x: 0xFF,
        oam_index: 0xFF,
    };

    /// Dots 257-320 of ANY rendering-enabled scanline (visible or
    /// pre-render — module doc): reset the output units at the start of
    /// the window ([`Ppu::reset_sprite_output_units`]), then, ticked
    /// dot-by-dot from `background.rs`'s `process_render_dot`
    /// ([`Ppu::run_sprite_fetch_dot`]), fetch each of the 8 sprite output
    /// units' CHR pattern bytes and latch the real ones into
    /// `active_sprites`, the render-ready units [`Ppu::sprite_pixel`]
    /// reads while drawing the scanline that follows.
    ///
    /// ## Every slot fetches, not just the occupied ones, at real hardware's
    /// own per-dot cadence (ticket W2-03)
    ///
    /// nesdev.org/wiki/PPU_rendering: "each memory access takes 2 PPU
    /// cycles to complete, and 4 are performed for each of the 8 sprites:
    /// garbage nametable byte, garbage nametable byte, pattern table tile
    /// low, pattern table tile high" — i.e. an 8-dot cadence per slot
    /// (garbage NT fetches at local dots 1-2/3-4, the two REAL pattern
    /// fetches at local dots 5-6/7-8), the same "access happens on the
    /// 2nd dot of each pair" convention `background.rs`'s BG fetch table
    /// already documents, applied here to [`Ppu::run_sprite_fetch_dot`]'s
    /// `phase` (1/3/5/7, matching that file's `fetch_phase`).
    ///
    /// This is load-bearing for MMC3's A12 IRQ counter, in two ways this
    /// ticket's fetched oracle (`mmc3_test_2`) independently exercises:
    /// - **Slots beyond `secondary_oam_count` still fetch** (garbage-tile
    ///   `$FF`, matching real hardware's own `$FF`-initialized secondary
    ///   OAM — nesdev.org/wiki/PPU_sprite_evaluation) — the mechanism that
    ///   makes exactly one PPU-A12 rising edge happen per scanline
    ///   regardless of how many sprites are actually in range
    ///   (`2-details.s` test 8, "Counter should be clocked 241 times in
    ///   PPU frame", runs with OAM deliberately cleared first).
    /// - **The real pattern fetches land on dots 261/263 of sprite 0's
    ///   slot (257 + local 4/6), not dot 257 itself** — collapsing all 8
    ///   slots' fetches into one instant at dot 257 (this function's
    ///   pre-W2-03 shape) passes every sub-ROM except
    ///   `4-scanline_timing.s`, which asserts IRQ delivery to
    ///   single-CPU-cycle resolution against real hardware's own timing
    ///   and fails exactly the way an ~3-dot-early edge predicts
    ///   (nesdev's own hedge, "the IRQ counter should decrement on PPU
    ///   cycle 260, right after the visible part of the target scanline
    ///   has ended", is consistent with this: the FIRST rising edge, if
    ///   sprites use $1xxx, lands at the local-dot-5 PT-lo fetch of slot
    ///   0, absolute dot 257+4=261, one access-pair after garbage NT #2).
    ///
    /// `active_sprite_count` still gates [`Ppu::sprite_pixel`]'s
    /// iteration, so rendered pixels are unaffected by any of this — only
    /// which dots the PPU bus is touched on changes (fetched bytes for
    /// unused slots are computed and discarded, never stored).
    pub(super) fn reset_sprite_output_units(&mut self) {
        self.active_sprites = [EMPTY_SPRITE_UNIT; 8];
        self.active_sprite_count = self.secondary_oam_count;
    }

    /// One dot of the sprite-fetch window (`dot` in `257..=320`) — see
    /// [`Ppu::reset_sprite_output_units`]'s doc for the full per-dot
    /// cadence this implements. A no-op outside the four fetch phases
    /// (1/3/5/7 of each slot's local 0-7 dot range).
    pub(super) fn run_sprite_fetch_dot(&mut self, dot: u16) {
        let local = dot - 257;
        let slot = (local / 8) as usize;
        let phase = local % 8;

        let count = self.secondary_oam_count;
        let candidate = if (slot as u8) < count {
            self.secondary_oam[slot]
        } else {
            Self::DUMMY_SPRITE
        };
        // `self.scanline` here is the SAME scanline `evaluate_sprites` used
        // as its in-range comparison a moment ago (dot 65 of this
        // scanline) — module doc: that already guarantees
        // `sprite.y <= self.scanline < sprite.y + height`, so
        // `self.scanline - sprite.y` is already the correct 0-indexed row
        // for the scanline these units render NEXT.
        //
        // That guarantee holds ONLY if `evaluate_sprites` actually ran on
        // THIS scanline. It doesn't if rendering was toggled off at dot 65
        // (skipping evaluation) and back on before dot 257 (a normal
        // mid-scanline raster-effect pattern) — `secondary_oam` is then
        // stale from whatever earlier scanline last evaluated, and
        // `sprite.y` may not satisfy the invariant against the CURRENT
        // `self.scanline` at all (row could be `>= height`, or `sprite.y >
        // self.scanline` entirely, which would underflow the subtraction
        // in `sprite_fetch_address_parts`). Re-checking with the same
        // `sprite_in_range` evaluation uses, and simply not latching a
        // sprite that fails it, turns that stale-data case into "this
        // sprite doesn't render this frame" instead of a wrong pixel
        // (leftover `EMPTY_SPRITE_UNIT` is fully transparent) or a panic —
        // same treatment `DUMMY_SPRITE`'s `y: 0xFF` gets for free (out of
        // range on any real scanline 0-239, though NOT the pre-render line
        // 261 — harmless either way, since `real` below is independently
        // gated on `slot < count`, which is always false for a dummy slot
        // regardless of what `sprite_in_range` returns for it).
        let real = (slot as u8) < count && self.sprite_in_range(candidate.y);

        match phase {
            1 | 3 => {
                // Garbage nametable fetch (nesdev.org/wiki/PPU_rendering,
                // quoted in this function's doc) — content discarded; the
                // address is A12=0 regardless of its low 12 bits, matching
                // real hardware's own NT-range (`$2000-$2FFF`) fetch here.
                let _ = self.mem_read(0x2000);
            }
            5 => {
                let (bank, tile_index, fine_row) = self.sprite_fetch_address_parts(candidate, real);
                let addr_lo = bank | ((tile_index as u16) << 4) | fine_row;
                self.sprite_pattern_lo_latch = self.mem_read(addr_lo);
            }
            7 => {
                let (bank, tile_index, fine_row) = self.sprite_fetch_address_parts(candidate, real);
                let addr_lo = bank | ((tile_index as u16) << 4) | fine_row;
                let addr_hi = addr_lo | 0x08;
                let pattern_hi = self.mem_read(addr_hi);
                if real {
                    self.active_sprites[slot] = SpriteUnit {
                        pattern_lo: self.sprite_pattern_lo_latch,
                        pattern_hi,
                        attr: candidate.attr,
                        x: candidate.x,
                        oam_index: candidate.oam_index,
                    };
                }
            }
            _ => {}
        }
    }

    /// `(bank, tile_index, fine_row)` for `candidate`'s pattern-table
    /// address — 8x16 mode: bank + top/bottom tile selection from OAM byte
    /// 1 itself (nesdev.org/wiki/PPU_OAM); 8x8 mode: PPUCTRL bit 3 selects
    /// one shared bank for every sprite. `real` must be the SAME
    /// `sprite_in_range`-gated value [`Ppu::run_sprite_fetch_dot`] computed
    /// — when `false` (garbage/stale slot), `row` is forced to 0 rather
    /// than computed from `candidate.y`, which would underflow the
    /// subtraction for `DUMMY_SPRITE`'s `y: 0xFF` on any real scanline.
    fn sprite_fetch_address_parts(&self, candidate: EvaluatedSprite, real: bool) -> (u16, u8, u16) {
        let height = self.sprite_height();
        let row = if real {
            let mut row = self.scanline - candidate.y as u16;
            if candidate.attr & 0x80 != 0 {
                row = height as u16 - 1 - row;
            }
            row
        } else {
            0
        };

        if height == 16 {
            let bank = if candidate.tile & 0x01 != 0 {
                0x1000u16
            } else {
                0x0000u16
            };
            if row < 8 {
                (bank, candidate.tile & 0xFE, row)
            } else {
                (bank, (candidate.tile & 0xFE) + 1, row - 8)
            }
        } else {
            (self.sprite_pattern_table_base(), candidate.tile, row)
        }
    }

    /// Convenience wrapper for tests that don't drive the full per-dot
    /// tick loop (`ppu/tests/sprite_evaluation.rs`): runs the entire dot
    /// 257-320 sprite-fetch window in one call. Identical FINAL
    /// `active_sprites` content to ticking through it dot-by-dot (the
    /// fetch/gating logic is exactly [`Ppu::run_sprite_fetch_dot`],
    /// called here for every dot in the window instead of spread across
    /// real `Ppu::tick` calls) — only the PPU-bus-access TIMING differs,
    /// which is why the real tick loop (`background.rs`'s
    /// `process_render_dot`) does NOT use this and calls
    /// [`Ppu::reset_sprite_output_units`]/[`Ppu::run_sprite_fetch_dot`]
    /// directly instead (ticket W2-03: the collapsed-to-one-instant timing
    /// this function has is exactly what made `mmc3_test_2/
    /// 4-scanline_timing`'s cycle-exact assertions fail before this split).
    #[cfg(test)]
    pub(super) fn load_sprite_units(&mut self) {
        self.reset_sprite_output_units();
        for dot in 257..=320u16 {
            self.run_sprite_fetch_dot(dot);
        }
    }

    /// Resolve the sprite-layer contribution at screen x (0-255) from the
    /// currently active sprite units. OAM-order priority ("lower OAM index
    /// wins" — nesdev.org/wiki/PPU_sprite_evaluation) falls out for free:
    /// `active_sprites` is in ascending-`n` (evaluation) order already, so
    /// the first opaque match in iteration order IS the lowest-OAM-index
    /// winner. `None` when no active sprite covers `x` with an opaque
    /// (nonzero) pattern value there.
    fn sprite_pixel(&self, x: u16) -> Option<SpritePixel> {
        if !self.mask_show_sprites() {
            return None;
        }
        // $2001 bit 2: "Show sprites in leftmost 8 pixels" — forced fully
        // transparent when clear, regardless of any sprite's own data.
        if x < 8 && !self.mask_show_sprites_left8() {
            return None;
        }
        for sprite in self
            .active_sprites
            .iter()
            .take(self.active_sprite_count as usize)
        {
            let sprite_x = sprite.x as u16;
            if x < sprite_x || x >= sprite_x + 8 {
                continue;
            }
            let mut col = x - sprite_x; // 0..=7, left edge of the sprite
            if sprite.attr & 0x40 != 0 {
                col = 7 - col; // horizontal flip
            }
            let bit = 7 - col; // MSB of the pattern byte is the leftmost pixel
            let p0 = (sprite.pattern_lo >> bit) & 1;
            let p1 = (sprite.pattern_hi >> bit) & 1;
            let pattern = (p1 << 1) | p0;
            if pattern == 0 {
                // Transparent at this column: a LOWER-priority (later in
                // iteration order) sprite may still show through here.
                continue;
            }
            let palette_group = sprite.attr & 0x03;
            return Some(SpritePixel {
                palette_addr: 0x10 | (palette_group << 2) | pattern,
                oam_index: sprite.oam_index,
                behind_background: sprite.attr & 0x20 != 0,
            });
        }
        None
    }

    /// Write `line_buffer[x]` for visible-scanline dot `x + 1`: combines
    /// [`Ppu::background_pixel`] and [`Ppu::sprite_pixel`] per nesdev.org/
    /// wiki/PPU_rendering's "Priority multiplexer decision table" (BG
    /// pixel x sprite pixel x sprite priority bit -> output):
    ///
    /// | BG    | sprite | priority | output |
    /// |-------|--------|----------|--------|
    /// | 0     | 0      | X        | backdrop ($3F00) |
    /// | 0     | 1-3    | X        | sprite |
    /// | 1-3   | 0      | X        | BG |
    /// | 1-3   | 1-3    | 0        | sprite |
    /// | 1-3   | 1-3    | 1        | BG |
    ///
    /// (0/1-3 here are the 2-bit *pattern* value — transparent vs. opaque —
    /// not the resolved palette index; `crate::ppu`'s module doc's
    /// `palette_index` semantics commitment still applies to the value
    /// finally written.) `dropped_by_limit` is always `false` — module
    /// doc's "`dropped_by_limit`" section: a limit-dropped sprite is
    /// already excluded from `active_sprites` and so never reaches this
    /// function to be flagged at all.
    pub(super) fn output_pixel(&mut self, x: u16) {
        let (bg_addr, bg_layer) = self.background_pixel(x);
        let sprite = self.sprite_pixel(x);

        // Sprite-0 hit (ticket W1-05b, nesdev.org/wiki/PPU_OAM "Sprite zero
        // hits" — see this module's doc for the full condition-by-condition
        // derivation): opaque BG (`bg_addr.is_some()`, already left-8/
        // rendering-gated by `background_pixel`) AND sprite 0 is the opaque
        // winner here (already left-8/rendering-gated by `sprite_pixel`,
        // and priority-blind by construction), except at x=255.
        if x != 255 && bg_addr.is_some() && matches!(sprite, Some(s) if s.oam_index == 0) {
            self.status |= STATUS_SPRITE0_HIT;
        }

        let (palette_addr, layer, sprite_id, priority) = match (bg_addr, sprite) {
            (None, None) => (0u16, bg_layer, None, 0u8),
            (Some(addr), None) => (addr as u16, bg_layer, None, 0),
            (None, Some(s)) => (
                s.palette_addr as u16,
                PixelLayer::Sprite,
                Some(s.oam_index),
                u8::from(s.behind_background),
            ),
            (Some(addr), Some(s)) => {
                if s.behind_background {
                    (addr as u16, bg_layer, None, 0)
                } else {
                    (
                        s.palette_addr as u16,
                        PixelLayer::Sprite,
                        Some(s.oam_index),
                        u8::from(s.behind_background),
                    )
                }
            }
        };

        let palette_index = self.palette_read(palette_addr) & 0x3F;
        self.line_buffer[x as usize] = PpuPixel {
            palette_index,
            layer,
            sprite_id,
            priority,
            dropped_by_limit: false,
        };
    }
}

#[cfg(test)]
mod tests {
    use crate::ppu::tests::test_ppu;

    #[test]
    fn sprite_height_reads_ppuctrl_bit5() {
        let mut ppu = test_ppu();
        assert_eq!(ppu.sprite_height(), 8);
        ppu.ctrl = 0x20;
        assert_eq!(ppu.sprite_height(), 16);
    }

    #[test]
    fn sprite_in_range_uses_the_current_scanline_and_height() {
        let mut ppu = test_ppu();
        ppu.scanline = 10;
        assert!(!ppu.sprite_in_range(11), "sprite starts after this line");
        assert!(ppu.sprite_in_range(10), "sprite's first (top) row");
        assert!(
            ppu.sprite_in_range(3),
            "row 7 of an 8px sprite starting at y=3"
        );
        assert!(
            !ppu.sprite_in_range(2),
            "row 8 doesn't exist for an 8px sprite"
        );

        ppu.ctrl = 0x20; // 8x16
        ppu.scanline = 20;
        assert!(
            ppu.sprite_in_range(5),
            "row 15 of a 16px sprite starting at y=5 (the last valid row)"
        );
        assert!(
            !ppu.sprite_in_range(4),
            "row 16 doesn't exist even for a 16px sprite"
        );
    }
}
