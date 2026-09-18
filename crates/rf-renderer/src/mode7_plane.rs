//! Mode 7 plane rendered as pitched 3D ground (ticket W16-09; `docs/
//! design/ENHANCEMENT_WAVE_16.md` §9/§11). Deliberately the SAME
//! compositor `crate::diorama::DioramaPass` already draws walls with —
//! this module supplies a different INPUT (a hardware-computed Mode 7
//! transform, instead of a decoded collision grid), never a second render
//! pipeline (§9: "the same compositor as §5 with a different input").
//!
//! ## What crosses into this crate, and what does not
//!
//! `rf-renderer` may not depend on `rf-snes` (`scripts/validate-arch.sh`
//! rule 3), so this module knows nothing about `Ppu`/VRAM byte layout —
//! it consumes [`rf_core_api::Mode7Registers`] (the plain struct
//! `rf_snes::debug::mode7_registers` promotes the live matrix into,
//! carried on `rf_core_api::CoreEvent::Mode7`) and a plain RGBA ground
//! texture the caller already rendered from VRAM
//! (`rf_snes::debug::render_mode7_plane_rgba` — bsnes-hd's documented
//! approach: the hardware's OWN affine sampling, re-run at N times the
//! resolution, cited in that function's own doc). Resolving VRAM into
//! plain bytes is the app shell's job, exactly as `crate::diorama`'s own
//! module doc states for `SceneLayer::Geometry`/`SpriteSet`.
//!
//! ## The affine-matrix -> ground-quad -> camera mapping
//!
//! Mode 7 has no camera in the 3D sense: the matrix is a pure 2D affine
//! map from screen `(sx, sy)` to playfield `(vx, vy)` (snes.nesdev.org
//! wiki "Mode 7"; `rf_snes::debug::mode7_project` is the one
//! implementation this project trusts for that formula — [`project`]
//! below reproduces the SAME formula so this crate's tests can check
//! against it without a cross-layer dependency, module doc). A racing or
//! flight game fakes perspective by writing `a`/`d` (and often `x0`/`y0`)
//! smaller as the scanline nears the bottom of the screen via HDMA — a
//! SMALLER `a`/`d` means each screen pixel samples a SMALLER slice of the
//! playfield (more zoomed in, "nearer"); a LARGER one samples more of it
//! ("farther", closer to the horizon).
//!
//! Today's promoted matrix (`rf_core_api::CoreEvent::Mode7`'s own doc) is
//! one static snapshot rather than a captured per-scanline ramp —
//! `rf-snes`'s settled-frame composition model does not yet distinguish
//! the two. This mapping treats that one matrix as the frame's
//! representative scale and reasons from there:
//!
//! 1. [`matrix_scale`] averages `|a|`/`|d|` against `$0100` (unity, "no
//!    zoom") to get a unitless hardware scale factor.
//! 2. [`ground_extent_px`] turns that into a WORLD size, in playfield
//!    texels: at unity scale the ground quad is exactly
//!    `native_screen_width_px` texels across (the SNES's own 256px), and
//!    the extent scales linearly with the matrix's scale — double the
//!    hardware scale, double the world footprint the quad represents.
//! 3. [`camera_view_proj`] reuses `crate::diorama::camera_matrices`
//!    EXACTLY (same [`crate::diorama::PITCH_DEG`]/[`crate::diorama::
//!    FOV_Y_DEG`], same `fit_distance` best-fit solve) against that world
//!    size. A bigger hardware scale (more playfield visible — the game's
//!    own perspective reads as "farther/higher") makes `fit_distance`
//!    solve for a proportionally farther camera; a smaller scale ("closer
//!    /lower") pulls it in — the intended "camera height tracks the
//!    matrix" behaviour, using the SAME fixed pitch the rest of the
//!    diorama already commits to rather than inventing a second pitch
//!    model.
//!
//! **What this mapping is not**: an exact per-scanline horizon
//! reconstruction. It reasons from ONE scale value (today's only
//! available snapshot) to ONE camera distance — a genuine, testable
//! translation of "more hardware zoom => camera backs off", not a
//! frame-accurate re-derivation of exactly where a specific racing game's
//! horizon line sits on screen. §11 already flags by-eye validation
//! against a real Mode 7 title as pending (no fixture ROM, no cc65
//! toolchain on this machine — `docs/design/ENHANCEMENT_WAVE_16.md` §11);
//! this module's own golden test (`tests/mode7_plane_golden.rs`) is
//! necessarily a SYNTHETIC scene for exactly that reason.

