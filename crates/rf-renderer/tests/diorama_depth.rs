//! W34-05 (D5/D6): the Diorama pass has a depth buffer and 4x MSAA, renders
//! at the size it is asked for, and keeps its GPU resources between frames.
//! Clip-space quads (an identity view-projection) make the depth outcome
//! exact and independent of any camera.

use rf_renderer::diorama::DioramaPass;
use rf_renderer::diorama_mesh::{kind, Vertex};
use rf_renderer::gpu::GpuContext;

const IDENTITY: [f32; 16] = [
    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
];

fn gpu_or_skip(name: &str) -> Option<GpuContext> {
    match GpuContext::request_headless() {
        Ok(g) => Some(g),
        Err(e) => {
            if std::env::var_os("CI").is_some() {
                panic!("{name} cannot skip in CI: {e}");
            }
            eprintln!("SKIP {name}: no wgpu adapter ({e})");
            None
        }
    }
}

/// A clip-space rectangle at depth `z`, drawn as two triangles.
fn quad(x0: f32, y0: f32, x1: f32, y1: f32, z: f32, color: [f32; 4], k: u32) -> Vec<Vertex> {
    let v = |x, y, u, w| Vertex {
        pos: [x, y, z],
        uv: [u, w],
        color,
        kind: k,
    };
    let (a, b, c, d) = (
        v(x0, y0, 0.0, 1.0),
        v(x1, y0, 1.0, 1.0),
        v(x1, y1, 1.0, 0.0),
        v(x0, y1, 0.0, 0.0),
    );
    vec![a, b, c, a, c, d]
}

fn px(rgba: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * w + x) * 4) as usize;
    [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
}

fn render(
    pass: &DioramaPass,
    gpu: &GpuContext,
    v: &[Vertex],
    sprite: &[u8],
    w: u32,
    h: u32,
) -> Vec<u8> {
    pass.render_with_vp(gpu, v, &[255; 4], 1, 1, sprite, 1, 1, IDENTITY, w, h)
        .expect("renders")
}

const GREEN: [f32; 4] = [0.0, 1.0, 0.0, 1.0];
const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];

#[test]
fn depth_keeps_the_near_quad_even_when_the_far_one_is_drawn_after_it() {
    let Some(gpu) = gpu_or_skip("depth_keeps_the_near_quad") else {
        return;
    };
    let pass = DioramaPass::new(&gpu);
    let mut v = quad(-0.8, -0.8, 0.8, 0.8, 0.2, GREEN, kind::SIDE);
    v.extend(quad(-0.8, -0.8, 0.8, 0.8, 0.7, RED, kind::SIDE));
    let out = render(&pass, &gpu, &v, &[0; 4], 64, 64);
    assert_eq!(
        px(&out, 64, 32, 32),
        [0, 255, 0, 255],
        "the nearer quad stays"
    );
}

#[test]
fn a_transparent_sprite_texel_does_not_hide_what_is_behind_it() {
    let Some(gpu) = gpu_or_skip("a_transparent_sprite_texel") else {
        return;
    };
    let pass = DioramaPass::new(&gpu);
    // A sprite quad in front (its single texel is fully transparent), a
    // green quad behind it.
    let mut v = quad(-0.8, -0.8, 0.8, 0.8, 0.2, [1.0; 4], kind::SPRITE);
    v.extend(quad(-0.8, -0.8, 0.8, 0.8, 0.7, GREEN, kind::SIDE));
    let out = render(&pass, &gpu, &v, &[0, 0, 0, 0], 64, 64);
    assert_eq!(px(&out, 64, 32, 32), [0, 255, 0, 255]);
}

