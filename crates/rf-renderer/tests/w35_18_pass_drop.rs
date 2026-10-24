//! W35-18: a pass dropped with a double-buffered frame still in flight (a
//! `map_async` pending) gives every GPU resource back, raises no wgpu error
//! (a validation error panics in the default handler), and a new pass at
//! another size is not affected by it. This is what a ROM switch does to the
//! live Diorama: the app drops the pass and builds a new one for the new game.
//!
//! One test: the live counts are process-wide.

use rf_renderer::diorama::{live_resources, vertices_for_camera, DioramaCamera, DioramaPass};
use rf_renderer::diorama_mesh::DioramaScene;
use rf_renderer::gpu::GpuContext;

const COLS: u32 = 16;
const TILE: u32 = 16;

fn frame(gpu: &GpuContext, pass: &DioramaPass, shade: u8, w: u32, h: u32) -> Vec<u8> {
    let n = (COLS * COLS) as usize;
    let mut solid = vec![0u8; n];
    let mut depth = vec![0u8; n];
    solid[(6 * COLS + 6) as usize] = 1;
    depth[(6 * COLS + 6) as usize] = 16;
    let scene = DioramaScene {
        tiles_w: COLS,
        tiles_h: COLS,
        tile_px: TILE as f32,
        solid: &solid,
        depth: &depth,
        billboards: &[],
    };
    let camera = DioramaCamera::default();
    let v = vertices_for_camera(&scene, w, h, &camera);
    let side = COLS * TILE;
    let ground = [shade, shade, shade, 255].repeat((side * side) as usize);
    let sprite = vec![0u8; 16 * 16 * 4];
    pass.render_camera(
        gpu, &v, &ground, side, side, &sprite, 16, 16, COLS, COLS, TILE, w, h, &camera,
    )
    .expect("renders")
}

#[test]
fn dropping_a_pass_with_a_frame_in_flight_frees_everything_and_leaves_no_error() {
    let gpu = match GpuContext::request_headless() {
        Ok(g) => g,
        Err(e) => {
            assert!(std::env::var_os("CI").is_none(), "cannot skip in CI: {e}");
            eprintln!("SKIP: no wgpu adapter ({e})");
            return;
        }
    };
    let base = live_resources();
    assert_eq!(base.passes, 0);
    let sizes = [(256, 224), (256, 240), (512, 224), (256, 224)];
    for round in 0..10u32 {
        let (w, h) = sizes[round as usize % sizes.len()];
        let pass = DioramaPass::new(&gpu);
        pass.set_async_readback(true);
        for i in 0..3u8 {
            let got = frame(&gpu, &pass, 40 + i, w, h);
            assert_eq!(got.len(), (w * h * 4) as usize, "round {round}");
        }
        // A frame is in flight now. Live: one pass, its fixed pair of
        // staging buffers, its two kept textures.
        let live = live_resources();
        assert_eq!(live.passes, 1, "round {round}");
        assert_eq!(live.readback_buffers, 2, "round {round}");
        assert_eq!(live.sampled_textures, 2, "round {round}");
        drop(pass);
        assert_eq!(
            live_resources(),
            base,
            "round {round}: a dropped pass left resources"
        );
    }
    // The next pass, at the next game's size, answers with that size and its
    // own first frame (not a leftover of the dropped one).
    let reference = DioramaPass::new(&gpu);
    let want = frame(&gpu, &reference, 200, 256, 240);
    let pass = DioramaPass::new(&gpu);
    pass.set_async_readback(true);
    let first = frame(&gpu, &pass, 200, 256, 240);
    assert_eq!(first, want, "the first frame of a new pass is its own");
    drop((pass, reference));
    assert_eq!(live_resources(), base);
}
