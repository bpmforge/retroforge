//! Headless GPU tests for the HD-2D lens and lighting pass (ticket W30-04;
//! acceptance: "each effect has a strength setting and a headless test of
//! its parameters").
//!
//! Same conventions as `fog_golden.rs`: a real wgpu adapter (Metal on the
//! developer's machine), a clean skip when none exists, and assertions
//! about what each effect does to a synthetic scene rather than pinned
//! hashes -- a blur or a sky is easy to describe and hard to hash across
//! drivers. Every effect is checked three ways: strength 0 is the
//! identity, a positive strength does the described thing, and the thing
//! scales with the strength where that is meaningful.

use rf_renderer::gpu::GpuContext;
use rf_renderer::lens::{Effect, LensParams, LensPass, SpriteLayer, Weather};

fn gpu_or_skip(test_name: &str) -> Option<GpuContext> {
    match GpuContext::request_headless() {
        Ok(gpu) => Some(gpu),
        Err(e) => {
            if std::env::var_os("CI").is_some() {
                panic!("{test_name} cannot skip in CI: {e}");
            }
            eprintln!("SKIP {test_name}: no wgpu adapter in this environment ({e})");
            None
        }
    }
}

fn px(img: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * w + x) * 4) as usize;
    [img[i], img[i + 1], img[i + 2], img[i + 3]]
}

fn luma(p: [u8; 4]) -> f32 {
    0.2126 * f32::from(p[0]) + 0.7152 * f32::from(p[1]) + 0.0722 * f32::from(p[2])
}

fn mean_luma(img: &[u8]) -> f32 {
    img.chunks_exact(4)
        .map(|p| luma([p[0], p[1], p[2], p[3]]))
        .sum::<f32>()
        / (img.len() / 4) as f32
}

fn flat(w: u32, h: u32, v: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..w * h {
        out.extend_from_slice(&[v, v, v, 255]);
    }
    out
}

/// A scene with colour, gradients, hard edges and a translucent corner, so
/// "unchanged" means something.
fn busy(w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let checker = ((x / 3) + (y / 3)) % 2 == 0;
            let base = if checker { 210u32 } else { 35 };
            out.extend_from_slice(&[
                ((base + x) % 256) as u8,
                ((base + y * 2) % 256) as u8,
                (255 - base) as u8,
                if x < 4 && y < 4 { 90 } else { 255 },
            ]);
        }
    }
    out
}

fn max_diff(a: &[u8], b: &[u8]) -> u8 {
    a.iter()
        .zip(b)
        .map(|(x, y)| x.abs_diff(*y))
        .max()
        .unwrap_or(0)
}

fn changed(a: &[u8], b: &[u8]) -> usize {
    a.chunks_exact(4)
        .zip(b.chunks_exact(4))
        .filter(|(p, q)| p != q)
        .count()
}

/// A sprite layer with one opaque square (in `rect`), as RGBA.
fn sprite_square(w: u32, h: u32, rect: (u32, u32, u32, u32)) -> Vec<u8> {
    let mut out = vec![0u8; (w * h * 4) as usize];
    for y in rect.1..rect.3 {
        for x in rect.0..rect.2 {
            let i = ((y * w + x) * 4) as usize;
            out[i..i + 4].copy_from_slice(&[230, 40, 40, 255]);
        }
    }
    out
}

const W: u32 = 128;
const H: u32 = 96;

#[test]
fn every_effect_at_strength_zero_leaves_the_image_unchanged() {
    let Some(gpu) = gpu_or_skip("every_effect_at_strength_zero_leaves_the_image_unchanged") else {
        return;
    };
    let pass = LensPass::new(&gpu);
    let scene = busy(W, H);
    let sprite = sprite_square(W, H, (20, 20, 40, 40));
    let layer = Some(SpriteLayer {
        rgba: &sprite,
        width: W,
        height: H,
    });
    let zero = Effect::on(0.0);
    let cases = [
        (
            "bloom",
            LensParams {
                bloom: zero,
                ..LensParams::default()
            },
        ),
        (
            "tilt-shift",
            LensParams {
                tilt_shift: zero,
                ..LensParams::default()
            },
        ),
        (
            "shadows",
            LensParams {
                shadows: zero,
                ..LensParams::default()
            },
        ),
        (
            "grade",
            LensParams {
                grade: zero,
                time_of_day: 0.0,
                ..LensParams::default()
            },
        ),
        (
            "weather",
            LensParams {
                weather: Weather::Rain,
                weather_density: 0.0,
                ..LensParams::default()
            },
        ),
        ("nothing", LensParams::default()),
        // Flags off with big stored strengths: still the identity.
        (
            "disabled",
            LensParams {
                bloom: Effect {
                    enabled: false,
                    strength: 1.0,
                },
                tilt_shift: Effect {
                    enabled: false,
                    strength: 1.0,
                },
                shadows: Effect {
                    enabled: false,
                    strength: 1.0,
                },
                grade: Effect {
                    enabled: false,
                    strength: 1.0,
                },
                time_of_day: 0.0,
                ..LensParams::default()
            },
        ),
    ];
    for (name, params) in cases {
        let out = pass
            .render(&gpu, &scene, W, H, layer, &params)
            .expect("lens pass renders");
        assert_eq!(out.len(), scene.len());
        assert!(
            max_diff(&scene, &out) <= 1,
            "{name} at strength 0 changed the image"
        );
    }
}

