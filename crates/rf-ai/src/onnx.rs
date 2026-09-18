//! The ONNX-backed [`Upscaler`] (ticket W8-10, criterion 1's inference
//! half; `docs/design/ENHANCEMENT_RUNTIME.md` §6's "local ONNX
//! ESRGAN-class model").
//!
//! **Compiled only under the `onnx` feature, which is off by default.**
//! Everything the honesty contract rests on — grouping, keying, refusal —
//! lives in [`crate::pipeline`] and is tested without this module, so
//! `cargo test --workspace` needs no runtime, no model and no network.
//!
//! ## The runtime is loaded, not linked
//!
//! `ort` is taken with `default-features = false, features =
//! ["load-dynamic"]`, so the ONNX Runtime shared library is `dlopen`'d at
//! run time from a path we choose rather than downloaded at build time.
//! Two consequences, both deliberate:
//!
//! 1. No build-time network access and no vendored binary blob in git —
//!    the same posture the ROM and vector fetchers take (NFR-006).
//! 2. A missing runtime is a **runtime error on a machine that asked for
//!    inference**, not a build failure on a machine that never will.
//!
//! [`OnnxUpscaler::load`] therefore calls `ort::init_from`, which *returns
//! an error* when the dylib is absent. It deliberately does NOT rely on
//! `ort`'s lazy default path: that one `expect()`s and would abort the
//! process (`ort-2.0.0-rc.13/src/lib.rs:234`), turning a missing optional
//! dependency into a crash.
//!
//! ## Static-shape models are tiled automatically (ticket W16-12)
//!
//! Every model this crate has ledgered so far (`ai-model-manifest.toml`)
//! declares a fixed input size (64x64, 128x128) — feeding it anything
//! else fails outright. [`Upscaler::upscale`] reads the session's own
//! declared input shape ([`OnnxUpscaler::declared_input_shape`]) and, when
//! it is static, hands the work to [`crate::tiling::tile_and_blend`]
//! instead of calling the model directly: the source image is split into
//! overlapping tiles at the model's own size, reflection-padded at the
//! image edges, each tile run through [`OnnxUpscaler::run_batch`]
//! (batched together when the model's batch dimension is dynamic, one at
//! a time when it is fixed — every ledgered model today), and the results
//! blended back with a linear feather across the overlaps. A model whose
//! input IS dynamic skips all of this and just runs the whole image, as
//! before. Alpha never goes through the model either way — see
//! [`reattach_alpha_nearest`].
//!
//! ## Reproducibility is bounded, and says so
//!
//! Float inference is not bit-reproducible across runtime versions or
//! execution providers. [`Upscaler::provenance`] therefore reports the
//! runtime and model identity, and [`crate::pipeline`] folds it into the
//! cache key — so a runtime change produces a *different key* rather than
//! different pixels behind the same one. See `docs/design/AI_UPSCALING.md`
//! §4.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ort::session::Session;
use ort::value::Tensor;

/// Re-exported so a caller that needs `ort` types this module's own API
/// surface exposes (`load_with_providers`'s `&[ort::ep::ExecutionProviderDispatch]`,
/// e.g. `ort::ep::CoreML::default().build()`) can name them without
/// taking a second, potentially version-skewed, direct dependency on
/// `ort` itself — Rust's extern prelude only resolves a crate a
/// `Cargo.toml` names directly, and `rf-ai`'s own optional/load-dynamic
/// pin (see this crate's `Cargo.toml`) is the one copy of `ort` this
/// workspace's arch gate (`scripts/validate-arch.sh`'s single-`wgpu`
/// reasoning applies the same logic here, informally) should ever see.
pub use ort;

use crate::upscale::{Rgba8, UpscaleError, Upscaler};

