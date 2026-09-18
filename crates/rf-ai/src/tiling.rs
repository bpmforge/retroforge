//! Tile-and-blend an image through a fixed-size model (ticket W16-12;
//! `docs/design/AI_UPSCALING.md`).
//!
//! **Deliberately NOT feature-gated behind `onnx`.** [`crate::onnx`] is the
//! only caller today, but the algorithm here has no dependency on ONNX,
//! `ort`, or any runtime at all — it is a pure function over
//! [`crate::upscale::Rgba8`] and a caller-supplied "run these tiles"
//! closure. Keeping it unconditional means `cargo test -p rf-ai` (no
//! `onnx` feature, no model, no dylib) exercises every line the gate
//! actually needs to trust: the seam maths, the reflection padding, the
//! degenerate sizes. [`crate::upscale::NearestUpscaler`] stands in for the
//! "tile model" in this file's own tests, exactly as it does everywhere
//! else in this crate (see `docs/design/AI_UPSCALING.md` §1's table).
//!
//! ## Why overlap-and-feather rather than overlap-and-discard
//!
//! A common tiling scheme runs oversized tiles and *discards* a border
//! strip from each before stitching, trusting only each tile's interior.
//! That would work here too, but it wastes every pixel in the discarded
//! border on inference that is then thrown away. Feathering keeps all of
//! it: every tile's border pixels contribute to the final image, weighted
//! down near a seam and weighted up away from one, so two overlapping
//! tiles' predictions blend into one continuous value instead of one of
//! them winning outright.
//!
//! ## Why normalization, not exact-weight construction, does the real work
//!
//! [`feather_weight`]'s ramp is built so that two tiles overlapping by
//! EXACTLY `pad` pixels contribute weights that sum to 1 at every shared
//! pixel — see its own doc and the `feather_weights_sum_to_one` test.  But
//! [`tile_starts`] cannot always deliver that exact overlap: the last tile
//! in a row is clamped flush with the image's far edge so it never reads
//! past it, and when the image length does not divide evenly into steps
//! that clamp can make the last pair of tiles overlap by *more* than
//! `pad`. In that stretch the raw weights would sum to something other
//! than 1. [`tile_and_blend`] therefore always divides the accumulated
//! colour by the accumulated weight at the end — the exact-sum-to-one case
//! is what makes that division a no-op (weight sum already 1), and the
//! clamped case is what makes it load-bearing (weight sum >1, division
//! corrects it back to a proper weighted average). Both cases go through
//! the same arithmetic; there is no special-cased branch for either.

use crate::upscale::{Rgba8, UpscaleError};

/// Overlap, in source pixels, between adjacent tiles. The default a caller
/// gets from [`crate::onnx::OnnxUpscaler`] unless it is configured
/// otherwise.
pub const DEFAULT_PAD: u32 = 8;

/// Starting offsets (in source pixels) of tiles of width `tile` covering
/// `[0, len)`, each overlapping its predecessor by `pad` pixels.
///
/// A `Vec` built from a computed length rather than a hand-rolled
/// `while`/`loop` (CLAUDE.md law 8): the count of tiles is known up front
/// from `len`, `tile` and `step`, so the iteration is a `map` over a
/// `Range` and termination is structural, not something a reader has to
/// verify step by step.
///
/// `len <= tile` (the "smaller than the tile" case) returns `[0]`: a
/// single tile whose extra rows/columns beyond `len` are supplied by
/// reflection ([`reflect_index`]) when the caller extracts pixels, not by
/// this function.
fn tile_starts(len: u32, tile: u32, pad: u32) -> Vec<u32> {
    if len <= tile {
        return vec![0];
    }
    // `pad < tile` is an invariant this module enforces before calling
    // here (see `tile_and_blend`), so `step` is always positive.
    let step = tile - pad;
    let n = 1 + (len - tile).div_ceil(step);
    (0..n)
        .map(|i| {
            let x = i * step;
            // Clamp the last tile flush with the far edge instead of
            // letting it run past `len` — this is the irregular-overlap
            // case the module doc's normalization note is about.
            x.min(len - tile)
        })
        .collect()
}

