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
}

impl std::fmt::Debug for OnnxUpscaler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OnnxUpscaler")
            .field("model_id", &self.model_id)
            .field("runtime_id", &self.runtime_id)
            .field("model_path", &self.model_path)
            .field("scale", &self.scale)
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

        let session = Session::builder()
            .and_then(|mut b| b.commit_from_file(model_path))
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
        })
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
        // NCHW float RGB in 0..=1, the ESRGAN-class convention. Alpha is
        // carried around the model rather than through it: these models
        // are trained on opaque RGB, and feeding alpha through a network
        // that never saw it produces halos on every sprite edge.
        let (w, h) = (src.width as usize, src.height as usize);
        let mut chw = vec![0f32; 3 * w * h];
        for (i, px) in src.pixels.chunks_exact(4).enumerate() {
            chw[i] = f32::from(px[0]) / 255.0;
            chw[w * h + i] = f32::from(px[1]) / 255.0;
            chw[2 * w * h + i] = f32::from(px[2]) / 255.0;
        }

        let input = Tensor::from_array((vec![1_i64, 3, h as i64, w as i64], chw)).map_err(|e| {
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

        // Trust the DECLARED scale only after checking it against what the
        // model actually returned — split_sheet's geometry depends on it,
        // and a silent mismatch would slice every frame at the wrong
        // offset rather than failing.
        let dims: Vec<i64> = shape.iter().copied().collect();
        let (out_h, out_w) = match dims.as_slice() {
            [_, _, oh, ow] => (*oh as u32, *ow as u32),
            other => {
                return Err(UpscaleError::Runtime {
                    detail: format!("expected a 4-D NCHW output, got shape {other:?}"),
                })
            }
        };
        if out_w != src.width * self.scale || out_h != src.height * self.scale {
            return Err(UpscaleError::Runtime {
                detail: format!(
                    "model declared scale {} ({}x{} -> {}x{}) but returned {out_w}x{out_h}",
                    self.scale,
                    src.width,
                    src.height,
                    src.width * self.scale,
                    src.height * self.scale
                ),
            });
        }

        let plane = out_w as usize * out_h as usize;
        if data.len() < 3 * plane {
            return Err(UpscaleError::Runtime {
                detail: format!(
                    "output tensor has {} values, need {} for {out_w}x{out_h} RGB",
                    data.len(),
                    3 * plane
                ),
            });
        }

        let mut pixels = Vec::with_capacity(plane * 4);
        for i in 0..plane {
            pixels.push(to_u8(data[i]));
            pixels.push(to_u8(data[plane + i]));
            pixels.push(to_u8(data[2 * plane + i]));
            pixels.push(255);
        }
        Rgba8::new(out_w, out_h, pixels)
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
