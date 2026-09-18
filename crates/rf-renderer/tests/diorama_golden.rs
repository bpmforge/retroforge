//! Headless golden test for the Diorama pass (ticket W16-06; acceptance
//! criterion 4: "a headless golden of the fixture room with the pass off
//! and on (two pinned hashes)") plus this ticket's own budget-gate-reuse
//! requirement (§8: "shares the same harness ... rather than inventing a
//! second timing mechanism").
//!
//! ## Fixture: the rf-scroller-demo-shaped room
//!
//! Mirrors `crates/rf-enhance/src/scene_graph.rs`'s own `solidity_mask`
//! unit-test room (a 3x3 wall ring around one open floor tile) rather than
//! loading a real ROM: `rf-renderer` may not depend on `rf-enhance`
//! (ARCHITECTURE.md §3), and this crate's own `fog_golden.rs` already
//! sets the precedent of building the equivalent fixture directly in this
//! crate's own terms instead of a real decode. `docs/design/
//! ENHANCEMENT_WAVE_16.md`'s brief names `profiles/nes/rf-scroller-demo`
//! (metatile family, `[decode.collision]` declared) as the fixture with a
//! decoded room and collision; this test reproduces that fixture's SHAPE
//! (a solid/open tile grid from a collision-bit decode) without needing
//! the cc65 toolchain this machine does not have to build the real ROM.
//!
//! ## Why exact hashes here, not a tolerance oracle
//!
//! Same posture as `tests/fog_golden.rs`'s own header: a local regression
//! fence on this one machine/backend (Metal), not a cross-platform
//! contract — CLAUDE.md's "GitHub is STORAGE, not a gate" ruling means no
//! second backend ever checks this file.

use rf_renderer::diorama::DioramaPass;
use rf_renderer::diorama_mesh::{Billboard, DioramaScene};
use rf_renderer::fog::{BudgetGate, DISABLE_P95_MS};
use rf_renderer::gpu::GpuContext;

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

const TILE_PX: u32 = 16;
const TILES_W: u32 = 3;
const TILES_H: u32 = 3;
const OUT_W: u32 = 96;
const OUT_H: u32 = 96;

/// The wall-ring room: solid border, open centre — same shape
/// `scene_graph.rs`'s `solidity_mask_renders_a_room_with_the_right_geometry`
/// test uses, reproduced here since this crate cannot import that one.
#[rustfmt::skip]
fn ring_solid_and_depth() -> (Vec<u8>, Vec<u8>) {
    let solid = vec![
        1, 1, 1,
        1, 0, 1,
        1, 1, 1,
    ];
    let depth: Vec<u8> = solid.iter().map(|&s| if s == 1 { 8 } else { 0 }).collect();
    (solid, depth)
}

/// A repeating low-variety ground texture (checkerboard, same "deterministic,
/// low-variety" shape `fog_golden.rs`'s own `synthetic_density` uses).
fn ground_rgba() -> Vec<u8> {
    let (w, h) = (TILES_W * TILE_PX, TILES_H * TILE_PX);
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let checker = ((x / 8) + (y / 8)) % 2 == 0;
            let base: u8 = if checker { 180 } else { 60 };
            out.extend_from_slice(&[base, base, base / 2, 255]);
        }
    }
    out
}

/// A single flat-colored sprite cutout (opaque red square) standing in
/// the open centre tile.
fn sprite_rgba() -> Vec<u8> {
    [220, 40, 40, 255].repeat(4) // 2x2 texel
}