/// Reflect an out-of-range coordinate back into `[0, len)` ("reflect-101":
/// the edge pixel is not repeated, matching e.g. `numpy.pad(mode="reflect")`).
///
/// Used for BOTH edge padding (a source smaller than the tile, or a tile
/// straddling the image border) and has no notion of tiling itself — it
/// is just an index transform, which is why it takes and returns bare
/// coordinates rather than any tile state.
fn reflect_index(i: i64, len: u32) -> u32 {
    if len <= 1 {
        return 0;
    }
    let len_i = i64::from(len);
    let period = 2 * (len_i - 1);
    let mut m = i.rem_euclid(period);
    if m >= len_i {
        m = period - m;
    }
    m as u32
}

/// Weight this tile contributes at local offset `local` (0-based, within
/// `[0, tile)`) along one axis.
///
/// Full weight (`1.0`) through the interior. Ramps DOWN toward a
/// neighbour on either side the tile actually has one — `at_start`/`at_end`
/// say whether this tile is the first/last along the axis, in which case
/// there is no neighbour on that side and the tile keeps full weight all
/// the way to the image border (an edge tile must not be dimmed just
/// because it happens to be `pad` pixels wide at its border).
///
/// The ramp is built so that, for two tiles overlapping by exactly `pad`
/// pixels, `exiting-tile's weight at offset k` + `entering-tile's weight
/// at the matching offset` `== 1.0` for every `k` in `0..pad` — see
/// `feather_weights_sum_to_one`. Using `k+1` and `pad-k` over `pad+1`
/// (rather than `k`/`pad-1-k` over `pad`) means neither ramp ever touches
/// exactly `0.0`, so no pixel this tile covers is ever given zero weight.
fn feather_weight(local: u32, tile: u32, pad: u32, at_start: bool, at_end: bool) -> f32 {
    if pad == 0 || tile <= 2 * pad {
        return 1.0;
    }
    let denom = f32::from(pad as u16) + 1.0;
    let mut w = 1.0f32;
    if !at_start && local < pad {
        // Entering ramp: rises from 1/(pad+1) toward 1.
        w = w.min((f32::from(local as u16) + 1.0) / denom);
    }
    if !at_end && local >= tile - pad {
        // Exiting ramp: falls from 1 toward 1/(pad+1).
        let k = local - (tile - pad);
        w = w.min((f32::from(pad as u16) - f32::from(k as u16)) / denom);
    }
    w
}

/// Extract one `tile_w x tile_h` tile from `src` at source offset
/// `(x0, y0)`, reflecting any coordinate that falls outside `src`'s
/// bounds (either because the tile straddles an edge, or because `src` is
/// smaller than the tile).
fn extract_tile(src: &Rgba8, x0: i64, y0: i64, tile_w: u32, tile_h: u32) -> Rgba8 {
    let mut pixels = vec![0u8; (tile_w * tile_h * 4) as usize];
    for ty in 0..tile_h {
        let sy = reflect_index(y0 + i64::from(ty), src.height);
        for tx in 0..tile_w {
            let sx = reflect_index(x0 + i64::from(tx), src.width);
            let dst = ((ty * tile_w + tx) * 4) as usize;
            let px = src.pixel(sx, sy).expect("reflect_index stays in bounds");
            pixels[dst..dst + 4].copy_from_slice(&px);
        }
    }
    Rgba8::new(tile_w, tile_h, pixels).expect("tile has the declared buffer length")
}