#[test]
fn msaa_gives_a_slanted_edge_partial_coverage_and_one_sample_does_not() {
    let Some(gpu) = gpu_or_skip("msaa_gives_a_slanted_edge") else {
        return;
    };
    let tri = |v: &mut Vec<Vertex>| {
        let p = |x, y| Vertex {
            pos: [x, y, 0.5],
            uv: [0.0, 0.0],
            color: [1.0, 1.0, 1.0, 1.0],
            kind: kind::SIDE,
        };
        v.extend([p(-0.9, -0.7), p(0.8, -0.9), p(0.1, 0.9)]);
    };
    let mut v = Vec::new();
    tri(&mut v);
    let partial = |pass: &DioramaPass| {
        let out = render(pass, &gpu, &v, &[0; 4], 64, 64);
        out.chunks_exact(4)
            .filter(|p| p[3] != 0 && p[3] != 255)
            .count()
    };
    let four = DioramaPass::new(&gpu);
    if four.samples() == 1 {
        eprintln!("SKIP: 4x MSAA unsupported here");
        return;
    }
    let one = DioramaPass::with_samples(&gpu, 1);
    assert_eq!(partial(&one), 0, "one sample: coverage is all or nothing");
    assert!(partial(&four) > 20, "4x MSAA antialiases the edge");
}

#[test]
fn it_renders_at_the_size_it_is_asked_for_and_straight_alpha_edges_keep_their_colour() {
    let Some(gpu) = gpu_or_skip("it_renders_at_the_size") else {
        return;
    };
    let pass = DioramaPass::new(&gpu);
    let v = quad(-0.5, -0.5, 0.5, 0.5, 0.5, GREEN, kind::SIDE);
    for (w, h) in [(64, 48), (300, 200), (1024, 896)] {
        let out = render(&pass, &gpu, &v, &[0; 4], w, h);
        assert_eq!(out.len() as u32, w * h * 4);
        assert_eq!(px(&out, w, w / 2, h / 2), [0, 255, 0, 255]);
        assert_eq!(px(&out, w, 1, 1)[3], 0, "outside the quad is transparent");
    }
    // A pixel half covered by a quad edge is half-transparent GREEN, not
    // half-dark green (straight alpha, ticket W34-05).
    let v = quad(-0.5, -0.5, 0.0001, 0.5, 0.5, GREEN, kind::SIDE);
    let out = render(&pass, &gpu, &v, &[0; 4], 33, 33);
    for p in out.chunks_exact(4).filter(|p| p[3] != 0 && p[3] != 255) {
        assert!(
            p[1] >= 250 && p[0] == 0,
            "an edge pixel keeps its colour: {p:?}"
        );
    }
}

#[test]
fn a_steady_scene_creates_no_gpu_resources_after_the_first_frame() {
    let Some(gpu) = gpu_or_skip("a_steady_scene_creates_no_gpu_resources") else {
        return;
    };
    let pass = DioramaPass::new(&gpu);
    let v = quad(-0.5, -0.5, 0.5, 0.5, 0.5, GREEN, kind::SIDE);
    render(&pass, &gpu, &v, &[0; 4], 128, 96);
    let first = pass.resource_counts();
    assert!(first.textures > 0 && first.buffers > 0 && first.bind_groups > 0);
    for _ in 0..10 {
        render(&pass, &gpu, &v, &[0; 4], 128, 96);
    }
    assert_eq!(pass.resource_counts(), first, "nothing was created again");

    // A different mesh uploads, but still creates nothing.
    let w = quad(-0.4, -0.4, 0.4, 0.4, 0.5, RED, kind::SIDE);
    render(&pass, &gpu, &w, &[0; 4], 128, 96);
    let after = pass.resource_counts();
    assert_eq!(after.vertex_uploads, first.vertex_uploads + 1);
    assert_eq!(
        (after.textures, after.buffers, after.bind_groups),
        (first.textures, first.buffers, first.bind_groups)
    );

    // A new output size rebuilds the targets, and only them.
    render(&pass, &gpu, &w, &[0; 4], 200, 96);
    let sized = pass.resource_counts();
    assert!(sized.textures > after.textures && sized.buffers > after.buffers);
    assert_eq!(sized.bind_groups, after.bind_groups);
}
