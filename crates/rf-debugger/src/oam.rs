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

/// The hardware sprites-per-scanline cap (nesdev: the PPU evaluates OAM in
/// ascending slot order and keeps the first 8 that cover the line).
pub const SPRITES_PER_SCANLINE_LIMIT: usize = 8;

/// The slots the 8-per-scanline limit DROPPED on `scanline` — everything
/// [`scanline_occupancy`] found past the first eight (ticket W4-06c,
/// FR-DBG-006).
///
/// Derived from `scanline_occupancy` rather than re-scanning, so the
/// panel and the occupancy bar can never disagree about which sprites
/// were on a line: the dropped set is by construction the tail of the
/// same list, in the same evaluation order hardware uses.
///
/// Empty for a line under the limit — which is the common case, and is
/// why a caller should render "no drops" rather than an empty row.
#[must_use]
pub fn dropped_by_limit(
    sprites: &[SpriteEntry; OAM_ENTRY_COUNT],
    scanline: u16,
    sprite_h: u8,
) -> Vec<u8> {
    let mut covering = scanline_occupancy(sprites, scanline, sprite_h);
    if covering.len() <= SPRITES_PER_SCANLINE_LIMIT {
        return Vec::new();
    }
    covering.split_off(SPRITES_PER_SCANLINE_LIMIT)
}

/// Which of a sprite's fields moved between two frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChangedFields {
    pub y: bool,
    pub tile: bool,
    pub x: bool,
    /// Palette, priority or either flip — the attribute byte as a whole.
    pub attrs: bool,
}

impl ChangedFields {
    #[must_use]
    pub fn any(self) -> bool {
        self.y || self.tile || self.x || self.attrs
    }

    /// A short human label like `"x, tile"`, for the panel.
    #[must_use]
    pub fn summary(self) -> String {
        let mut parts = Vec::new();
        if self.y {
            parts.push("y");
        }
        if self.x {
            parts.push("x");
        }
        if self.tile {
            parts.push("tile");
        }
        if self.attrs {
            parts.push("attrs");
        }
        parts.join(", ")
    }
}

/// One sprite that changed between two frames (FR-DBG-006).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpriteDelta {
    pub index: u8,
    pub before: SpriteEntry,
    pub after: SpriteEntry,
    pub fields: ChangedFields,
}

/// Which sprites changed between `previous` and `current` OAM.
///
/// **Both inputs must be the PPU's own OAM** (`rf_nes::Ppu::oam`, which
/// reaches this crate as `core_thread::FrameMsg::oam`) — never the
/// CPU-side shadow at `$0200`. W2-10a's gem defect was exactly the two
/// disagreeing: an OAM DMA straddling the dots-257-320 `OAMADDR` reset
/// window scrambled the copy, so a panel reading the shadow would have
/// shown a picture the PPU never had. A debugger that lies about what the
/// hardware did is worse than no debugger.
///
/// Returns only changed slots, in ascending slot order. A frame where
/// nothing moved returns empty, which the panel reports as "no changes"
/// rather than as a blank list.
#[must_use]
pub fn diff_oam(previous: &[u8; 256], current: &[u8; 256]) -> Vec<SpriteDelta> {
    let before = decode_oam(previous);
    let after = decode_oam(current);
    let mut out = Vec::new();
    for i in 0..OAM_ENTRY_COUNT {
        let (b, a) = (before[i], after[i]);
        let fields = ChangedFields {
            y: b.y != a.y,
            tile: b.tile != a.tile,
            x: b.x != a.x,
            attrs: b.palette != a.palette
                || b.priority_behind_bg != a.priority_behind_bg
                || b.flip_h != a.flip_h
                || b.flip_v != a.flip_v,
        };
        if fields.any() {
            out.push(SpriteDelta {
                index: a.index,
                before: b,
                after: a,
                fields,
            });
        }
    }
    out
}

#[cfg(test)]
mod diff_tests {
    use super::*;

    fn oam_with(entries: &[(usize, [u8; 4])]) -> [u8; 256] {
        // 0xFF Y = hardware's "off screen, never evaluated" convention,
        // so unset slots do not accidentally sit at y=0 (a perfectly
        // valid ON-screen position) and pollute occupancy counts.
        let mut oam = [0u8; 256];
        for slot in 0..OAM_ENTRY_COUNT {
            oam[slot * 4] = 0xFF;
        }
        for (slot, bytes) in entries {
            oam[slot * 4..slot * 4 + 4].copy_from_slice(bytes);
        }
        oam
    }

