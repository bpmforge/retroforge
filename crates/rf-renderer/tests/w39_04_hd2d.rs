//! W39-04: the HD-2D render path on a synthetic scene (no ROM content): wall
//! extrusion from a baked-style depth grid, billboard objects with per-class
//! shadows, a tilted camera and a depth of field.
//!
//! Set `RF_W39_SHEET_DIR=<dir>` to also write the tour screenshots
//! (`docs/screens/w39` is where they are committed):
//!
//! ```text
//! RF_W39_SHEET_DIR=docs/screens/w39 cargo test -p rf-renderer --test w39_04_hd2d screenshots
//! ```

use rf_renderer::diorama::DioramaPass;
use rf_renderer::diorama_objects::ObjectClass;
use rf_renderer::gpu::GpuContext;
use rf_renderer::hd2d::{
    code, plan, render, Hd2dLattice, Hd2dLook, Hd2dMarker, Hd2dStage, MAX_ENTITIES,
};
use rf_renderer::lens::LensPass;

const W: u32 = 256;
const H: u32 = 224;
const T: u32 = 16;
const COLS: u32 = W / T;
const ROWS: u32 = H / T;

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

struct Scene {
    picture: Vec<u8>,
    sprites: Vec<u8>,
    depth: Vec<u8>,
    markers: Vec<Hd2dMarker>,
}

fn put(buf: &mut [u8], x: usize, y: usize, c: [u8; 4]) {
    let i = (y * W as usize + x) * 4;
    buf[i..i + 4].copy_from_slice(&c);
}

/// A made-up level: a sky, a brick floor, two floating brick platforms, and
/// five blobs (one per class) on the sprite layer.
fn scene() -> Scene {
    let (cols, rows) = (COLS as usize, ROWS as usize);
    let mut depth = vec![0u8; cols * rows];
    let mut solid = |c: usize, r: usize| {
        if c < cols && r < rows {
            depth[r * cols + c] = 8;
        }
    };
    for c in 0..cols {
        solid(c, rows - 2);
        solid(c, rows - 1);
    }
    for c in 3..7 {
        solid(c, 8);
    }
    for c in 10..13 {
        solid(c, 5);
    }
    solid(14, rows - 3);
    solid(15, rows - 3);
    solid(15, rows - 4);

    let mut picture = vec![0u8; (W * H * 4) as usize];
    for y in 0..H as usize {
        for x in 0..W as usize {
            // Sky: a vertical gradient with a few soft hills near the floor.
            let t = y as f32 / H as f32;
            let hill = ((x as f32 / 38.0).sin() * 14.0) as i32 + 170;
            let c = if (y as i32) > hill {
                [70, 140 + (x % 7) as u8, 80, 255]
            } else {
                [(90.0 + 90.0 * t) as u8, (150.0 + 70.0 * t) as u8, 235, 255]
            };
            put(&mut picture, x, y, c);
        }
    }
    for r in 0..rows {
        for c in 0..cols {
            if depth[r * cols + c] == 0 {
                continue;
            }
            for y in 0..T as usize {
                for x in 0..T as usize {
                    let edge = x == 0 || y == 0 || x == T as usize - 1 || y == T as usize - 1;
                    let v = if edge {
                        110
                    } else if (x / 4 + y / 4) % 2 == 0 {
                        190
                    } else {
                        170
                    };
                    put(
                        &mut picture,
                        c * T as usize + x,
                        r * T as usize + y,
                        [v, v / 2 + 20, 50, 255],
                    );
                }
            }
        }
    }

    let mut sprites = vec![0u8; (W * H * 4) as usize];
    let floor = ((rows - 2) as u32 * T) as i32;
    // (class code, x, feet y, w, h, colour)
    let specs = [
        (code::PLAYER, 40, floor, 16, 28, [60u8, 90, 230]),
        (code::ENEMY, 88, floor, 16, 16, [220, 50, 50]),
        (code::PROJECTILE, 150, floor - 40, 10, 10, [250, 220, 40]),
        (code::PICKUP, 190, floor - 6, 12, 12, [60, 200, 90]),
        (code::SCENERY, 226, floor, 18, 20, [140, 100, 60]),
    ];
    let mut markers = Vec::new();
    for (cc, x, feet, w, h, col) in specs {
        let (x0, y0) = (x - w / 2, feet - h);
        for yy in 0..h {
            for xx in 0..w {
                let (dx, dy) = (
                    (xx as f32 + 0.5) / w as f32 - 0.5,
                    (yy as f32 + 0.5) / h as f32 - 0.5,
                );
                if dx * dx * 4.0 + dy * dy * 4.0 <= 1.0 {
                    put(
                        &mut sprites,
                        (x0 + xx) as usize,
                        (y0 + yy) as usize,
                        [col[0], col[1], col[2], 255],
                    );
                }
            }
        }
        markers.push(Hd2dMarker::from_rect(x0, y0, w as u16, h as u16, cc));
    }
    Scene {
        picture,
        sprites,
        depth,
        markers,
    }
}

