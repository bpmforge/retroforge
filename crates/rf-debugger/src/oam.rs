//! OAM/sprite viewer data provider (FR-DBG-001, DEBUGGER.md §3 "OAM/
//! Sprite" row: "64 entries, per-scanline occupancy bar (8-limit visual),
//! `dropped_by_limit` highlight").
//!
//! ## Data source — the one viewer with a real, live path (unlike
//! `crate::palette`/`crate::nametable`)
//!
//! `rf_nes::system::NesBus::oam(&self) -> &[u8; 256]` is already `pub`
//! (verified against source, ticket W4-06a pre-flight) and forwarded all
//! the way to `retroforge::stepper::EmuStepper::oam()` — also already
//! `pub`. `crate::core_thread`'s `FrameMsg` carries a copy of it every
//! frame (same "same-frame extraction" shape ticket W3-03 already used for
//! `bg_rgba`/`sprite_rgba`), so [`decode_oam`] below has a genuine live
//! producer, unlike the CGRAM/VRAM viewers.
//!
//! ## Byte layout ([nesdev.org/wiki/PPU_OAM](https://www.nesdev.org/wiki/PPU_OAM))
//!
//! 64 sprites x 4 bytes: byte 0 = Y position (of the sprite's top, minus
//! 1 — this module reports the raw stored byte, not the display-adjusted
//! value, matching `rf_core_api::PpuPixel::palette_index`'s "raw stored
//! byte, not a resolved display value" precedent), byte 1 = tile index,
//! byte 2 = attributes (bits 0-1 palette, bit 5 priority-behind-background,
//! bit 6 flip horizontal, bit 7 flip vertical), byte 3 = X position.
//!
//! ## `dropped_by_limit` highlight — deliberately NOT provided here
//!
//! DEBUGGER.md's OAM row also asks for a `dropped_by_limit` highlight.
//! `rf_core_api::PpuPixel::dropped_by_limit`'s own doc is explicit: **"On
//! the NES path this is always `false`"** — the sprite-limit-bypass
//! overlay (W3-05a) records dropped sprites on a separate channel
//! (`OverlayPixel`/`CoreSink::overlay_scanline`) precisely so it never has
//! to touch this always-accuracy-exact field. A helper here that filtered
//! `PpuPixel::dropped_by_limit` would compile, pass a synthetic test built
//! to set the flag by hand, and then return an empty `Vec` against every
//! real frame this workspace's NES core ever produces — exactly the
//! "exercises a path without discriminating it" vacuity class this
//! ticket's brief warns about. Not built; would need the `OverlayPixel`
//! stream (a different, currently UI-only-consumed channel) as its real
//! input instead.

/// Bytes per OAM entry.
const OAM_ENTRY_BYTES: usize = 4;
/// Sprite entries in the full 256-byte OAM.
pub const OAM_ENTRY_COUNT: usize = 64;

/// One decoded OAM entry — see module doc for the byte layout each field
/// comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpriteEntry {
    /// OAM slot 0-63 (the physical index — `rf_core_api::PpuPixel::
    /// sprite_id`'s counterpart, distinct from any logical/game-side
    /// identity `rf_enhance::SpriteHistorian` tracks across slot rotation).
    pub index: u8,
    pub y: u8,
    pub tile: u8,
    pub x: u8,
    pub palette: u8,
    pub priority_behind_bg: bool,
    pub flip_h: bool,
    pub flip_v: bool,
}

/// Decode the full 256-byte OAM into its 64 sprite entries, in physical
/// slot order.
#[must_use]
pub fn decode_oam(oam: &[u8; 256]) -> [SpriteEntry; OAM_ENTRY_COUNT] {
    let mut out = [SpriteEntry {
        index: 0,
        y: 0,
        tile: 0,
        x: 0,
        palette: 0,
        priority_behind_bg: false,
        flip_h: false,
        flip_v: false,
    }; OAM_ENTRY_COUNT];
    for (i, entry) in out.iter_mut().enumerate() {
        let base = i * OAM_ENTRY_BYTES;
        let y = oam[base];
        let tile = oam[base + 1];
        let attrs = oam[base + 2];
        let x = oam[base + 3];
        *entry = SpriteEntry {
            index: i as u8,
            y,
            tile,
            x,
            palette: attrs & 0x03,
            priority_behind_bg: attrs & 0x20 != 0,
            flip_h: attrs & 0x40 != 0,
            flip_v: attrs & 0x80 != 0,
        };
    }
    out
}