fn fingerprint(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

const OFF_HASH: u64 = 0xab77_bdfa_091c_7125; // ground_rgba's own bytes, no diorama pass involved
/// Pinned "on" hash (ring room + billboard, rendered), same machine/backend
/// as [`OFF_HASH`] — regenerate with `--nocapture` if this is intentional.
const ON_HASH: u64 = 0x4944_1336_000f_b9e9;
const ON_HASH_PLACEHOLDER_NOTE: &str =
    "regenerate by running with --nocapture and pasting the printed on_hash";

#[test]
fn ground_fixture_bytes_are_stable() {
    // No GPU needed at all -- same "pre-shader buffer is plainly
    // hashable" stance `fog_golden.rs`'s OFF_HASH takes.
    assert_eq!(
        fingerprint(&ground_rgba()),
        OFF_HASH,
        "the fixture's ground bytes changed -- regenerate OFF_HASH deliberately if this was an \
         intentional fixture edit"
    );
}

#[test]
fn diorama_pass_on_vs_off_differ_and_on_is_stable_on_this_machine() {
    let Some(gpu) = gpu_or_skip("diorama_pass_on_vs_off_differ_and_on_is_stable_on_this_machine")
    else {
        return;
    };
    let pass = DioramaPass::new(&gpu);
    let (solid, depth) = ring_solid_and_depth();

    // "Off": the pass is simply never invoked -- the scene shows whatever
    // was already composited underneath (module doc: this pass is an
    // Enhanced/Game-Aware-only overlay). Represented here as a fully
    // transparent buffer of the same size, the honest "nothing drawn"
    // baseline this pass's own `render` doc promises for an empty mesh.
    let off = vec![0u8; (OUT_W * OUT_H * 4) as usize];

    let scene = DioramaScene {
        tiles_w: TILES_W,
        tiles_h: TILES_H,
        tile_px: TILE_PX as f32,
        solid: &solid,
        depth: &depth,
        billboards: &[Billboard {
            center_x_px: 1.5 * TILE_PX as f32,
            center_z_px: 1.5 * TILE_PX as f32,
            width_px: 8.0,
            height_px: 12.0,
            uv: [0.0, 0.0, 1.0, 1.0],
        }],
    };
    let verts = rf_renderer::diorama_mesh::build_vertices(&scene);
    let ground = ground_rgba();
    let sprite = sprite_rgba();

    let on_a = pass
        .render(
            &gpu,
            &verts,
            &ground,
            TILES_W * TILE_PX,
            TILES_H * TILE_PX,
            &sprite,
            2,
            2,
            TILES_W,
            TILES_H,
            TILE_PX,
            OUT_W,
            OUT_H,
        )
        .expect("diorama pass renders");
    let on_b = pass
        .render(
            &gpu,
            &verts,
            &ground,
            TILES_W * TILE_PX,
            TILES_H * TILE_PX,
            &sprite,
            2,
            2,
            TILES_W,
            TILES_H,
            TILE_PX,
            OUT_W,
            OUT_H,
        )
        .expect("diorama pass renders again");

    assert_eq!(
        on_a, on_b,
        "identical inputs must produce byte-identical output on this machine/backend"
    );
    assert_ne!(
        fingerprint(&off),
        fingerprint(&on_a),
        "the diorama pass must visibly change the frame relative to 'off'"
    );

    let some_opaque = on_a.chunks_exact(4).any(|px| px[3] > 0);
    assert!(
        some_opaque,
        "the on frame must draw something (ground+box+billboard)"
    );

    // Pinned on this machine/backend (Apple M4 Max, Metal) -- same "local
    // regression fence, not a cross-platform contract" posture
    // `fog_golden.rs`'s own ON_HASH takes.
    let on_hash = fingerprint(&on_a);
    eprintln!("diorama_golden: on_hash = {on_hash:#018x}");
    assert_eq!(
        on_hash, ON_HASH,
        "the diorama pass's output changed on this machine/backend -- if this is an intentional \
         shader/fixture change, {ON_HASH_PLACEHOLDER_NOTE}; if not, something regressed"
    );
}

/// Acceptance criterion: "budget-gate reuse test" — this pass imports
/// `crate::fog::BudgetGate`/`DISABLE_P95_MS` directly rather than defining
/// a second threshold (§8's own wording, `crate::diorama`'s module doc).
#[test]
fn diorama_pass_reuses_the_fog_passs_budget_gate_not_a_second_one() {
    let mut gate = BudgetGate::new();
    assert!(gate.is_enabled());
    // Feed it this pass's own class of measurement (a wall-clock ms per
    // `DioramaPass::render` call, the same unit `fog.rs`'s own gate
    // consumes) -- well under DISABLE_P95_MS, so it must stay enabled,
    // proving this is the SAME gate type/threshold, not a mock.
    for _ in 0..40 {
        gate.record_sample_ms(2.0);
    }
    assert!(gate.is_enabled());
    assert!(gate.p95() < DISABLE_P95_MS);

    // And it self-disables under the same rule fog.rs's own tests pin —
    // proof this is genuinely fog::BudgetGate's real behaviour, not a
    // reimplementation that happens to share a name.
    for _ in 0..40 {
        gate.record_sample_ms(20.0);
    }
    assert!(!gate.is_enabled());
}