/// Tile `src` into `tile_w x tile_h` pieces overlapping by `pad` pixels,
/// run them through `run` in batches of at most `batch`, and blend the
/// (already-upscaled, by `scale`) results back into one
/// `src.width*scale x src.height*scale` image.
///
/// `run` receives a batch of 1..=`batch` tiles and must return exactly as
/// many outputs, each `tile_w*scale x tile_h*scale`, in the same order —
/// this is the seam between this pure module and whatever actually runs
/// the model ([`crate::onnx::OnnxUpscaler`] batches when the model's batch
/// dimension is dynamic and `batch > 1`; every ledgered model today has a
/// fixed batch of 1, so its caller always passes `batch == 1`, but the
/// path is one code path either way rather than a separate branch for
/// each — see `feather_weights_sum_to_one` and
/// `tiled_matches_whole_image_for_a_batching_model` for both cases
/// exercised without any model at all).
///
/// # Errors
/// Whatever `run` returns; [`UpscaleError::ZeroScale`] if `scale` is 0.
pub fn tile_and_blend<F>(
    src: &Rgba8,
    tile_w: u32,
    tile_h: u32,
    pad: u32,
    scale: u32,
    batch: usize,
    mut run: F,
) -> Result<Rgba8, UpscaleError>
where
    F: FnMut(&[Rgba8]) -> Result<Vec<Rgba8>, UpscaleError>,
{
    if scale == 0 {
        return Err(UpscaleError::ZeroScale);
    }
    let pad = pad
        .min(tile_w.saturating_sub(1) / 2)
        .min(tile_h.saturating_sub(1) / 2);

    let xs = tile_starts(src.width, tile_w, pad);
    let ys = tile_starts(src.height, tile_h, pad);
    let (n_x, n_y) = (xs.len(), ys.len());

    // Every (x, y) tile position, in row-major order, alongside whether it
    // is the first/last along each axis — needed by `feather_weight` so an
    // outer tile is not dimmed at the image border.
    let positions: Vec<(u32, u32, bool, bool, bool, bool)> = ys
        .iter()
        .enumerate()
        .flat_map(|(yi, &y)| {
            xs.iter()
                .enumerate()
                .map(move |(xi, &x)| (x, y, xi == 0, xi == n_x - 1, yi == 0, yi == n_y - 1))
        })
        .collect();

    let out_w = src.width * scale;
    let out_h = src.height * scale;
    let mut accum = vec![0f32; out_w as usize * out_h as usize * 3];
    let mut weight_sum = vec![0f32; out_w as usize * out_h as usize];

    for chunk in positions.chunks(batch.max(1)) {
        let tiles: Vec<Rgba8> = chunk
            .iter()
            .map(|&(x, y, ..)| extract_tile(src, i64::from(x), i64::from(y), tile_w, tile_h))
            .collect();
        let outputs = run(&tiles)?;
        if outputs.len() != chunk.len() {
            return Err(UpscaleError::Runtime {
                detail: format!(
                    "tile runner returned {} outputs for {} inputs",
                    outputs.len(),
                    chunk.len()
                ),
            });
        }
        for (&(x, y, at_x_start, at_x_end, at_y_start, at_y_end), out) in chunk.iter().zip(outputs)
        {
            let want = (tile_w * scale, tile_h * scale);
            if (out.width, out.height) != want {
                return Err(UpscaleError::Runtime {
                    detail: format!(
                        "tile at ({x},{y}) came back {}x{}, expected {}x{}",
                        out.width, out.height, want.0, want.1
                    ),
                });
            }
            let scaled_pad = pad * scale;
            for ty in 0..out.height {
                let wy = feather_weight(ty, out.height, scaled_pad, at_y_start, at_y_end);
                let dst_y = y * scale + ty;
                if dst_y >= out_h {
                    continue;
                }
                for tx in 0..out.width {
                    let wx = feather_weight(tx, out.width, scaled_pad, at_x_start, at_x_end);
                    let dst_x = x * scale + tx;
                    if dst_x >= out_w {
                        continue;
                    }
                    let w = wx * wy;
                    let px = out.pixel(tx, ty).expect("in bounds by construction");
                    let di = (dst_y as usize * out_w as usize + dst_x as usize) * 3;
                    let wi = dst_y as usize * out_w as usize + dst_x as usize;
                    accum[di] += f32::from(px[0]) * w;
                    accum[di + 1] += f32::from(px[1]) * w;
                    accum[di + 2] += f32::from(px[2]) * w;
                    weight_sum[wi] += w;
                }
            }
        }
    }

    let mut pixels = Vec::with_capacity(out_w as usize * out_h as usize * 4);
    for i in 0..(out_w as usize * out_h as usize) {
        let w = weight_sum[i];
        // Every output pixel is covered by at least one tile with
        // strictly positive weight (feather_weight never returns exactly
        // 0 — see its own doc), so this is never a division by zero.
        pixels.push(to_u8(accum[i * 3] / w));
        pixels.push(to_u8(accum[i * 3 + 1] / w));
        pixels.push(to_u8(accum[i * 3 + 2] / w));
        pixels.push(255); // Alpha is not this module's job; see crate::onnx.
    }
    Rgba8::new(out_w, out_h, pixels)
}

