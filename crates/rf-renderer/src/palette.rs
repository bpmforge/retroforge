//! NES 2C02 palette-to-RGB lookup table (ticket W1-06), and the written
//! SNES palette path beside it (ticket W7-20, [`LinePalette`]).
//!
//! [`rf_core_api::CoreSink`]/`PpuPixel` carry only a
//! `palette_index: u8` — resolving that to a display color is explicitly
//! the *renderer's* job (ARCHITECTURE §5, ADR-4; see `rf_core_api::video`'s
//! module doc), never the core's. This is that resolution step for the CPU
//! blit path this ticket builds (acceptance criterion 1 permits "CPU blit
//! ... pre-W3").
//!
//! ## Source
//!
//! [nesdev.org/wiki/PPU_palettes](https://www.nesdev.org/wiki/PPU_palettes)
//! documents the 64-entry (`$00`-`$3F`) 2C02 palette as a 3-bytes-per-entry
//! `.pal` file (no single composite palette is hardware-exact — the page
//! ships several generator outputs, all in that format) but does not print
//! a machine-readable table inline. Rather than transcribe swatch colors by
//! eye from a wiki image, this table's values are read directly from
//! `tetanes-core/ntscpalette.pal` in
//! [lukexor/tetanes](https://github.com/lukexor/tetanes) — the exact
//! project `docs/TECH_STACK.md` §1 already cites as this workspace's
//! "proof-of-stack" (active Rust NES emulator shipping the identical
//! wgpu+egui+winit dependency combination). That file is 512 RGB triples
//! (64 base colors × 8 PPUMASK color-emphasis combinations, `.pal`
//! convention); [`NES_PALETTE`] below is the first 64 — the no-emphasis
//! (`000`) slice, matching what `PpuPixel::palette_index` alone can
//! address today. Emphasis bits are not part of the `CoreSink` contract
//! yet, so the other 448 entries aren't needed here.
//!
//! Verified 2026-08-03 by fetching that file directly
//! (`raw.githubusercontent.com/lukexor/tetanes/main/tetanes-core/ntscpalette.pal`)
//! and decoding its first 192 bytes as 64 `(R, G, B)` triples — not
//! reproduced from memory/training data.
//!
//! ## Data, not code (ticket W3-01, acceptance criterion 6)
//!
//! Those 192 bytes are checked in as `assets/nes.pal` — the exact bytes a
//! `.pal` consumer/generator can hand around unchanged — rather than
//! transcribed into this file as a literal. [`NES_PALETTE`] below is
//! `const`-decoded from that asset at compile time (`include_bytes!` +
//! [`decode_pal`], a `const fn`, so this is still a zero-cost compile-time
//! constant, not a runtime load). `src/bin/gen_pal.rs` is the generator:
//! re-run it against a freshly fetched upstream `.pal` to update the asset
//! when palette research changes — this file and `shaders/palette.wgsl`
//! never need touching for that.
#![allow(clippy::unreadable_literal)]

/// The checked-in asset: 64 RGB triples, 192 bytes, no color emphasis. See
/// module doc for provenance and `src/bin/gen_pal.rs` for how to regenerate
/// it from a different upstream `.pal` source.
const NES_PAL_BYTES: &[u8] = include_bytes!("../assets/nes.pal");

/// Decode a `.pal`-convention byte slice (3 bytes per entry: R, G, B) into
/// [`NES_PALETTE`]'s array form. `const fn` so [`NES_PALETTE`] stays a
/// compile-time constant despite no longer being a literal.
///
/// # Panics (at compile time, via the `const` context evaluating this)
/// Panics if `bytes.len() != 64 * 3` — a malformed/truncated asset file
/// must fail the build, not silently produce a short or garbled palette.
const fn decode_pal(bytes: &[u8]) -> [[u8; 3]; 64] {
    assert!(
        bytes.len() == 64 * 3,
        "assets/nes.pal must be exactly 192 bytes (64 RGB triples)"
    );
    let mut table = [[0u8; 3]; 64];
    let mut i = 0;
    while i < 64 {
        table[i] = [bytes[i * 3], bytes[i * 3 + 1], bytes[i * 3 + 2]];
        i += 1;
    }
    table
}

