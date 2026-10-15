//! HD layer compositor (ticket W30-02): a pure function that lays an HD
//! Mode 7 layer over the nearest-neighbour upscale of the native frame.
//!
//! rf-renderer never sees console state (validate-arch rule 3): the core
//! side renders the HD layer to plain RGBA and the frame sink supplies the
//! native frame plus a per-pixel "pure BG1" mask
//! ([`crate::frame::FrameBuffer::mode7_mask`]). Which LINES are Mode 7 is
//! already encoded in the HD layer's alpha -- non-Mode-7 lines are alpha 0
//! -- so no separate line list is needed here.

/// Composite `hd_rgba` over `native_rgba` upscaled by `scale`.
///
/// `native_rgba` is `width * height * 4`; `mask` is `width * height`
/// flags (true = the native pixel is BG1 with no colour math, so the HD
/// layer may replace it). `hd_rgba` is straight-alpha RGBA, `width *
/// scale` wide; it may be SHORTER than `height * scale` rows (only the
/// visible lines), and missing rows count as alpha 0. The result is
/// `width * scale` by `height * scale`, alpha 255: each output pixel is its
/// native pixel (nearest neighbour), and where the native pixel's mask is
/// true and the HD alpha is above 0 the HD RGB is blended over it by that
/// alpha (255 gives the HD colour exactly).
#[must_use]
pub fn composite_hd_layer(
    native_rgba: &[u8],
    width: usize,
    height: usize,
    mask: &[bool],
    hd_rgba: &[u8],
    scale: usize,
) -> Vec<u8> {
    let scale = scale.max(1);
    let (out_w, out_h) = (width * scale, height * scale);
    let mut out = vec![0u8; out_w * out_h * 4];
    for (oy, out_row) in out.chunks_exact_mut(out_w * 4).enumerate() {
        let ny = oy / scale;
        for (ox, px) in out_row.chunks_exact_mut(4).enumerate() {
            let nx = ox / scale;
            let n = (ny * width + nx) * 4;
            px[..3].copy_from_slice(&native_rgba[n..n + 3]);
            px[3] = 255;
            if !mask.get(ny * width + nx).copied().unwrap_or(false) {
                continue;
            }
            let h = (oy * out_w + ox) * 4;
            let Some(hd) = hd_rgba.get(h..h + 4) else {
                continue; // beyond the HD layer's rows: alpha 0
            };
            let a = u32::from(hd[3]);
            if a == 0 {
                continue;
            }
            for c in 0..3 {
                let blended = (u32::from(hd[c]) * a + u32::from(px[c]) * (255 - a) + 127) / 255;
                px[c] = blended as u8;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2x2 native frame, four distinct colours.
    fn native() -> Vec<u8> {
        vec![
            10, 20, 30, 255, 40, 50, 60, 255, //
            70, 80, 90, 255, 100, 110, 120, 255,
        ]
    }

    fn hd(rgba: [u8; 4], scale: usize) -> Vec<u8> {
        rgba.repeat(2 * scale * 2 * scale)
    }

    #[test]
    fn mask_false_keeps_the_native_pixel() {
        let out = composite_hd_layer(&native(), 2, 2, &[false; 4], &hd([255, 0, 0, 255], 2), 2);
        assert_eq!(out.len(), 4 * 4 * 4);
        assert_eq!(&out[0..4], &[10, 20, 30, 255]);
        // Nearest neighbour: output (2,2) is native (1,1).
        let at = (2 * 4 + 2) * 4;
        assert_eq!(&out[at..at + 4], &[100, 110, 120, 255]);
    }

    #[test]
    fn alpha_255_replaces_and_128_blends() {
        let mask = [true, false, false, false];
        let out = composite_hd_layer(&native(), 2, 2, &mask, &hd([200, 100, 0, 255], 2), 2);
        assert_eq!(&out[0..4], &[200, 100, 0, 255]);
        assert_eq!(&out[8..12], &[40, 50, 60, 255], "unmasked native pixel");

        let out = composite_hd_layer(&native(), 2, 2, &mask, &hd([200, 100, 0, 128], 2), 2);
        // (200*128 + 10*127 + 127)/255 = 105; (100*128 + 20*127 + 127)/255 = 60; 15.
        assert_eq!(&out[0..4], &[105, 60, 15, 255]);
        // The whole 2x2 block of the masked native pixel is blended.
        let below = (4 + 1) * 4;
        assert_eq!(&out[below..below + 4], &[105, 60, 15, 255]);
    }

    #[test]
    fn scale_1_with_an_empty_hd_layer_is_the_native_frame() {
        let out = composite_hd_layer(&native(), 2, 2, &[true; 4], &[], 1);
        assert_eq!(out, native());
    }

    #[test]
    fn a_short_hd_layer_leaves_missing_rows_native() {
        // Only the first native row's worth of HD rows (scale 2 -> 2 rows).
        let short = [9u8, 9, 9, 255].repeat(4 * 2);
        let out = composite_hd_layer(&native(), 2, 2, &[true; 4], &short, 2);
        assert_eq!(&out[0..4], &[9, 9, 9, 255]);
        let last = (3 * 4 + 3) * 4;
        assert_eq!(&out[last..last + 4], &[100, 110, 120, 255]);
    }
}
