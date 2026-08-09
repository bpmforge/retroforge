//! Palette viewer data provider (FR-DBG-001, DEBUGGER.md §3 "Palette" row:
//! "32-entry, emphasis variants").
//!
//! ## Data source (read `crate::pattern`'s module doc first — same gap)
//!
//! NES palette RAM ("CGRAM" in `rf_core_api::StateView`'s field naming) is
//! 32 bytes: 4 background palettes and 4 sprite palettes of 4 entries each
//! (entry 0 of every BG palette mirrors the universal backdrop color,
//! [nesdev.org/wiki/PPU_palettes](https://www.nesdev.org/wiki/PPU_palettes)).
//! It lives inside `rf_nes::ppu::Ppu`'s `pub(super) palette: [u8; 32]`
//! field, invisible even to `rf-nes`'s own `system` module, let alone this
//! crate (which additionally may not depend on `rf-nes` at all,
//! `scripts/validate-arch.sh` rule 3) or `retroforge` (which could reach
//! it in principle — it already depends on `rf-nes` — but has no accessor
//! to call: `NesBus` exposes `oam()`/`prg_ram()` but no `palette()`
//! passthrough). **Verified, not assumed** (ticket W4-06a pre-flight):
//! `grep -n "pub fn" crates/rf-nes/src/ppu/mod.rs crates/rf-nes/src/system/mod.rs`
//! shows no such getter anywhere in either file.
//!
//! Unlike CHR (`crate::pattern`'s ROM-bytes workaround), there is no static
//! fallback here either — palette RAM is written by the running game, not
//! carried in the ROM file. So [`decode_palette`] below is decode logic
//! only, fully tested against synthetic bytes; `retroforge`'s panel has
//! nothing real to feed it until a future ticket adds `NesBus::palette()`
//! mirroring the existing `NesBus::oam()` (same seam, recorded here rather
//! than worked around by reaching for `Ppu::read_register($2007)`, which
//! would perturb `v`/`w`/the read-buffer — the exact W3-05a hazard class
//! the ticket brief warns about).
use rf_renderer::palette_index_to_rgb;

/// Palette RAM is 32 bytes on the NES.
pub const PALETTE_RAM_LEN: usize = 32;
/// 4 BG palettes + 4 sprite palettes.
pub const PALETTE_GROUP_COUNT: usize = 8;
/// 4 color entries per palette.
pub const PALETTE_ENTRY_COUNT: usize = 4;

/// One resolved palette entry: the raw byte as stored in CGRAM (masked to
/// the hardware's 6 significant bits — `rf_core_api::PpuPixel::
/// palette_index`'s own doc: "the raw 6-bit value") and the RGB color it
/// resolves to via [`rf_renderer::NES_PALETTE`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaletteSwatch {
    pub raw_index: u8,
    pub rgb: [u8; 3],
}

/// Decode 32 bytes of CGRAM into 8 groups (BG 0-3, then sprite 0-3) of 4
/// swatches each — `cgram[group * 4 + entry]`, the NES's own flat layout
/// ([nesdev.org/wiki/PPU_palettes](https://www.nesdev.org/wiki/PPU_palettes)).
/// Returns `None` if `cgram` is shorter than [`PALETTE_RAM_LEN`] (no live
/// producer yet — module doc — so a caller with nothing real to pass
/// should pass an empty slice and get `None` back, not a bogus all-black
/// palette).
#[must_use]
pub fn decode_palette(
    cgram: &[u8],
) -> Option<[[PaletteSwatch; PALETTE_ENTRY_COUNT]; PALETTE_GROUP_COUNT]> {
    let cgram: &[u8; PALETTE_RAM_LEN] = cgram.get(..PALETTE_RAM_LEN)?.try_into().ok()?;
    let mut groups = [[PaletteSwatch {
        raw_index: 0,
        rgb: [0, 0, 0],
    }; PALETTE_ENTRY_COUNT]; PALETTE_GROUP_COUNT];
    for (group, slot) in groups.iter_mut().enumerate() {
        for (entry, swatch) in slot.iter_mut().enumerate() {
            let raw = cgram[group * PALETTE_ENTRY_COUNT + entry];
            *swatch = PaletteSwatch {
                raw_index: raw,
                rgb: palette_index_to_rgb(raw),
            };
        }
    }
    Some(groups)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_palette_places_a_known_byte_at_the_expected_group_and_entry() {
        let mut cgram = [0u8; PALETTE_RAM_LEN];
        // Sprite palette 2 (group index 4+2=6), entry 3: raw index 0x16.
        cgram[6 * PALETTE_ENTRY_COUNT + 3] = 0x16;
        let groups = decode_palette(&cgram).expect("32 bytes must decode");
        assert_eq!(groups[6][3].raw_index, 0x16);
        assert_eq!(groups[6][3].rgb, palette_index_to_rgb(0x16));
        // Every other slot stays raw index 0, not garbage.
        assert_eq!(groups[0][0].raw_index, 0);
        assert_eq!(groups[7][3].raw_index, 0);
    }

    #[test]
    fn decode_palette_reuses_the_renderer_lut_not_a_reinvented_one() {
        let mut cgram = [0u8; PALETTE_RAM_LEN];
        cgram[0] = 0x0F; // a real hardware color, not black
        let groups = decode_palette(&cgram).unwrap();
        assert_eq!(groups[0][0].rgb, rf_renderer::NES_PALETTE[0x0F]);
    }

    #[test]
    fn decode_palette_masks_to_6_bits_matching_ppu_pixel_semantics() {
        let mut cgram = [0u8; PALETTE_RAM_LEN];
        cgram[0] = 0xFF; // top 2 bits must be masked off by palette_index_to_rgb
        let groups = decode_palette(&cgram).unwrap();
        assert_eq!(groups[0][0].rgb, palette_index_to_rgb(0x3F));
    }

    #[test]
    fn decode_palette_returns_none_for_a_short_slice_not_a_partial_table() {
        assert_eq!(decode_palette(&[]), None);
        assert_eq!(decode_palette(&[0u8; 31]), None);
    }
}
