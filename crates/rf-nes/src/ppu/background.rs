//! Background fetch pipeline: the per-dot NT/AT/pattern-table fetch cadence,
//! the 16-bit shift registers, and per-pixel palette-address resolution.
//!
//! ## The dot table (nesdev.org/wiki/PPU_rendering)
//!
//! "Each memory access takes 2 PPU cycles to complete, and 4 must be
//! performed per tile: Nametable byte, Attribute table byte, Pattern table
//! tile low, Pattern table tile high." That is 8 dots per tile: this module
//! treats the first dot of each 2-dot access as address setup (no bus
//! activity modeled — nothing in this crate observes the intermediate
//! address-only state) and the second as the actual fetch:
//!
//! | dot mod 8 (1-based, 1..=8) | phase | action |
//! |---|---|---|
//! | 1 | 0 | NT address setup |
//! | 2 | 1 | NT byte fetched -> `nt_latch` |
//! | 3 | 2 | AT address setup |
//! | 4 | 3 | AT byte fetched, palette-quadrant bits extracted -> `at_latch` |
//! | 5 | 4 | pattern-low address setup |
//! | 6 | 5 | pattern-low byte fetched -> `pt_lo_latch` |
//! | 7 | 6 | pattern-high address setup |
//! | 8 | 7 | pattern-high byte fetched -> `pt_hi_latch` |
//!
//! This 8-dot cadence runs during dots 1-256 (32 tiles for the current
//! line) and dots 321-336 (2 tiles prefetched for the *next* line) —
//! nesdev's own text for the second window: "the first two tiles for the
//! next scanline are fetched, and loaded into the shift registers." Dots
//! 257-320 are the sprite-fetch window (`sprites.rs`, ticket W1-05a;
//! nothing BG-related happens there) and 337-340 are two documented
//! "unused nametable fetches" (nesdev: "the purpose for this is unknown")
//! — neither is modeled.
//!
//! ## Reload and coarse-X-increment dots
//!
//! nesdev states verbatim: "The shifters reload at dots 9, 17, 25, ...,
//! 257." Reload of tile N's data into the shift registers necessarily
//! happens at the first dot of tile N+1's fetch (the same 8-dot-later
//! pattern extends into the 321-336 prefetch window: 329 and 337). For the
//! reload at dot 9 to fetch the *correct* nametable byte for tile N+1, `v`'s
//! coarse X must already have advanced past tile N by then — so this
//! module increments coarse X one dot earlier, immediately after each
//! tile's pattern-high fetch (dots 8, 16, ..., 256, 328, 336), derived from
//! that same reload fact rather than a separately-quoted increment-dot
//! list.
//!
//! ## Attribute quadrant selection (derived, not a literal nesdev quote)
//!
//! The attribute address formula (`0x23C0 | (v & 0x0C00) | ((v >> 4) &
//! 0x38) | ((v >> 2) & 0x07)`) is quoted directly from
//! nesdev.org/wiki/PPU_scrolling's "Tile and attribute fetching" section.
//! That section does not give the shift used to pick 2 of the attribute
//! byte's 8 bits for a given tile; this module derives it instead: each
//! attribute byte covers a 32x32 pixel (4x4 tile) area split into four
//! 16x16 pixel (2x2 tile) quadrants, so the quadrant is selected by bit 1 of
//! coarse X and bit 1 of coarse Y (both extracted from `v`), each quadrant
//! occupying 2 bits of the byte: `shift = (row_quadrant << 1 |
//! col_quadrant) * 2`.
use super::Ppu;
use rf_core_api::PixelLayer;

/// Which 2-dot memory access phase (0-7) `dot` falls in, if any — `None`
/// outside the two BG-fetch windows (1..=256, 321..=336). See this module's
/// doc table.
fn fetch_phase(dot: u16) -> Option<u8> {
    if (1..=256).contains(&dot) {
        Some(((dot - 1) % 8) as u8)
    } else if (321..=336).contains(&dot) {
        Some(((dot - 321) % 8) as u8)
    } else {
        None
    }
}

