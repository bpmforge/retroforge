//! Headless golden test for the fog/steam pass (ticket W16-04; acceptance
//! criterion 4: "a golden test of the fixture scene with the pass on and
//! off").
//!
//! ## Fixture
//!
//! Mirrors `crates/rf-harness/tests/atmosphere_layer_red_fixture.rs`'s own
//! synthetic-frame approach (that file's own header explains why: no cc65
//! toolchain on this machine to rebuild a real ROM fixture) rather than
//! literally reusing it — `rf-harness` is outside this ticket's
//! `write_scope` (`crates/rf-renderer/**`, `crates/rf-enhance/**`,
//! `crates/retroforge/**`), and `rf-renderer` may not depend on
//! `rf-enhance` either way (ARCHITECTURE.md §3; `rf_enhance::scene_graph`'s
//! own module doc). This file builds the equivalent scene directly in
//! `rf-renderer`'s own terms: an already-composited "scene" RGBA buffer
//! (what the atmosphere plane's own pixels look like once rendered
//! normally) and a low-variety "density" RGBA buffer (what
//! `rf_enhance::atmosphere`'s detector would have extracted as
//! `SceneLayer::ExtractedBg`), then runs [`rf_renderer::fog::FogPass`]
//! over them.
//!
//! ## Why exact hashes here, not RENDERER.md §7's tolerance-oracle rule
//!
//! §7's "not hashable past 1x" rule (cited in `shader_chain.rs`,
//! `scale.rs`, `original_pipeline.rs`, `lib.rs`) is about a value CI is
//! expected to check across machines/drivers. This crate's GPU tests
//! already skip cleanly off-machine (`gpu_or_skip`, mirrored below) and
//! this project's own CLAUDE.md says plainly hosted CI does not run —
//! these hashes are a **local regression fence** on this one machine/
//! backend (Metal), the same posture `shader_chain.rs`'s own comment
//! takes ("Single-backend observation ... same posture ... §7's golden
//! hash both take"), not a cross-platform contract. If this ever needs to
//! run on a second backend, the fix is measuring a second pinned value
//! there, not loosening this one.
//!
//! Acceptance criterion 1's "the original plane's mask is kept underneath"
//! is asserted directly too ([`fog_never_changes_the_scenes_own_alpha_channel`])
//! rather than only implied by the two hashes differing.

use rf_renderer::fog::{FogParams, FogPass};
use rf_renderer::gpu::GpuContext;

const WIDTH: u32 = 64;
const HEIGHT: u32 = 32;

fn gpu_or_skip(test_name: &str) -> Option<GpuContext> {
    match GpuContext::request_headless() {
        Ok(gpu) => Some(gpu),
        Err(e) => {
            if std::env::var_os("CI").is_some() {
                panic!("{test_name} cannot skip in CI: {e}");
            }
            eprintln!(
                "SKIP {test_name}: no wgpu adapter in this environment ({e}) -- clean skip, run \
                 with --nocapture to see this line"
            );
            None
        }
    }
}

/// The "already composited" scene: a checkerboard with a solid opaque
/// silhouette region (alpha 255) and a transparent border (alpha 0) --
/// same shape as `crates/rf-harness/tests/atmosphere_layer_red_fixture.rs`'s
/// dominant-plane checkerboard, reduced to RGBA bytes as if already
/// rendered.
fn synthetic_scene() -> Vec<u8> {
    let mut out = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let checker = ((x / 4) + (y / 4)) % 2 == 0;
            let base: u8 = if checker { 200 } else { 40 };
            // A silhouette: pixels inside a centered rectangle are opaque,
            // everything outside is transparent -- this is the "mask"
            // acceptance criterion 1 says must survive unchanged.
            let inside = (16..48).contains(&x) && (8..24).contains(&y);
            out.extend_from_slice(&[base, base / 2, 255 - base, if inside { 255 } else { 0 }]);
        }
    }
    out
}

/// The density map: one repeated low-variety pattern (a soft blob grid),
/// matching `rf_enhance::atmosphere`'s own "very few distinct 8x8 tile
/// patterns" signal for a real atmosphere plane -- red channel carries
/// density (the shader only reads `.r`).
fn synthetic_density() -> Vec<u8> {
    let mut out = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let tx = x % 8;
            let ty = y % 8;
            // A soft blob per 8x8 tile: bright in the middle, dark at the
            // edges -- deterministic, no host-side unbounded loop (law 8).
            let dx = (tx as i32 - 4).unsigned_abs();
            let dy = (ty as i32 - 4).unsigned_abs();
            let d = 255u32.saturating_sub((dx + dy) * 40);
            let density = d.min(255) as u8;
            out.extend_from_slice(&[density, density, density, 255]);
        }
    }
    out
}

