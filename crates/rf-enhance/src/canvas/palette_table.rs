//! Ticket W33-02: per-scene palette snapshots for the stitched canvases.
//!
//! A canvas cell stores a palette INDEX, not a colour (so palette cycling
//! and colour math stay recomposable). Resolving that index with the palette
//! of the row being drawn NOW is wrong whenever CGRAM changed since the cell
//! was seen: a per-area palette load, a cycling water level, a fade. So every
//! stitched row interns the CGRAM it was drawn with into a [`PaletteTable`]
//! and the cells it writes carry the table id; a margin resolves a remembered
//! cell with ITS palette.
//!
//! * **Key.** The 256 CGRAM words, compared exactly (a hash only picks the
//!   bucket; two palettes are the same palette only when every word matches).
//!   Brightness (`$2100`, fullsnes "2100h - INIDISP") is NOT part of the key:
//!   it is applied to the picture and to the margins alike at compose time,
//!   so a fade-in neither multiplies the table (15 ids per palette) nor
//!   leaves a cell remembered at brightness 4 darker than the picture now.
//! * **Id.** `u8`, so a scene holds at most [`MAX_PALETTES`] = 256 (plan
//!   `docs/design/STITCHING_PLAN.md` §2.4/§5 W33-02). First come, first
//!   served: when the table is full a new palette gets no id
//!   ([`PaletteTable::intern`] returns `None`, counted in
//!   [`PaletteTable::overflowed`]) and its cells fall back to the row's
//!   current palette, which is exactly the pre-W33-02 behaviour. Replacing an
//!   old entry (LRU) would silently recolour every cell that still points at
//!   it, which is worse than falling back.
//! * **Where the id lives.** In `PpuPixel::sprite_id` of the LAYER canvas
//!   cells only. That field is documented as "the OAM index of the sprite,
//!   `None` for background/backdrop pixels", and sprites are never stitched
//!   (`crate::stitcher` module doc), so on a canvas cell it is free; the
//!   cell type, every blit/merge/crop and the core's pixels (hence the
//!   accuracy hash) stay untouched. [`tag`] and [`PaletteTable::word`] are the
//!   only readers and writers.
//! * **NES.** Its `palette_index` is the 6-bit master-colour value already
//!   read out of palette RAM per dot (`rf_nes::ppu` module doc, nesdev.org
//!   "PPU palettes"), so a remembered NES cell is immune to later palette
//!   writes by construction and needs no snapshot.

use std::collections::HashMap;

use rf_core_api::PpuPixel;

/// Entries in a written palette: the SNES's CGRAM holds 256 words.
pub const LINE_PALETTE_ENTRIES: usize = 256;

/// One row's written palette (raw BGR555 words, fullsnes "SNES Color
/// Palette").
pub type Cgram = [u16; LINE_PALETTE_ENTRIES];

/// Most palettes one scene remembers (ids are `u8`).
pub const MAX_PALETTES: usize = 256;

/// One scene's palette snapshots.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaletteTable {
    palettes: Vec<[u16; LINE_PALETTE_ENTRIES]>,
    /// FNV-1a of the words -> ids with that hash (exact compare decides).
    by_hash: HashMap<u64, Vec<u8>>,
    overflowed: u64,
}

fn fnv1a(words: &[u16; LINE_PALETTE_ENTRIES]) -> u64 {
    words.iter().fold(0xcbf2_9ce4_8422_2325, |h, w| {
        (h ^ u64::from(*w)).wrapping_mul(0x0100_0000_01b3)
    })
}

impl PaletteTable {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The id of `words`, appended on first sight; `None` when the table is
    /// full and `words` is new.
    pub fn intern(&mut self, words: &[u16; LINE_PALETTE_ENTRIES]) -> Option<u8> {
        let bucket = self.by_hash.entry(fnv1a(words)).or_default();
        if let Some(id) = bucket
            .iter()
            .copied()
            .find(|&id| self.palettes[usize::from(id)] == *words)
        {
            return Some(id);
        }
        let Ok(id) = u8::try_from(self.palettes.len()) else {
            self.overflowed += 1;
            return None;
        };
        self.palettes.push(*words);
        bucket.push(id);
        Some(id)
    }

    /// The snapshot with this id.
    #[must_use]
    pub fn get(&self, id: u8) -> Option<&Cgram> {
        self.palettes.get(usize::from(id))
    }

    /// The snapshots in id order (what persistence writes, W33-05).
    #[must_use]
    pub fn palettes(&self) -> &[Cgram] {
        &self.palettes
    }

