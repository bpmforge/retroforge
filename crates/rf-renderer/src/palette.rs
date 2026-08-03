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
#![allow(clippy::unreadable_literal)]

/// `$00`-`$3F` → `[R, G, B]`, no color emphasis. See module doc for
/// provenance.
pub const NES_PALETTE: [[u8; 3]; 64] = [
    [91, 91, 91],    // $00
    [0, 35, 123],    // $01
    [18, 17, 157],   // $02
    [49, 4, 153],    // $03
    [79, 0, 113],    // $04
    [96, 1, 53],     // $05
    [94, 9, 1],      // $06
    [73, 24, 0],     // $07
    [42, 44, 0],     // $08
    [12, 60, 0],     // $09
    [0, 69, 0],      // $0A
    [0, 67, 9],      // $0B
    [0, 54, 66],     // $0C
    [0, 0, 0],       // $0D
    [3, 3, 3],       // $0E
    [3, 3, 3],       // $0F
    [170, 170, 170], // $10
    [20, 85, 217],   // $11
    [58, 56, 255],   // $12
    [107, 34, 255],  // $13
    [152, 24, 202],  // $14
    [178, 27, 113],  // $15
    [174, 43, 28],   // $16
    [144, 68, 0],    // $17
    [96, 98, 0],     // $18
    [48, 124, 0],    // $19
    [14, 137, 0],    // $1A
    [0, 133, 43],    // $1B
    [1, 114, 132],   // $1C
    [3, 3, 3],       // $1D
    [3, 3, 3],       // $1E
    [3, 3, 3],       // $1F
    [255, 255, 255], // $20
    [92, 171, 255],  // $21
    [140, 137, 255], // $22
    [197, 111, 255], // $23
    [247, 98, 255],  // $24
    [255, 102, 203], // $25
    [255, 121, 103], // $26
    [237, 152, 31],  // $27
    [184, 186, 2],   // $28
    [128, 215, 4],   // $29
    [84, 230, 42],   // $2A
    [62, 226, 121],  // $2B
    [65, 204, 224],  // $2C
    [68, 68, 68],    // $2D
    [3, 3, 3],       // $2E
    [3, 3, 3],       // $2F
    [255, 255, 255], // $30
    [191, 226, 255], // $31
    [212, 211, 255], // $32
    [237, 199, 255], // $33
    [255, 193, 255], // $34
    [255, 195, 240], // $35
    [255, 204, 196], // $36
    [254, 218, 160], // $37
    [232, 233, 141], // $38
    [207, 245, 143], // $39
    [187, 251, 166], // $3A
    [176, 249, 204], // $3B
    [178, 240, 249], // $3C
    [179, 179, 179], // $3D
    [3, 3, 3],       // $3E
    [3, 3, 3],       // $3F
];

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