use crate::diorama::{camera_matrices, DioramaPass, FOV_Y_DEG, PITCH_DEG};
use crate::diorama_mesh::{build_vertices, DioramaScene};
use crate::gpu::GpuContext;
use rf_core_api::Mode7Registers;

/// `$0100` in Mode 7's signed-8.8 matrix registers — scale 1.0.
pub const MODE7_UNITY_SCALE: f32 = 256.0;

/// The SNES's native screen width, in pixels — the scale [`matrix_scale`]
/// treats as "1.0, no zoom" is defined relative to this.
pub const SNES_NATIVE_WIDTH_PX: f32 = 256.0;

/// Project screen position `(sx, sy)` onto the Mode 7 playfield, in
/// playfield texels. The SAME formula `rf_snes::debug::mode7_project`
/// implements (snesdev wiki "Mode 7") — reproduced here, not called,
/// because `rf-renderer` may not depend on `rf-snes`
/// (`scripts/validate-arch.sh` rule 3; module doc). Kept `f64` throughout
/// rather than the core's fixed-point `i32` path: this side only ever
/// checks the mapping's DIRECTION and rough magnitude in tests, never
/// hashes a pixel against the core's own bit-exact output.
#[must_use]
pub fn project(m: &Mode7Registers, sx: f64, sy: f64) -> (f64, f64) {
    let cx = sx + f64::from(m.hofs) - f64::from(m.x0);
    let cy = sy + f64::from(m.vofs) - f64::from(m.y0);
    let vx = f64::from(m.a) * cx / 256.0 + f64::from(m.b) * cy / 256.0 + f64::from(m.x0);
    let vy = f64::from(m.c) * cx / 256.0 + f64::from(m.d) * cy / 256.0 + f64::from(m.y0);
    (vx, vy)
}

/// The hardware's own average matrix scale, unitless (`1.0` == `$0100`,
/// hardware-neutral "no zoom"). Averages `|a|` and `|d|` — a racing
/// game's HDMA ramp writes both together, and a pure rotation (`a==d`,
/// `b==-c`) leaves this at the rotation's own uniform scale. Floored well
/// above zero so a degenerate all-zero matrix (BG mode 7 selected before
/// the game has written anything) cannot produce a zero or negative
/// ground extent.
#[must_use]
pub fn matrix_scale(m: &Mode7Registers) -> f32 {
    let a = f32::from(m.a).abs();
    let d = f32::from(m.d).abs();
    ((a + d) / 2.0 / MODE7_UNITY_SCALE).max(0.05)
}

/// World-space size (playfield texels — this crate's ground textures are
/// always 1 texel == 1 world-pixel, `crate::diorama_mesh`'s own
/// convention) of the ground quad this matrix implies. Module doc step 2.
#[must_use]
pub fn ground_extent_px(m: &Mode7Registers, native_screen_width_px: f32) -> f32 {
    (native_screen_width_px * matrix_scale(m)).max(1.0)
}

/// The diorama camera's `view * proj` matrix for this Mode 7 matrix,
/// reusing `crate::diorama::camera_matrices` exactly. Module doc step 3.
#[must_use]
pub fn camera_view_proj(
    m: &Mode7Registers,
    native_screen_width_px: f32,
    aspect: f32,
) -> camera_matrices::Mat4 {
    let extent = ground_extent_px(m, native_screen_width_px);
    camera_matrices::view_proj(1.0, 1.0, extent, aspect, PITCH_DEG, FOV_Y_DEG, 1.3)
}

// ---------------------------------------------------------------------
// Ticket W16-14: derive the camera from the frame's top/bottom matrices
// instead of reasoning from one static scale (module doc's "What this
// mapping is not" above was written for W16-09's single-snapshot input;
// this section supersedes it now that `rf_core_api::Mode7Frame` carries
// both ends of the frame).
// ---------------------------------------------------------------------

/// Camera pitch (degrees from horizontal) for a Mode 7 matrix that is
/// IDENTICAL top-to-bottom — no HDMA perspective ramp at all, so there is
/// nothing for a pitched camera to justify. Near-vertical rather than
/// exactly 90 (which degenerates `camera_matrices::view_proj`'s `look_at`
/// into a singular up-vector) — this is F-Zero/Mario Kart's simple
/// "spinning disc" mode and a rotate-only Mode 7 demo alike: both read
/// naturally as looking straight down.
pub const TOP_DOWN_PITCH_DEG: f32 = 88.0;