    /// The table [`Self::palettes`] and [`Self::overflowed`] describe, with
    /// its lookup rebuilt (the inverse of persisting it). `None` for more
    /// palettes than [`MAX_PALETTES`] or two equal ones (ids would not be
    /// unique).
    #[must_use]
    pub fn from_parts(palettes: Vec<Cgram>, overflowed: u64) -> Option<Self> {
        if palettes.len() > MAX_PALETTES {
            return None;
        }
        let mut table = Self::default();
        for words in &palettes {
            let before = table.palettes.len();
            table.intern(words)?;
            if table.palettes.len() == before {
                return None;
            }
        }
        table.overflowed = overflowed;
        Some(table)
    }

    /// Distinct palettes remembered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.palettes.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.palettes.is_empty()
    }

    /// New palettes that found the table full (each one is a lookup that fell
    /// back to the row's current palette).
    #[must_use]
    pub fn overflowed(&self) -> u64 {
        self.overflowed
    }

    /// The BGR555 word a remembered `cell` shows: its own snapshot, else the
    /// row's `current` palette (a cell with no id, or an id from a table that
    /// has no such entry).
    #[must_use]
    pub fn word(&self, cell: &PpuPixel, current: &Cgram) -> u16 {
        let index = usize::from(cell.palette_index);
        cell.sprite_id
            .and_then(|id| self.get(id))
            .map_or(current[index], |w| w[index])
    }
}

/// `cell` carrying palette snapshot `id` (`None` = no snapshot).
#[must_use]
pub fn tag(mut cell: PpuPixel, id: Option<u8>) -> PpuPixel {
    cell.sprite_id = id;
    cell
}

#[cfg(test)]
mod tests {
    use super::*;
    use rf_core_api::PixelLayer;

    fn pal(fill: u16) -> [u16; LINE_PALETTE_ENTRIES] {
        let mut w = [0u16; LINE_PALETTE_ENTRIES];
        w[1] = fill;
        w
    }

    fn cell(index: u8, id: Option<u8>) -> PpuPixel {
        tag(
            PpuPixel {
                palette_index: index,
                layer: PixelLayer::Background(0),
                sprite_id: None,
                priority: 0,
            },
            id,
        )
    }

    #[test]
    fn ids_are_assigned_in_order_and_equal_palettes_share_one() {
        let mut t = PaletteTable::new();
        assert_eq!(t.intern(&pal(1)), Some(0));
        assert_eq!(t.intern(&pal(2)), Some(1));
        assert_eq!(t.intern(&pal(1)), Some(0), "same words, same id");
        assert_eq!(t.len(), 2);
        assert_eq!(t.get(1).map(|w| w[1]), Some(2));
        assert_eq!(t.get(2), None);
    }

    #[test]
    fn the_key_is_every_word_not_just_the_hash_bucket() {
        let mut t = PaletteTable::new();
        let mut a = pal(0);
        let mut b = pal(0);
        a[255] = 7;
        b[255] = 8;
        assert_ne!(t.intern(&a), t.intern(&b));
        assert_eq!(t.len(), 2);
    }

    #[test]
    fn the_table_caps_at_256_and_a_new_palette_past_it_gets_no_id() {
        let mut t = PaletteTable::new();
        for i in 0..MAX_PALETTES as u16 {
            assert_eq!(t.intern(&pal(i)), Some(i as u8));
        }
        assert_eq!(t.intern(&pal(1000)), None, "full: no id, no eviction");
        assert_eq!(t.overflowed(), 1);
        assert_eq!(t.len(), MAX_PALETTES);
        assert_eq!(t.intern(&pal(5)), Some(5), "known palettes still resolve");
        assert_eq!(t.overflowed(), 1);
    }

    #[test]
    fn a_cell_resolves_with_its_own_old_palette_not_the_current_row() {
        let mut t = PaletteTable::new();
        let old = t.intern(&pal(0x1111)).unwrap();
        let new = t.intern(&pal(0x2222)).unwrap();
        let current = pal(0x2222);
        assert_eq!(t.word(&cell(1, Some(old)), &current), 0x1111);
        assert_eq!(t.word(&cell(1, Some(new)), &current), 0x2222);
        // No id, or an id this table does not have: the current palette.
        assert_eq!(t.word(&cell(1, None), &current), 0x2222);
        assert_eq!(t.word(&cell(1, Some(200)), &current), 0x2222);
    }
}