fn is_fetch_window(dot: u16) -> bool {
    (1..=256).contains(&dot) || (321..=336).contains(&dot)
}

/// Coarse-X-increment dots: 8, 16, ..., 256, 328, 336 (see module doc).
fn is_coarse_x_increment_dot(dot: u16) -> bool {
    is_fetch_window(dot) && dot.is_multiple_of(8)
}

/// Shift-register reload dots: 9, 17, ..., 257, 329, 337 — one dot after
/// each coarse-X-increment dot (see module doc).
fn is_reload_dot(dot: u16) -> bool {
    dot >= 9 && dot % 8 == 1 && is_fetch_window(dot - 1)
}

/// Extract the 2-bit palette-quadrant value for the tile `v` currently
/// points at, out of one already-fetched attribute byte (see module doc's
/// "Attribute quadrant selection").
fn attribute_quadrant_bits(attr_byte: u8, v: u16) -> u8 {
    let coarse_x = v & 0x1F;
    let coarse_y = (v >> 5) & 0x1F;
    let col_quadrant = (coarse_x >> 1) & 1;
    let row_quadrant = (coarse_y >> 1) & 1;
    let shift = (row_quadrant << 1 | col_quadrant) * 2;
    (attr_byte >> shift) & 0x03
}

impl Ppu {
    /// Process one dot of a visible (`is_visible = true`) or pre-render
    /// (`is_visible = false`) scanline. See [`Ppu::tick`]'s doc for the
    /// overall dot-diagram summary this implements.
    pub(super) fn process_render_dot(&mut self, is_visible: bool) {
        let dot = self.dot;
        if self.rendering_enabled() {
            if (2..=257).contains(&dot) || (322..=337).contains(&dot) {
                self.shift_left();
            }
            if let Some(phase) = fetch_phase(dot) {
                self.run_fetch_phase(phase);
            }
            if is_reload_dot(dot) {
                self.reload_shift_registers();
            }
            if is_coarse_x_increment_dot(dot) {
                self.increment_coarse_x();
            }
            if dot == 256 {
                self.increment_y();
            }
            if dot == 257 {
                self.copy_horizontal();
            }
            if !is_visible && (280..=304).contains(&dot) {
                self.copy_vertical();
            }
            // ---- sprites (ticket W1-05a; see `sprites.rs` module doc) ----
            // Dots 65-256, visible scanlines only: nesdev.org/wiki/
            // PPU_sprite_evaluation, "Sprite evaluation does not happen on
            // the pre-render scanline."
            if is_visible && dot == 65 {
                self.evaluate_sprites();
            }
            // "OAMADDR is set to 0 during each of ticks 257-320... of the
            // pre-render and visible scanlines" (nesdev.org/wiki/
            // PPU_registers) -- both line kinds, so re-checked every dot in
            // range rather than once.
            if (257..=320).contains(&dot) {
                self.oam_addr = 0;
            }
            // Same "sprite tile loading interval" window's other half:
            // latch this scanline's evaluated sprites into render-ready
            // units for the NEXT scanline.
            if dot == 257 {
                self.load_sprite_units();
            }
        }
        // Pre-render dot 1: unconditional, NOT gated on `rendering_enabled`
        // above, so a mid-frame rendering toggle can never leave a stale
        // sprite set behind for scanline 0 -- see `sprites.rs` module doc's
        // "one-scanline pipeline delay" section.
        if !is_visible && dot == 1 {
            self.clear_sprite_units();
        }
        if is_visible && (1..=256).contains(&dot) {
            self.output_pixel(dot - 1);
        }
        if is_visible && dot == 256 {
            self.finish_scanline();
        }
    }