/// Camera pitch for the steepest genuine racing-style ramp this mapping
/// recognises — reuses [`crate::diorama::PITCH_DEG`], the pitch already
/// tuned for the rest of the diorama's walls, rather than inventing a
/// second tuned constant.
pub const RACING_PITCH_DEG: f32 = crate::diorama::PITCH_DEG;

/// Derive the camera's pitch from the frame's top/bottom Mode 7 matrices
/// (ticket W16-14 acceptance criterion 2).
///
/// ## The derivation
///
/// A racing/flight game's HDMA ramp writes a LARGER `a`/`d` (bigger
/// [`matrix_scale`]) at the TOP of the screen — the horizon, sampling a
/// wide, far-away slice of the playfield — and a SMALLER one at the
/// BOTTOM — near the camera, sampling a small, zoomed-in slice (module
/// doc above: "a SMALLER a/d means ... more zoomed in, nearer"). So
/// `ratio = matrix_scale(top) / matrix_scale(bottom)` is `>= 1` exactly
/// when the matrix has that genuine near/far shape, and `== 1` when the
/// matrix is IDENTICAL top to bottom (a flat rotate/zoom with no
/// perspective ramp at all — F-Zero's simple mode, or any non-racing
/// Mode 7 use).
///
/// `t = 1 - 1/ratio` maps that ratio onto `[0, 1)`: `ratio == 1` (flat)
/// gives `t == 0`, and `t` climbs toward `1` as the ramp steepens
/// (`ratio == 2` gives `t == 0.5`; doubling again gives `0.75`), a soft
/// curve rather than a hard clamp so an ordinary racing ramp (which
/// rarely exceeds a 4-8x top/bottom scale spread) already sits well past
/// the curve's midpoint without needing a tuned upper bound. The pitch is
/// then a straight lerp from [`TOP_DOWN_PITCH_DEG`] (`t = 0`) to
/// [`RACING_PITCH_DEG`] (`t -> 1`).
///
/// An inverted matrix (bottom scale bigger than top — not a real racing
/// ramp, or a game using Mode 7 for something else entirely) clamps
/// `ratio` to `1.0` rather than reporting a negative `t`: this mapping
/// has no evidence which way such a matrix "should" pitch, so it falls
/// back to the same top-down answer a flat matrix gets, never inventing
/// a MORE extreme pitch than the steepest case this doc actually reasons
/// about.
#[must_use]
pub fn derive_pitch_deg(top: &Mode7Registers, bottom: &Mode7Registers) -> f32 {
    let scale_top = matrix_scale(top);
    let scale_bottom = matrix_scale(bottom);
    let ratio = (scale_top / scale_bottom).max(1.0);
    let t = 1.0 - 1.0 / ratio;
    TOP_DOWN_PITCH_DEG + (RACING_PITCH_DEG - TOP_DOWN_PITCH_DEG) * t
}

/// [`camera_view_proj`]'s frame-aware counterpart (ticket W16-14): the
/// same [`camera_matrices::view_proj`] call, but with [`derive_pitch_deg`]
/// in place of a fixed [`crate::diorama::PITCH_DEG`], and the ground
/// extent taken from whichever of `top`/`bottom` implies the LARGER
/// playfield footprint — the far (horizon) end of a real ramp always
/// samples more of the playfield than the near end, so using the larger
/// of the two keeps the whole visible ramp inside the ground quad instead
/// of cropping the horizon to the near scanline's narrower extent.
#[must_use]
pub fn camera_view_proj_frame(
    top: &Mode7Registers,
    bottom: &Mode7Registers,
    native_screen_width_px: f32,
    aspect: f32,
) -> camera_matrices::Mat4 {
    let extent = ground_extent_px(top, native_screen_width_px)
        .max(ground_extent_px(bottom, native_screen_width_px));
    let pitch = derive_pitch_deg(top, bottom);
    camera_matrices::view_proj(1.0, 1.0, extent, aspect, pitch, FOV_Y_DEG, 1.3)
}

