//! Ticket W3-01, acceptance criterion 3: "headless render-to-buffer hashing
//! used by rf-harness golden tests" -- exercises `rf_renderer`'s real wgpu
//! original-pipeline palette pass (indexed R8Uint texture -> LUT lookup ->
//! RGBA8 target -> readback) against a synthetic indexed frame and checks
//! two independent things:
//!
//! 1. **Correctness, re-verified every run**: every readback pixel equals
//!    `rf_renderer::palette_index_to_rgb` applied to the same index --  a
//!    plain CPU function, not routed through the GPU shader, so this is a
//!    real oracle rather than "whatever the GPU produced, canonized."
//! 2. **Golden-hash regression, a frozen literal**: the readback bytes'
//!    SHA-256 must equal a hash computed once and checked in here. This is
//!    what actually catches a corrupted LUT: a bug that (e.g.) swaps two
//!    `NES_PALETTE` entries changes what *both* the GPU pass and the CPU
//!    oracle above compute, so assertion 1 alone would pass vacuously (both
//!    sides move together) -- only a hash frozen independently of the
//!    live `NES_PALETTE` catches that class of regression. See this
//!    ticket's plan.json notes: "swap two LUT entries -> the golden-hash
//!    test must FAIL. If it passes, the hashing proves nothing."
//!
//! Per `docs/design/RENDERER.md` §6/§7: this hashes the **1x post-palette
//! buffer only** (pre-scale, pre-shader-chain) -- the palette pass is
//! bit-exact by construction (integer LUT lookups, no filtering, no sRGB
//! math), so this hash is stable across backends/drivers. Anything after
//! this buffer is explicitly out of CI's golden-hash scope.
//!
//! **Headless CI must not require a GPU** (this ticket's own instructions):
//! if no adapter is available, this test skips -- except in CI (detected via
//! the `CI` env var GitHub Actions sets), where `.github/workflows/ci.yml`
//! configures `WGPU_BACKEND=gl` + `LIBGL_ALWAYS_SOFTWARE=1` specifically so
//! an adapter is always available; a missing adapter there is a real
//! regression, not an environment limitation, so it fails loudly instead of
//! silently skipping. A plain `cargo test` output cannot otherwise
//! distinguish a skip from a pass (verified empirically: `eprintln!` in a
//! passing test is captured and hidden by default, same as the ROM-suite
//! skip path `docs/work/HANDOFF.md` already documents) -- keying the hard
//! failure on `CI` closes that gap for the run that actually gates merges.

use rf_renderer::{palette_index_to_rgb, GpuContext, IndexedFrame, PalettePass};
use sha2::{Digest, Sha256};

const WIDTH: u32 = 256;
const HEIGHT: u32 = 240;

/// Deterministic synthetic frame covering the full `$00`-`$3F` palette
/// range (a diagonal stripe pattern), so a two-entry LUT swap almost
/// certainly touches at least one pixel's expected color.
fn synthetic_indices() -> Vec<u8> {
    (0..HEIGHT)
        .flat_map(|y| (0..WIDTH).map(move |x| ((x.wrapping_add(y)) % 64) as u8))
        .collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    use std::fmt::Write;
    digest.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Frozen 2026-08-07: SHA-256 of the RGBA8 readback for [`synthetic_indices`]
/// through [`PalettePass`], measured on Metal (Apple M5 Max). RENDERER.md
/// §7's bit-exactness argument (integer LUT lookup, no filtering, no sRGB)
/// says this value is *derived* to be backend/driver-independent -- that is
/// not yet independently confirmed on more than this one backend. CI's
/// first green run on `WGPU_BACKEND=gl` + `LIBGL_ALWAYS_SOFTWARE=1`
/// (llvmpipe) is what turns that argument into an observed fact; if CI goes
/// red on this specific assertion, treat it as a cross-backend question to
/// investigate first, not automatically a code regression.
const GOLDEN_HASH: &str = "7a6a00b6618255c3be0ca4e0be67c974990d8d261afb88b3eef28a90e830e047";

#[test]
fn palette_pass_golden_hash() {
    let gpu = match GpuContext::request_headless() {
        Ok(gpu) => gpu,
        Err(e) => {
            if std::env::var_os("CI").is_some() {
                panic!(
                    "golden GPU pipeline test cannot skip in CI: {e} \
                     (WGPU_BACKEND={:?}, LIBGL_ALWAYS_SOFTWARE={:?}) -- CI's \
                     software-rasterizer fallback must always produce an \
                     adapter; a missing one here is a real regression",
                    std::env::var("WGPU_BACKEND"),
                    std::env::var("LIBGL_ALWAYS_SOFTWARE"),
                );
            }
            eprintln!(
                "SKIP palette_pass_golden_hash: no wgpu adapter in this environment ({e}) -- \
                 clean skip outside CI, run with --nocapture to see this line"
            );
            return;
        }
    };

    let pass = PalettePass::new(&gpu);
    let indices = synthetic_indices();
    let frame = IndexedFrame::new(WIDTH, HEIGHT, indices.clone());
    let rgba = pass
        .render(&gpu, &frame)
        .expect("palette pass readback must complete within the bounded GPU wait");

    assert_eq!(
        rgba.len(),
        (WIDTH * HEIGHT * 4) as usize,
        "readback buffer must be exactly width*height*4 bytes (RGBA8, no row padding at this resolution)"
    );

    // --- 1. Correctness, re-verified every run against a CPU oracle that
    // never touches the GPU shader. ---
    for (i, chunk) in rgba.chunks_exact(4).enumerate() {
        let expected = palette_index_to_rgb(indices[i]);
        assert_eq!(
            [chunk[0], chunk[1], chunk[2]],
            expected,
            "pixel {i} (index {}): GPU palette pass diverged from the CPU oracle",
            indices[i]
        );
        assert_eq!(chunk[3], 0xFF, "pixel {i}: alpha must always be opaque");
    }

    // --- 2. Golden-hash regression against a frozen literal. ---
    let actual_hash = sha256_hex(&rgba);
    assert_eq!(
        actual_hash, GOLDEN_HASH,
        "palette pass RGBA8 output changed -- if this is an intentional LUT/pipeline \
         change, regenerate GOLDEN_HASH; if not, this is the regression the golden \
         hash exists to catch"
    );
}