/// An upscaler backed by a local ONNX model.
///
/// The session is behind a [`Mutex`] because `ort`'s `Session::run` takes
/// `&mut self` while [`Upscaler::upscale`] takes `&self` — the trait is
/// shared by implementations that genuinely have no state, and widening it
/// to `&mut` for this one would be the tail wagging the dog.
pub struct OnnxUpscaler {
    session: Mutex<Session>,
    model_id: String,
    runtime_id: String,
    model_path: PathBuf,
    scale: u32,
    /// Overlap, in source pixels, between adjacent tiles when the model's
    /// input is a static size (ticket W16-12; see [`crate::tiling`]).
    /// Meaningless for a dynamic-shape model, which is never tiled.
    pad: u32,
}

/// Batches of at most this many tiles are sent to the model in one
/// `Session::run` call when its batch dimension is dynamic. A fixed cap
/// rather than "all tiles at once": a 512x448 frame at a 64px tile can
/// produce 50+ tiles, and there is no dynamic-shape ledgered model today
/// to measure a better number against (see `ai-model-manifest.toml`'s
/// criterion-3 note) — 8 is a conservative starting point, not a measured
/// one, and is easy to change in one place if a future model calls for it.
const MAX_DYNAMIC_BATCH: usize = 8;

impl std::fmt::Debug for OnnxUpscaler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OnnxUpscaler")
            .field("model_id", &self.model_id)
            .field("runtime_id", &self.runtime_id)
            .field("model_path", &self.model_path)
            .field("scale", &self.scale)
            .field("pad", &self.pad)
            .finish()
    }
}

impl OnnxUpscaler {
    /// Load a model, dynamically loading the ONNX Runtime from
    /// `dylib_path`.
    ///
    /// `model_id` is recorded verbatim in the pack manifest and the cache
    /// key. It should identify the *weights*, not the architecture — two
    /// different checkpoints of the same network produce different pixels
    /// and must not share a key.
    ///
    /// # Errors
    /// [`UpscaleError::Runtime`] if the dylib cannot be loaded, the model
    /// cannot be read, or the session cannot be built. Every one of these
    /// carries the runtime's own message: this is the failure a user is
    /// most likely to hit (a missing or mismatched runtime), so the
    /// diagnostic has to be the real one rather than "inference failed".
    pub fn load(
        dylib_path: &Path,
        model_path: &Path,
        model_id: &str,
        scale: u32,
    ) -> Result<Self, UpscaleError> {
        Self::load_with_providers(dylib_path, model_path, model_id, scale, &[])
    }

    /// Same as [`Self::load`], but registers `providers` on the session
    /// builder first (ticket W16-01's ort/CoreML benchmark spike;
    /// `docs/design/ENHANCEMENT_WAVE_16.md` §7-8: "CoreML EP, CPU EP as
    /// control"). An empty slice — [`Self::load`]'s own contract — leaves
    /// ONNX Runtime's default CPU EP as the only provider, which is why
    /// this is the one true constructor and `load` a thin wrapper rather
    /// than two independent code paths that could drift apart.
    ///
    /// # Errors
    /// Same as [`Self::load`], plus [`UpscaleError::Runtime`] if EP
    /// registration itself fails (a provider dispatch's default
    /// `fail_silently` behavior means an *unavailable* EP does not error
    /// here — see `ort::ep::ExecutionProviderDispatch::error_on_failure`
    /// if a caller wants that promoted to a hard failure instead).
    pub fn load_with_providers(
        dylib_path: &Path,
        model_path: &Path,
        model_id: &str,
        scale: u32,
        providers: &[ort::ep::ExecutionProviderDispatch],
    ) -> Result<Self, UpscaleError> {
        if scale == 0 {
            return Err(UpscaleError::ZeroScale);
        }
        // init_from returns an error when the dylib is missing; the lazy
        // default path expect()s instead. See the module doc.
        let env = ort::init_from(dylib_path).map_err(|e| UpscaleError::Runtime {
            detail: format!("loading ONNX Runtime from {}: {e}", dylib_path.display()),
        })?;
        // `commit()` returns a bool rather than a Result: false means an
        // environment was already committed by an earlier load. That is
        // not an error — the runtime is process-global and a second
        // upscaler legitimately reuses it — so it is deliberately not
        // treated as one. Verified against ort-2.0.0-rc.13
        // src/environment.rs:658.
        let _already_initialised = !env.commit();

        let mut builder = Session::builder().map_err(|e| UpscaleError::Runtime {
            detail: format!("creating session builder: {e}"),
        })?;
        if !providers.is_empty() {
            // `with_execution_providers` takes `self` by value and its
            // `BuilderResult` error type carries a "recover the builder"
            // typestate different from `Session::builder()`'s own `Result`
            // — mapped to `UpscaleError` immediately, per step, rather
            // than chained, so the two mismatched `Error<T>` instantiations
            // never need to unify.
            builder =
                builder
                    .with_execution_providers(providers)
                    .map_err(|e| UpscaleError::Runtime {
                        detail: format!("registering execution providers: {e}"),
                    })?;
        }
        let session = builder
            .commit_from_file(model_path)
            .map_err(|e| UpscaleError::Runtime {
                detail: format!("opening model {}: {e}", model_path.display()),
            })?;

        Ok(Self {
            session: Mutex::new(session),
            model_id: model_id.to_string(),
            // The dylib path is part of the identity because the runtime
            // version it points at can change the output bytes.
            runtime_id: dylib_path.display().to_string(),
            model_path: model_path.to_path_buf(),
            scale,
            pad: crate::tiling::DEFAULT_PAD,
        })
    }

