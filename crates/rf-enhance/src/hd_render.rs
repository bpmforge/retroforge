//! Compositing a frame with an HD pack applied (ticket W11-05).
//!
//! [`crate::hdpack`] parses packs and answers "is there a rule for this
//! tile?". This module is the other half: given the tiles a frame
//! actually drew and the pack's decoded images, produce the larger
//! picture.
//!
//! ## Console-agnostic on purpose
//!
//! Nothing here mentions the NES. The caller hands over [`Placement`]s —
//! where a tile landed and what it was — exactly as
//! [`crate::widescreen`] takes a `LayerView` rather than a PPU. The
//! emulation crates may not depend on this one (`ARCHITECTURE` §6), so
//! the shell converts.
//!
//! ## Every cell is accounted for
//!
//! A cell with no rule is **upscaled, not left blank**. That is what
//! makes a partial pack usable: the replaced art appears and everything
//! else keeps playing. [`CompositeReport`] then says how much was
//! replaced and how much was not, because a pack that half-applies
//! silently is the most confusing possible outcome — some tiles change
//! and some do not, with no way to tell which was intended. That is the
//! same reasoning [`crate::hdpack::Import::summary`] already carries.

use crate::hdpack::{HdPack, TileData};

/// One decoded pack image.
///
/// This crate decodes no pixels — the caller owns an image decoder and
/// supplies the result, the same split [`crate::hdpack::ImageInfo`]
/// already uses for dimensions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackImage {
    pub width: u32,
    pub height: u32,
    /// RGBA8, `width * height * 4` long.
    pub rgba: Vec<u8>,
}

/// Where a tile landed, and what it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    /// Screen x of the tile's leftmost pixel. **Signed**: fine scrolling
    /// pushes the first tile of a line partly off the left edge, and it
    /// still has to be drawn.
    pub x: i16,
    /// Screen y of the tile's top row.
    pub y: i16,
    pub tile: TileData,
    /// The four palette bytes the pack's rules are keyed on.
    pub palette: [u8; 4],
    /// Which layer drew this tile. A replacement may only paint pixels
    /// the ORIGINAL frame drew from the same layer — see [`composite`].
    pub layer: Layer,
    /// Sprite mirroring. A pack's replacement art must be flipped the
    /// same way the original was, or a character faces the wrong way.
    pub flip_x: bool,
    pub flip_y: bool,
}

/// Which layer a tile came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    Background,
    Sprite,
}

/// What compositing did, for the UI.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CompositeReport {
    /// Tiles a rule replaced.
    pub replaced: u32,
    /// Tiles drawn from the original picture because no rule matched.
    pub unmatched: u32,
}

impl CompositeReport {
    /// A line for the UI. Never "everything is fine" when it is not.
    #[must_use]
    pub fn summary(&self) -> String {
        let total = self.replaced + self.unmatched;
        if total == 0 {
            return "no tiles drawn".to_string();
        }
        if self.unmatched == 0 {
            return format!("{} tile(s), all replaced by the pack", total);
        }
        format!(
            "{} of {} tile(s) replaced; {} drawn from the original (no rule matched)",
            self.replaced, total, self.unmatched
        )
    }
}

/// The side of one tile, in original pixels.
pub const TILE: usize = 8;