#[test]
fn bloom_brightens_the_neighbourhood_of_a_bright_pixel_and_scales_with_strength() {
    let Some(gpu) = gpu_or_skip("bloom_brightens_the_neighbourhood_of_a_bright_pixel") else {
        return;
    };
    let pass = LensPass::new(&gpu);
    let mut scene = flat(W, H, 40);
    for y in 46..50 {
        for x in 62..66 {
            let i = ((y * W + x) * 4) as usize;
            scene[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
        }
    }
    let run = |s: f32| {
        pass.render(
            &gpu,
            &scene,
            W,
            H,
            None,
            &LensParams {
                bloom: Effect::on(s),
                ..LensParams::default()
            },
        )
        .expect("renders")
    };
    let weak = run(0.3);
    let strong = run(1.0);
    let near = (68, 48); // a few pixels from the bright block
    let far = (4, 4);
    let base = luma(px(&scene, W, near.0, near.1));
    assert!(
        luma(px(&weak, W, near.0, near.1)) > base + 2.0,
        "glow around the bright block"
    );
    assert!(
        luma(px(&strong, W, near.0, near.1)) > luma(px(&weak, W, near.0, near.1)),
        "more strength, more glow"
    );
    assert!(
        (luma(px(&strong, W, far.0, far.1)) - 40.0).abs() <= 1.0,
        "far from anything bright, nothing changes"
    );
    // A scene with nothing over the threshold gets no bloom at all.
    let dim = flat(W, H, 40);
    let out = pass
        .render(
            &gpu,
            &dim,
            W,
            H,
            None,
            &LensParams {
                bloom: Effect::on(1.0),
                ..LensParams::default()
            },
        )
        .expect("renders");
    assert!(
        max_diff(&dim, &out) <= 1,
        "nothing is bright, nothing blooms"
    );
}

#[test]
fn tilt_shift_keeps_the_focus_band_sharp_and_blurs_the_top_and_bottom() {
    let Some(gpu) = gpu_or_skip("tilt_shift_keeps_the_focus_band_sharp") else {
        return;
    };
    let pass = LensPass::new(&gpu);
    // Vertical 1-pixel stripes: maximum horizontal detail on every row.
    let mut scene = Vec::new();
    for _y in 0..H {
        for x in 0..W {
            let v = if x % 2 == 0 { 230 } else { 20 };
            scene.extend_from_slice(&[v, v, v, 255]);
        }
    }
    let run = |s: f32| {
        pass.render(
            &gpu,
            &scene,
            W,
            H,
            None,
            &LensParams {
                tilt_shift: Effect::on(s),
                tilt_center: 0.5,
                tilt_band: 0.3,
                ..LensParams::default()
            },
        )
        .expect("renders")
    };
    // Detail on a row = mean absolute difference between neighbours.
    let detail = |img: &[u8], y: u32| -> f32 {
        (1..W)
            .map(|x| (luma(px(img, W, x, y)) - luma(px(img, W, x - 1, y))).abs())
            .sum::<f32>()
            / (W - 1) as f32
    };
    let out = run(1.0);
    // The band is rows 34..62 of 96 (centre 48, height 0.3 * 96 = 28.8).
    for y in [40, 48, 56] {
        assert!(
            (detail(&out, y) - detail(&scene, y)).abs() < 1.0,
            "row {y} is in the focus band and stays sharp"
        );
    }
    assert!(
        detail(&out, 2) < detail(&scene, 2) * 0.5,
        "the top is blurred"
    );
    assert!(
        detail(&out, H - 3) < detail(&scene, H - 3) * 0.5,
        "the bottom is blurred"
    );
    let half = run(0.5);
    assert!(
        detail(&half, 2) > detail(&out, 2) && detail(&half, 2) < detail(&scene, 2),
        "half strength blurs half way"
    );
}

#[test]
fn shadows_darken_below_and_right_of_a_sprite_and_not_the_sprite_itself() {
    let Some(gpu) = gpu_or_skip("shadows_darken_below_and_right_of_a_sprite") else {
        return;
    };
    let pass = LensPass::new(&gpu);
    let rect = (30, 20, 50, 40);
    let sprite = sprite_square(W, H, rect);
    let mut scene = flat(W, H, 120);
    for y in rect.1..rect.3 {
        for x in rect.0..rect.2 {
            let i = ((y * W + x) * 4) as usize;
            scene[i..i + 4].copy_from_slice(&[230, 40, 40, 255]);
        }
    }
    let layer = Some(SpriteLayer {
        rgba: &sprite,
        width: W,
        height: H,
    });
    let run = |s: f32, layer: Option<SpriteLayer<'_>>| {
        pass.render(
            &gpu,
            &scene,
            W,
            H,
            layer,
            &LensParams {
                shadows: Effect::on(s),
                ..LensParams::default()
            },
        )
        .expect("renders")
    };
    let out = run(1.0, layer);
    // Just past the bottom-right corner, inside the shadow.
    let below_right = px(&out, W, rect.2 + 1, rect.3 + 1);
    assert!(below_right[0] < 100, "shadow below-right: {below_right:?}");
    // Mirror position past the top-left corner: the shadow is offset away.
    let above_left = px(&out, W, rect.0 - 2, rect.1 - 2);
    assert!(
        above_left[0] > below_right[0] + 10,
        "lighter up-left ({above_left:?}) than down-right ({below_right:?})"
    );
    // The sprite itself is untouched, and so is a pixel far from it.
    for (x, y) in [(rect.0 + 2, rect.1 + 2), (rect.2 - 1, rect.3 - 1), (40, 30)] {
        assert_eq!(
            px(&out, W, x, y),
            px(&scene, W, x, y),
            "sprite pixel ({x},{y})"
        );
    }
    assert!(
        max_diff(&px(&out, W, 3, 90), &px(&scene, W, 3, 90)) <= 1,
        "far corner"
    );
    // Strength scales the darkness.
    let soft = run(0.4, layer);
    assert!(
        px(&soft, W, rect.2 + 1, rect.3 + 1)[0] > below_right[0] + 5,
        "weaker shadow"
    );
    // Alpha untouched; no sprite layer, no shadow.
    assert!(out.chunks_exact(4).all(|p| p[3] == 255));
    let none = run(1.0, None);
    assert!(
        max_diff(&scene, &none) <= 1,
        "without a sprite layer shadows are a no-op"
    );
    // A layer whose size does not match its bytes is ignored, not trusted.
    let bad = run(
        1.0,
        Some(SpriteLayer {
            rgba: &sprite[..64],
            width: W,
            height: H,
        }),
    );
    assert!(max_diff(&scene, &bad) <= 1);
}