/// `$00`-`$3F` → `[R, G, B]`, no color emphasis. See module doc for
/// provenance; decoded from `assets/nes.pal` at compile time via
/// [`decode_pal`], not a hand-transcribed literal.
pub const NES_PALETTE: [[u8; 3]; 64] = decode_pal(NES_PAL_BYTES);

/// Resolve a `PpuPixel::palette_index` to opaque RGB. Masks to 6 bits
/// first (`& 0x3F`) since the NES palette only ever has 64 entries and a
/// core is never supposed to emit anything wider, but this stays a total
/// function rather than panicking on a malformed index — a renderer
/// bug/garbage frame should draw wrong colors, not crash the frontend.
#[must_use]
pub fn palette_index_to_rgb(index: u8) -> [u8; 3] {
    NES_PALETTE[(index & 0x3F) as usize]
}

/// Entries in a written palette: the SNES's CGRAM holds 256.
pub const LINE_PALETTE_ENTRIES: usize = 256;

/// One scanline's written palette and master brightness (ticket W7-20),
/// as a core sends it through `CoreSink::palette_scanline`.
///
/// Until W7-20 every pixel was resolved through [`NES_PALETTE`], SNES
/// ones included, so SNES games showed in NES colours. A line that has
/// one of these is resolved through it; a line without (any NES frame)
/// keeps the fixed table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinePalette {
    /// Raw BGR555 words (fullsnes "SNES Color Palette").
    pub words: [u16; LINE_PALETTE_ENTRIES],
    /// Master brightness `0..=15`; `0` is black (fullsnes "2100h -
    /// INIDISP").
    pub brightness: u8,
}

impl LinePalette {
    /// From what a core sent: entries past 256 are ignored, missing ones
    /// read as black, brightness is clamped to 15.
    #[must_use]
    pub fn from_words(palette: &[u16], brightness: u8) -> Self {
        let mut words = [0u16; LINE_PALETTE_ENTRIES];
        for (slot, word) in words.iter_mut().zip(palette) {
            *slot = *word;
        }
        LinePalette {
            words,
            brightness: brightness.min(15),
        }
    }

    #[must_use]
    pub fn rgb(&self, index: u8) -> [u8; 3] {
        bgr555_to_rgb(self.words[usize::from(index)], self.brightness)
    }
}

/// One BGR555 word as RGB888 at a master brightness (ticket W7-20).
///
/// Blue is in the HIGH bits (fullsnes "SNES Color Palette": "Bit 0-4 Red,
/// 5-9 Green, 10-14 Blue"). Each channel scales by `*255/31` so 31 is 255,
/// the same conversion `rf_snes::debug::cgram_rgb` uses (a test pins the
/// two equal at full brightness). Brightness N in `1..=15` scales by
/// `(N+1)/16` and 0 is black (fullsnes "2100h - INIDISP").
#[must_use]
pub fn bgr555_to_rgb(word: u16, brightness: u8) -> [u8; 3] {
    if brightness == 0 {
        return [0, 0, 0];
    }
    let scale = u32::from(brightness.min(15)) + 1;
    let ch = |shift: u16| -> u8 {
        let full = u32::from((word >> shift) & 0x1F) * 255 / 31;
        (full * scale / 16) as u8
    };
    [ch(0), ch(5), ch(10)]
}

/// Combine a main and a sub/fixed BGR555 colour (ticket W7-21): per 5-bit
/// channel, add or subtract, clamp to `0..=31`, then halve if asked —
/// fullsnes "SNES PPU Color-Math" (`$2131` bits 6-7). The same arithmetic
/// as `rf_snes::ppu::window::ColorMath::blend`, done here because the
/// result is a colour, which only the renderer may produce (law 4).
#[must_use]
pub fn color_math_bgr555(main: u16, sub: u16, op: rf_core_api::ColorMathOp) -> u16 {
    use rf_core_api::ColorMathOp as Op;
    let (subtract, half) = match op {
        Op::None => return main,
        Op::Add => (false, false),
        Op::AddHalf => (false, true),
        Op::Subtract => (true, false),
        Op::SubtractHalf => (true, true),
    };
    let mut out = 0u16;
    for shift in [0u16, 5, 10] {
        let m = i32::from((main >> shift) & 0x1F);
        let s = i32::from((sub >> shift) & 0x1F);
        let v = if subtract { m - s } else { m + s };
        let v = if half { v / 2 } else { v };
        out |= (v.clamp(0, 31) as u16) << shift;
    }
    out
}

