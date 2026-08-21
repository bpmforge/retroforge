//! The upscaler seam (ticket W8-10; FR-AI-001/003,
//! `docs/design/ENHANCEMENT_RUNTIME.md` §6).
//!
//! ## Why a trait and not a function that calls ONNX
//!
//! §6's v1 target is a "local ONNX ESRGAN-class model", and Brad's
//! 2026-08-20 ruling on this ticket authorised adding one. But the *pack
//! pipeline* — the keying, the animation-set coherence, the all-or-nothing
//! refusal — is the part the honesty contract actually rests on, and none
//! of it is ONNX-specific. Putting a trait here means all of it is built
//! and tested with **no model, no runtime, and no network**, so
//! `cargo test --workspace` keeps CLAUDE.md's "no external deps" promise
//! and CI never grows an artifact.
//!
//! [`NearestUpscaler`] is not a placeholder for a real implementation. It
//! is the reference the tests are written against and the fallback a build
//! without the `onnx` feature actually uses — exact, integer, and
//! bit-reproducible on every machine, which is more than the ONNX path can
//! promise (see [`Upscaler::provenance`]).
//!
//! ## Zero AI on the frame path
//!
//! FR-AI-001 is explicit about it. Nothing in this module is called by the
//! composer; it runs in the offline pack builder
//! ([`crate::pipeline`]) and its output reaches the screen only as a
//! cached, reviewed, validated pack.

/// A decoded RGBA8 image: `width * height * 4` bytes, row-major, no
/// padding.
///
/// The pipeline works in RGBA rather than indexed pixels because that is
/// what an image model consumes — but the *identity* of an asset stays
/// indexed-pixels-plus-palette ([`crate::pack::asset_hash`]). Converting
/// here rather than at the cache boundary keeps that distinction intact:
/// what is hashed is what the core produced, not what the upscaler was
/// handed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgba8 {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes. Invariant enforced by [`Rgba8::new`].
    pub pixels: Vec<u8>,
}

/// Why an upscale could not be performed.
///
/// Every variant names both sides of the mismatch. A diagnostic that says
/// only "bad size" costs the pack author a debugging session; one that
/// says what was expected and what arrived is actionable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpscaleError {
    /// Pixel buffer length disagrees with the declared dimensions.
    BadBufferLength {
        width: u32,
        height: u32,
        expected: usize,
        found: usize,
    },
    /// A zero-dimension image. Refused rather than silently producing an
    /// empty result, because an empty replacement image is exactly the
    /// half-applied pack the honesty contract forbids.
    EmptyImage { width: u32, height: u32 },
    /// A palette index the palette has no entry for.
    PaletteIndexOutOfRange { index: u8, palette_len: usize },
    /// The palette is not a whole number of RGBA quads.
    MalformedPalette { len: usize },
    /// A scale factor of zero, which would erase the image.
    ZeroScale,
    /// A sheet is too small to contain the region a member should occupy
    /// at the declared scale — i.e. the upscaler did not scale by the
    /// factor it reported.
    ///
    /// Its own variant rather than a reused length error, because the two
    /// sides that matter here are the sheet the model returned and the
    /// region the layout needs; reporting a buffer length would print
    /// numbers that do not answer the question.
    SheetTooSmall {
        sheet_width: u32,
        sheet_height: u32,
        need_x: u32,
        need_y: u32,
    },
    /// The model runtime failed or is unavailable. Carries the runtime's
    /// own message — this is the only variant the `onnx` feature can
    /// produce that the pure-Rust path cannot.
    Runtime { detail: String },
}

impl std::fmt::Display for UpscaleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UpscaleError::BadBufferLength {
                width,
                height,
                expected,
                found,
            } => write!(
                f,
                "{width}x{height} needs {expected} bytes of RGBA, got {found}"
            ),
            UpscaleError::EmptyImage { width, height } => {
                write!(f, "image is empty ({width}x{height})")
            }
            UpscaleError::PaletteIndexOutOfRange { index, palette_len } => write!(
                f,
                "pixel references palette index {index}, but the palette has {palette_len} entries"
            ),
            UpscaleError::MalformedPalette { len } => write!(
                f,
                "palette is {len} bytes, which is not a whole number of RGBA quads"
            ),
            UpscaleError::ZeroScale => write!(f, "scale factor is zero"),
            UpscaleError::SheetTooSmall {
                sheet_width,
                sheet_height,
                need_x,
                need_y,
            } => write!(
                f,
                "sheet is {sheet_width}x{sheet_height}, but the layout needs {need_x}x{need_y} \
                 — the upscaler did not scale by the factor it declared"
            ),
            UpscaleError::Runtime { detail } => write!(f, "model runtime failed: {detail}"),
        }
    }
}

impl std::error::Error for UpscaleError {}