/// Composite `original` at `pack.scale`, replacing what the pack covers.
///
/// `original` is RGBA8 of `width * height`. The output is
/// `width * scale` by `height * scale`, also RGBA8.
///
/// Cells with no matching rule are scaled up by nearest-neighbour, which
/// is deliberately the plainest possible fallback: anything cleverer
/// would be a second enhancement smuggled in under this one, and law 6
/// says enhancements are opt-in individually.
#[must_use]
pub fn composite(
    pack: &HdPack,
    images: &[PackImage],
    placements: &[Placement],
    original: &[u8],
    layers: &[Layer],
    width: usize,
    height: usize,
) -> (Vec<u8>, CompositeReport) {
    let scale = pack.scale.max(1) as usize;
    let out_w = width * scale;
    let out_h = height * scale;
    let mut out = vec![0u8; out_w * out_h * 4];

    // 1. Nearest-neighbour upscale of the whole frame, so nothing is ever
    //    blank and a pack covering one tile still leaves a playable game.
    for oy in 0..out_h {
        let sy = oy / scale;
        for ox in 0..out_w {
            let sx = ox / scale;
            let src = (sy * width + sx) * 4;
            let dst = (oy * out_w + ox) * 4;
            if src + 4 <= original.len() {
                out[dst..dst + 4].copy_from_slice(&original[src..src + 4]);
            }
        }
    }

    // 2. Replace the cells the pack has art for.
    let mut report = CompositeReport::default();
    for p in placements {
        let Some(rule) = pack.lookup(&p.tile, &p.palette) else {
            report.unmatched += 1;
            continue;
        };
        let Some(img) = images.get(rule.img) else {
            // A rule pointing at an image that did not load is exactly
            // what `Import::Partial` reports; counting it as unmatched
            // keeps the tally honest rather than claiming a replacement
            // that did not happen.
            report.unmatched += 1;
            continue;
        };
        if blit(
            &mut out,
            (out_w, out_h),
            img,
            (rule.x as usize, rule.y as usize),
            (i32::from(p.x) * scale as i32, i32::from(p.y) * scale as i32),
            TILE * scale,
            rule.brightness,
            (p.flip_x, p.flip_y),
            &Mask {
                layers,
                want: p.layer,
                width,
                height,
                scale,
            },
        ) {
            report.replaced += 1;
        } else {
            report.unmatched += 1;
        }
    }
    (out, report)
}

/// Which original pixels a replacement is allowed to paint over.
///
/// **This is what makes priority work without modelling priority.** The
/// original frame already has the PPU's own answer baked in: a sprite
/// behind the background simply is not visible there. So a replacement
/// paints only where the original shows that same layer, and a
/// behind-background sprite is masked out for free — as is a background
/// tile that a sprite is standing in front of.
///
/// Without it, the two failure modes are opposite and both wrong: a
/// sprite replacement paints over the wall it is hiding behind, and a
/// background replacement erases the character standing on it.
struct Mask<'a> {
    layers: &'a [Layer],
    want: Layer,
    width: usize,
    height: usize,
    scale: usize,
}

impl Mask<'_> {
    /// May a replacement paint the OUTPUT pixel at `(x, y)`?
    fn allows(&self, x: usize, y: usize) -> bool {
        if self.layers.is_empty() {
            // No mask supplied: paint everything, which is what a caller
            // with no layer information means.
            return true;
        }
        let (sx, sy) = (x / self.scale, y / self.scale);
        if sx >= self.width || sy >= self.height {
            return false;
        }
        self.layers
            .get(sy * self.width + sx)
            .is_some_and(|l| *l == self.want)
    }
}