#[test]
fn the_night_grade_lowers_brightness_and_dusk_warms_it() {
    let Some(gpu) = gpu_or_skip("the_night_grade_lowers_brightness") else {
        return;
    };
    let pass = LensPass::new(&gpu);
    let scene = flat(W, H, 150);
    let run = |s: f32, t: f32| {
        pass.render(
            &gpu,
            &scene,
            W,
            H,
            None,
            &LensParams {
                grade: Effect::on(s),
                time_of_day: t,
                ..LensParams::default()
            },
        )
        .expect("renders")
    };
    let noon = run(1.0, 0.5);
    assert!(max_diff(&scene, &noon) <= 1, "noon is neutral");
    let night = run(1.0, 0.0);
    assert!(
        mean_luma(&night) < mean_luma(&scene) - 30.0,
        "night is darker"
    );
    let n = px(&night, W, 5, 5);
    assert!(n[2] > n[0], "night is blue: {n:?}");
    let dusk = run(1.0, 0.73);
    let d = px(&dusk, W, 5, 5);
    assert!(d[0] > d[2], "dusk is orange: {d:?}");
    let half = run(0.5, 0.0);
    assert!(
        mean_luma(&half) < mean_luma(&scene) && mean_luma(&half) > mean_luma(&night),
        "half strength is half way"
    );
}