impl Rgba8 {
    /// Build an image, checking the buffer length against the dimensions.
    ///
    /// # Errors
    /// [`UpscaleError::EmptyImage`] for a zero dimension,
    /// [`UpscaleError::BadBufferLength`] if `pixels` is the wrong size.
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self, UpscaleError> {
        if width == 0 || height == 0 {
            return Err(UpscaleError::EmptyImage { width, height });
        }
        let expected = width as usize * height as usize * 4;
        if pixels.len() != expected {
            return Err(UpscaleError::BadBufferLength {
                width,
                height,
                expected,
                found: pixels.len(),
            });
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    /// Decode indexed pixels against an RGBA palette.
    ///
    /// §6: extraction is exact — "indexed pixels + palette from the core".
    /// An out-of-range index is refused rather than clamped or rendered
    /// transparent: a clamp would silently paint the wrong colour, and the
    /// whole point of hash-based matching is that we know exactly what the
    /// core produced.
    ///
    /// # Errors
    /// [`UpscaleError::MalformedPalette`],
    /// [`UpscaleError::PaletteIndexOutOfRange`], plus [`Rgba8::new`]'s.
    pub fn from_indexed(
        indexed: &[u8],
        palette: &[u8],
        width: u32,
        height: u32,
    ) -> Result<Self, UpscaleError> {
        if !palette.len().is_multiple_of(4) {
            return Err(UpscaleError::MalformedPalette { len: palette.len() });
        }
        let palette_len = palette.len() / 4;
        if width == 0 || height == 0 {
            return Err(UpscaleError::EmptyImage { width, height });
        }
        let expected = width as usize * height as usize;
        if indexed.len() != expected {
            return Err(UpscaleError::BadBufferLength {
                width,
                height,
                expected: expected * 4,
                found: indexed.len() * 4,
            });
        }

        let mut pixels = Vec::with_capacity(expected * 4);
        for &ix in indexed {
            let entry = ix as usize;
            if entry >= palette_len {
                return Err(UpscaleError::PaletteIndexOutOfRange {
                    index: ix,
                    palette_len,
                });
            }
            pixels.extend_from_slice(&palette[entry * 4..entry * 4 + 4]);
        }
        Rgba8::new(width, height, pixels)
    }

    /// The RGBA quad at `(x, y)`, or `None` when out of bounds.
    #[must_use]
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let i = (y as usize * self.width as usize + x as usize) * 4;
        Some([
            self.pixels[i],
            self.pixels[i + 1],
            self.pixels[i + 2],
            self.pixels[i + 3],
        ])
    }
}

/// Something that turns an extracted asset into a larger one.
///
/// Implementors are the *only* place a model appears. Everything upstream
/// (grouping, keying, refusal) is written against this trait, which is why
/// the pipeline's tests need no runtime.
pub trait Upscaler {
    /// What produced these pixels, for `CacheKey::model_id` and
    /// `PackManifest::model_id`. §6's "one pipeline for AI packs and
    /// artist packs" means this is never empty — it just says something
    /// different for a model than for a person.
    fn model_id(&self) -> &str;

    /// Everything *besides* the model that changes the output bytes.
    ///
    /// **This exists because float inference is not bit-reproducible.**
    /// The same ONNX graph run under a different runtime version or a
    /// different execution provider can produce different pixels, so
    /// `model_id` alone does not pin the output — and criterion 1 asks for
    /// a pack that is "reproducible from its inputs". The pipeline folds
    /// this string into the settings map
    /// ([`crate::pipeline::PROVENANCE_SETTING`]) so that a runtime change
    /// changes the cache key rather than silently changing the pixels
    /// behind an unchanged one.
    ///
    /// A pure-Rust implementation returns something fixed, because it
    /// genuinely is reproducible everywhere.
    fn provenance(&self) -> String;

    /// The integer factor this upscaler multiplies dimensions by.
    fn scale(&self) -> u32;

    /// Produce the enlarged image.
    ///
    /// # Errors
    /// Implementation-defined; see [`UpscaleError`].
    fn upscale(&self, src: &Rgba8) -> Result<Rgba8, UpscaleError>;
}

/// Exact integer nearest-neighbour scaling.
///
/// The default, and the one every pipeline test runs against. It has no
/// dependencies, cannot fail for a valid image, and produces identical
/// bytes on every machine and every build — so a pack built with it is
/// reproducible in the strongest sense, which is the baseline the ONNX
/// path is measured against rather than a stand-in for it.
///
/// Nearest-neighbour specifically, not bilinear: these are indexed-palette
/// sprites, and interpolating between two palette entries invents a colour
/// the hardware never had.
#[derive(Debug, Clone)]
pub struct NearestUpscaler {
    factor: u32,
}

impl NearestUpscaler {
    /// # Errors
    /// [`UpscaleError::ZeroScale`] if `factor` is 0, which would erase the
    /// image rather than scale it.
    pub fn new(factor: u32) -> Result<Self, UpscaleError> {
        if factor == 0 {
            return Err(UpscaleError::ZeroScale);
        }
        Ok(Self { factor })
    }
}

impl Upscaler for NearestUpscaler {
    fn model_id(&self) -> &str {
        "rf-nearest-v1"
    }

