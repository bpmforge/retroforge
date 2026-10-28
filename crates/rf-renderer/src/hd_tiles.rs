//! HD tile layer (ticket W39-06; `docs/design/REBUILD_LAB.md` section 9).
//!
//! Draws pre-upscaled tiles from a ROM's [`HdAssetSet`] over the
//! nearest-neighbour upscale of the native frame. Everything here is a
//! hash lookup plus a pixel blit: the model ran earlier, offline, in
//! rf-ai's `hd_pass`, and this crate does not depend on rf-ai at all (a
//! test below pins that), so no inference can happen at draw time.
//!
//! Accuracy mode is the reference (CLAUDE.md law 6): the layer is OFF
//! unless the game's toggle is on, and an off layer returns the plain
//! nearest-neighbour frame without a single lookup.

use std::cell::Cell;
use std::collections::BTreeSet;

use rf_cache::{HdAssetSet, HdTile};

/// Per-game opt-in switches, keyed by normalised ROM SHA-256. Empty by
/// default, so every game starts in accuracy mode.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HdTileToggles {
    enabled: BTreeSet<String>,
}

impl HdTileToggles {
    #[must_use]
    pub fn is_enabled(&self, rom_sha256: &str) -> bool {
        self.enabled.contains(rom_sha256)
    }

    pub fn set(&mut self, rom_sha256: &str, on: bool) {
        if on {
            self.enabled.insert(rom_sha256.to_string());
        } else {
            self.enabled.remove(rom_sha256);
        }
    }

    /// One enabled ROM hash per line, sorted (for a settings file).
    #[must_use]
    pub fn to_text(&self) -> String {
        self.enabled.iter().fold(String::new(), |mut s, h| {
            s.push_str(h);
            s.push('\n');
            s
        })
    }

    /// Inverse of [`Self::to_text`]; blank and non-hex lines are ignored.
    #[must_use]
    pub fn from_text(text: &str) -> Self {
        let enabled = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && l.bytes().all(|b| b.is_ascii_hexdigit()))
            .map(str::to_string)
            .collect();
        Self { enabled }
    }
}

/// The renderer-side view of one game's HD tiles.
#[derive(Debug)]
pub struct HdTileLayer {
    enabled: bool,
    set: Option<HdAssetSet>,
    lookups: Cell<u64>,
}

impl HdTileLayer {
    /// A layer that never draws anything (accuracy mode).
    #[must_use]
    pub fn off() -> Self {
        Self {
            enabled: false,
            set: None,
            lookups: Cell::new(0),
        }
    }

    /// `enabled` is the game's toggle; `set` the cached assets, if any.
    #[must_use]
    pub fn new(enabled: bool, set: Option<HdAssetSet>) -> Self {
        Self {
            enabled,
            set,
            lookups: Cell::new(0),
        }
    }

    #[must_use]
    pub fn is_active(&self) -> bool {
        self.enabled && self.set.is_some()
    }

    /// Scale of the loaded set, 1 when inactive.
    #[must_use]
    pub fn scale(&self) -> u32 {
        if self.is_active() {
            self.set.as_ref().map_or(1, |s| s.scale.max(1))
        } else {
            1
        }
    }

    /// Hash lookups performed since creation (the draw-time cost unit).
    #[must_use]
    pub fn lookups(&self) -> u64 {
        self.lookups.get()
    }

    /// One lookup by tile content hash. Inactive layers return `None`
    /// without touching the map or the counter.
    #[must_use]
    pub fn lookup(&self, tile_hash: &str) -> Option<&HdTile> {
        if !self.enabled {
            return None;
        }
        let set = self.set.as_ref()?;
        self.lookups.set(self.lookups.get() + 1);
        set.get(tile_hash)
    }
}

/// A native-resolution tile position on screen, with the content hash the
/// core side computed for it.
#[derive(Debug, Clone, Copy)]
pub struct TilePlacement<'a> {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
    pub hash: &'a str,
}