#[test]
fn weather_adds_pixels_and_density_scales_the_count() {
    let Some(gpu) = gpu_or_skip("weather_adds_pixels_and_density_scales_the_count") else {
        return;
    };
    let pass = LensPass::new(&gpu);
    let (w, h) = (256, 224);
    let scene = flat(w, h, 100);
    let run = |weather: Weather, density: f32, frame: u32| {
        pass.render(
            &gpu,
            &scene,
            w,
            h,
            None,
            &LensParams {
                weather,
                weather_density: density,
                frame,
                ..LensParams::default()
            },
        )
        .expect("renders")
    };
    for weather in [Weather::Rain, Weather::Snow] {
        let none = run(weather, 0.0, 10);
        assert_eq!(changed(&scene, &none), 0, "{weather:?} at density 0");
        let light = changed(&scene, &run(weather, 0.2, 10));
        let heavy = changed(&scene, &run(weather, 0.8, 10));
        assert!(light > 0, "{weather:?} draws something at 0.2");
        assert!(
            heavy > light * 2,
            "{weather:?}: density 0.8 ({heavy}) well over 0.2 ({light})"
        );
        // Deterministic from the frame counter; it moves as the counter does.
        assert_eq!(
            run(weather, 0.5, 33),
            run(weather, 0.5, 33),
            "same frame, same sky"
        );
        assert_ne!(
            run(weather, 0.5, 33),
            run(weather, 0.5, 34),
            "the sky moves"
        );
    }
    // The sky never touches alpha.
    assert!(run(Weather::Rain, 1.0, 1)
        .chunks_exact(4)
        .all(|p| p[3] == 255));
    // Rain and snow are different skies.
    assert_ne!(run(Weather::Rain, 0.5, 5), run(Weather::Snow, 0.5, 5));
    // No sky selected, no weather, whatever the density.
    assert_eq!(changed(&scene, &run(Weather::None, 1.0, 5)), 0);
}

#[test]
fn the_effects_stack_and_keep_alpha() {
    let Some(gpu) = gpu_or_skip("the_effects_stack_and_keep_alpha") else {
        return;
    };
    let pass = LensPass::new(&gpu);
    let scene = busy(W, H);
    let sprite = sprite_square(W, H, (30, 30, 50, 50));
    let all = LensParams {
        bloom: Effect::on(0.6),
        tilt_shift: Effect::on(0.6),
        shadows: Effect::on(0.6),
        grade: Effect::on(0.6),
        time_of_day: 0.2,
        weather: Weather::Snow,
        weather_density: 0.4,
        frame: 7,
        ..LensParams::default()
    };
    let out = pass
        .render(
            &gpu,
            &scene,
            W,
            H,
            Some(SpriteLayer {
                rgba: &sprite,
                width: W,
                height: H,
            }),
            &all,
        )
        .expect("renders");
    assert_ne!(out, scene);
    for (a, b) in scene.chunks_exact(4).zip(out.chunks_exact(4)) {
        assert_eq!(a[3], b[3], "alpha is never touched");
    }
}

/// Cost, not correctness: `cargo test -p rf-renderer --release --test
/// lens_pass cost -- --ignored --nocapture`. Includes upload and readback,
/// exactly as the shell's call pays them.
#[test]
#[ignore = "timing report, run by hand"]
fn cost_per_frame_at_native_and_4x() {
    let Some(gpu) = gpu_or_skip("cost_per_frame_at_native_and_4x") else {
        return;
    };
    let pass = LensPass::new(&gpu);
    let all = LensParams {
        bloom: Effect::on(0.5),
        tilt_shift: Effect::on(0.5),
        shadows: Effect::on(0.5),
        grade: Effect::on(0.5),
        time_of_day: 0.3,
        weather: Weather::Rain,
        weather_density: 0.5,
        ..LensParams::default()
    };
    let grade_only = LensParams {
        grade: Effect::on(0.5),
        ..LensParams::default()
    };
    for (w, h) in [(256u32, 224u32), (1024, 896)] {
        let scene = busy(w, h);
        let sprite = sprite_square(w, h, (w / 4, h / 4, w / 2, h / 2));
        for (name, params) in [("all five", &all), ("grade only", &grade_only)] {
            let layer = Some(SpriteLayer {
                rgba: &sprite,
                width: w,
                height: h,
            });
            for _ in 0..5 {
                pass.render(&gpu, &scene, w, h, layer, params)
                    .expect("warm up");
            }
            let n = 60;
            let start = std::time::Instant::now();
            for _ in 0..n {
                pass.render(&gpu, &scene, w, h, layer, params)
                    .expect("renders");
            }
            let ms = start.elapsed().as_secs_f64() * 1000.0 / f64::from(n);
            eprintln!("lens {name} at {w}x{h}: {ms:.2} ms/frame");
        }
    }
}