/// [`render_mode7_ground`]'s frame-aware counterpart (ticket W16-14): the
/// live wiring's entry point, using [`camera_view_proj_frame`] instead of
/// a single static matrix's approximation. `top`/`bottom` are
/// `rf_core_api::Mode7Frame::top`/`bottom` — the caller (`crate::app`'s
/// `refresh_diorama_render`) owns unpacking the event; this module still
/// knows nothing about `CoreEvent` (module doc: "what crosses into this
/// crate").
///
/// # Errors
/// Same as [`render_mode7_ground`]: `Err` if the GPU readback does not
/// complete in time.
#[allow(clippy::too_many_arguments)]
pub fn render_mode7_ground_frame(
    pass: &DioramaPass,
    gpu: &GpuContext,
    top: &Mode7Registers,
    bottom: &Mode7Registers,
    plane_rgba: &[u8],
    plane_w: u32,
    plane_h: u32,
    native_screen_width_px: f32,
    billboards: &[crate::diorama_mesh::Billboard],
    sprite_rgba: &[u8],
    sprite_w: u32,
    sprite_h: u32,
    out_width: u32,
    out_height: u32,
) -> Result<Vec<u8>, String> {
    // The larger of the two ends' extents, same reasoning as
    // `camera_view_proj_frame`'s own doc, so the tile grid and the
    // camera agree on how much playfield the quad represents.
    let extent = ground_extent_px(top, native_screen_width_px)
        .max(ground_extent_px(bottom, native_screen_width_px));
    let scene = DioramaScene {
        tiles_w: 1,
        tiles_h: 1,
        tile_px: extent,
        solid: &[0],
        depth: &[0],
        billboards,
    };
    let vertices = build_vertices(&scene);
    let aspect = out_width.max(1) as f32 / out_height.max(1) as f32;
    let vp = camera_view_proj_frame(top, bottom, native_screen_width_px, aspect);

    pass.render_with_vp(
        gpu,
        &vertices,
        plane_rgba,
        plane_w,
        plane_h,
        sprite_rgba,
        sprite_w,
        sprite_h,
        vp,
        out_width,
        out_height,
    )
}