    fn run_fetch_phase(&mut self, phase: u8) {
        match phase {
            1 => {
                let addr = 0x2000 | (self.v & 0x0FFF);
                self.nt_latch = self.mem_read(addr);
            }
            3 => {
                let addr =
                    0x23C0 | (self.v & 0x0C00) | ((self.v >> 4) & 0x38) | ((self.v >> 2) & 0x07);
                let raw = self.mem_read(addr);
                self.at_latch = attribute_quadrant_bits(raw, self.v);
            }
            5 => {
                let addr = self.bg_pattern_table_base()
                    | ((self.nt_latch as u16) << 4)
                    | ((self.v >> 12) & 0x07);
                self.pt_lo_latch = self.mem_read(addr);
            }
            7 => {
                let addr = self.bg_pattern_table_base()
                    | ((self.nt_latch as u16) << 4)
                    | 0x08
                    | ((self.v >> 12) & 0x07);
                self.pt_hi_latch = self.mem_read(addr);
            }
            _ => {}
        }
    }

    fn bg_pattern_table_base(&self) -> u16 {
        if self.ctrl & 0x10 != 0 {
            0x1000
        } else {
            0x0000
        }
    }

    fn shift_left(&mut self) {
        self.bg_pattern_shift_lo <<= 1;
        self.bg_pattern_shift_hi <<= 1;
        self.bg_attr_shift_lo <<= 1;
        self.bg_attr_shift_hi <<= 1;
    }

    /// Load the just-fetched tile into the low byte of each shift register
    /// (the high byte already holds the in-progress tile from the ongoing
    /// per-dot shifts) — nesdev: "the first two tiles for the next scanline
    /// are fetched, and loaded into the shift registers."
    fn reload_shift_registers(&mut self) {
        self.bg_pattern_shift_lo = (self.bg_pattern_shift_lo & 0xFF00) | self.pt_lo_latch as u16;
        self.bg_pattern_shift_hi = (self.bg_pattern_shift_hi & 0xFF00) | self.pt_hi_latch as u16;
        let attr_lo_fill = if self.at_latch & 0x01 != 0 {
            0xFF
        } else {
            0x00
        };
        let attr_hi_fill = if self.at_latch & 0x02 != 0 {
            0xFF
        } else {
            0x00
        };
        self.bg_attr_shift_lo = (self.bg_attr_shift_lo & 0xFF00) | attr_lo_fill;
        self.bg_attr_shift_hi = (self.bg_attr_shift_hi & 0xFF00) | attr_hi_fill;
    }

    /// Resolve the background palette-RAM address (0-15, `$3F00`-relative)
    /// for the pixel currently selected by fine X, or `None` if this pixel
    /// is transparent (pattern bits `00`) — nesdev.org/wiki/PPU_rendering's
    /// "Priority multiplexer" section: "transparent pixels always display
    /// the color at $3F00... referred to as the 'backdrop' color", i.e. a
    /// transparent pixel's address is forced to 0 regardless of the tile's
    /// own attribute bits (NOT `attribute << 2 | 0`, a classic bug this
    /// module avoids by branching explicitly).
    fn background_palette_addr(&self) -> Option<u8> {
        if !self.mask_show_background() {
            return None;
        }
        let bit = 15 - self.x;
        let p0 = (self.bg_pattern_shift_lo >> bit) & 1;
        let p1 = (self.bg_pattern_shift_hi >> bit) & 1;
        let pattern = ((p1 << 1) | p0) as u8;
        if pattern == 0 {
            return None;
        }
        let a0 = (self.bg_attr_shift_lo >> bit) & 1;
        let a1 = (self.bg_attr_shift_hi >> bit) & 1;
        let attr = ((a1 << 1) | a0) as u8;
        Some((attr << 2) | pattern)
    }

