//! Ticket W35-16: the game's own objects in the widescreen margins -- only the part of an object the game
//! itself placed past the picture's edge (`docs/research/W35-16-margin-sprites.md`).
//!
//! Pure and console-free: the shell decodes a machine's object table into `(index, x, width)` triples, and
//! this module decides WHICH objects may be painted past the edge and copies their dots. Nothing here
//! guesses: an object's position is its table entry's, and a dot is the core's own pixel.
//!
//! Hardware (fullsnes "SNES PPU Sprites (OBJs)"): an entry's X is 9-bit signed, so it can hang off the left
//! edge by up to its width and off the right by at most `width - 1`. Games cull the rest themselves, so this
//! can complete an object that is already entering; it cannot show one before the game writes it.
//!
//! **The rule (bsnes-hd's `safe`, [`crate::widescreen::ObjPolicy::Safe`]):** an entry is painted past the edge
//! only when part of it is on the picture. An entry wholly off it is a parked sprite, or a different
//! object altogether, and painting it would put things in the margin where the game has none.

/// [`SpriteSet`] holds the 128 SNES object indices.
pub type SpriteSet = u128;

/// A dot no sprite won (the value `rf_renderer::frame::NO_SPRITE` also uses).
pub const NO_SPRITE: u8 = 0xFF;

/// Whether an entry of width `w` at screen `x` is partly on a picture `picture_w` dots wide AND partly past
/// one of its edges.
#[must_use]
pub const fn straddles(x: i32, w: i32, picture_w: i32) -> bool {
    x + w > 0 && x < picture_w && (x < 0 || x + w > picture_w)
}

/// The entries `(index, x, width)` that straddle an edge of a `picture_w`-dot picture. Indices >= 128 are
/// ignored (the set holds the SNES's 128).
#[must_use]
pub fn edge_straddlers(
    entries: impl IntoIterator<Item = (u8, i32, i32)>,
    picture_w: i32,
) -> SpriteSet {
    entries
        .into_iter()
        .filter(|&(i, x, w)| i < 128 && straddles(x, w, picture_w))
        .fold(0, |set, (i, _, _)| set | (1u128 << i))
}

/// Per dot: 1 where the sprite that won it (`ids`, [`NO_SPRITE`] for none) is in `set`, else 0.
#[must_use]
pub fn mask_of(ids: &[u8], set: SpriteSet) -> Vec<u8> {
    ids.iter()
        .map(|&id| u8::from(id < 128 && set >> id & 1 != 0))
        .collect()
}

/// Copy the core's own dots into the margins of `out` (`out_w` x `h` RGBA, already filled): every dot of
/// `native` (`native_w` wide, centred in `out`) whose `mask` entry is set and that lies OUTSIDE the central
/// `keep_w` dots (the 256 hardware dots, or a patch's whole width; those are never touched). Same geometry
/// as `widescreen::fill_margins_keeping`. Returns how many dots were painted.
pub fn paint(
    out: &mut [u8],
    out_w: usize,
    h: usize,
    native: &[u8],
    native_w: usize,
    mask: &[u8],
    keep_w: usize,
) -> usize {
    let pad = (out_w as i64 - native_w as i64).div_euclid(2);
    let keep_start = (out_w as i64 - keep_w as i64).div_euclid(2);
    let central = keep_start..keep_start + keep_w as i64;
    let mut painted = 0;
    // Bounded `for` ranges only (law 8): every native dot is visited once.
    for y in 0..h {
        for nx in 0..native_w {
            let m = y * native_w + nx;
            if mask.get(m) != Some(&1) {
                continue;
            }
            let ox = nx as i64 + pad;
            if ox < 0 || ox >= out_w as i64 || central.contains(&ox) {
                continue;
            }
            let (src, dst) = (m * 4, (y * out_w + ox as usize) * 4);
            if let (Some(s), Some(d)) = (native.get(src..src + 3), out.get_mut(dst..dst + 4)) {
                d[..3].copy_from_slice(s);
                d[3] = 0xFF;
                painted += 1;
            }
        }
    }
    painted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_entry_part_on_the_picture_and_part_past_an_edge_straddles() {
        // Wholly inside, wholly off either side, and parked far left: none straddle.
        assert!(!straddles(100, 16, 256));
        assert!(!straddles(-16, 16, 256), "ends exactly at the edge");
        assert!(!straddles(-64, 16, 256), "parked off the left");
        assert!(!straddles(256, 16, 256), "starts exactly at the right edge");
        assert!(!straddles(-180, 16, 256));
        // Across the left edge, across the right edge.
        assert!(straddles(-8, 16, 256));
        assert!(straddles(-15, 16, 256));
        assert!(straddles(248, 16, 256));
        assert!(
            straddles(255, 16, 256),
            "the farthest the hardware allows: width - 1 past"
        );
        // A big sprite spanning the whole picture straddles both edges.
        assert!(straddles(-8, 300, 256));
    }

    #[test]
    fn the_set_holds_straddlers_by_index_and_ignores_the_rest() {
        let set = edge_straddlers(
            [
                (3u8, -8, 16),
                (4, 100, 16),
                (5, 250, 16),
                (6, -64, 16),
                (200, -8, 16),
            ],
            256,
        );
        assert_eq!(set, (1 << 3) | (1 << 5));
    }

    #[test]
    fn the_mask_follows_the_sprite_that_won_each_dot() {
        let set: SpriteSet = 1 << 7;
        assert_eq!(
            mask_of(&[7, 8, NO_SPRITE, 7, 200], set),
            vec![1, 0, 0, 1, 0]
        );
    }

    fn solid(w: usize, h: usize, v: u8) -> Vec<u8> {
        (0..w * h).flat_map(|_| [v, v, v, 0xFF]).collect()
    }

    #[test]
    fn paint_copies_margin_dots_only_and_never_the_central_strip() {
        // Native 8 wide in a 12-wide output, keep the central 4: pad 2, central output dots 4..8.
        let (nw, ow, h) = (8usize, 12usize, 1usize);
        let native = solid(nw, h, 9);
        let mut out = solid(ow, h, 1);
        // Native dots 0 (output 2, margin), 3 (output 5, central), 7 (output 9, margin), all set.
        let mut mask = vec![0u8; nw];
        for i in [0, 3, 7] {
            mask[i] = 1;
        }
        let n = paint(&mut out, ow, h, &native, nw, &mask, 4);
        assert_eq!(n, 2);
        let px = |x: usize| out[x * 4];
        assert_eq!((px(2), px(5), px(9)), (9, 1, 9));
        assert_eq!((px(0), px(3), px(8)), (1, 1, 1), "unset dots keep the fill");
    }

    #[test]
    fn an_all_zero_mask_changes_nothing() {
        let native = solid(8, 2, 9);
        let mut out = solid(12, 2, 1);
        let before = out.clone();
        assert_eq!(paint(&mut out, 12, 2, &native, 8, &[0; 16], 4), 0);
        assert_eq!(out, before);
    }

    #[test]
    fn a_short_mask_or_native_is_survived() {
        let mut out = solid(12, 2, 1);
        // Mask claims a dot past the native buffer: no panic, nothing painted.
        assert_eq!(paint(&mut out, 12, 2, &[0; 4], 8, &[1; 16], 4), 1);
    }
}