/// Physical OAM slots whose sprite covers display scanline `scanline`
/// (DEBUGGER.md's "per-scanline occupancy bar (8-limit visual)"), in
/// ascending slot order — the same evaluation order real hardware (and
/// this project's PPU, `rf-nes/src/ppu/sprites.rs`'s own doc) scans in, so
/// "the first 8 returned" is meaningfully "the 8 hardware would have
/// picked" for a viewer overlaying the accuracy 8-sprite cap. `sprite_h`
/// is 8 or 16 (`$2000` bit 5, 8x8 vs 8x16 mode) — this function has no PPU
/// register access of its own, so the caller supplies it.
///
/// A sprite at raw `y` covers scanlines `y+1 ..= y+sprite_h`
/// ([nesdev.org/wiki/PPU_OAM](https://www.nesdev.org/wiki/PPU_OAM): "Y
/// position of top of sprite ... Sprite data is delayed by one scanline;
/// you must subtract 1 from the sprite's Y coordinate before writing it
/// here"). `y == 0xFF` (and any `y` that would place every covered row at
/// or past the visible 240, i.e. `y >= 240`) never renders — matches
/// hardware's own off-screen-Y convention (`sprites.rs`'s own evaluation
/// doc).
#[must_use]
pub fn scanline_occupancy(
    sprites: &[SpriteEntry; OAM_ENTRY_COUNT],
    scanline: u16,
    sprite_h: u8,
) -> Vec<u8> {
    sprites
        .iter()
        .filter(|s| {
            if s.y >= 240 {
                return false;
            }
            let top = u16::from(s.y) + 1;
            let bottom = top + u16::from(sprite_h) - 1;
            (top..=bottom).contains(&scanline)
        })
        .map(|s| s.index)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every OTHER slot's Y is `0xFF` (hardware's own "off screen, never
    /// evaluated" convention) rather than left at the all-zero default —
    /// `y == 0` is a perfectly valid on-screen sprite (covers scanlines
    /// `1..=y+sprite_h`), so leaving 63 slots at Y=0 would silently make
    /// them collide with whatever scanline a `scanline_occupancy` test
    /// happens to probe near the top of the frame.
    fn synthetic_oam_with_one_sprite(slot: usize, y: u8, tile: u8, attrs: u8, x: u8) -> [u8; 256] {
        let mut oam = [0xFFu8; 256];
        for entry in 0..OAM_ENTRY_COUNT {
            let base = entry * OAM_ENTRY_BYTES;
            oam[base + 1] = 0;
            oam[base + 2] = 0;
            oam[base + 3] = 0;
        }
        let base = slot * OAM_ENTRY_BYTES;
        oam[base] = y;
        oam[base + 1] = tile;
        oam[base + 2] = attrs;
        oam[base + 3] = x;
        oam
    }

    #[test]
    fn decode_oam_places_a_known_sprite_at_its_slot_with_every_field_correct() {
        // Slot 5: Y=100, tile=0x42, attrs = palette 2, priority behind bg,
        // flip both = 0b1110_0010, X=200.
        let attrs = 0b1110_0010;
        let oam = synthetic_oam_with_one_sprite(5, 100, 0x42, attrs, 200);
        let sprites = decode_oam(&oam);

        let s = sprites[5];
        assert_eq!(s.index, 5);
        assert_eq!(s.y, 100);
        assert_eq!(s.tile, 0x42);
        assert_eq!(s.x, 200);
        assert_eq!(s.palette, 2);
        assert!(s.priority_behind_bg);
        assert!(s.flip_h);
        assert!(s.flip_v);

        // A neighboring, untouched slot must decode its own (off-screen
        // placeholder) bytes, not leak slot 5's — `y` comes from the
        // helper's 0xFF off-screen fill (its own doc), everything else
        // stays zeroed.
        let neighbor = sprites[4];
        assert_eq!(neighbor.y, 0xFF);
        assert_eq!(neighbor.tile, 0);
        assert!(!neighbor.flip_h);
    }

    /// Ticket W4-06a mutation finding: the test above sets bits 6 AND 7
    /// together (`0b1110_0010`), so a mutation that swapped which bit maps
    /// to `flip_h` vs `flip_v` would pass it unnoticed (both end up
    /// `true` either way) — verified: swapping `attrs & 0x40`/`attrs &
    /// 0x80` between the two fields left every other test in this module
    /// green. This test sets each flip bit independently to close that
    /// gap.
    #[test]
    fn decode_oam_distinguishes_flip_h_from_flip_v_when_only_one_bit_is_set() {
        let h_only = synthetic_oam_with_one_sprite(0, 0, 0, 0b0100_0000, 0);
        let s = decode_oam(&h_only)[0];
        assert!(s.flip_h, "bit 6 alone must set flip_h");
        assert!(!s.flip_v, "bit 6 alone must NOT set flip_v");

        let v_only = synthetic_oam_with_one_sprite(0, 0, 0, 0b1000_0000, 0);
        let s = decode_oam(&v_only)[0];
        assert!(!s.flip_h, "bit 7 alone must NOT set flip_h");
        assert!(s.flip_v, "bit 7 alone must set flip_v");
    }

    #[test]
    fn decode_oam_reads_attribute_bits_that_must_stay_off_as_off() {
        // Palette 0, no priority/flip flags at all.
        let oam = synthetic_oam_with_one_sprite(0, 10, 1, 0b0000_0000, 0);
        let s = decode_oam(&oam)[0];
        assert_eq!(s.palette, 0);
        assert!(!s.priority_behind_bg);
        assert!(!s.flip_h);
        assert!(!s.flip_v);
    }

    #[test]
    fn scanline_occupancy_covers_exactly_the_sprites_own_row_range_in_8x8_mode() {
        // Y=9 -> covers scanlines 10..=17 in 8x8 mode (top = y+1 = 10).
        let oam = synthetic_oam_with_one_sprite(3, 9, 0, 0, 0);
        let sprites = decode_oam(&oam);

        assert_eq!(scanline_occupancy(&sprites, 9, 8), Vec::<u8>::new());
        assert_eq!(scanline_occupancy(&sprites, 10, 8), vec![3]);
        assert_eq!(scanline_occupancy(&sprites, 17, 8), vec![3]);
        assert_eq!(scanline_occupancy(&sprites, 18, 8), Vec::<u8>::new());
    }

    #[test]
    fn scanline_occupancy_covers_16_rows_in_8x16_mode() {
        let oam = synthetic_oam_with_one_sprite(0, 9, 0, 0, 0);
        let sprites = decode_oam(&oam);
        assert_eq!(scanline_occupancy(&sprites, 10, 16), vec![0]);
        assert_eq!(scanline_occupancy(&sprites, 25, 16), vec![0]);
        assert_eq!(scanline_occupancy(&sprites, 26, 16), Vec::<u8>::new());
    }

    #[test]
    fn scanline_occupancy_excludes_off_screen_y_sprites() {
        let oam = synthetic_oam_with_one_sprite(0, 0xFF, 0, 0, 0);
        let sprites = decode_oam(&oam);
        for line in [0u16, 100, 239] {
            assert!(scanline_occupancy(&sprites, line, 8).is_empty());
        }
    }

    #[test]
    fn scanline_occupancy_reports_multiple_overlapping_sprites_in_slot_order() {
        let mut oam = [0u8; 256];
        // Slot 1 and slot 6, both covering scanline 50 in 8x8 mode.
        oam[OAM_ENTRY_BYTES] = 45;
        oam[6 * OAM_ENTRY_BYTES] = 44;
        let sprites = decode_oam(&oam);
        assert_eq!(scanline_occupancy(&sprites, 50, 8), vec![1, 6]);
    }
}