    fn provenance(&self) -> String {
        // Fixed on purpose: integer pixel replication has no runtime, no
        // float rounding and no execution provider, so there is nothing
        // here that could vary between machines.
        "exact/integer".to_string()
    }

    fn scale(&self) -> u32 {
        self.factor
    }

    fn upscale(&self, src: &Rgba8) -> Result<Rgba8, UpscaleError> {
        let f = self.factor;
        let out_w = src.width * f;
        let out_h = src.height * f;
        let mut pixels = Vec::with_capacity(out_w as usize * out_h as usize * 4);

        // Iterators, not hand-rolled indices (CLAUDE.md law 8): every
        // bound here is a range, so termination is structural rather than
        // something a reader has to verify.
        for y in 0..out_h {
            let sy = y / f;
            let row = sy as usize * src.width as usize * 4;
            for x in 0..out_w {
                let sx = (x / f) as usize;
                let i = row + sx * 4;
                pixels.extend_from_slice(&src.pixels[i..i + 4]);
            }
        }

        Rgba8::new(out_w, out_h, pixels)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2x1 image: one red pixel, one green.
    fn two_px() -> Rgba8 {
        Rgba8::new(2, 1, vec![255, 0, 0, 255, 0, 255, 0, 255]).unwrap()
    }

    #[test]
    fn new_rejects_wrong_buffer_length() {
        let e = Rgba8::new(2, 2, vec![0; 8]).unwrap_err();
        assert_eq!(
            e,
            UpscaleError::BadBufferLength {
                width: 2,
                height: 2,
                expected: 16,
                found: 8
            }
        );
    }

    #[test]
    fn new_rejects_empty() {
        assert_eq!(
            Rgba8::new(0, 4, vec![]).unwrap_err(),
            UpscaleError::EmptyImage {
                width: 0,
                height: 4
            }
        );
    }

    #[test]
    fn from_indexed_decodes_against_palette() {
        // Palette: 0 = red, 1 = green.
        let palette = [255, 0, 0, 255, 0, 255, 0, 255];
        let img = Rgba8::from_indexed(&[0, 1], &palette, 2, 1).unwrap();
        assert_eq!(img, two_px());
    }

    #[test]
    fn from_indexed_refuses_out_of_range_index_rather_than_clamping() {
        // A clamp would silently paint the wrong colour, and hash-based
        // matching only means anything if we know what the core produced.
        let palette = [255, 0, 0, 255];
        let e = Rgba8::from_indexed(&[0, 7], &palette, 2, 1).unwrap_err();
        assert_eq!(
            e,
            UpscaleError::PaletteIndexOutOfRange {
                index: 7,
                palette_len: 1
            }
        );
    }

    #[test]
    fn from_indexed_refuses_partial_palette() {
        let e = Rgba8::from_indexed(&[0], &[255, 0, 0], 1, 1).unwrap_err();
        assert_eq!(e, UpscaleError::MalformedPalette { len: 3 });
    }

    #[test]
    fn zero_scale_is_refused() {
        assert_eq!(
            NearestUpscaler::new(0).unwrap_err(),
            UpscaleError::ZeroScale
        );
    }

    #[test]
    fn nearest_replicates_pixels_exactly() {
        let up = NearestUpscaler::new(2).unwrap();
        let out = up.upscale(&two_px()).unwrap();
        assert_eq!((out.width, out.height), (4, 2));
        // Every one of the 2x2 blocks is the source pixel, unmodified.
        for y in 0..2 {
            assert_eq!(out.pixel(0, y), Some([255, 0, 0, 255]));
            assert_eq!(out.pixel(1, y), Some([255, 0, 0, 255]));
            assert_eq!(out.pixel(2, y), Some([0, 255, 0, 255]));
            assert_eq!(out.pixel(3, y), Some([0, 255, 0, 255]));
        }
    }

    #[test]
    fn nearest_invents_no_colour() {
        // The reason it is nearest-neighbour and not bilinear: an
        // interpolated pixel would be a colour the palette never had.
        let up = NearestUpscaler::new(3).unwrap();
        let out = up.upscale(&two_px()).unwrap();
        let allowed = [[255, 0, 0, 255], [0, 255, 0, 255]];
        for y in 0..out.height {
            for x in 0..out.width {
                let px = out.pixel(x, y).unwrap();
                assert!(allowed.contains(&px), "invented colour {px:?} at {x},{y}");
            }
        }
    }

    #[test]
    fn nearest_is_reproducible_across_runs() {
        // The property the ONNX path cannot promise, asserted for the one
        // that can.
        let up = NearestUpscaler::new(4).unwrap();
        let a = up.upscale(&two_px()).unwrap();
        let b = up.upscale(&two_px()).unwrap();
        assert_eq!(a, b);
        assert_eq!(up.provenance(), "exact/integer");
    }

    #[test]
    fn scale_of_one_is_the_identity() {
        let up = NearestUpscaler::new(1).unwrap();
        assert_eq!(up.upscale(&two_px()).unwrap(), two_px());
    }
}