    /// Override the tile overlap (default [`crate::tiling::DEFAULT_PAD`])
    /// used when the model's input is a static size. No effect on a
    /// dynamic-shape model, which runs whole with no tiling at all.
    #[must_use]
    pub fn with_pad(mut self, pad: u32) -> Self {
        self.pad = pad;
        self
    }

    /// The model's declared input `(batch, height, width)`, `-1` meaning
    /// "dynamic" for any of the three (ONNX Runtime's own convention —
    /// see `ort::value::Shape`'s doc).
    ///
    /// # Errors
    /// [`UpscaleError::Runtime`] if the session has no inputs, the first
    /// input is not a 4-D NCHW tensor, or the channel dimension is not 3
    /// (this crate's RGB convention — see the module doc).
    fn declared_input_shape(&self) -> Result<(i64, i64, i64), UpscaleError> {
        let session = self.session.lock().map_err(|_| UpscaleError::Runtime {
            detail: "ONNX session mutex was poisoned by an earlier panic".to_string(),
        })?;
        let input = session
            .inputs()
            .first()
            .ok_or_else(|| UpscaleError::Runtime {
                detail: format!("model {} declares no inputs", self.model_path.display()),
            })?;
        match input.dtype() {
            ort::value::ValueType::Tensor { shape, .. } => match &shape[..] {
                [n, c, h, w] if *c == 3 => Ok((*n, *h, *w)),
                other => Err(UpscaleError::Runtime {
                    detail: format!(
                        "model {} input is {other:?}, expected a 4-D NCHW tensor with 3 channels",
                        self.model_path.display()
                    ),
                }),
            },
            other => Err(UpscaleError::Runtime {
                detail: format!(
                    "model {} input is {other:?}, not a tensor",
                    self.model_path.display()
                ),
            }),
        }
    }