/// Round (not truncate — see the module doc's exactness note) a
/// normalized float back into a byte.
fn to_u8(v: f32) -> u8 {
    v.clamp(0.0, 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::upscale::{NearestUpscaler, Upscaler};

    fn checker(w: u32, h: u32) -> Rgba8 {
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                pixels[i] = ((x * 37 + y * 11) % 256) as u8;
                pixels[i + 1] = ((x * 5 + y * 71) % 256) as u8;
                pixels[i + 2] = ((x * 97 + y * 3) % 256) as u8;
                pixels[i + 3] = 255;
            }
        }
        Rgba8::new(w, h, pixels).unwrap()
    }

    /// Run tiles one at a time through `NearestUpscaler`, standing in for
    /// a fixed-batch-of-1 model — the shape every ledgered model actually
    /// has today.
    fn nearest_runner(scale: u32) -> impl FnMut(&[Rgba8]) -> Result<Vec<Rgba8>, UpscaleError> {
        let up = NearestUpscaler::new(scale).unwrap();
        move |tiles: &[Rgba8]| tiles.iter().map(|t| up.upscale(t)).collect()
    }

    #[test]
    fn feather_weights_sum_to_one_across_a_matched_overlap() {
        // The core seam-blending guarantee, asserted directly on the raw
        // ramp rather than through a full tiled image: for two tiles
        // overlapping by EXACTLY `pad`, the exiting tile's weight at
        // offset k plus the entering tile's weight at the same offset is
        // 1 for every k in the overlap.
        let tile = 64;
        let pad = 8;
        for k in 0..pad {
            let exiting = feather_weight(tile - pad + k, tile, pad, false, false);
            let entering = feather_weight(k, tile, pad, false, false);
            assert!(
                (exiting + entering - 1.0).abs() < 1e-6,
                "k={k}: exiting={exiting} entering={entering}"
            );
        }
    }

    #[test]
    fn feather_weight_never_reaches_zero() {
        // A zero-weight pixel would make normalization degenerate if it
        // were the pixel's ONLY contributor.
        let tile = 64;
        let pad = 8;
        for local in 0..tile {
            let w = feather_weight(local, tile, pad, false, false);
            assert!(w > 0.0, "local={local} got weight {w}");
        }
    }

    #[test]
    fn edge_tiles_keep_full_weight_at_the_image_border() {
        // A tile at the start/end of an axis has no neighbour on that
        // side, so it must not be dimmed there.
        let tile = 64;
        let pad = 8;
        assert_eq!(feather_weight(0, tile, pad, true, false), 1.0);
        assert_eq!(feather_weight(tile - 1, tile, pad, false, true), 1.0);
    }

    #[test]
    fn tiled_matches_whole_image_exactly_for_an_exact_multiple_size() {
        // NearestUpscaler is scale()-exact and context-independent (every
        // tile's replicated pixels do not depend on its neighbours), so
        // tiling it and blending the overlaps must reproduce the SAME
        // bytes as one whole-image call, not just a close approximation.
        let src = checker(128, 64); // exactly 2x1 tiles of 64x64, no reflection needed
        let scale = 3;
        let whole = NearestUpscaler::new(scale).unwrap().upscale(&src).unwrap();
        let tiled =
            tile_and_blend(&src, 64, 64, DEFAULT_PAD, scale, 1, nearest_runner(scale)).unwrap();
        assert_eq!(tiled, whole);
    }

    #[test]
    fn tiled_matches_whole_image_exactly_for_a_non_multiple_size() {
        // The irregular-overlap case the module doc calls out: 96 does
        // not divide evenly into 64-tile/8-pad steps, so the last tile
        // pair overlaps by more than `pad` and normalization is load-
        // bearing, not a no-op.
        let src = checker(96, 50);
        let scale = 2;
        let whole = NearestUpscaler::new(scale).unwrap().upscale(&src).unwrap();
        let tiled =
            tile_and_blend(&src, 64, 64, DEFAULT_PAD, scale, 1, nearest_runner(scale)).unwrap();
        assert_eq!(tiled, whole);
    }

    #[test]
    fn tiled_matches_whole_image_for_a_source_smaller_than_the_tile() {
        // "sizes smaller than the tile work" (acceptance 1): reflection
        // padding fills the rest of the tile, and the output is cropped
        // back to the source's own (scaled) size.
        let src = checker(20, 13);
        let scale = 4;
        let whole = NearestUpscaler::new(scale).unwrap().upscale(&src).unwrap();
        let tiled =
            tile_and_blend(&src, 64, 64, DEFAULT_PAD, scale, 1, nearest_runner(scale)).unwrap();
        assert_eq!(tiled, whole);
    }

    #[test]
    fn tiled_matches_whole_image_for_an_odd_size_smaller_on_one_axis_only() {
        let src = checker(96, 31); // wider than the tile, shorter than it
        let scale = 2;
        let whole = NearestUpscaler::new(scale).unwrap().upscale(&src).unwrap();
        let tiled =
            tile_and_blend(&src, 64, 64, DEFAULT_PAD, scale, 1, nearest_runner(scale)).unwrap();
        assert_eq!(tiled, whole);
    }

    #[test]
    fn tiled_matches_whole_image_when_batched() {
        // The dynamic-batch-dimension path: every tile position is handed
        // to `run` in one call instead of one at a time. NearestUpscaler
        // doesn't care about batching, so this asserts the SAME bytes
        // come back either way — the only thing exercising this is the
        // batch size passed to `tile_and_blend`.
        let src = checker(150, 96);
        let scale = 2;
        let whole = NearestUpscaler::new(scale).unwrap().upscale(&src).unwrap();
        let up = NearestUpscaler::new(scale).unwrap();
        let batched_runner = |tiles: &[Rgba8]| tiles.iter().map(|t| up.upscale(t)).collect();
        let tiled = tile_and_blend(&src, 64, 64, DEFAULT_PAD, scale, 8, batched_runner).unwrap();
        assert_eq!(tiled, whole);
    }

    #[test]
    fn reflect_index_never_repeats_the_edge_pixel() {
        // reflect-101: index -1 maps to 1, not 0 (0 would be a repeat).
        assert_eq!(reflect_index(-1, 10), 1);
        assert_eq!(reflect_index(-2, 10), 2);
        assert_eq!(reflect_index(10, 10), 8);
        assert_eq!(reflect_index(0, 10), 0);
        assert_eq!(reflect_index(9, 10), 9);
    }

    #[test]
    fn reflect_index_handles_a_length_of_one() {
        assert_eq!(reflect_index(-5, 1), 0);
        assert_eq!(reflect_index(5, 1), 0);
    }

    #[test]
    fn tile_and_blend_refuses_zero_scale() {
        let src = checker(8, 8);
        let e = tile_and_blend(&src, 64, 64, DEFAULT_PAD, 0, 1, nearest_runner(1)).unwrap_err();
        assert_eq!(e, UpscaleError::ZeroScale);
    }

    #[test]
    fn tile_and_blend_surfaces_a_runner_error() {
        let src = checker(8, 8);
        let e = tile_and_blend(&src, 64, 64, DEFAULT_PAD, 2, 1, |_: &[Rgba8]| {
            Err(UpscaleError::Runtime {
                detail: "boom".to_string(),
            })
        })
        .unwrap_err();
        assert_eq!(
            e,
            UpscaleError::Runtime {
                detail: "boom".to_string()
            }
        );
    }

    #[test]
    fn tile_starts_covers_the_image_with_no_gap() {
        let xs = tile_starts(200, 64, 8);
        assert_eq!(xs[0], 0);
        assert_eq!(*xs.last().unwrap() + 64, 200);
        for w in xs.windows(2) {
            assert!(w[1] > w[0], "starts must strictly advance");
            assert!(w[1] < w[0] + 64, "adjacent tiles must overlap, not gap");
        }
    }

    #[test]
    fn tile_starts_is_a_single_zero_when_smaller_than_the_tile() {
        assert_eq!(tile_starts(30, 64, 8), vec![0]);
    }
}