/// Resolve `index` through `palette` when the line has one, else through
/// the fixed NES table — the one place every RGB-producing sink decides.
#[must_use]
pub fn resolve_index(index: u8, palette: Option<&LinePalette>) -> [u8; 3] {
    match palette {
        Some(p) => p.rgb(index),
        None => palette_index_to_rgb(index),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_math_adds_subtracts_clamps_and_halves_per_channel() {
        use rf_core_api::ColorMathOp as Op;
        let c = |r: u16, g: u16, b: u16| r | (g << 5) | (b << 10);
        assert_eq!(
            color_math_bgr555(c(10, 10, 10), c(5, 5, 5), Op::Add),
            c(15, 15, 15)
        );
        assert_eq!(
            color_math_bgr555(c(30, 0, 0), c(10, 0, 0), Op::Add),
            c(31, 0, 0)
        );
        assert_eq!(
            color_math_bgr555(c(30, 0, 0), c(10, 0, 0), Op::AddHalf),
            c(20, 0, 0)
        );
        assert_eq!(
            color_math_bgr555(c(2, 9, 0), c(10, 4, 0), Op::Subtract),
            c(0, 5, 0)
        );
        assert_eq!(
            color_math_bgr555(c(10, 0, 0), c(4, 0, 0), Op::SubtractHalf),
            c(3, 0, 0)
        );
        assert_eq!(
            color_math_bgr555(c(7, 8, 9), c(31, 31, 31), Op::None),
            c(7, 8, 9)
        );
    }

    #[test]
    fn bgr555_puts_blue_high_and_full_scale_at_255() {
        assert_eq!(bgr555_to_rgb(0x001F, 15), [255, 0, 0]);
        assert_eq!(bgr555_to_rgb(0x03E0, 15), [0, 255, 0]);
        assert_eq!(bgr555_to_rgb(0x7C00, 15), [0, 0, 255]);
        assert_eq!(bgr555_to_rgb(0x7FFF, 0), [0, 0, 0], "brightness 0 is black");
        // (N+1)/16: brightness 7 is half.
        assert_eq!(bgr555_to_rgb(0x001F, 7), [127, 0, 0]);
    }

    #[test]
    fn a_line_palette_resolves_its_own_words_and_none_keeps_the_nes_table() {
        let mut words = vec![0u16; 4];
        words[3] = 0x7C00;
        let p = LinePalette::from_words(&words, 15);
        assert_eq!(resolve_index(3, Some(&p)), [0, 0, 255]);
        assert_eq!(resolve_index(3, None), palette_index_to_rgb(3));
        assert_eq!(p.rgb(200), [0, 0, 0], "missing entries are black");
    }

    #[test]
    fn table_has_64_entries() {
        assert_eq!(NES_PALETTE.len(), 64);
    }

    #[test]
    fn index_zero_resolves_to_the_documented_grey() {
        assert_eq!(palette_index_to_rgb(0x00), [91, 91, 91]);
    }

    #[test]
    fn index_masks_to_six_bits_rather_than_panicking() {
        // 0xFF & 0x3F == 0x3F: must not panic (out-of-range core output
        // should degrade to a wrong-but-valid color, never crash the UI).
        assert_eq!(palette_index_to_rgb(0xFF), NES_PALETTE[0x3F]);
    }

    #[test]
    fn white_entries_are_present_where_expected() {
        // $20 and $30 are the two documented white/near-white entries.
        assert_eq!(palette_index_to_rgb(0x20), [255, 255, 255]);
        assert_eq!(palette_index_to_rgb(0x30), [255, 255, 255]);
    }
}