    /// Run one batch of same-sized RGB tiles (already NCHW-float-ready)
    /// through the session in a single `Session::run` call, returning one
    /// RGB [`Rgba8`] (alpha forced to 255 — the caller reattaches the real
    /// alpha; see the module doc's "alpha is carried around the model"
    /// note) per input tile, in order.
    ///
    /// This is the ONE place that actually touches `ort` on the inference
    /// path — [`crate::tiling::tile_and_blend`]'s `run` closure and the
    /// dynamic-shape whole-image path both call through here, so there is
    /// exactly one tensor-building/extracting implementation rather than
    /// two that could drift apart.
    fn run_batch(&self, tiles: &[Rgba8]) -> Result<Vec<Rgba8>, UpscaleError> {
        let Some(first) = tiles.first() else {
            return Ok(Vec::new());
        };
        let (w, h) = (first.width as usize, first.height as usize);
        let n = tiles.len();

        // NCHW float RGB in 0..=1, the ESRGAN-class convention. Alpha is
        // carried around the model rather than through it: these models
        // are trained on opaque RGB, and feeding alpha through a network
        // that never saw it produces halos on every sprite edge.
        let mut chw = vec![0f32; n * 3 * w * h];
        for (b, tile) in tiles.iter().enumerate() {
            if tile.width as usize != w || tile.height as usize != h {
                return Err(UpscaleError::Runtime {
                    detail: format!(
                        "batched tiles must share one size: tile 0 is {w}x{h}, tile {b} is {}x{}",
                        tile.width, tile.height
                    ),
                });
            }
            let base = b * 3 * w * h;
            for (i, px) in tile.pixels.chunks_exact(4).enumerate() {
                chw[base + i] = f32::from(px[0]) / 255.0;
                chw[base + w * h + i] = f32::from(px[1]) / 255.0;
                chw[base + 2 * w * h + i] = f32::from(px[2]) / 255.0;
            }
        }

        let input =
            Tensor::from_array((vec![n as i64, 3, h as i64, w as i64], chw)).map_err(|e| {
                UpscaleError::Runtime {
                    detail: format!("building input tensor: {e}"),
                }
            })?;

        let mut session = self.session.lock().map_err(|_| UpscaleError::Runtime {
            detail: "ONNX session mutex was poisoned by an earlier panic".to_string(),
        })?;
        let outputs = session
            .run(ort::inputs![input])
            .map_err(|e| UpscaleError::Runtime {
                detail: format!("running model {}: {e}", self.model_path.display()),
            })?;

        let (shape, data) =
            outputs[0]
                .try_extract_tensor::<f32>()
                .map_err(|e| UpscaleError::Runtime {
                    detail: format!("reading output tensor: {e}"),
                })?;

        let dims: Vec<i64> = shape.iter().copied().collect();
        let (out_n, out_h, out_w) = match dims.as_slice() {
            [on, _, oh, ow] => (*on as usize, *oh as u32, *ow as u32),
            other => {
                return Err(UpscaleError::Runtime {
                    detail: format!("expected a 4-D NCHW output, got shape {other:?}"),
                })
            }
        };
        if out_n != n {
            return Err(UpscaleError::Runtime {
                detail: format!("sent a batch of {n} tiles, model returned {out_n}"),
            });
        }
        // Trust the DECLARED scale only after checking it against what the
        // model actually returned — split_sheet's geometry depends on it,
        // and a silent mismatch would slice every frame at the wrong
        // offset rather than failing.
        if out_w != first.width * self.scale || out_h != first.height * self.scale {
            return Err(UpscaleError::Runtime {
                detail: format!(
                    "model declared scale {} ({}x{} -> {}x{}) but returned {out_w}x{out_h}",
                    self.scale,
                    first.width,
                    first.height,
                    first.width * self.scale,
                    first.height * self.scale
                ),
            });
        }

        let plane = out_w as usize * out_h as usize;
        if data.len() < out_n * 3 * plane {
            return Err(UpscaleError::Runtime {
                detail: format!(
                    "output tensor has {} values, need {} for {out_n}x3x{out_w}x{out_h}",
                    data.len(),
                    out_n * 3 * plane
                ),
            });
        }

        let mut results = Vec::with_capacity(out_n);
        for b in 0..out_n {
            let base = b * 3 * plane;
            let mut pixels = Vec::with_capacity(plane * 4);
            for i in 0..plane {
                pixels.push(to_u8(data[base + i]));
                pixels.push(to_u8(data[base + plane + i]));
                pixels.push(to_u8(data[base + 2 * plane + i]));
                pixels.push(255);
            }
            results.push(Rgba8::new(out_w, out_h, pixels)?);
        }
        Ok(results)
    }
}