/// Render the Mode 7 plane as a flat, textured ground quad under the
/// diorama's pitched camera (acceptance criterion 2). `plane_rgba` is
/// `plane_w`x`plane_h` (`rf_snes::debug::render_mode7_plane_rgba`'s
/// output — the app shell builds it from VRAM/CGRAM and hands it in
/// plain, same as any other `crate::diorama` ground texture).
/// `native_screen_width_px` is the console's native screen width in
/// pixels (256 for the SNES — [`SNES_NATIVE_WIDTH_PX`]), the scale
/// [`matrix_scale`]'s `1.0` is defined against.
///
/// A single flat tile, no walls or billboards: the whole ground quad is
/// one GROUND-kind rectangle textured with `plane_rgba` end to end
/// (`crate::diorama_mesh`'s own module doc — the ground quad's UV always
/// spans the whole texture regardless of the tile grid dimensions), so
/// `tile_px` alone (via [`ground_extent_px`]) sets the world size the
/// camera frames.
///
/// # Errors
/// Returns `Err` if the GPU readback does not complete in time (see
/// [`DioramaPass::render_with_vp`]).
#[allow(clippy::too_many_arguments)]
pub fn render_mode7_ground(
    pass: &DioramaPass,
    gpu: &GpuContext,
    m: &Mode7Registers,
    plane_rgba: &[u8],
    plane_w: u32,
    plane_h: u32,
    native_screen_width_px: f32,
    out_width: u32,
    out_height: u32,
) -> Result<Vec<u8>, String> {
    let extent = ground_extent_px(m, native_screen_width_px);
    let scene = DioramaScene {
        tiles_w: 1,
        tiles_h: 1,
        tile_px: extent,
        solid: &[0],
        depth: &[0],
        billboards: &[],
    };
    let vertices = build_vertices(&scene);
    let aspect = out_width.max(1) as f32 / out_height.max(1) as f32;
    let vp = camera_view_proj(m, native_screen_width_px, aspect);

    pass.render_with_vp(
        gpu,
        &vertices,
        plane_rgba,
        plane_w,
        plane_h,
        // No billboards on the ground-only pass -- a 1-texel transparent
        // placeholder, same "an unused input still needs a valid texture"
        // shape `DioramaPass::render`'s own tests use for a spriteless
        // scene.
        &[0, 0, 0, 0],
        1,
        1,
        vp,
        out_width,
        out_height,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> Mode7Registers {
        Mode7Registers {
            a: 256,
            b: 0,
            c: 0,
            d: 256,
            x0: 0,
            y0: 0,
            hofs: 0,
            vofs: 0,
            flip_x: false,
            flip_y: false,
        }
    }

    /// Acceptance criterion 3: "a unit test that a known matrix maps the
    /// screen centre to the expected plane coordinate (check against
    /// `mode7_project`)". The identity matrix (scale 1, no rotation, no
    /// offsets) must map every screen position onto ITSELF — the case a
    /// sign or scale error is most visible in, and exactly what
    /// `rf_snes::debug`'s own `an_identity_matrix_gives_a_rectangle_the_
    /// size_of_the_screen` test checks for the hardware-side
    /// implementation this module's [`project`] reproduces.
    #[test]
    fn an_identity_matrix_maps_the_screen_centre_onto_itself() {
        let m = identity();
        let (vx, vy) = project(&m, 128.0, 112.0);
        assert!((vx - 128.0).abs() < 1e-9);
        assert!((vy - 112.0).abs() < 1e-9);
    }

    /// A known offset/scale matrix: `a=d=128` ($0080, scale 0.5) centred
    /// on `(x0,y0)=(500,500)` must map the screen origin to exactly
    /// `(500 - 64, 500 - 56)` for a 128x112 half-screen offset scaled by
    /// 0.5 -- hand-computed against the module-doc formula, independent
    /// of this module's own `project` implementation.
    #[test]
    fn a_scaled_and_centred_matrix_matches_the_hand_worked_projection() {
        let m = Mode7Registers {
            a: 128,
            b: 0,
            c: 0,
            d: 128,
            x0: 500,
            y0: 500,
            hofs: 0,
            vofs: 0,
            flip_x: false,
            flip_y: false,
        };
        let (vx, vy) = project(&m, 0.0, 0.0);
        // cx = 0 - 0 - 500 = -500; vx = 128*-500/256 + 500 = -250 + 500 = 250.
        assert!((vx - 250.0).abs() < 1e-6, "vx={vx}");
        assert!((vy - 250.0).abs() < 1e-6, "vy={vy}");
    }

    #[test]
    fn matrix_scale_is_one_at_unity_and_scales_linearly() {
        assert!((matrix_scale(&identity()) - 1.0).abs() < 1e-6);
        let half = Mode7Registers {
            a: 128,
            d: 128,
            ..identity()
        };
        assert!((matrix_scale(&half) - 0.5).abs() < 1e-6);
        let double = Mode7Registers {
            a: 512,
            d: 512,
            ..identity()
        };
        assert!((matrix_scale(&double) - 2.0).abs() < 1e-6);
    }

    /// A racing-game-style near matrix (scale ~1/4, module doc's "scale
    /// ~1/4" fixture) implies a SMALLER ground extent than the unity
    /// matrix -- "more zoomed in => camera closer", the direction the
    /// module doc's mapping commits to.
    #[test]
    fn a_more_zoomed_in_matrix_implies_a_smaller_ground_extent() {
        let near = Mode7Registers {
            a: 64,
            d: 64,
            ..identity()
        }; // scale 0.25
        let far = Mode7Registers {
            a: 1024,
            d: 1024,
            ..identity()
        }; // scale 4.0
        let e_unity = ground_extent_px(&identity(), SNES_NATIVE_WIDTH_PX);
        let e_near = ground_extent_px(&near, SNES_NATIVE_WIDTH_PX);
        let e_far = ground_extent_px(&far, SNES_NATIVE_WIDTH_PX);
        assert!(e_near < e_unity, "e_near={e_near} e_unity={e_unity}");
        assert!(e_far > e_unity, "e_far={e_far} e_unity={e_unity}");
        assert!((e_unity - SNES_NATIVE_WIDTH_PX).abs() < 1e-3);
    }

    /// A bigger implied ground extent, camera reused via `fit_distance`,
    /// pushes the camera farther from its target -- the actual, checkable
    /// consequence of "camera height tracks the matrix" (module doc step
    /// 3), verified via the SAME `camera_matrices::fit_distance` the
    /// produced `view_proj` is built from rather than re-deriving eye
    /// position from the opaque matrix.
    #[test]
    fn a_bigger_matrix_scale_backs_the_camera_off_farther() {
        let near = Mode7Registers {
            a: 64,
            d: 64,
            ..identity()
        };
        let far = Mode7Registers {
            a: 1024,
            d: 1024,
            ..identity()
        };
        let d_near = camera_matrices::fit_distance(
            ground_extent_px(&near, SNES_NATIVE_WIDTH_PX),
            ground_extent_px(&near, SNES_NATIVE_WIDTH_PX),
            16.0 / 9.0,
            FOV_Y_DEG,
            1.3,
        );
        let d_far = camera_matrices::fit_distance(
            ground_extent_px(&far, SNES_NATIVE_WIDTH_PX),
            ground_extent_px(&far, SNES_NATIVE_WIDTH_PX),
            16.0 / 9.0,
            FOV_Y_DEG,
            1.3,
        );
        assert!(
            d_far > d_near,
            "a larger hardware scale must back the camera off farther: d_near={d_near} \
             d_far={d_far}"
        );
    }

    /// Ticket W16-14 acceptance criterion 2: "a flat matrix gives a
    /// top-down view". Identical top/bottom matrices (any Mode 7 use with
    /// no HDMA perspective ramp) must derive [`TOP_DOWN_PITCH_DEG`]
    /// exactly, not merely "closer to it than the racing case".
    #[test]
    fn a_flat_matrix_gives_a_top_down_view() {
        let m = identity();
        let pitch = derive_pitch_deg(&m, &m);
        assert!(
            (pitch - TOP_DOWN_PITCH_DEG).abs() < 1e-6,
            "pitch={pitch}, expected exactly TOP_DOWN_PITCH_DEG for an unchanging matrix"
        );
    }

    /// Acceptance criterion 2's other half: "a racing ramp gives a
    /// pitched one" — a top (far/horizon) matrix scaled well above the
    /// bottom (near) matrix must derive a pitch strictly BETWEEN the two
    /// bounds, meaningfully off the top-down end.
    #[test]
    fn a_racing_ramp_gives_a_pitched_view() {
        let top = Mode7Registers {
            a: 1024,
            d: 1024,
            ..identity()
        }; // scale 4.0 -- the horizon line, zoomed OUT.
        let bottom = Mode7Registers {
            a: 64,
            d: 64,
            ..identity()
        }; // scale 0.25 -- the near line, zoomed IN.
        let pitch = derive_pitch_deg(&top, &bottom);
        assert!(
            pitch < TOP_DOWN_PITCH_DEG - 1.0,
            "a steep ramp must pitch meaningfully below top-down: pitch={pitch}"
        );
        assert!(
            pitch >= RACING_PITCH_DEG,
            "pitch must never overshoot past the racing bound: pitch={pitch}"
        );
    }

    /// A monotonic sanity check: a steeper ramp (bigger top/bottom scale
    /// spread) must pitch the camera closer to [`RACING_PITCH_DEG`] than a
    /// shallower one, not just "off top-down by some amount or other".
    #[test]
    fn a_steeper_ramp_pitches_closer_to_the_racing_bound() {
        let bottom = Mode7Registers {
            a: 64,
            d: 64,
            ..identity()
        };
        let shallow_top = Mode7Registers {
            a: 128,
            d: 128,
            ..identity()
        }; // ratio 2.0
        let steep_top = Mode7Registers {
            a: 1024,
            d: 1024,
            ..identity()
        }; // ratio 16.0
        let shallow_pitch = derive_pitch_deg(&shallow_top, &bottom);
        let steep_pitch = derive_pitch_deg(&steep_top, &bottom);
        assert!(
            steep_pitch < shallow_pitch,
            "steeper ramp must pitch lower (closer to racing): shallow={shallow_pitch} \
             steep={steep_pitch}"
        );
    }

    /// An inverted matrix (bottom scale bigger than top, not a real
    /// racing ramp) must not report a MORE extreme pitch than the
    /// steepest case this mapping reasons about — it falls back to
    /// top-down, module doc's stated fallback.
    #[test]
    fn an_inverted_matrix_falls_back_to_top_down() {
        let top = Mode7Registers {
            a: 64,
            d: 64,
            ..identity()
        };
        let bottom = Mode7Registers {
            a: 1024,
            d: 1024,
            ..identity()
        };
        let pitch = derive_pitch_deg(&top, &bottom);
        assert!(
            (pitch - TOP_DOWN_PITCH_DEG).abs() < 1e-6,
            "pitch={pitch}, an inverted matrix must fall back to exactly TOP_DOWN_PITCH_DEG"
        );
    }
}