    #[test]
    fn an_unchanged_frame_produces_no_deltas() {
        let oam = oam_with(&[(0, [10, 1, 0, 20])]);
        assert!(diff_oam(&oam, &oam).is_empty());
    }

    /// Each field is detected independently — a diff that only noticed
    /// "something changed" would be far less useful, and one that missed
    /// the attribute byte would hide palette and flip changes entirely.
    #[test]
    fn each_field_is_detected_separately() {
        let base = oam_with(&[(3, [10, 1, 0x00, 20])]);

        let moved = oam_with(&[(3, [10, 1, 0x00, 44])]);
        let d = diff_oam(&base, &moved);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].index, 3);
        assert_eq!(
            d[0].fields,
            ChangedFields {
                x: true,
                ..Default::default()
            }
        );
        assert_eq!(d[0].fields.summary(), "x");
        assert_eq!(d[0].before.x, 20);
        assert_eq!(d[0].after.x, 44, "the delta must carry BOTH sides");

        let retiled = oam_with(&[(3, [10, 9, 0x00, 20])]);
        assert_eq!(diff_oam(&base, &retiled)[0].fields.summary(), "tile");

        let dropped_down = oam_with(&[(3, [77, 1, 0x00, 20])]);
        assert_eq!(diff_oam(&base, &dropped_down)[0].fields.summary(), "y");

        // Attribute byte: palette, priority and both flips all route to
        // `attrs`, so a flip change is not silently invisible.
        for attr in [0x01u8, 0x20, 0x40, 0x80] {
            let changed = oam_with(&[(3, [10, 1, attr, 20])]);
            let d = diff_oam(&base, &changed);
            assert_eq!(
                d.len(),
                1,
                "attribute byte {attr:#04x} must register as a change"
            );
            assert!(d[0].fields.attrs, "{attr:#04x}");
        }
    }

    #[test]
    fn several_changed_sprites_come_back_in_slot_order() {
        let a = oam_with(&[(1, [10, 0, 0, 10]), (5, [20, 0, 0, 20])]);
        let b = oam_with(&[(1, [11, 0, 0, 10]), (5, [20, 0, 0, 21])]);
        let d = diff_oam(&a, &b);
        assert_eq!(d.iter().map(|x| x.index).collect::<Vec<_>>(), vec![1, 5]);
    }

    /// FR-DBG-006's other half: which sprites the 8-per-scanline limit
    /// dropped. Nine sprites on one line means exactly one is dropped —
    /// and it must be the NINTH in evaluation order, not an arbitrary one.
    #[test]
    fn the_ninth_sprite_on_a_line_is_the_one_reported_as_dropped() {
        let entries: Vec<(usize, [u8; 4])> =
            (0..9).map(|i| (i, [50u8, 0, 0, (i as u8) * 8])).collect();
        let sprites = decode_oam(&oam_with(&entries));

        // Scanline 51 = y(50) + 1, the first line these sprites cover.
        let covering = scanline_occupancy(&sprites, 51, 8);
        assert_eq!(covering.len(), 9, "all nine cover the line");

        let dropped = dropped_by_limit(&sprites, 51, 8);
        assert_eq!(
            dropped,
            vec![8],
            "hardware keeps the first 8 in slot order, so slot 8 is dropped"
        );

        // Eight is not over the limit — the boundary itself, asserted so
        // an off-by-one cannot pass.
        let eight: Vec<(usize, [u8; 4])> =
            (0..8).map(|i| (i, [50u8, 0, 0, (i as u8) * 8])).collect();
        assert!(dropped_by_limit(&decode_oam(&oam_with(&eight)), 51, 8).is_empty());
    }

    /// A line nobody is on drops nothing — the common case, and the one
    /// that would make a buggy implementation look fine on a busy frame.
    #[test]
    fn an_empty_scanline_drops_nothing() {
        let sprites = decode_oam(&oam_with(&[(0, [50, 0, 0, 0])]));
        assert!(dropped_by_limit(&sprites, 200, 8).is_empty());
    }
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