fn stage<'a>(s: &'a Scene, depth: &'a [u8]) -> Hd2dStage<'a> {
    Hd2dStage {
        picture_rgba: &s.picture,
        sprite_rgba: &s.sprites,
        size: (W, H),
        lattice: Hd2dLattice {
            origin: (0.0, 0.0),
            tile_px: T as f32,
            cols: COLS,
            rows: ROWS,
            depth,
        },
    }
}

fn look() -> Hd2dLook {
    Hd2dLook {
        out: (1280, 720),
        ..Hd2dLook::default()
    }
}

/// Sum of absolute horizontal neighbour differences over rows `y0..y1`: low
/// when the rows are blurred.
fn detail(rgba: &[u8], w: u32, y0: u32, y1: u32) -> u64 {
    let mut e = 0u64;
    for y in y0..y1 {
        let row = &rgba[(y * w * 4) as usize..((y + 1) * w * 4) as usize];
        for x in 1..w as usize {
            for c in 0..3 {
                e += u64::from(row[x * 4 + c].abs_diff(row[(x - 1) * 4 + c]));
            }
        }
    }
    e
}

#[test]
fn the_scene_stands_five_classed_entities_and_extrudes_its_walls() {
    let s = scene();
    let p = plan(&stage(&s, &s.depth), &s.markers, &look());
    assert_eq!(p.entities.len(), 5);
    // Entities come in scan order; each class appears exactly once.
    let mut classes: Vec<String> = p.classes.iter().map(|c| format!("{c:?}")).collect();
    classes.sort();
    let mut want: Vec<String> = [
        ObjectClass::Player,
        ObjectClass::Enemy,
        ObjectClass::Projectile,
        ObjectClass::Pickup,
        ObjectClass::Scenery,
    ]
    .iter()
    .map(|c| format!("{c:?}"))
    .collect();
    want.sort();
    assert_eq!(classes, want);
    assert!(p.entities.len() <= MAX_ENTITIES);
    let flat = vec![0u8; s.depth.len()];
    let pf = plan(&stage(&s, &flat), &s.markers, &look());
    assert!(
        p.vertices.len() > pf.vertices.len(),
        "walls add faces: {} vs {}",
        p.vertices.len(),
        pf.vertices.len()
    );
    // Per-class shadows: no markers (all legacy) has more shadow quads than
    // the classed scene, whose scenery casts none.
    let shadows = |v: &[rf_renderer::diorama_mesh::Vertex]| {
        v.iter()
            .filter(|x| x.kind == rf_renderer::diorama_mesh::kind::SHADOW)
            .count()
    };
    let none = plan(&stage(&s, &s.depth), &[], &look());
    // Scenery casts none: one quad (6 vertices) fewer than the legacy look.
    assert!(shadows(&none.vertices) > 0);
    assert_eq!(shadows(&none.vertices) - shadows(&p.vertices), 6);
}

#[test]
fn the_focus_follows_the_player_down_the_frame() {
    let s = scene();
    let st = stage(&s, &s.depth);
    let at = |y: i32| {
        plan(
            &st,
            &[Hd2dMarker::from_rect(100, y, 16, 24, code::PLAYER)],
            &look(),
        )
        .focus_frac
    };
    assert!(at(180) > at(20));
}

