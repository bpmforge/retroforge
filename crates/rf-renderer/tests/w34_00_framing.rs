//! Ticket W34-00: where the Diorama camera puts the game's own picture.
//!
//! HD2D_PLAN B2 says `placement` solves the camera distance from the whole
//! ground rectangle and divides by zoom, so "zoom 1.6x" is a different
//! distance for every level and the game's viewport is a shrinking slice of
//! the 3D image on a wide level. This prints the numbers (`--nocapture`) and
//! pins the two facts the plan relies on: the follow target lands in the
//! middle of the image, and the viewport's share of the image width falls as
//! the level widens.
//!
//! For each case the 256 x 224 window the game shows, centred on the follow
//! target, is projected through the real `view_proj_camera` matrix; the
//! share is its on-screen width over the image width at the target's depth.

use rf_renderer::diorama::{camera_matrices as cm, DioramaCamera, DISTANCE_FACTOR, FOV_Y_DEG};

/// Project a ground point (x, 0, z) to NDC.
fn ndc(vp: cm::Mat4, p: [f32; 3]) -> [f32; 2] {
    let c = |row: usize| vp[row] * p[0] + vp[4 + row] * p[1] + vp[8 + row] * p[2] + vp[12 + row];
    let w = c(3);
    [c(0) / w, c(1) / w]
}

struct Case {
    what: &'static str,
    ground: (f32, f32),
    follow: Option<(f32, f32)>,
}

fn share(case: &Case, zoom: f32, aspect: f32, pitch: f32) -> (f32, f32, f32, f32) {
    let cam = DioramaCamera {
        pitch_deg: pitch,
        yaw_deg: 0.0,
        zoom,
        follow: case.follow,
    };
    let (gw, gh) = case.ground;
    let vp = cm::view_proj_camera(
        gw / 16.0,
        gh / 16.0,
        16.0,
        aspect,
        FOV_Y_DEG,
        DISTANCE_FACTOR,
        &cam,
    );
    let p = cm::placement(
        gw / 16.0,
        gh / 16.0,
        16.0,
        aspect,
        FOV_Y_DEG,
        DISTANCE_FACTOR,
        &cam,
    );
    let (tx, tz) = (p.target[0], p.target[2]);
    // The game's window around the target: 256 wide, 224 deep.
    let l = ndc(vp, [tx - 128.0, 0.0, tz]);
    let r = ndc(vp, [tx + 128.0, 0.0, tz]);
    let c = ndc(vp, [tx, 0.0, tz]);
    ((r[0] - l[0]) / 2.0, c[0], c[1], p.distance)
}

#[test]
fn the_follow_target_is_centred_and_the_viewport_share_falls_with_level_width() {
    let cases = [
        Case {
            what: "live screen (33x29 cells of 8 px = 264x232)",
            ground: (264.0, 232.0),
            follow: None,
        },
        Case {
            what: "RF-Scroller fixture (48x14 metatiles = 768x224)",
            ground: (768.0, 224.0),
            follow: Some((384.0, 112.0)),
        },
        Case {
            what: "SMW-like level (3328x432)",
            ground: (3328.0, 432.0),
            follow: Some((1664.0, 216.0)),
        },
        Case {
            what: "long level (5000x432)",
            ground: (5000.0, 432.0),
            follow: Some((2500.0, 216.0)),
        },
    ];
    let mut shares = Vec::new();
    for aspect in [256.0 / 224.0, 16.0 / 9.0] {
        for zoom in [1.0f32, 1.6, 4.0] {
            for case in &cases {
                let (share_w, cx, cy, dist) = share(case, zoom, aspect, 40.0);
                println!(
                    "FRAMING aspect {aspect:.2} zoom {zoom:.1} {}: distance {dist:.0} px, game window = {:.1}% of image width, target at ndc ({cx:.3},{cy:.3})",
                    case.what,
                    share_w * 100.0
                );
                if aspect < 1.2 && (zoom - 1.6).abs() < 0.01 {
                    shares.push(share_w);
                }
                assert!(cx.abs() < 1e-3 && cy.abs() < 0.2, "the target is mid-image");
            }
        }
    }
    // At the preset's 1.6x on a 4:3-ish image the window shrinks as levels widen.
    assert!(shares.windows(2).all(|w| w[1] <= w[0] + 1e-4), "{shares:?}");
    assert!(
        shares[3] < 0.5 * shares[0],
        "a 5000 px level shrinks the window: {shares:?}"
    );
}