/// Nearest-neighbour upscale of just the alpha channel, run AFTER the RGB
/// path (whole-image or tiled) so a model that never saw alpha cannot
/// paint over a sprite's cutout — see the module doc's "alpha is carried
/// around the model" note and `crate::studio`'s edge mask, which handles
/// the halo this still leaves at a scaled-up hard edge.
fn reattach_alpha_nearest(rgb: &mut Rgba8, src: &Rgba8, scale: u32) {
    for y in 0..rgb.height {
        let sy = (y / scale).min(src.height - 1);
        for x in 0..rgb.width {
            let sx = (x / scale).min(src.width - 1);
            let a = src.pixel(sx, sy).expect("in bounds by construction")[3];
            let i = (y as usize * rgb.width as usize + x as usize) * 4 + 3;
            rgb.pixels[i] = a;
        }
    }
}

impl Upscaler for OnnxUpscaler {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn provenance(&self) -> String {
        // NOT a fixed string, unlike NearestUpscaler's. This is the whole
        // reason `provenance` exists as a separate concept from
        // `model_id`: the runtime is an input to the pixels.
        format!("onnx/{}", self.runtime_id)
    }

    fn scale(&self) -> u32 {
        self.scale
    }

    fn upscale(&self, src: &Rgba8) -> Result<Rgba8, UpscaleError> {
        let (batch_dim, in_h, in_w) = self.declared_input_shape()?;

        let mut out = if in_h > 0 && in_w > 0 {
            // STATIC input size (both of this ticket's ledgered models:
            // `ai-model-manifest.toml`'s 64x64 and 128x128 rows) — tile.
            // The model's own batch dim decides whether tiles are sent one
            // at a time (a fixed batch, universally 1 today) or several
            // per call (a dynamic batch, `-1`): see `MAX_DYNAMIC_BATCH`.
            let batch = if batch_dim < 0 { MAX_DYNAMIC_BATCH } else { 1 };
            crate::tiling::tile_and_blend(
                src,
                in_w as u32,
                in_h as u32,
                self.pad,
                self.scale,
                batch,
                |tiles: &[Rgba8]| self.run_batch(tiles),
            )?
        } else {
            // DYNAMIC input size: no tile-size to tile TO, so run the
            // whole image through in one call.
            self.run_batch(std::slice::from_ref(src))?
                .into_iter()
                .next()
                .expect("run_batch returns exactly one output for one input")
        };

        reattach_alpha_nearest(&mut out, src, self.scale);
        Ok(out)
    }
}

/// Clamp a model's float output into a byte.
///
/// Clamped rather than wrapped: a network can and does emit values
/// slightly outside 0..=1, and `as u8` on an out-of-range float would wrap
/// a near-white pixel to near-black — a spectacular artefact from a
/// rounding detail.
fn to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_u8_clamps_rather_than_wrapping() {
        assert_eq!(to_u8(0.0), 0);
        assert_eq!(to_u8(1.0), 255);
        // The artefact this guards against: 1.02 as u8 must not wrap.
        assert_eq!(to_u8(1.02), 255);
        assert_eq!(to_u8(-0.3), 0);
    }

    #[test]
    fn zero_scale_is_refused_before_any_runtime_is_touched() {
        // Cheap to assert and it runs on a machine with no runtime at all,
        // which is the point: the argument check precedes the dlopen.
        let e = OnnxUpscaler::load(
            Path::new("/nonexistent/libonnxruntime.dylib"),
            Path::new("/nonexistent/model.onnx"),
            "test",
            0,
        )
        .unwrap_err();
        assert_eq!(e, UpscaleError::ZeroScale);
    }

    #[test]
    fn a_missing_runtime_is_an_error_not_a_panic() {
        // The reason load() uses init_from rather than ort's lazy default
        // path: the default expect()s and would abort the process.
        let e = OnnxUpscaler::load(
            Path::new("/nonexistent/libonnxruntime.dylib"),
            Path::new("/nonexistent/model.onnx"),
            "test",
            4,
        )
        .unwrap_err();
        match e {
            UpscaleError::Runtime { detail } => {
                assert!(detail.contains("libonnxruntime"), "unhelpful: {detail}");
            }
            other => panic!("expected a Runtime error, got {other:?}"),
        }
    }
}