#[test]
fn rendering_is_deterministic_and_leaves_its_inputs_alone() {
    let Some(gpu) = gpu_or_skip("rendering_is_deterministic") else {
        return;
    };
    let s = scene();
    let (pic, spr, depth) = (s.picture.clone(), s.sprites.clone(), s.depth.clone());
    let (pass, lens) = (DioramaPass::new(&gpu), LensPass::new(&gpu));
    let st = stage(&s, &s.depth);
    let (a, _) = render(&gpu, &pass, &lens, &st, &s.markers, &look()).expect("renders");
    let (b, _) = render(&gpu, &pass, &lens, &st, &s.markers, &look()).expect("renders");
    assert_eq!(a, b);
    assert_eq!((pic, spr, depth), (s.picture, s.sprites, s.depth));
}

#[test]
fn depth_of_field_blurs_away_from_the_focus_and_keeps_the_focus_sharp() {
    let Some(gpu) = gpu_or_skip("depth_of_field") else {
        return;
    };
    let s = scene();
    let st = stage(&s, &s.depth);
    let (pass, lens) = (DioramaPass::new(&gpu), LensPass::new(&gpu));
    let sharp_look = Hd2dLook { dof: 0.0, ..look() };
    let (sharp, _) = render(&gpu, &pass, &lens, &st, &s.markers, &sharp_look).expect("renders");
    let (soft, plan) = render(&gpu, &pass, &lens, &st, &s.markers, &look()).expect("renders");
    assert_ne!(sharp, soft);
    let (w, h) = look().out;
    let focus_row = (plan.focus_frac * h as f32) as u32;
    // Rows well above the focus (the player stands low) that hold hills and
    // platform edges.
    let far = (h * 36 / 100, h * 54 / 100);
    let (d_sharp, d_soft) = (
        detail(&sharp, w, far.0, far.1),
        detail(&soft, w, far.0, far.1),
    );
    assert!(d_soft < d_sharp, "far rows blurred: {d_soft} vs {d_sharp}");
    // At the focus the two agree closely.
    let band = (focus_row.saturating_sub(6), (focus_row + 6).min(h));
    let (n_sharp, n_soft) = (
        detail(&sharp, w, band.0, band.1) as f64,
        detail(&soft, w, band.0, band.1) as f64,
    );
    assert!(
        n_soft > 0.8 * n_sharp,
        "focus kept sharp: {n_soft} vs {n_sharp}"
    );
}

/// The tour screenshots (only when `RF_W39_SHEET_DIR` is set).
#[test]
fn screenshots() {
    let Some(dir) = std::env::var_os("RF_W39_SHEET_DIR") else {
        return;
    };
    let Some(gpu) = gpu_or_skip("screenshots") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    let s = scene();
    let st = stage(&s, &s.depth);
    let (pass, lens) = (DioramaPass::new(&gpu), LensPass::new(&gpu));
    let save = |name: &str, l: &Hd2dLook, depth: &[u8]| {
        let st = Hd2dStage {
            lattice: Hd2dLattice {
                depth,
                ..st.lattice
            },
            ..st
        };
        let (img, _) = render(&gpu, &pass, &lens, &st, &s.markers, l).expect("renders");
        let png = rf_renderer::png::encode_rgba(&img, l.out.0, l.out.1);
        std::fs::write(dir.join(name), png).unwrap();
    };
    save("hd2d-synthetic.png", &look(), &s.depth);
    save(
        "hd2d-synthetic-no-dof.png",
        &Hd2dLook { dof: 0.0, ..look() },
        &s.depth,
    );
    save(
        "hd2d-synthetic-dusk.png",
        &Hd2dLook {
            sun_time: Some(0.7),
            yaw_deg: -8.0,
            ..look()
        },
        &s.depth,
    );
    // The same picture flat (no walls, straight-on): what extrusion adds.
    save(
        "hd2d-synthetic-flat.png",
        &Hd2dLook {
            pitch_deg: 20.0,
            yaw_deg: 0.0,
            dof: 0.0,
            ..look()
        },
        &vec![0u8; s.depth.len()],
    );
}