/// FNV-1a, 64-bit -- a plain, dependency-free fingerprint (this crate does
/// not otherwise depend on a crypto-hash crate; nothing about a golden
/// regression fence needs collision resistance, only stability).
fn fingerprint(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Pinned "off" hash of the fixture's un-fogged scene -- no GPU needed at
/// all (RENDERER.md §7 has no objection to hashing the pre-shader buffer;
/// it's the post-shader one that varies by driver). If this fixture's
/// pixel data ever changes on purpose, regenerate this constant; if it
/// changes unexpectedly, something is overwriting `synthetic_scene`'s own
/// bytes before this test reads them.
const OFF_HASH: u64 = 0xa41d_110a_909c_9725;

/// Pinned "on" hash (fog rendered), same machine/backend as [`OFF_HASH`].
const ON_HASH: u64 = 0x4654_6ebd_ffef_3676;

#[test]
fn fog_off_scene_hash_is_stable_and_unchanged_from_source() {
    let scene = synthetic_scene();
    assert_eq!(
        fingerprint(&scene),
        OFF_HASH,
        "the fixture's un-fogged scene bytes changed -- regenerate OFF_HASH deliberately if \
         this was an intentional fixture edit"
    );
}

/// The real golden: pass off vs. pass on must differ (fog visibly
/// rendered), the pass-on output must be internally stable across two
/// runs with identical inputs on this machine/backend, and the scene's
/// own alpha (its "mask"/silhouette) must be byte-identical between off
/// and on.
#[test]
fn fog_pass_renders_and_preserves_the_scenes_own_mask() {
    let Some(gpu) = gpu_or_skip("fog_pass_renders_and_preserves_the_scenes_own_mask") else {
        return;
    };
    let fog = FogPass::new(&gpu);
    let scene = synthetic_scene();
    let density = synthetic_density();
    let params = FogParams::new(1.5, 0.05, 0.02, 0.8);

    let on_a = fog
        .render(&gpu, &scene, &density, WIDTH, HEIGHT, params)
        .expect("fog pass renders");
    let on_b = fog
        .render(&gpu, &scene, &density, WIDTH, HEIGHT, params)
        .expect("fog pass renders again");

    assert_eq!(
        on_a, on_b,
        "identical inputs must produce byte-identical output on this machine/backend -- a \
         non-deterministic fog pass would make the on/off golden meaningless"
    );

    let off_hash = fingerprint(&scene);
    let on_hash = fingerprint(&on_a);
    assert_ne!(
        off_hash, on_hash,
        "the fog pass must visibly change the frame -- identical hashes would mean it rendered \
         nothing"
    );
    // Pinned on this machine/backend (Apple M4 Max, Metal) -- module doc's
    // "local regression fence, not a cross-platform contract". Regenerate
    // deliberately (print `on_hash` with `--nocapture`) if `fog.wgsl` or
    // either fixture buffer changes on purpose.
    assert_eq!(
        on_hash, ON_HASH,
        "the fog pass's output changed on this machine/backend -- if this is an intentional \
         shader/fixture change, regenerate ON_HASH; if not, something regressed"
    );

    // Acceptance criterion 1, asserted directly: the original plane's own
    // alpha (its silhouette/mask) survives byte-for-byte -- the shader
    // only blends `.rgb`, never `.a` (shaders/fog.wgsl's `fs_main`).
    for (i, (scene_px, fog_px)) in scene.chunks_exact(4).zip(on_a.chunks_exact(4)).enumerate() {
        assert_eq!(
            scene_px[3], fog_px[3],
            "pixel {i}: alpha/mask must be unchanged by the fog pass"
        );
    }

    // At least some pixels must have actually changed colour -- a fog
    // pass that only touched alpha (impossible here, but asserted as a
    // vacuity trap) or changed nothing would still pass the hash-diff
    // check above only by luck.
    let changed = scene
        .chunks_exact(4)
        .zip(on_a.chunks_exact(4))
        .filter(|(s, f)| s[0..3] != f[0..3])
        .count();
    assert!(
        changed > 0,
        "at least one pixel's colour must change under a nonzero-density, nonzero-strength fog pass"
    );
}