/// Nearest-neighbour upscale of `native_rgba` by `scale`, then each
/// placement whose hash has an HD tile of exactly `width*scale` by
/// `height*scale` is blitted over it (straight-alpha blend). Placements
/// without an HD tile keep their nearest-neighbour pixels. With an
/// inactive layer the result is the plain nearest-neighbour frame at
/// scale 1 -- native pixels untouched.
#[must_use]
pub fn composite_hd_tiles(
    native_rgba: &[u8],
    width: usize,
    height: usize,
    placements: &[TilePlacement<'_>],
    layer: &HdTileLayer,
) -> Vec<u8> {
    let scale = layer.scale() as usize;
    let (out_w, out_h) = (width * scale, height * scale);
    let mut out = vec![0u8; out_w * out_h * 4];
    for (oy, row) in out.chunks_exact_mut(out_w * 4).enumerate() {
        for (ox, px) in row.chunks_exact_mut(4).enumerate() {
            let n = ((oy / scale) * width + ox / scale) * 4;
            px.copy_from_slice(&native_rgba[n..n + 4]);
        }
    }
    if !layer.is_active() {
        return out;
    }
    for p in placements {
        let Some(tile) = layer.lookup(p.hash) else {
            continue;
        };
        let (tw, th) = (tile.width as usize, tile.height as usize);
        if tw != p.width * scale || th != p.height * scale {
            continue; // wrong size for this placement: keep native
        }
        for ty in 0..th {
            let oy = p.y * scale + ty;
            if oy >= out_h {
                break;
            }
            for tx in 0..tw {
                let ox = p.x * scale + tx;
                if ox >= out_w {
                    break;
                }
                let s = (ty * tw + tx) * 4;
                let d = (oy * out_w + ox) * 4;
                let a = u32::from(tile.rgba[s + 3]);
                for c in 0..3 {
                    let v =
                        (u32::from(tile.rgba[s + c]) * a + u32::from(out[d + c]) * (255 - a) + 127)
                            / 255;
                    out[d + c] = v as u8;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn set() -> HdAssetSet {
        let mut tiles = BTreeMap::new();
        tiles.insert(
            "t1".to_string(),
            HdTile::new(4, 4, [200, 100, 0, 255].repeat(16)).unwrap(),
        );
        HdAssetSet {
            scale: 2,
            model_id: "stub".into(),
            tiles,
        }
    }

    /// 4x2 native frame, all one colour.
    fn native() -> Vec<u8> {
        [10, 20, 30, 255].repeat(8)
    }

    fn place(hash: &str) -> TilePlacement<'_> {
        TilePlacement {
            x: 2,
            y: 0,
            width: 2,
            height: 2,
            hash,
        }
    }

    #[test]
    fn toggles_default_off_and_round_trip() {
        let mut t = HdTileToggles::default();
        assert!(!t.is_enabled("ab12"));
        t.set("ab12", true);
        t.set("cd34", true);
        t.set("cd34", false);
        let back = HdTileToggles::from_text(&t.to_text());
        assert!(back.is_enabled("ab12") && !back.is_enabled("cd34"));
        assert_eq!(back, t);
    }

    #[test]
    fn toggle_off_means_no_lookup_and_native_pixels() {
        let layer = HdTileLayer::new(false, Some(set()));
        let out = composite_hd_tiles(&native(), 4, 2, &[place("t1")], &layer);
        assert_eq!(out, native(), "scale 1, untouched");
        assert_eq!(layer.lookups(), 0);
        assert!(layer.lookup("t1").is_none());
        assert_eq!(layer.lookups(), 0);
        // No assets loaded behaves the same.
        let none = HdTileLayer::new(true, None);
        assert_eq!(
            composite_hd_tiles(&native(), 4, 2, &[place("t1")], &none),
            native()
        );
        assert_eq!(HdTileLayer::off().lookups(), 0);
    }

    #[test]
    fn toggle_on_draws_the_hd_tile_and_leaves_the_rest() {
        let layer = HdTileLayer::new(true, Some(set()));
        let out = composite_hd_tiles(&native(), 4, 2, &[place("t1")], &layer);
        assert_eq!(out.len(), 8 * 4 * 4);
        assert_eq!(&out[0..4], &[10, 20, 30, 255], "outside the tile: native");
        let at = (2 * 8 + 4) * 4;
        assert_eq!(&out[at..at + 4], &[200, 100, 0, 255], "inside: HD");
    }

    #[test]
    fn same_hash_draws_identically_everywhere() {
        let layer = HdTileLayer::new(true, Some(set()));
        let a = TilePlacement {
            x: 0,
            ..place("t1")
        };
        let b = place("t1");
        let out = composite_hd_tiles(&native(), 4, 2, &[a, b], &layer);
        let px = |x: usize, y: usize| out[(y * 8 + x) * 4..(y * 8 + x) * 4 + 4].to_vec();
        for (dx, dy) in [(0, 0), (3, 3)] {
            assert_eq!(px(dx, dy), px(4 + dx, dy));
        }
    }

    /// The draw-time cost bound: drawing N placements performs exactly N
    /// hash lookups (misses included), nothing proportional to the tile
    /// pixels or the model, and the crate cannot reach a model at all.
    #[test]
    fn draw_cost_is_one_lookup_per_placement_and_no_model_dependency() {
        let layer = HdTileLayer::new(true, Some(set()));
        let ps = [place("t1"), place("missing"), place("t1")];
        let _ = composite_hd_tiles(&native(), 4, 2, &ps, &layer);
        assert_eq!(layer.lookups(), 3);

        let manifest = include_str!("../Cargo.toml");
        let dep = |name: &str| {
            manifest
                .lines()
                .any(|l| l.trim_start().starts_with(&format!("{name} =")))
        };
        assert!(!dep("rf-ai"), "renderer must not link the model crate");
        assert!(!dep("ort"), "no inference runtime in the renderer");
    }

    #[test]
    fn wrong_sized_tile_is_ignored() {
        let layer = HdTileLayer::new(true, Some(set()));
        let bad = TilePlacement {
            width: 1,
            ..place("t1")
        };
        let out = composite_hd_tiles(&native(), 4, 2, &[bad], &layer);
        let at = (2 * 8 + 4) * 4;
        assert_eq!(&out[at..at + 4], &[10, 20, 30, 255]);
    }
}
