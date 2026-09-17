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

/// Extract the 2-bit palette-quadrant value for one attribute byte, given
/// the tile's coarse X/Y (see module doc's "Attribute quadrant
/// selection"). Shared by the ordinary attribute fetch (`v`'s own coarse
/// X/Y) and MMC5's vertical split (ticket W14-17), which fetches its
/// attribute byte from extended RAM using a coarse X/Y pair `v` never
/// holds (the split region has its own row, from `$5201`, not `v`'s).
fn quadrant_bits_from_coarse(attr_byte: u8, coarse_x: u16, coarse_y: u16) -> u8 {
    let col_quadrant = (coarse_x >> 1) & 1;
    let row_quadrant = (coarse_y >> 1) & 1;
    let shift = (row_quadrant << 1 | col_quadrant) * 2;
    (attr_byte >> shift) & 0x03
}

/// Extract the 2-bit palette-quadrant value for the tile `v` currently
/// points at, out of one already-fetched attribute byte (see module doc's
/// "Attribute quadrant selection").
fn attribute_quadrant_bits(attr_byte: u8, v: u16) -> u8 {
    quadrant_bits_from_coarse(attr_byte, v & 0x1F, (v >> 5) & 0x1F)
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
            // fetch this scanline's evaluated sprites' CHR pattern bytes,
            // spread across the real dots real hardware fetches them on
            // (ticket W2-03; see `sprites.rs`'s
            // `Ppu::reset_sprite_output_units`/`Ppu::run_sprite_fetch_dot`
            // doc for why this is no longer a single dot-257 call) —
            // latching render-ready units for the NEXT scanline.
            if dot == 257 {
                self.reset_sprite_output_units();
            }
            if (257..=320).contains(&dot) {
                self.run_sprite_fetch_dot(dot);
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
                self.nt_latch = if let Some(col) = self.split_column() {
                    let row = self.split_row();
                    self.ext_ram_read(usize::from(row) * 32 + usize::from(col))
                } else {
                    let addr = 0x2000 | (self.v & 0x0FFF);
                    self.mem_read(addr)
                };
            }
            3 => {
                self.at_latch = if let Some(col) = self.split_column() {
                    let row = self.split_row();
                    let attr_addr = 0x3C0 + usize::from(row / 4) * 8 + usize::from(col / 4);
                    let raw = self.ext_ram_read(attr_addr);
                    quadrant_bits_from_coarse(raw, col, row)
                } else if self.ext_attr_enabled {
                    // MMC5 ExGrafix (ticket W14-17; `crate::mappers::Mmc5`
                    // module doc's "Slice 2"): the palette AND the CHR
                    // bank for this tile both come from one ext RAM byte
                    // at the tile's own nametable offset -- no separate
                    // attribute-table fetch at all. `ext_attr_bank` is
                    // consumed by the pattern fetch below (phases 5/7).
                    let ext_addr = usize::from(self.v & 0x03FF);
                    let byte = self.ext_ram_read(ext_addr);
                    self.ext_attr_bank = byte & 0x3F;
                    (byte >> 6) & 0x03
                } else {
                    let addr = 0x23C0
                        | (self.v & 0x0C00)
                        | ((self.v >> 4) & 0x38)
                        | ((self.v >> 2) & 0x07);
                    let raw = self.mem_read(addr);
                    attribute_quadrant_bits(raw, self.v)
                };
            }
            5 => self.pt_lo_latch = self.bg_pattern_byte(false),
            7 => self.pt_hi_latch = self.bg_pattern_byte(true),
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

    /// This tile's screen column (`v`'s coarse X, 0-31) if MMC5's
    /// vertical split (ticket W14-17) covers it -- `None` if the split is
    /// off or this column is outside its side/threshold. Used by both the
    /// NT and AT fetch phases above, since the split substitutes its own
    /// source for both.
    ///
    /// Decode verified 2026-09-17 against nesdev.org/wiki/MMC5 "Vertical
    /// Split Mode": `$5200` is `ESxW WWWW` -- E enables, S picks the side
    /// (0 left, 1 right), W is the "split threshold tile count". Left:
    /// tiles `0..T-1` are the split region and the rest render normally;
    /// right: tiles `0..T-1` render normally and tiles `T` onward are the
    /// split region. Both sides share the one threshold, which is what
    /// `< tile` / `>= tile` below encodes.
    ///
    /// Approximation, recorded on the ticket: nesdev says the MMC5 counts
    /// the scanline's fetches itself to decide the tile column; this reads
    /// `v`'s coarse X instead, so a horizontal `$2005` scroll shifts the
    /// split column by the same amount where hardware would not.
    fn split_column(&self) -> Option<u16> {
        if !self.split_enabled {
            return None;
        }
        let col = self.v & 0x1F;
        let threshold = u16::from(self.split_tile);
        let hit = if self.split_right {
            col >= threshold
        } else {
            col < threshold
        };
        hit.then_some(col)
    }

    /// The split region's own tile row, derived from `$5201` as an
    /// independent vertical position from `v`'s own coarse Y -- the whole
    /// point of a split region is that it scrolls on its own axis.
    /// nesdev.org/wiki/MMC5: `$5201` is "the vertical scroll value to use
    /// in split region", scrolling "like normal vertical scrolling", and
    /// the split always reads its nametable from the extended RAM. Not
    /// corrected for this pipeline's 2-tile fetch lookahead or for the
    /// pre-render-line clamp `bg_pattern_byte` shares with this method
    /// (see there) -- the same directed-test standard this ticket's split
    /// test uses, not a pixel-accurate scroll oracle.
    fn split_row(&self) -> u16 {
        let scanline = self.scanline.min(super::POSTRENDER_SCANLINE - 1);
        ((u16::from(self.split_scroll) + scanline) / 8) % 30
    }

    /// One pattern-table byte (`hi` = the high bit-plane) for the tile
    /// currently in `nt_latch`, honoring whichever CHR source is active
    /// this dot (ticket W14-17): the vertical split's own fixed `$5202`
    /// bank, MMC5 ExGrafix's per-tile ext-RAM bank (`ext_attr_bank`,
    /// latched by the attribute-fetch phase above), or the ordinary
    /// `chr`/`ctrl`-selected half every other board and mode uses.
    fn bg_pattern_byte(&mut self, hi: bool) -> u8 {
        let split_col = self.split_column();
        // `mmc5_full_chr` is `None` whenever this board has no CHR ROM to
        // bank (CHR-RAM MMC5 carts, `Mapper::chr_rom_full`'s doc) -- fall
        // through to the ordinary fetch rather than reading a blank 0,
        // the same "degrade to normal fetching" shape `chr_window`'s own
        // CHR-RAM caveat already documents, not a silent black screen.
        if (split_col.is_some() || self.ext_attr_enabled) && self.mmc5_full_chr.is_some() {
            let bank_4k = if split_col.is_some() {
                usize::from(self.split_chr_bank)
            } else {
                (usize::from(self.ext_attr_chr_high) << 6) | usize::from(self.ext_attr_bank)
            };
            let len = self.mmc5_full_chr.as_ref().expect("checked above").len();
            let banks = (len / 4096).max(1);
            let base = (bank_4k % banks) * 4096 + usize::from(self.nt_latch) * 16;
            let fine_y = if split_col.is_some() {
                // Clamped to the last visible scanline (`split_row`'s own
                // doc names this too): on the pre-render line (261) this
                // reads row 29/fine-Y 7 for both prefetched tiles rather
                // than scanline 0's actual row -- an approximation, not a
                // verified hardware behavior (see `split_row`'s doc).
                let scanline = self.scanline.min(super::POSTRENDER_SCANLINE - 1);
                (u16::from(self.split_scroll) + scanline) % 8
            } else {
                (self.v >> 12) & 0x07
            };
            let offset = base + usize::from(fine_y) + usize::from(hi) * 8;
            let addr = self.bg_pattern_table_base()
                | ((self.nt_latch as u16) << 4)
                | (u16::from(hi) << 3)
                | ((self.v >> 12) & 0x07);
            // Ticket W2-03's A12 filter is a PPU-bus-electrical fact, not
            // a `chr` (the materialized 8 KiB window)-specific one: real
            // hardware still drives this same $0000-$1FFF address while
            // MMC5 answers it from its own wider CHR, exactly the
            // reasoning `Ppu::sprite_pattern_read` already documents for
            // its own mapper-supplied-window case.
            self.observe_ppu_bus_address(addr);
            let chr = self.mmc5_full_chr.as_ref().expect("checked above");
            return chr[offset % chr.len()];
        }
        let addr = self.bg_pattern_table_base()
            | ((self.nt_latch as u16) << 4)
            | (u16::from(hi) << 3)
            | ((self.v >> 12) & 0x07);
        self.mem_read(addr)
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
    /// Where the tile being reloaded at `dot` will actually appear.
    ///
    /// **A reloaded tile is not the tile being displayed.** It goes into
    /// the LOW byte of a 16-bit shifter whose HIGH bits feed the output,
    /// so it becomes visible eight shifts later — pixel `dot + 7`, not
    /// `dot - 1`. Getting this wrong puts every tile one tile-width left
    /// of where it was drawn, which looks plausible on a repeating
    /// background and is obvious on a status bar.
    ///
    /// The two prefetch reloads (dots 329 and 337) load the first two
    /// tiles of the NEXT scanline, landing at its pixels 0 and 8.
    ///
    /// Fine X shifts the whole window left, so it is subtracted from the
    /// result rather than folded into the fetch.
    fn drawn_tile_position(&self, dot: u16) -> Option<(i16, u16)> {
        let fine_x = i16::from(self.x);
        if dot >= 329 {
            // Prefetch for the next scanline. On the pre-render line that
            // is scanline 0, which is why this wraps rather than adding.
            let y = if self.scanline == super::PRERENDER_SCANLINE {
                0
            } else {
                self.scanline + 1
            };
            if y >= super::POSTRENDER_SCANLINE {
                return None;
            }
            let x0 = if dot == 329 { 0i16 } else { 8 };
            return Some((x0 - fine_x, y));
        }
        // Same scanline, and only if it is a visible one.
        if self.scanline >= super::POSTRENDER_SCANLINE {
            return None;
        }
        let x = i16::try_from(dot + 7).ok()? - fine_x;
        // Tiles reloaded late in the line are fetched but never displayed:
        // the fetch window runs to dot 256, and anything landing at or
        // past the 256th pixel is off the right edge.
        if x >= 256 {
            return None;
        }
        Some((x, self.scanline))
    }

    fn reload_shift_registers(&mut self) {
        if self.tile_capture {
            if let Some((x, y)) = self.drawn_tile_position(self.dot) {
                let tile = super::DrawnTile {
                    x,
                    y,
                    tile: self.nt_latch,
                    base: self.bg_pattern_table_base(),
                    palette: self.at_latch,
                };
                self.drawn_tiles.push(tile);
            }
        }
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
    use rf_cart::Mirroring;

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

    /// **MMC5 ExGrafix (`$5104` mode 1) drives one tile's CHR bank and
    /// palette from extended RAM** (ticket W14-17, acceptance 1): the tile
    /// ID still comes from the ordinary nametable fetch, but the
    /// attribute fetch is replaced entirely by one ext RAM byte at the
    /// same nametable offset -- bits 6-7 the palette, bits 0-5 (widened by
    /// `$5130`'s bits as the high bits) the pattern table's 4 KiB bank.
    #[test]
    fn ex_grafix_mode_takes_a_tiles_chr_bank_and_palette_from_extended_ram() {
        // Both banks are pre-populated before the first push, since
        // `set_mmc5_ext_view` copies the CHR bytes only once (this file's
        // doc: they never change after cart load) -- a mutation made
        // AFTER that first push would never reach the PPU's own copy.
        let mut chr = vec![0u8; 6 * 4096];
        chr[5 * 4096] = 0xAB; // bank 5, tile 0, row 0: low plane
        chr[5 * 4096 + 8] = 0xCD; // ...high plane
        chr[3 * 4096] = 0xEF; // bank 3, tile 0, row 0: low plane
        chr[3 * 4096 + 8] = 0x12; // ...high plane
        let mut ppu = Ppu::new(vec![0u8; 0x2000], false, Mirroring::Horizontal);
        ppu.set_mmc5_view(None, None, true); // has_ext_nametable_ram
        ppu.set_mmc5_ext_view(Some(&chr), Some(0), None); // $5130 high bits = 0
                                                          // Tile 0 (v == 0): palette 2, CHR bank 5.
        ppu.ext_ram_write(0, (2u8 << 6) | 5);
        ppu.run_fetch_phase(1); // NT: the ordinary (zeroed) nametable -> tile 0
        assert_eq!(ppu.nt_latch, 0);
        ppu.run_fetch_phase(3); // AT: overridden by the ext RAM byte
        assert_eq!(ppu.at_latch, 2, "palette from ext RAM bits 6-7");
        ppu.run_fetch_phase(5);
        ppu.run_fetch_phase(7);
        assert_eq!(ppu.pt_lo_latch, 0xAB, "bank 5's tile 0, low plane");
        assert_eq!(ppu.pt_hi_latch, 0xCD, "...high plane");

        // `$5130`'s bits must actually widen the 6-bit ext-RAM field, not
        // just be accepted and ignored: with high bits == 1, the same
        // low-6-bits value 5 now names bank (1 << 6 | 5) % 6 == 3, not 5.
        ppu.set_mmc5_ext_view(Some(&chr), Some(1), None);
        ppu.run_fetch_phase(3);
        ppu.run_fetch_phase(5);
        ppu.run_fetch_phase(7);
        assert_eq!(ppu.pt_lo_latch, 0xEF, "$5130's high bits shifted the bank");
        assert_eq!(ppu.pt_hi_latch, 0x12);
    }

    /// **The vertical split pulls its side of the screen from extended
    /// RAM** (ticket W14-17, acceptance 2), pinned to one column and one
    /// side: `$5200` selects the left side and a 3-tile-wide split, so
    /// coarse X 2 (inside) reads tile/attribute/CHR from ext RAM under
    /// the split's own `$5202` bank, while coarse X 10 (outside) still
    /// answers from the ordinary, untouched nametable.
    #[test]
    fn the_vertical_split_pulls_its_side_of_the_screen_from_extended_ram() {
        let mut chr = vec![0u8; 3 * 4096];
        chr[2 * 4096 + 7 * 16] = 0x11; // split bank 2, tile 7, row 0: low
        chr[2 * 4096 + 7 * 16 + 8] = 0x22; // ...high
        let mut ppu = Ppu::new(vec![0u8; 0x2000], false, Mirroring::Horizontal);
        ppu.set_mmc5_view(None, None, true);
        // Left side, split tile 3 (columns 0-2 are the split), no
        // vertical scroll, CHR bank 2.
        ppu.set_mmc5_ext_view(Some(&chr), None, Some((false, 3, 0, 2)));
        ppu.scanline = 0; // pin: row 0, fine Y 0, so the tile's row-0 bytes above apply
        let row = ppu.split_row();
        let col = 2u16;
        ppu.ext_ram_write(usize::from(row) * 32 + usize::from(col), 7); // tile id
        let attr_offset = 0x3C0 + usize::from(row / 4) * 8 + usize::from(col / 4);
        ppu.ext_ram_write(attr_offset, 0b11);
        let expected_palette = quadrant_bits_from_coarse(0b11, col, row);

        ppu.v = col; // coarse X 2: inside the split (< 3)
        ppu.run_fetch_phase(1);
        assert_eq!(ppu.nt_latch, 7, "tile id came from ext RAM");
        ppu.run_fetch_phase(3);
        assert_eq!(ppu.at_latch, expected_palette);
        ppu.run_fetch_phase(5);
        ppu.run_fetch_phase(7);
        assert_eq!(ppu.pt_lo_latch, 0x11, "the split's own $5202 bank");
        assert_eq!(ppu.pt_hi_latch, 0x22);

        ppu.v = 10; // coarse X 10: outside the split (not < 3)
        ppu.run_fetch_phase(1);
        assert_eq!(
            ppu.nt_latch, 0,
            "outside the split, the ordinary (empty) nametable answers"
        );
    }

    /// The same split, on the right side: `$5200`'s tile field is one
    /// shared delimiter column for both sides (this file's `split_column`
    /// doc), so the right side is "at or past it," not a mirrored width.
    #[test]
    fn the_vertical_split_also_works_pinned_to_the_right_side() {
        let mut chr = vec![0u8; 3 * 4096];
        chr[2 * 4096 + 7 * 16] = 0x11;
        chr[2 * 4096 + 7 * 16 + 8] = 0x22;
        let mut ppu = Ppu::new(vec![0u8; 0x2000], false, Mirroring::Horizontal);
        ppu.set_mmc5_view(None, None, true);
        // Right side, delimiter column 20: columns 20-31 are the split.
        ppu.set_mmc5_ext_view(Some(&chr), None, Some((true, 20, 0, 2)));
        ppu.scanline = 0;
        let row = ppu.split_row();
        let col = 25u16;
        ppu.ext_ram_write(usize::from(row) * 32 + usize::from(col), 7);

        ppu.v = col; // coarse X 25: inside the right split (>= 20)
        ppu.run_fetch_phase(1);
        assert_eq!(ppu.nt_latch, 7, "tile id came from ext RAM");
        ppu.run_fetch_phase(5);
        ppu.run_fetch_phase(7);
        assert_eq!(ppu.pt_lo_latch, 0x11);
        assert_eq!(ppu.pt_hi_latch, 0x22);

        ppu.v = 10; // coarse X 10: not >= 20, so outside the right split
        ppu.run_fetch_phase(1);
        assert_eq!(ppu.nt_latch, 0, "outside the split on the right side too");
    }
}