/// Copy one `side`x`side` block, clipping at every edge and honouring
/// both the sprite flips and the layer mask.
///
/// Returns `false` when the source rectangle is not wholly inside the
/// image — a rule whose coordinates run off its own tileset is a pack
/// authoring error, and drawing a torn fragment would hide it.
#[allow(clippy::too_many_arguments)]
fn blit(
    out: &mut [u8],
    (out_w, out_h): (usize, usize),
    img: &PackImage,
    (sx, sy): (usize, usize),
    (dx, dy): (i32, i32),
    side: usize,
    brightness: f32,
    (flip_x, flip_y): (bool, bool),
    mask: &Mask<'_>,
) -> bool {
    if sx + side > img.width as usize || sy + side > img.height as usize {
        return false;
    }
    let mut painted = false;
    for row in 0..side {
        let ty = dy + row as i32;
        if ty < 0 || ty as usize >= out_h {
            continue;
        }
        // Mirroring reads the SOURCE from the far end; the destination
        // walks forward either way, so a flipped tile still lands in the
        // same screen cell.
        let srow = if flip_y { side - 1 - row } else { row };
        for col in 0..side {
            let tx = dx + col as i32;
            if tx < 0 || tx as usize >= out_w {
                continue;
            }
            if !mask.allows(tx as usize, ty as usize) {
                continue;
            }
            let scol = if flip_x { side - 1 - col } else { col };
            let s = ((sy + srow) * img.width as usize + (sx + scol)) * 4;
            let d = (ty as usize * out_w + tx as usize) * 4;
            if s + 4 > img.rgba.len() || d + 4 > out.len() {
                continue;
            }
            // Fully transparent source pixels leave the original showing
            // through, which is how a pack draws a shape that is not a
            // full square.
            if img.rgba[s + 3] == 0 {
                continue;
            }
            for c in 0..3 {
                let v = f32::from(img.rgba[s + c]) * brightness;
                out[d + c] = v.clamp(0.0, 255.0) as u8;
            }
            out[d + 3] = img.rgba[s + 3];
            painted = true;
        }
    }
    // A rule whose every pixel was masked away did not replace anything,
    // and saying it did would overstate what the pack achieved.
    painted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hdpack::{HdPack, TileKey, TileRule};
    use std::collections::BTreeMap;

    fn pack(scale: u32, rules: Vec<TileRule>) -> HdPack {
        HdPack {
            version: 100,
            scale,
            overscan: [0; 4],
            images: vec!["t.png".into()],
            tiles: rules,
            conditions: BTreeMap::new(),
            options: Vec::new(),
            unevaluated: Vec::new(),
            unsupported: BTreeMap::new(),
        }
    }

    fn rule(tile: u32, palette: [u8; 4], x: u32, y: u32) -> TileRule {
        TileRule {
            key: TileKey {
                tile: TileData::ChrRom(tile),
                palette,
            },
            img: 0,
            x,
            y,
            brightness: 1.0,
            default_tile: false,
            condition: None,
        }
    }

    /// A tileset image whose every pixel is one colour.
    fn solid(side: u32, rgb: [u8; 3]) -> PackImage {
        let mut rgba = Vec::new();
        for _ in 0..(side * side) {
            rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 0xFF]);
        }
        PackImage {
            width: side,
            height: side,
            rgba,
        }
    }

    fn black_frame(w: usize, h: usize) -> Vec<u8> {
        let mut v = vec![0u8; w * h * 4];
        for px in v.chunks_exact_mut(4) {
            px[3] = 0xFF;
        }
        v
    }

    /// **A cell with no rule is upscaled, not blanked.** That is what
    /// makes a partial pack usable at all.
    #[test]
    fn unmatched_cells_keep_the_original_picture() {
        let p = pack(2, Vec::new());
        let mut original = black_frame(16, 16);
        // Paint the top-left original pixel red.
        original[0..4].copy_from_slice(&[0xFF, 0, 0, 0xFF]);
        let placements = vec![Placement {
            x: 0,
            y: 0,
            tile: TileData::ChrRom(1),
            palette: [0; 4],
            layer: Layer::Background,
            flip_x: false,
            flip_y: false,
        }];
        let (out, report) = composite(&p, &[], &placements, &original, &[], 16, 16);
        assert_eq!(out.len(), 32 * 32 * 4, "scale 2 doubles both axes");
        assert_eq!(report.replaced, 0);
        assert_eq!(report.unmatched, 1);
        // The red pixel became a 2x2 red block, not a hole.
        for (x, y) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            let i = (y * 32 + x) * 4;
            assert_eq!(&out[i..i + 4], &[0xFF, 0, 0, 0xFF], "at ({x},{y})");
        }
    }

    /// A matching rule replaces its cell, and only its cell.
    #[test]
    fn a_matching_rule_replaces_exactly_its_own_cell() {
        let p = pack(2, vec![rule(1, [0; 4], 0, 0)]);
        let img = solid(16, [0, 0xFF, 0]);
        let original = black_frame(16, 16);
        let placements = vec![Placement {
            x: 0,
            y: 0,
            tile: TileData::ChrRom(1),
            palette: [0; 4],
            layer: Layer::Background,
            flip_x: false,
            flip_y: false,
        }];
        let (out, report) = composite(&p, &[img], &placements, &original, &[], 16, 16);
        assert_eq!(report.replaced, 1);
        assert_eq!(report.unmatched, 0);
        // Inside the replaced 16x16 block (8 tile px * scale 2): green.
        let i = (3 * 32 + 3) * 4;
        assert_eq!(&out[i..i + 4], &[0, 0xFF, 0, 0xFF], "inside the cell");
        // Just outside it: still the original black, NOT smeared green.
        let j = (3 * 32 + 20) * 4;
        assert_eq!(
            &out[j..j + 4],
            &[0, 0, 0, 0xFF],
            "a rule must not paint past its own 8x8 cell"
        );
    }

    /// **A tile hanging off the left edge is drawn, clipped.** Fine
    /// scrolling puts one there on most frames, and dropping it would
    /// leave a bare column that flickers as the player walks.
    #[test]
    fn a_partly_offscreen_tile_is_clipped_not_dropped() {
        let p = pack(1, vec![rule(1, [0; 4], 0, 0)]);
        let img = solid(8, [0, 0, 0xFF]);
        let original = black_frame(16, 16);
        let placements = vec![Placement {
            x: -3,
            y: 0,
            tile: TileData::ChrRom(1),
            palette: [0; 4],
            layer: Layer::Background,
            flip_x: false,
            flip_y: false,
        }];
        let (out, report) = composite(&p, &[img], &placements, &original, &[], 16, 16);
        assert_eq!(report.replaced, 1, "it was drawn, not skipped");
        // x = -3 means original x 0..4 show the tile's columns 3..7.
        assert_eq!(&out[0..4], &[0, 0, 0xFF, 0xFF], "leftmost visible column");
        let j = (5 * 16 + 5) * 4;
        assert_eq!(
            &out[j..j + 4],
            &[0, 0, 0, 0xFF],
            "beyond the tile: original"
        );
    }

    /// A rule pointing outside its own tileset is an authoring error and
    /// is counted as unmatched rather than drawn as a torn fragment.
    #[test]
    fn a_rule_running_off_its_tileset_is_reported_not_drawn() {
        let p = pack(1, vec![rule(1, [0; 4], 4, 0)]); // 4 + 8 > 8
        let img = solid(8, [0xFF, 0, 0xFF]);
        let original = black_frame(16, 16);
        let placements = vec![Placement {
            x: 0,
            y: 0,
            tile: TileData::ChrRom(1),
            palette: [0; 4],
            layer: Layer::Background,
            flip_x: false,
            flip_y: false,
        }];
        let (_out, report) = composite(&p, &[img], &placements, &original, &[], 16, 16);
        assert_eq!(report.replaced, 0);
        assert_eq!(report.unmatched, 1);
    }

    /// **A replacement may only paint its own layer.**
    ///
    /// This is the bug the mask exists to prevent, and it has two
    /// opposite halves: a background replacement must not erase a sprite
    /// standing in front of it, and a sprite replacement must not paint
    /// over the wall it is hiding behind. The original frame already
    /// carries the PPU's own priority answer, so masking against it gets
    /// both right without modelling priority at all.
    #[test]
    fn a_replacement_paints_only_where_its_own_layer_shows() {
        let p = pack(1, vec![rule(1, [0; 4], 0, 0)]);
        let img = solid(8, [0, 0xFF, 0]);
        let original = black_frame(16, 16);
        // The left half of the cell is a sprite standing in front of the
        // background tile the pack replaces.
        let mut layers = vec![Layer::Background; 16 * 16];
        for y in 0..16 {
            for x in 0..4 {
                layers[y * 16 + x] = Layer::Sprite;
            }
        }
        let placements = vec![Placement {
            x: 0,
            y: 0,
            tile: TileData::ChrRom(1),
            palette: [0; 4],
            layer: Layer::Background,
            flip_x: false,
            flip_y: false,
        }];
        let (out, report) = composite(&p, &[img], &placements, &original, &layers, 16, 16);
        assert_eq!(report.replaced, 1, "the tile was still replaced");
        // Where the sprite is, the original survives.
        assert_eq!(
            &out[0..4],
            &[0, 0, 0, 0xFF],
            "a background replacement must not erase the sprite in front of it"
        );
        // Where the background shows, the pack's art is drawn.
        // Row 0, column 6 — past the sprite in the left four columns.
        let i = 6 * 4;
        assert_eq!(
            &out[i..i + 4],
            &[0, 0xFF, 0, 0xFF],
            "background was replaced"
        );
    }

    /// A rule every one of whose pixels is masked away replaced nothing,
    /// and must not be counted as a replacement.
    #[test]
    fn a_fully_masked_rule_is_not_counted_as_replaced() {
        let p = pack(1, vec![rule(1, [0; 4], 0, 0)]);
        let img = solid(8, [0, 0xFF, 0]);
        let original = black_frame(16, 16);
        let layers = vec![Layer::Sprite; 16 * 16];
        let placements = vec![Placement {
            x: 0,
            y: 0,
            tile: TileData::ChrRom(1),
            palette: [0; 4],
            layer: Layer::Background,
            flip_x: false,
            flip_y: false,
        }];
        let (_out, report) = composite(&p, &[img], &placements, &original, &layers, 16, 16);
        assert_eq!(report.replaced, 0);
        assert_eq!(report.unmatched, 1, "it matched a rule but painted nothing");
    }

    /// A flipped sprite's replacement art is mirrored the same way.
    #[test]
    fn a_flipped_sprite_mirrors_its_replacement() {
        let p = pack(1, vec![rule(1, [0; 4], 0, 0)]);
        // Left half red, right half blue, so a mirror is visible.
        let mut rgba = Vec::new();
        for _ in 0..8 {
            for x in 0..8 {
                let c = if x < 4 { [0xFF, 0, 0] } else { [0, 0, 0xFF] };
                rgba.extend_from_slice(&[c[0], c[1], c[2], 0xFF]);
            }
        }
        let img = PackImage {
            width: 8,
            height: 8,
            rgba,
        };
        let original = black_frame(16, 16);
        let mk = |flip_x: bool| Placement {
            x: 0,
            y: 0,
            tile: TileData::ChrRom(1),
            palette: [0; 4],
            layer: Layer::Background,
            flip_x,
            flip_y: false,
        };
        let one = std::slice::from_ref(&img);
        let (plain, _) = composite(&p, one, &[mk(false)], &original, &[], 16, 16);
        let (flipped, _) = composite(&p, one, &[mk(true)], &original, &[], 16, 16);
        assert_eq!(
            &plain[0..4],
            &[0xFF, 0, 0, 0xFF],
            "unflipped: red on the left"
        );
        assert_eq!(
            &flipped[0..4],
            &[0, 0, 0xFF, 0xFF],
            "flipped: the right half of the art is now on the left"
        );
    }

    /// The report must never read as complete when it is not.
    #[test]
    fn the_summary_names_what_was_not_replaced() {
        let r = CompositeReport {
            replaced: 3,
            unmatched: 7,
        };
        let s = r.summary();
        assert!(
            s.contains('3') && s.contains("10") && s.contains('7'),
            "{s}"
        );
        assert!(CompositeReport {
            replaced: 4,
            unmatched: 0
        }
        .summary()
        .contains("all replaced"),);
    }
}
