//! Headless golden test for the Mode 7 ground pass (ticket W16-09;
//! acceptance criterion 3: "a synthetic Mode 7 scene ... rendered with the
//! pass, hash pinned, on/off").
//!
//! ## Fixture: synthetic, by necessity
//!
//! No Mode 7 render fixture ROM exists in this repo (`example-mode7` has
//! had no `[decode]` section since W9-08), and there is no cc65/ca65
//! toolchain on this machine to build one (`docs/design/
//! ENHANCEMENT_WAVE_16.md` §11's open question). So this test builds its
//! own small procedurally-generated plane texture and a "plausible
//! racing-game" matrix — scale ~1/4 (`a = d = 64`, `$0040`), the shape a
//! real HDMA perspective ramp's NEAREST scanline would write — rather than
//! loading anything from a ROM. Same posture `tests/diorama_golden.rs`'s
//! own header takes for its wall-ring fixture.
//!
//! ## Why exact hashes here, not a tolerance oracle
//!
//! Same posture as `tests/diorama_golden.rs`/`tests/fog_golden.rs`: a
//! local regression fence on this one machine/backend (Metal), not a
//! cross-platform contract — CLAUDE.md's "GitHub is STORAGE, not a gate"
//! ruling means no second backend ever checks this file.

use rf_core_api::Mode7Registers;
use rf_renderer::diorama::DioramaPass;
use rf_renderer::fog::{BudgetGate, DISABLE_P95_MS};
use rf_renderer::gpu::GpuContext;
use rf_renderer::mode7_plane::{render_mode7_ground, SNES_NATIVE_WIDTH_PX};

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

const PLANE_W: u32 = 64;
const PLANE_H: u32 = 64;
const OUT_W: u32 = 96;
const OUT_H: u32 = 96;

/// A small, deterministic, low-variety plane texture (checkerboard road
/// stripes) -- the same "deterministic, low-variety" shape
/// `diorama_golden.rs`'s own `ground_rgba` uses.
fn synthetic_plane_rgba() -> Vec<u8> {
    let mut out = Vec::with_capacity((PLANE_W * PLANE_H * 4) as usize);
    for _y in 0..PLANE_H {
        for x in 0..PLANE_W {
            let stripe = (x / 8) % 2 == 0;
            let base: u8 = if stripe { 200 } else { 40 };
            out.extend_from_slice(&[base, base, base, 255]);
        }
    }
    out
}

/// "A plausible racing-game matrix: scale ~1/4" -- module doc.
fn racing_matrix() -> Mode7Registers {
    Mode7Registers {
        a: 64, // $0040, scale 0.25 -- near-scanline zoom.
        b: 0,
        c: 0,
        d: 64,
        x0: 0,
        y0: 0,
        hofs: 0,
        vofs: 0,
        flip_x: false,
        flip_y: false,
    }
}

fn fingerprint(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

const OFF_HASH: u64 = 0xa087_4faf_a6fe_a325; // synthetic_plane_rgba's own bytes, no pass involved
/// Pinned "on" hash (racing matrix, rendered), same machine/backend as
/// [`OFF_HASH`] -- regenerate with `--nocapture` if this is intentional.
/// Pinned on an Apple M4 Max, Metal backend (same posture as
/// `diorama_golden.rs`'s own `ON_HASH`).
const ON_HASH: u64 = 0x3d51_9e12_1df0_a435;
const ON_HASH_PLACEHOLDER_NOTE: &str =
    "regenerate by running with --nocapture and pasting the printed on_hash";

#[test]
fn plane_fixture_bytes_are_stable() {
    assert_eq!(
        fingerprint(&synthetic_plane_rgba()),
        OFF_HASH,
        "the fixture's plane bytes changed -- regenerate OFF_HASH deliberately if this was an \
         intentional fixture edit"
    );
}

#[test]
fn mode7_ground_pass_on_vs_off_differ_and_on_is_stable_on_this_machine() {
    let Some(gpu) =
        gpu_or_skip("mode7_ground_pass_on_vs_off_differ_and_on_is_stable_on_this_machine")
    else {
        return;
    };
    let pass = DioramaPass::new(&gpu);
    let m = racing_matrix();
    let plane = synthetic_plane_rgba();

    // "Off": the pass is never invoked -- fully transparent, the honest
    // "nothing drawn" baseline (same stance `diorama_golden.rs` takes).
    let off = vec![0u8; (OUT_W * OUT_H * 4) as usize];

    let on_a = render_mode7_ground(
        &pass,
        &gpu,
        &m,
        &plane,
        PLANE_W,
        PLANE_H,
        SNES_NATIVE_WIDTH_PX,
        OUT_W,
        OUT_H,
    )
    .expect("mode7 ground pass renders");
    let on_b = render_mode7_ground(
        &pass,
        &gpu,
        &m,
        &plane,
        PLANE_W,
        PLANE_H,
        SNES_NATIVE_WIDTH_PX,
        OUT_W,
        OUT_H,
    )
    .expect("mode7 ground pass renders again");

    assert_eq!(
        on_a, on_b,
        "identical inputs must produce byte-identical output on this machine/backend"
    );
    assert_ne!(
        fingerprint(&off),
        fingerprint(&on_a),
        "the mode7 ground pass must visibly change the frame relative to 'off'"
    );
    let some_opaque = on_a.chunks_exact(4).any(|px| px[3] > 0);
    assert!(some_opaque, "the on frame must draw the ground quad");

    let on_hash = fingerprint(&on_a);
    eprintln!("mode7_plane_golden: on_hash = {on_hash:#018x}");
    assert_eq!(
        on_hash, ON_HASH,
        "the mode7 ground pass's output changed on this machine/backend -- if this is an \
         intentional shader/mapping change, {ON_HASH_PLACEHOLDER_NOTE}; if not, something \
         regressed"
    );
}

/// Same acceptance shape as `diorama_golden.rs`'s own budget-gate-reuse
/// test: this pass shares `crate::fog::BudgetGate`, not a second one.
#[test]
fn mode7_ground_pass_reuses_the_fog_passs_budget_gate_not_a_second_one() {
    let mut gate = BudgetGate::new();
    assert!(gate.is_enabled());
    for _ in 0..40 {
        gate.record_sample_ms(2.0);
    }
    assert!(gate.is_enabled());
    assert!(gate.p95() < DISABLE_P95_MS);
    for _ in 0..40 {
        gate.record_sample_ms(20.0);
    }
    assert!(!gate.is_enabled());
}
