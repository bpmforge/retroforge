//! NES 2C02 palette-to-RGB lookup table (ticket W1-06).
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

#[cfg(test)]
mod tests {
    use super::*;

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
