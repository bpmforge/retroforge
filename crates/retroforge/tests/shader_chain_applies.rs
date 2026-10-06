//! **Settings › Video's shader reaches the picture** (ticket W20-02;
//! `docs/design/UX_WAVE_20.md` §4).
//!
//! `rf_renderer::ShaderChain` shipped with W3-02/W3-02a and nothing in the
//! app constructed it. This drives the real `RetroForgeApp` on a real GPU
//! (`WgpuTestRenderer`, the `diorama_live_view.rs` convention): with no
//! shader the play view shows the resolved frame byte-for-byte; with
//! Scanlines it shows something upscaled and different; back to none, the
//! exact frame again. Skips when no GPU adapter exists.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use retroforge::app::{RetroForgeApp, WINDOW_SIZE};

const FIXTURE: &str = "../../fixtures/nes/rf-scroller/build/rf-scroller.nes";

fn fnv(bytes: &[u8]) -> u64 {
    // Same FNV-1a the app uses for its display fingerprint.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn settle(harness: &mut Harness<'_, RetroForgeApp>, frames: u64) {
    let target = harness.state().frame_count_for_test() + frames;
    let start = Instant::now();
    while harness.state().frame_count_for_test() < target
        && start.elapsed() < Duration::from_secs(30)
    {
        harness.run_steps(1);
        std::thread::sleep(Duration::from_millis(4));
    }
}

/// The displayed frame's fingerprint and whether it equals the resolved
/// frame's, read together after one more repaint.
fn shown(harness: &mut Harness<'_, RetroForgeApp>) -> (Option<[usize; 2]>, bool) {
    let display = harness.state_mut().hash_display_for_test();
    let frame = harness.state().last_frame_rgba_for_test().map(|f| fnv(&f));
    (
        harness.state().display_texture_size_for_test(),
        display.is_some() && display == frame,
    )
}

#[test]
fn scanlines_change_the_picture_and_none_restores_it_exactly() {
    let dir = std::env::temp_dir().join(format!("retroforge_shader_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // SAFETY: first statement of the only test in this binary.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("RETROFORGE_CONFIG_DIR", &dir);
    }
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    assert!(fixture.exists(), "fixture missing: {}", fixture.display());

    let mut harness = Harness::builder()
        .with_size(egui::vec2(WINDOW_SIZE[0], WINDOW_SIZE[1]))
        .renderer(egui_kittest::wgpu::WgpuTestRenderer::default())
        .build_eframe(|cc| RetroForgeApp::new(cc));
    harness.run();
    if !harness.state().gpu_available_for_test() {
        eprintln!("SKIP: no GPU adapter; the shader chain cannot run here");
        return;
    }
    harness.state_mut().launch_rom(&fixture);
    let _ = harness.state_mut().hash_display_for_test();
    settle(&mut harness, 60);

    let (size, exact) = shown(&mut harness);
    assert_eq!(
        size,
        Some([256, 240]),
        "no shader: the frame at its own size"
    );
    assert!(
        exact,
        "no shader: the screen shows the resolved frame byte-for-byte"
    );

    harness.state_mut().video_settings_mut_for_test().shader = Some("scanlines".into());
    settle(&mut harness, 10);
    let (size, exact) = shown(&mut harness);
    let [w, h] = size.expect("a texture");
    assert!(
        w > 256 && h > 240 && w % 256 == 0 && h % 240 == 0,
        "upscaled before shading: {w}x{h}"
    );
    assert!(!exact, "Scanlines must change what is on screen");

    harness.state_mut().video_settings_mut_for_test().shader = None;
    settle(&mut harness, 10);
    let (size, exact) = shown(&mut harness);
    assert_eq!(size, Some([256, 240]));
    assert!(
        exact,
        "back to none: byte-identical to the resolved frame again"
    );
}
