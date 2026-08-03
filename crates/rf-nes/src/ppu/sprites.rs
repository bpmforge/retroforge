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
use super::{EvaluatedSprite, Ppu, SpriteUnit, EMPTY_EVALUATED_SPRITE, EMPTY_SPRITE_UNIT};
use crate::ppu::STATUS_SPRITE_OVERFLOW;
use rf_core_api::{PixelLayer, PpuPixel};

/// The resolved sprite-layer contribution at one screen x, from
/// [`Ppu::sprite_pixel`] — the winning (highest-priority, first opaque)
/// active sprite, if any.
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

    /// Dot 257 of ANY rendering-enabled scanline (visible or pre-render —
    /// module doc): fetch each `secondary_oam` sprite's CHR pattern bytes
    /// and latch them into `active_sprites`, the render-ready units
    /// [`Ppu::sprite_pixel`] reads while drawing the scanline that follows.
    pub(super) fn load_sprite_units(&mut self) {
        // Copy out first (`EvaluatedSprite` is `Copy`): the loop below
        // calls `self.mem_read`, which needs `&self`, while also writing
        // `self.active_sprites` — working from an independent local copy
        // sidesteps any question of overlapping borrows entirely.
        let secondary = self.secondary_oam;
        let count = self.secondary_oam_count;
        let height = self.sprite_height();

        self.active_sprites = [EMPTY_SPRITE_UNIT; 8];
        self.active_sprite_count = count;

        for (i, sprite) in secondary.iter().enumerate().take(count as usize) {
            // `self.scanline` here is the SAME scanline `evaluate_sprites`
            // used as its in-range comparison a moment ago (dot 65 of this
            // scanline) — module doc: that already guarantees
            // `sprite.y <= self.scanline < sprite.y + height`, so this is
            // already the correct 0-indexed row for the scanline these
            // units render NEXT.
            let mut row = self.scanline - sprite.y as u16;
            let flip_v = sprite.attr & 0x80 != 0;
            if flip_v {
                row = height as u16 - 1 - row;
            }

            // 8x16 mode: bank + top/bottom tile selection from OAM byte 1
            // itself (nesdev.org/wiki/PPU_OAM); 8x8 mode: PPUCTRL bit 3
            // selects one shared bank for every sprite.
            let (bank, tile_index, fine_row) = if height == 16 {
                let bank = if sprite.tile & 0x01 != 0 {
                    0x1000u16
                } else {
                    0x0000u16
                };
                if row < 8 {
                    (bank, sprite.tile & 0xFE, row)
                } else {
                    (bank, (sprite.tile & 0xFE) + 1, row - 8)
                }
            } else {
                (self.sprite_pattern_table_base(), sprite.tile, row)
            };

            let addr_lo = bank | ((tile_index as u16) << 4) | fine_row;
            let addr_hi = addr_lo | 0x08;
            self.active_sprites[i] = SpriteUnit {
                pattern_lo: self.mem_read(addr_lo),
                pattern_hi: self.mem_read(addr_hi),
                attr: sprite.attr,
                x: sprite.x,
                oam_index: sprite.oam_index,
            };
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