    /// Resolve the background layer's contribution at visible-scanline dot
    /// `x + 1`: `Some(addr)` (a 0-15 palette-RAM address, module doc's
    /// `palette_index` semantics commitment — the caller resolves this
    /// through [`Ppu::palette_read`] itself) plus [`PixelLayer::Background`]
    /// if this pixel is opaque, or `None` plus [`PixelLayer::Backdrop`] if
    /// transparent. Pure/side-effect-free — ticket W1-05a split the old
    /// `output_pixel` (which wrote `line_buffer` directly) into this
    /// resolver plus [`Ppu::output_pixel`] (now in `sprites.rs`), because
    /// sprites now also contribute to the same pixel and compositing them
    /// needs both layers resolved before anything is written.
    pub(super) fn background_pixel(&self, x: u16) -> (Option<u8>, PixelLayer) {
        let mut addr = self.background_palette_addr();
        // $2001 bit 1: "Show background in leftmost 8 pixels" — forced to
        // backdrop when clear, regardless of the fetched tile.
        if x < 8 && !self.mask_show_background_left8() {
            addr = None;
        }
        match addr {
            Some(a) => (Some(a), PixelLayer::Background(0)),
            None => (None, PixelLayer::Backdrop),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_phase_matches_the_dot_table() {
        // dot -> (phase) for the first tile of a scanline (dots 1-8) and
        // the reload dot (9), cited directly from the module doc's table.
        assert_eq!(fetch_phase(1), Some(0)); // NT setup
        assert_eq!(fetch_phase(2), Some(1)); // NT fetch
        assert_eq!(fetch_phase(3), Some(2)); // AT setup
        assert_eq!(fetch_phase(4), Some(3)); // AT fetch
        assert_eq!(fetch_phase(5), Some(4)); // PT lo setup
        assert_eq!(fetch_phase(6), Some(5)); // PT lo fetch
        assert_eq!(fetch_phase(7), Some(6)); // PT hi setup
        assert_eq!(fetch_phase(8), Some(7)); // PT hi fetch
        assert_eq!(fetch_phase(9), Some(0)); // next tile's NT setup
    }

    #[test]
    fn fetch_phase_is_none_in_the_sprite_and_unused_windows() {
        for dot in [257u16, 260, 300, 320, 337, 338, 340] {
            assert_eq!(fetch_phase(dot), None, "dot {dot} should be inactive");
        }
    }

    #[test]
    fn fetch_phase_covers_the_prefetch_window() {
        assert_eq!(fetch_phase(321), Some(0));
        assert_eq!(fetch_phase(328), Some(7));
        assert_eq!(fetch_phase(329), Some(0));
        assert_eq!(fetch_phase(336), Some(7));
    }

    #[test]
    fn reload_dots_are_9_17_through_257_plus_329_and_337() {
        let mut expected: Vec<u16> = (9..=257).step_by(8).collect();
        expected.push(329);
        expected.push(337);
        let actual: Vec<u16> = (0u16..=340).filter(|&d| is_reload_dot(d)).collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn coarse_x_increment_dots_are_8_16_through_256_plus_328_and_336() {
        let mut expected: Vec<u16> = (8..=256).step_by(8).collect();
        expected.push(328);
        expected.push(336);
        let actual: Vec<u16> = (0u16..=340)
            .filter(|&d| is_coarse_x_increment_dot(d))
            .collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn attribute_quadrant_picks_all_four_2bit_fields() {
        // Attribute byte with 4 distinct 2-bit fields: bits [1:0]=1 (top-left),
        // [3:2]=2 (top-right), [5:4]=3 (bottom-left), [7:6]=0 (bottom-right).
        let byte = 0b0011_1001u8;
        // coarse X bit 1 is v bit 1; coarse Y bit 1 is v bit 6 (coarse Y =
        // (v>>5)&0x1F, so its bit 1 lands at v's bit 5+1=6).
        const COL1: u16 = 0x0002;
        const ROW1: u16 = 0x0040;
        assert_eq!(attribute_quadrant_bits(byte, 0x0000), 1, "col0,row0");
        assert_eq!(attribute_quadrant_bits(byte, COL1), 2, "col1,row0");
        assert_eq!(attribute_quadrant_bits(byte, ROW1), 3, "col0,row1");
        assert_eq!(attribute_quadrant_bits(byte, COL1 | ROW1), 0, "col1,row1");
    }
}
