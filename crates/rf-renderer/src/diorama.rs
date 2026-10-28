//! The Diorama pass ("walls pop up"; ticket W16-06; `docs/design/
//! ENHANCEMENT_WAVE_16.md` §5/§8) — a standalone GPU pass, following
//! `crate::fog::FogPass`'s own conventions exactly (that module's own doc
//! explains why a standalone pass rather than a
//! [`crate::shader_chain::ChainStage`]: this pass needs more than one
//! input texture and a bind-group layout `ChainStage` does not have).
//! [`DioramaPass`] takes two textures (a ground texture and a
//! sprite-cutout texture, both plain RGBA — this crate has no dependency
//! on `rf-enhance`, `ARCHITECTURE.md` §3, so it knows nothing about
//! `SceneLayer::Geometry` or `SceneLayer::SpriteSet`; resolving those into
//! plain bytes plus [`crate::diorama_mesh::Billboard`] footprints is the
//! app shell's job, `crates/retroforge/src/enhanced_view.rs`) and a real
//! per-tile mesh ([`crate::diorama_mesh::build_vertices`]) rather than a
//! fullscreen triangle.
//!
//! ## Camera and projection: a UBO, and (W34-05) a depth buffer
//!
//! [`camera_matrices::view_proj`] builds a fixed-pitch ([`PITCH_DEG`]),
//! fixed-FOV ([`FOV_Y_DEG`]) perspective camera framing the whole tile
//! grid; ticket W30-05's [`DioramaCamera`] makes pitch, yaw and zoom
//! adjustable and can follow a world point instead
//! ([`camera_matrices::view_proj_camera`]), the default being exactly this
//! fixed camera — "only the camera pitches" (§5), never the input/hit-testing
//! space, which stays 2D in the shell entirely (this crate has no
//! opinion on input at all). The resulting `view * proj` matrix is
//! uploaded as a 64-byte UBO (`shaders/diorama.wgsl`'s `Camera`) that the
//! vertex stage applies. Ticket W34-05 (D5) gave the pass a depth
//! attachment and 4x MSAA, rendered at the size the shell asks for (the
//! play rect), on textures and buffers it keeps between frames. The mesh is
//! still emitted back-to-front (that fixes blending); depth settles what
//! painter's order gets wrong. Fragments with nothing to show are discarded
//! so they cannot hide what is behind them.
//!
//! ## Frame-budget gate reuse (§8)
//!
//! This pass does **not** define its own `BudgetGate`/threshold —
//! [`crate::fog::BudgetGate`] and [`crate::fog::DISABLE_P95_MS`] are
//! reused directly (§8: "shares the same harness ... rather than
//! inventing a second timing mechanism"); see `tests/diorama_golden.rs`'s
//! budget-gate-reuse test.

use std::borrow::Cow;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use crate::diorama_mesh::Vertex;
use crate::gpu::{align_up, GpuContext};
use crate::lighting::{self, Aabb, LightUbo, Lighting, ShadowFit, ShadowGate};

const DIORAMA_SHADER_SRC: &str = include_str!("shaders/diorama.wgsl");

/// Fixed camera pitch, degrees below horizontal (§5: "the camera pitches
/// at a **fixed** angle" — the concept-shots' Link's Awakening-diorama
/// framing this ticket cites). Neither measured nor tunable at runtime;
/// a plain middle-of-the-road "looking down at a diorama" angle.
pub const PITCH_DEG: f32 = 52.0;
/// Fixed vertical field of view, degrees.
pub const FOV_Y_DEG: f32 = 50.0;

/// Vertical field of view of the window-fit cameras (W34-05, D1). Narrower
/// than [`FOV_Y_DEG`]: the game's own window is small next to the distance
/// that frames it, and at 50 degrees the near edge of a tilted window is a
/// third larger than the far one, so the window cannot fill the frame like
/// the flat picture does. 30 degrees keeps the depth cue and the keystone mild.
pub const WINDOW_FOV_Y_DEG: f32 = 30.0;

/// Margin [`DioramaPass::render`] gives [`camera_matrices::fit_distance`].
pub const DISTANCE_FACTOR: f32 = 1.3;

/// The vertices for one frame seen through `camera`. The default camera
/// returns [`crate::diorama_mesh::build_vertices`] unchanged (so the pinned
/// goldens hold); anything else takes the orbit-aware
/// [`crate::diorama_mesh::build_vertices_oriented`], which needs the eye
/// position the matching [`camera_matrices::placement`] gives.
#[must_use]
pub fn vertices_for_camera(
    scene: &crate::diorama_mesh::DioramaScene<'_>,
    out_width: u32,
    out_height: u32,
    camera: &DioramaCamera,
) -> Vec<Vertex> {
    vertices_for_camera_classed(scene, out_width, out_height, camera, &[])
}

/// [`vertices_for_camera`] with a per-billboard
/// [`ObjectClass`](crate::diorama_objects::ObjectClass) (W39-02): the depth
/// and shadow of each sprite come from its class. Empty `classes` is
/// [`vertices_for_camera`] exactly.
#[must_use]
pub fn vertices_for_camera_classed(
    scene: &crate::diorama_mesh::DioramaScene<'_>,
    out_width: u32,
    out_height: u32,
    camera: &DioramaCamera,
    classes: &[crate::diorama_objects::ObjectClass],
) -> Vec<Vertex> {
    if camera.clamped() == DioramaCamera::default() {
        return crate::diorama_mesh::build_vertices_classed(scene, classes);
    }
    let aspect = out_width.max(1) as f32 / out_height.max(1) as f32;
    let p = camera_matrices::placement(
        scene.tiles_w.max(1) as f32,
        scene.tiles_h.max(1) as f32,
        scene.tile_px.max(1.0),
        aspect,
        FOV_Y_DEG,
        DISTANCE_FACTOR,
        camera,
    );
    crate::diorama_mesh::build_vertices_oriented_classed(
        scene,
        camera.clamped().yaw_deg,
        [p.eye[0], p.eye[2]],
        classes,
    )
}

/// The user-adjustable diorama camera (ticket W30-05; presentation only,
/// nothing here reaches the simulation). [`Default`] is exactly the
/// pre-W30-05 fixed camera ([`PITCH_DEG`], no yaw, 1.0x, whole level), so
/// every existing caller and pinned golden is unchanged.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DioramaCamera {
    /// Degrees below horizontal, clamped to [`Self::PITCH_MIN`]`..=`[`Self::PITCH_MAX`].
    pub pitch_deg: f32,
    /// Degrees of orbit about the vertical axis, clamped to `-30..=30`.
    /// Positive swings the camera toward world +X.
    pub yaw_deg: f32,
    /// `1.0` is the best fit of the whole level; larger moves the camera
    /// closer (clamped to `1.0..=4.0`).
    pub zoom: f32,
    /// World-pixel `(x, z)` to centre on, or `None` to frame the whole
    /// level. The shell passes the live camera's viewport centre here.
    pub follow: Option<(f32, f32)>,
}

impl Default for DioramaCamera {
    fn default() -> Self {
        Self {
            pitch_deg: PITCH_DEG,
            yaw_deg: 0.0,
            zoom: 1.0,
            follow: None,
        }
    }
}

impl DioramaCamera {
    pub const PITCH_MIN: f32 = 20.0;
    pub const PITCH_MAX: f32 = 80.0;
    pub const YAW_LIMIT: f32 = 30.0;
    pub const ZOOM_MAX: f32 = 4.0;

    /// [`Self::clamped`] with the pitch range widened to what the matrix can
    /// represent (`1..=89`): the Mode 7 ground derives its own near-vertical
    /// 88 degree pitch (`crate::mode7_plane::TOP_DOWN_PITCH_DEG`), which is
    /// outside the 20..=80 the user may pick but must not be altered.
    fn matrix_safe(self) -> Self {
        let c = self.clamped();
        let pitch = if self.pitch_deg.is_finite() {
            self.pitch_deg.clamp(1.0, 89.0)
        } else {
            c.pitch_deg
        };
        Self {
            pitch_deg: pitch,
            ..c
        }
    }

    /// The same camera with every field forced into its documented range
    /// (a non-finite number falls back to the default for that field).
    #[must_use]
    pub fn clamped(self) -> Self {
        let fin = |v: f32, d: f32| if v.is_finite() { v } else { d };
        Self {
            pitch_deg: fin(self.pitch_deg, PITCH_DEG).clamp(Self::PITCH_MIN, Self::PITCH_MAX),
            yaw_deg: fin(self.yaw_deg, 0.0).clamp(-Self::YAW_LIMIT, Self::YAW_LIMIT),
            zoom: fin(self.zoom, 1.0).clamp(1.0, Self::ZOOM_MAX),
            follow: self.follow.filter(|(x, z)| x.is_finite() && z.is_finite()),
        }
    }
}

/// Critically damped follow for the diorama camera's target (D1, W34-05).
///
/// The camera should glide to the player rather than jump, and never
/// overshoot or swing: a critically damped spring has no overshoot from rest
/// and none when a target that was moving stops (the follower is already
/// lagging behind it). One step is one frame (60 per second), so the result
/// depends on the number of frames, not on wall-clock time, and replays the
/// same. A target farther away than `snap_distance` (a room change, a level
/// load) is taken at once.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CameraSmoother {
    state: Option<SmootherState>,
    /// The last target that was actually known, and how many steps ago.
    held: Option<([f32; 2], u32)>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct SmootherState {
    pos: [f32; 2],
    vel: [f32; 2],
}

impl CameraSmoother {
    /// Natural frequency, radians per second: the camera settles in about
    /// four time constants, a third of a second.
    pub const OMEGA: f32 = 12.0;
    /// Seconds per step.
    pub const DT: f32 = 1.0 / 60.0;
    /// Closer than this to the target (and slow) counts as arrived.
    const REST: f32 = 0.01;
    /// How many steps a lost target is held before the camera gives up on it
    /// and goes to the fallback (two seconds).
    pub const HOLD_STEPS: u32 = 120;

    /// Whether the camera is on a target the game gave it (the player, seen
    /// now or held for [`Self::HOLD_STEPS`] steps) rather than on the
    /// fallback. Valid after a [`Self::step`].
    #[must_use]
    pub fn tracking(&self) -> bool {
        self.held.is_some()
    }

    /// Forget where the camera was; the next step lands on its target.
    pub fn reset(&mut self) {
        self.state = None;
        self.held = None;
    }

    /// [`Self::step_to`] for a target that may be missing this frame (the
    /// player detector drops out now and then): the last known target is
    /// held for [`Self::HOLD_STEPS`] steps, so a dropout does not swing the
    /// camera to `fallback` and back; after that the camera goes to `fallback`.
    pub fn step(
        &mut self,
        target: Option<(f32, f32)>,
        fallback: (f32, f32),
        snap_distance: f32,
    ) -> (f32, f32) {
        let aim = match (target, self.held) {
            (Some(t), _) => {
                self.held = Some(([t.0, t.1], 0));
                t
            }
            (None, Some((t, n))) if n < Self::HOLD_STEPS => {
                self.held = Some((t, n + 1));
                (t[0], t[1])
            }
            (None, _) => {
                self.held = None;
                fallback
            }
        };
        self.step_to(aim, snap_distance)
    }

    /// Advance one frame toward `target` and return where the camera is.
    pub fn step_to(&mut self, target: (f32, f32), snap_distance: f32) -> (f32, f32) {
        let t = [target.0, target.1];
        let Some(s) = self.state.as_mut() else {
            self.state = Some(SmootherState {
                pos: t,
                vel: [0.0; 2],
            });
            return target;
        };
        let far = (s.pos[0] - t[0]).hypot(s.pos[1] - t[1]);
        if !far.is_finite() || far > snap_distance.max(0.0) {
            *s = SmootherState {
                pos: t,
                vel: [0.0; 2],
            };
            return target;
        }
        let (w, dt) = (Self::OMEGA, Self::DT);
        let e = (-w * dt).exp();
        for ((pos, vel), goal) in s.pos.iter_mut().zip(s.vel.iter_mut()).zip(t) {
            let d = *pos - goal;
            let b = (*vel + w * d) * dt;
            *pos = goal + (d + b) * e;
            *vel = (*vel - w * b) * e;
            if (*pos - goal).abs() < Self::REST && vel.abs() < Self::REST {
                *pos = goal;
                *vel = 0.0;
            }
        }
        (s.pos[0], s.pos[1])
    }
}

/// Small hand-rolled column-major 4x4 matrix math (this crate has no
/// `glam`/`nalgebra`/etc. dependency, `Cargo.toml`'s own dependency list —
/// adding one for two matrices is not proportionate). Column-major to
/// match WGSL's own `mat4x4<f32>` storage/multiplication convention
/// (`shaders/diorama.wgsl`'s `vs_main` does `camera.view_proj *
/// vec4(in.pos, 1.0)` directly against this layout). Plain textbook
/// perspective/look-at formulas — "arithmetic nobody owns", the same
/// provenance stance `shader_chain.rs`'s own module doc takes for
/// `nearest`/`scanlines`.
pub mod camera_matrices {
    use super::DioramaCamera;

    pub type Mat4 = [f32; 16];

    #[must_use]
    pub fn perspective(fovy_rad: f32, aspect: f32, near: f32, far: f32) -> Mat4 {
        let f = 1.0 / (fovy_rad / 2.0).tan();
        let mut m = [0.0f32; 16];
        m[0] = f / aspect;
        m[5] = f;
        m[10] = (far + near) / (near - far);
        m[11] = -1.0;
        m[14] = (2.0 * far * near) / (near - far);
        m
    }

    fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
    }
    fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    }
    fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
        a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
    }
    fn normalize(a: [f32; 3]) -> [f32; 3] {
        let len = dot(a, a).sqrt().max(1e-6);
        [a[0] / len, a[1] / len, a[2] / len]
    }

    #[must_use]
    pub fn look_at(eye: [f32; 3], center: [f32; 3], up: [f32; 3]) -> Mat4 {
        let f = normalize(sub(center, eye));
        let s = normalize(cross(f, up));
        let u = cross(s, f);
        [
            s[0],
            u[0],
            -f[0],
            0.0, //
            s[1],
            u[1],
            -f[1],
            0.0, //
            s[2],
            u[2],
            -f[2],
            0.0, //
            -dot(s, eye),
            -dot(u, eye),
            dot(f, eye),
            1.0,
        ]
    }

    /// Column-major matrix multiply, `a * b` (apply `b` first, then `a`
    /// — the same order `proj * view` names below).
    #[must_use]
    pub fn mul(a: Mat4, b: Mat4) -> Mat4 {
        let mut out = [0.0f32; 16];
        for col in 0..4 {
            for row in 0..4 {
                let mut sum = 0.0;
                for k in 0..4 {
                    sum += a[k * 4 + row] * b[col * 4 + k];
                }
                out[col * 4 + row] = sum;
            }
        }
        out
    }

    /// Solve the camera distance that fits a `ground_w`x`ground_h`
    /// (world-pixel) rectangle inside a `fovy_deg`-tall frustum at the
    /// given viewport `aspect` (ticket W16-13 — replaces the old fixed
    /// `distance * 1.3`-style heuristic, which "backed off until the grid
    /// probably fit" without regard for the grid's own aspect ratio or
    /// the viewport's).
    ///
    /// **The fit target is the rectangle's bounding sphere** (radius =
    /// half its diagonal), the same "frame selection" solve a 3D editor's
    /// camera uses: a sphere looks identical from every direction, so —
    /// unlike a derivation tied to this pass's specific pitch — if the
    /// whole sphere is inside the frustum at this distance, the flat
    /// ground rectangle it encloses is too, regardless of pitch. Checked
    /// against BOTH axes (`dist_v` for the vertical half-angle, `dist_h`
    /// for the horizontal one derived from `aspect`) and the larger wins,
    /// so a wide-and-shallow grid framed by a narrow-and-tall viewport (or
    /// vice versa) is still fully inside frame — the fixed heuristic had
    /// no such per-axis check at all (`tests::a_wide_grid_in_a_narrow_
    /// viewport_needs_more_than_the_old_fixed_heuristic_gave_it` below
    /// demonstrates a case the old formula clips and this one does not).
    /// `margin` (`>= 1.0`) backs the camera off further for headroom; a
    /// bare `1.0` has the rectangle's corners exactly touching the
    /// frustum's edges.
    #[must_use]
    pub fn fit_distance(
        ground_w: f32,
        ground_h: f32,
        aspect: f32,
        fovy_deg: f32,
        margin: f32,
    ) -> f32 {
        let radius = ((ground_w / 2.0).powi(2) + (ground_h / 2.0).powi(2))
            .sqrt()
            .max(1e-3);
        let fovy_half = (fovy_deg.to_radians() / 2.0).max(1e-4);
        let dist_v = radius / fovy_half.sin();
        let fovx_half = (fovy_half.tan() * aspect.max(1e-3)).atan().max(1e-4);
        let dist_h = radius / fovx_half.sin();
        dist_v.max(dist_h) * margin.max(1.0)
    }

    /// Fixed-pitch, fixed-FOV camera framing a `tiles_w * tile_px` by
    /// `tiles_h * tile_px` ground plane (module doc), at the exact
    /// distance [`fit_distance`] solves for that grid/aspect pair.
    /// `distance_factor` (`>= 1.0`) is [`fit_distance`]'s `margin` —
    /// [`super::DioramaPass`] always calls this with its own documented
    /// constant, exposed here only so the number is named and testable
    /// rather than a magic literal buried in a render call.
    ///
    /// The W30-05 adjustable camera is [`view_proj_camera`]; this is its
    /// no-yaw, no-zoom, whole-level special case.
    #[must_use]
    pub fn view_proj(
        tiles_w: f32,
        tiles_h: f32,
        tile_px: f32,
        aspect: f32,
        pitch_deg: f32,
        fovy_deg: f32,
        distance_factor: f32,
    ) -> Mat4 {
        let camera = DioramaCamera {
            pitch_deg,
            ..DioramaCamera::default()
        };
        view_proj_camera(
            tiles_w,
            tiles_h,
            tile_px,
            aspect,
            fovy_deg,
            distance_factor,
            &camera,
        )
    }

    /// Where the camera sits and what it looks at, world space.
    #[derive(Debug, Clone, Copy, PartialEq)]
    pub struct Placement {
        pub eye: [f32; 3],
        pub target: [f32; 3],
        pub distance: f32,
    }

    /// [`Placement`] for `camera` (W30-05). The eye orbits `target` on a
    /// sphere of radius `fit_distance / zoom`: pitch lifts it, yaw swings
    /// it about the vertical axis. The eye sits on the `+Z` side of the
    /// target looking toward `-Z` (W30-05b): level row 0 is the far edge
    /// (screen top) and world `+X` is screen-right, so the picture is the
    /// level image seen from above, not mirrored or turned half way round.
    #[must_use]
    pub fn placement(
        tiles_w: f32,
        tiles_h: f32,
        tile_px: f32,
        aspect: f32,
        fovy_deg: f32,
        distance_factor: f32,
        camera: &DioramaCamera,
    ) -> Placement {
        let camera = camera.matrix_safe();
        let ground_w = tiles_w * tile_px;
        let ground_h = tiles_h * tile_px;
        let target = match camera.follow {
            Some((x, z)) => [x.clamp(0.0, ground_w), 0.0, z.clamp(0.0, ground_h)],
            None => [ground_w / 2.0, 0.0, ground_h / 2.0],
        };
        let distance =
            fit_distance(ground_w, ground_h, aspect, fovy_deg, distance_factor) / camera.zoom;
        let pitch = camera.pitch_deg.to_radians();
        let yaw = camera.yaw_deg.to_radians();
        let eye = [
            target[0] + yaw.sin() * pitch.cos() * distance,
            pitch.sin() * distance,
            target[2] + yaw.cos() * pitch.cos() * distance,
        ];
        Placement {
            eye,
            target,
            distance,
        }
    }

    /// The adjustable diorama camera's `proj * view` (W30-05).
    #[must_use]
    pub fn view_proj_camera(
        tiles_w: f32,
        tiles_h: f32,
        tile_px: f32,
        aspect: f32,
        fovy_deg: f32,
        distance_factor: f32,
        camera: &DioramaCamera,
    ) -> Mat4 {
        let ground_w = tiles_w * tile_px;
        let ground_h = tiles_h * tile_px;
        let extent = ground_w.max(ground_h).max(tile_px);
        let p = placement(
            tiles_w,
            tiles_h,
            tile_px,
            aspect,
            fovy_deg,
            distance_factor,
            camera,
        );
        // The far plane keeps the whole-level distance even when zoomed in,
        // so nothing the zoomed camera can see is clipped away.
        let far_distance = p.distance * camera.matrix_safe().zoom;
        let proj = perspective(
            fovy_deg.to_radians(),
            aspect.max(1e-3),
            1.0,
            extent.max(far_distance) * 8.0,
        );
        let view = look_at(p.eye, p.target, [0.0, 1.0, 0.0]);
        mul(proj, view)
    }

    /// How much of the user's top-down tilt a side-view camera keeps: the
    /// default 52 degrees becomes 13, a gentle look down at an upright
    /// picture. The tilt is what makes the near edge of the window larger than
    /// the far one, so it is kept small (W34-05: at 18 the bottom of the
    /// window came out a fifth narrower than the top).
    pub const SIDE_PITCH_SCALE: f32 = 0.25;
    pub const SIDE_PITCH_MIN: f32 = 6.0;
    pub const SIDE_PITCH_MAX: f32 = 30.0;
    /// How far inside the frame the window's corners sit (1.0 = touching; a hair more so rounding cannot push a corner out).
    pub const WINDOW_MARGIN: f32 = 1.004;

    /// The tilt a side-view diorama uses for a user tilt of `pitch_deg`.
    #[must_use]
    pub fn side_pitch_deg(pitch_deg: f32) -> f32 {
        (pitch_deg * SIDE_PITCH_SCALE).clamp(SIDE_PITCH_MIN, SIDE_PITCH_MAX)
    }

    /// Which plane a window lies on.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum WindowPlane {
        /// The floor, `y = 0`: the window spans world `x` and `z` (top-down).
        Ground,
        /// The upright picture, `z = 0`: the window spans world `x` and `y`
        /// (side view, `crate::diorama_side`).
        Upright,
    }

    /// The camera distance at which the four corners of a `half_w x half_h`
    /// window (half extents, world pixels) centred on `target` all land
    /// inside the frame, looking from direction `dir` (unit, target to eye).
    /// Solved by bisection, because the answer depends on pitch and on the
    /// window's own aspect: distance is monotone (a farther eye only shrinks
    /// the window), so this converges in a few dozen steps. `aspect` is the
    /// frame's width over height as the projection sees it.
    fn fit_window_distance(
        plane: WindowPlane,
        half: (f32, f32),
        target: [f32; 3],
        dir: [f32; 3],
        aspect: f32,
        fovy_deg: f32,
        margin: f32,
    ) -> f32 {
        let f = [-dir[0], -dir[1], -dir[2]];
        let right = normalize(cross(f, [0.0, 1.0, 0.0]));
        let up = cross(right, f);
        let tan_half = (fovy_deg.to_radians() / 2.0).tan().max(1e-4);
        let (sx, sy) = (1.0 / (tan_half * aspect.max(1e-3)), 1.0 / tan_half);
        let limit = 1.0 / margin.max(1.0);
        let corners: [[f32; 3]; 4] = {
            let (hw, hh) = half;
            let at = |dx: f32, dy: f32| match plane {
                WindowPlane::Ground => [target[0] + dx, target[1], target[2] + dy],
                WindowPlane::Upright => [target[0] + dx, target[1] + dy, target[2]],
            };
            [at(-hw, -hh), at(hw, -hh), at(-hw, hh), at(hw, hh)]
        };
        let fits = |d: f32| {
            let eye = [
                target[0] + dir[0] * d,
                target[1] + dir[1] * d,
                target[2] + dir[2] * d,
            ];
            corners.iter().all(|c| {
                let rel = sub(*c, eye);
                let z = dot(rel, f);
                z > 1e-3
                    && (dot(rel, right) * sx / z).abs() <= limit
                    && (dot(rel, up) * sy / z).abs() <= limit
            })
        };
        let mut hi = half.0.max(half.1).max(1.0);
        for _ in 0..64 {
            if fits(hi) {
                break;
            }
            hi *= 2.0;
        }
        let mut lo = 0.0_f32;
        for _ in 0..40 {
            let mid = (lo + hi) * 0.5;
            if fits(mid) {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        hi
    }

    /// [`Placement`] that frames the game's own window (D1, W34-05) on
    /// `plane`: the distance is solved so the `view_w x view_h` window
    /// (world pixels), narrowed by the camera's zoom, fills the frame with
    /// `target` (world `(x, z)` on the ground, `(x, y)` on the upright
    /// picture) at its centre. Tilt and turn come from `camera`; an upright
    /// picture takes the gentler [`side_pitch_deg`] tilt.
    ///
    /// `aspect` is the frame's width over height **divided by the display's
    /// pixel aspect**, which is how the TV stretch (8:7 pixels) comes along:
    /// the picture is framed as if pixels were square and the GPU stretches
    /// it into the wider frame, so the window fills both axes at once and
    /// toggling Diorama never changes the shape of the picture.
    #[must_use]
    pub fn window_placement(
        plane: WindowPlane,
        view_w: f32,
        view_h: f32,
        aspect: f32,
        fovy_deg: f32,
        camera: &DioramaCamera,
        target: (f32, f32),
    ) -> Placement {
        let camera = camera.matrix_safe();
        let target = match plane {
            WindowPlane::Ground => [target.0, 0.0, target.1],
            WindowPlane::Upright => [target.0, target.1, 0.0],
        };
        let pitch_deg = match plane {
            WindowPlane::Ground => camera.pitch_deg,
            WindowPlane::Upright => side_pitch_deg(camera.pitch_deg),
        };
        let (pitch, yaw) = (pitch_deg.to_radians(), camera.yaw_deg.to_radians());
        let dir = [
            yaw.sin() * pitch.cos(),
            pitch.sin(),
            yaw.cos() * pitch.cos(),
        ];
        let half = (view_w / 2.0 / camera.zoom, view_h / 2.0 / camera.zoom);
        let distance =
            fit_window_distance(plane, half, target, dir, aspect, fovy_deg, WINDOW_MARGIN);
        Placement {
            eye: [
                target[0] + dir[0] * distance,
                target[1] + dir[1] * distance,
                target[2] + dir[2] * distance,
            ],
            target,
            distance,
        }
    }

    /// `proj * view` for [`window_placement`]. The near plane scales with the
    /// distance (a depth buffer needs a sensible near/far ratio; with a fixed
    /// 1 px near, depth steps on a 5000 px level are half a pixel).
    #[must_use]
    pub fn view_proj_window(
        plane: WindowPlane,
        view_w: f32,
        view_h: f32,
        aspect: f32,
        fovy_deg: f32,
        camera: &DioramaCamera,
        target: (f32, f32),
    ) -> Mat4 {
        let p = window_placement(plane, view_w, view_h, aspect, fovy_deg, camera, target);
        let near = (p.distance * 0.05).max(1.0);
        let proj = perspective(
            fovy_deg.to_radians(),
            aspect.max(1e-3),
            near,
            (p.distance * 8.0).max(view_w.max(view_h) * 8.0),
        );
        mul(proj, look_at(p.eye, p.target, [0.0, 1.0, 0.0]))
    }

    /// [`window_placement`] for a side-view diorama (ticket W34-04; the
    /// picture stands upright at `z = 0`, see `crate::diorama_side`).
    #[must_use]
    pub fn side_placement(
        view_w: f32,
        view_h: f32,
        aspect: f32,
        fovy_deg: f32,
        camera: &DioramaCamera,
        target: (f32, f32),
    ) -> Placement {
        window_placement(
            WindowPlane::Upright,
            view_w,
            view_h,
            aspect,
            fovy_deg,
            camera,
            target,
        )
    }

    /// `proj * view` for [`side_placement`].
    #[must_use]
    pub fn view_proj_side(
        view_w: f32,
        view_h: f32,
        aspect: f32,
        fovy_deg: f32,
        camera: &DioramaCamera,
        target: (f32, f32),
    ) -> Mat4 {
        view_proj_window(
            WindowPlane::Upright,
            view_w,
            view_h,
            aspect,
            fovy_deg,
            camera,
            target,
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::diorama::{DioramaCamera, FOV_Y_DEG, PITCH_DEG};

        #[test]
        fn the_side_camera_centres_its_target_and_fits_the_window() {
            let cam = DioramaCamera::default();
            let (w, h) = (256.0_f32, 224.0_f32);
            let aspect = w / h;
            let target = (100.0, 60.0);
            let vp = view_proj_side(w, h, aspect, FOV_Y_DEG, &cam, target);
            let ndc = |x: f32, y: f32| {
                let c = mul_vec4(vp, [x, y, 0.0, 1.0]);
                [c[0] / c[3], c[1] / c[3]]
            };
            let mid = ndc(target.0, target.1);
            assert!(mid[0].abs() < 1e-3 && mid[1].abs() < 1e-3, "{mid:?}");
            // The window's corners are inside the frame at zoom 1.
            for (dx, dy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                let n = ndc(target.0 + dx * w / 2.0, target.1 + dy * h / 2.0);
                assert!(n[0].abs() <= 1.0 && n[1].abs() <= 1.0, "{n:?}");
            }
        }

        #[test]
        fn the_side_tilt_is_a_gentle_fraction_of_the_top_down_tilt() {
            assert!((side_pitch_deg(52.0) - 13.0).abs() < 0.1);
            assert!((side_pitch_deg(80.0) - 20.0).abs() < 1e-4);
            assert_eq!(side_pitch_deg(10.0), SIDE_PITCH_MIN);
        }

        #[test]
        fn the_grids_own_centre_projects_inside_the_view_frustum() {
            let vp = view_proj(4.0, 4.0, 16.0, 16.0 / 9.0, 52.0, 50.0, 1.3);
            // Ground-plane centre, at y=0 (module doc's `target`).
            let center = [32.0, 0.0, 32.0, 1.0];
            let clip = mul_vec4(vp, center);
            assert!(clip[3] > 0.0, "w must be positive in front of the camera");
            let ndc = [clip[0] / clip[3], clip[1] / clip[3], clip[2] / clip[3]];
            assert!(
                ndc[0].abs() <= 1.01 && ndc[1].abs() <= 1.01,
                "the framed grid's own centre must land inside the NDC frustum: {ndc:?}"
            );
        }

        #[test]
        fn a_farther_row_and_a_nearer_row_are_both_in_front_of_the_camera() {
            let vp = view_proj(1.0, 4.0, 16.0, 1.0, 52.0, 50.0, 1.3);
            let near_row = mul_vec4(vp, [8.0, 0.0, 8.0, 1.0]); // row 0 centre
            let far_row = mul_vec4(vp, [8.0, 0.0, 56.0, 1.0]); // row 3 centre
            assert!(near_row[3] > 0.0);
            assert!(far_row[3] > 0.0);
        }

        fn window_corner_ndc(
            plane: WindowPlane,
            vp: Mat4,
            target: (f32, f32),
            half: (f32, f32),
            sx: f32,
            sy: f32,
        ) -> [f32; 2] {
            let (x, y) = (target.0 + sx * half.0, target.1 + sy * half.1);
            let p = match plane {
                WindowPlane::Ground => [x, 0.0, y, 1.0],
                WindowPlane::Upright => [x, y, 0.0, 1.0],
            };
            let c = mul_vec4(vp, p);
            [c[0] / c[3], c[1] / c[3]]
        }

        /// D1: the target lands at screen centre (within 2 px of a 1024x896
        /// frame) on a 5000 px level at every zoom, and the window's corners
        /// are inside the frame, touching it on at least one axis.
        #[test]
        fn the_window_fit_centres_the_target_and_fills_the_frame_on_a_big_level() {
            let out = (1024.0_f32, 896.0_f32);
            let aspect = out.0 / out.1;
            for plane in [WindowPlane::Ground, WindowPlane::Upright] {
                for zoom in [1.0_f32, 1.6, 2.5, 4.0] {
                    for pitch in [30.0_f32, 52.0, 70.0] {
                        let cam = DioramaCamera {
                            pitch_deg: pitch,
                            zoom,
                            ..DioramaCamera::default()
                        };
                        let target = (4321.0_f32, 345.0);
                        let (w, h) = (256.0_f32, 224.0_f32);
                        let vp = view_proj_window(plane, w, h, aspect, FOV_Y_DEG, &cam, target);
                        let half = (w / 2.0 / zoom, h / 2.0 / zoom);
                        let mid = match plane {
                            WindowPlane::Ground => mul_vec4(vp, [target.0, 0.0, target.1, 1.0]),
                            WindowPlane::Upright => mul_vec4(vp, [target.0, target.1, 0.0, 1.0]),
                        };
                        let (nx, ny) = (mid[0] / mid[3], mid[1] / mid[3]);
                        assert!(
                            (nx * out.0 / 2.0).abs() < 2.0 && (ny * out.1 / 2.0).abs() < 2.0,
                            "{plane:?} zoom {zoom} pitch {pitch}: target at ({nx}, {ny})"
                        );
                        let mut worst = 0.0_f32;
                        for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                            let n = window_corner_ndc(plane, vp, target, half, sx, sy);
                            assert!(
                                n[0].abs() <= 1.001 && n[1].abs() <= 1.001,
                                "{plane:?} zoom {zoom} pitch {pitch}: corner {n:?} outside"
                            );
                            worst = worst.max(n[0].abs()).max(n[1].abs());
                        }
                        assert!(
                            worst > 0.99,
                            "{plane:?} zoom {zoom} pitch {pitch}: the window leaves a gap ({worst})"
                        );
                    }
                }
            }
        }

        /// The TV stretch: framing with `aspect / par` leaves the window as
        /// wide as the frame once the GPU stretches it, at 8:7.
        #[test]
        fn the_pixel_aspect_stretch_keeps_the_window_filling_a_wider_frame() {
            let par = 8.0_f32 / 7.0;
            // The play rect for 256x224 at 8:7, four times over.
            let frame = (256.0 * par * 4.0, 224.0 * 4.0);
            let aspect = frame.0 / frame.1 / par;
            let cam = DioramaCamera::default();
            let vp = view_proj_window(
                WindowPlane::Upright,
                256.0,
                224.0,
                aspect,
                FOV_Y_DEG,
                &cam,
                (128.0, 112.0),
            );
            let mut worst_x = 0.0_f32;
            let mut worst_y = 0.0_f32;
            for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                let n = window_corner_ndc(
                    WindowPlane::Upright,
                    vp,
                    (128.0, 112.0),
                    (128.0, 112.0),
                    sx,
                    sy,
                );
                worst_x = worst_x.max(n[0].abs());
                worst_y = worst_y.max(n[1].abs());
            }
            assert!(worst_x <= 1.001 && worst_y <= 1.001, "{worst_x} {worst_y}");
            assert!(worst_x.max(worst_y) > 0.99);
        }

        fn mul_vec4(m: Mat4, v: [f32; 4]) -> [f32; 4] {
            let mut out = [0.0f32; 4];
            for row in 0..4 {
                let mut sum = 0.0;
                for col in 0..4 {
                    sum += m[col * 4 + row] * v[col];
                }
                out[row] = sum;
            }
            out
        }

        /// The corner check the centre-only test above cannot make: all
        /// FOUR corners of the ground rectangle, not just its centre, must
        /// land inside the NDC frustum for the SOLVED distance
        /// ([`fit_distance`]) — the actual "does the whole grid fit"
        /// question ticket W16-13's acceptance criterion asks.
        #[test]
        fn all_four_ground_corners_fit_within_the_frustum_for_the_solved_distance() {
            let (tiles_w, tiles_h, tile_px, aspect): (f32, f32, f32, f32) =
                (5.0, 3.0, 16.0, 16.0 / 9.0);
            let vp = view_proj(
                tiles_w, tiles_h, tile_px, aspect, PITCH_DEG, FOV_Y_DEG, 1.05,
            );
            let (ground_w, ground_h) = (tiles_w * tile_px, tiles_h * tile_px);
            for &(x, z) in &[
                (0.0, 0.0),
                (ground_w, 0.0),
                (0.0, ground_h),
                (ground_w, ground_h),
            ] {
                let clip = mul_vec4(vp, [x, 0.0, z, 1.0]);
                assert!(
                    clip[3] > 0.0,
                    "corner ({x}, {z}) must be in front of the camera"
                );
                let ndc = [clip[0] / clip[3], clip[1] / clip[3]];
                assert!(
                    ndc[0].abs() <= 1.02 && ndc[1].abs() <= 1.02,
                    "corner ({x}, {z}) must land inside the NDC frustum at the solved \
                     distance: {ndc:?}"
                );
            }
        }

        /// Proof the fixed `extent * distance_factor` heuristic this
        /// ticket replaced was genuinely insufficient, not just
        /// differently-approximate: a wide-and-shallow grid viewed through
        /// a TALL, narrow-aspect viewport needs a distance driven by the
        /// grid's horizontal extent AND the viewport's own aspect, which
        /// the old heuristic (an isotropic `extent * 1.3` with no per-axis
        /// or aspect reasoning at all) badly under-shoots.
        #[test]
        fn a_wide_grid_in_a_narrow_viewport_needs_more_than_the_old_fixed_heuristic_gave_it() {
            let (tiles_w, tiles_h, tile_px, aspect): (f32, f32, f32, f32) = (20.0, 2.0, 16.0, 0.2);
            let (ground_w, ground_h) = (tiles_w * tile_px, tiles_h * tile_px);

            // The OLD formula, reproduced here exactly (module history):
            // `extent * distance_factor.max(1.0)`, feeding the same
            // `perspective`/`look_at` this module has always used —
            // duplicated rather than kept as a callable function, since
            // the whole point is that this formula is gone from the real
            // code path now.
            let old_extent = ground_w.max(ground_h).max(tile_px);
            let old_distance = old_extent * 1.3;
            let target = [ground_w / 2.0, 0.0, ground_h / 2.0];
            let pitch = PITCH_DEG.to_radians();
            let old_eye = [
                target[0],
                pitch.sin() * old_distance,
                target[2] + pitch.cos() * old_distance,
            ];
            let old_proj = perspective(
                FOV_Y_DEG.to_radians(),
                aspect.max(1e-3),
                1.0,
                old_extent.max(old_distance) * 8.0,
            );
            let old_view = look_at(old_eye, target, [0.0, 1.0, 0.0]);
            let old_vp = mul(old_proj, old_view);
            let far_corner_old = mul_vec4(old_vp, [ground_w, 0.0, ground_h, 1.0]);
            let ndc_old = [
                far_corner_old[0] / far_corner_old[3],
                far_corner_old[1] / far_corner_old[3],
            ];
            assert!(
                ndc_old[0].abs() > 1.02,
                "fixture invalid: the old fixed heuristic must actually clip this grid's far \
                 corner horizontally for this test to demonstrate anything, got ndc {ndc_old:?}"
            );

            // The NEW solve: the same corner must now fit.
            let new_vp = view_proj(
                tiles_w, tiles_h, tile_px, aspect, PITCH_DEG, FOV_Y_DEG, 1.05,
            );
            let far_corner_new = mul_vec4(new_vp, [ground_w, 0.0, ground_h, 1.0]);
            let ndc_new = [
                far_corner_new[0] / far_corner_new[3],
                far_corner_new[1] / far_corner_new[3],
            ];
            assert!(
                ndc_new[0].abs() <= 1.02 && ndc_new[1].abs() <= 1.02,
                "the solved distance must fit the same corner the old heuristic clipped: \
                 {ndc_new:?}"
            );
        }

        // --- ticket W30-05: the adjustable camera ------------------------

        fn ndc(vp: Mat4, p: [f32; 3]) -> [f32; 2] {
            let c = mul_vec4(vp, [p[0], p[1], p[2], 1.0]);
            [c[0] / c[3], c[1] / c[3]]
        }

        /// The pre-W30-05 `view_proj` (with W30-05b's eye on the `+Z` side): the default
        /// camera must match it bit for bit.
        fn legacy_view_proj(
            (tiles_w, tiles_h, tile_px, aspect): (f32, f32, f32, f32),
            pitch_deg: f32,
        ) -> Mat4 {
            let ground_w = tiles_w * tile_px;
            let ground_h = tiles_h * tile_px;
            let target = [ground_w / 2.0, 0.0, ground_h / 2.0];
            let extent = ground_w.max(ground_h).max(tile_px);
            let distance = fit_distance(ground_w, ground_h, aspect, FOV_Y_DEG, 1.3);
            let pitch = pitch_deg.to_radians();
            let eye = [
                target[0],
                pitch.sin() * distance,
                target[2] + pitch.cos() * distance,
            ];
            let proj = perspective(
                FOV_Y_DEG.to_radians(),
                aspect.max(1e-3),
                1.0,
                extent.max(distance) * 8.0,
            );
            mul(proj, look_at(eye, target, [0.0, 1.0, 0.0]))
        }

        const GRID: (f32, f32, f32, f32) = (20.0, 12.0, 16.0, 16.0 / 9.0);

        #[test]
        fn the_default_camera_reproduces_todays_view_proj_exactly() {
            let new = view_proj_camera(
                GRID.0,
                GRID.1,
                GRID.2,
                GRID.3,
                FOV_Y_DEG,
                1.3,
                &DioramaCamera::default(),
            );
            assert_eq!(new, legacy_view_proj(GRID, PITCH_DEG));
            // ...and so does the legacy-signature wrapper, at any pitch
            // (Mode 7 derives 88 degrees, outside the user's 20..=80).
            for pitch in [PITCH_DEG, 88.0, 70.0] {
                assert_eq!(
                    view_proj(GRID.0, GRID.1, GRID.2, GRID.3, pitch, FOV_Y_DEG, 1.3),
                    legacy_view_proj(GRID, pitch)
                );
            }
        }

        #[test]
        fn pitch_changes_the_matrix_and_the_view() {
            let at = |pitch_deg: f32| {
                view_proj_camera(
                    GRID.0,
                    GRID.1,
                    GRID.2,
                    GRID.3,
                    FOV_Y_DEG,
                    1.3,
                    &DioramaCamera {
                        pitch_deg,
                        ..DioramaCamera::default()
                    },
                )
            };
            assert_ne!(at(30.0), at(75.0));
            // A point 40px up projects higher on a low camera than a steep one
            // relative to its own footing: the two views differ.
            let low = ndc(at(30.0), [160.0, 40.0, 96.0]);
            let high = ndc(at(75.0), [160.0, 40.0, 96.0]);
            assert!((low[1] - high[1]).abs() > 1e-3);
        }

        #[test]
        fn yaw_moves_the_eye_and_zoom_moves_it_closer() {
            let place = |camera: DioramaCamera| {
                placement(GRID.0, GRID.1, GRID.2, GRID.3, FOV_Y_DEG, 1.3, &camera)
            };
            let base = place(DioramaCamera::default());
            let yawed = place(DioramaCamera {
                yaw_deg: 30.0,
                ..DioramaCamera::default()
            });
            assert!(yawed.eye[0] > base.eye[0] + 1.0);
            assert!((yawed.distance - base.distance).abs() < 1e-3);
            let zoomed = place(DioramaCamera {
                zoom: 2.0,
                ..DioramaCamera::default()
            });
            assert!((zoomed.distance - base.distance / 2.0).abs() < 1e-3);
        }

        #[test]
        fn follow_centres_the_given_world_point_on_screen() {
            let point = (90.0, 140.0);
            for (pitch, yaw, zoom) in [(52.0, 0.0, 1.0), (30.0, 20.0, 3.0), (75.0, -30.0, 4.0)] {
                let vp = view_proj_camera(
                    GRID.0,
                    GRID.1,
                    GRID.2,
                    GRID.3,
                    FOV_Y_DEG,
                    1.3,
                    &DioramaCamera {
                        pitch_deg: pitch,
                        yaw_deg: yaw,
                        zoom,
                        follow: Some(point),
                    },
                );
                let n = ndc(vp, [point.0, 0.0, point.1]);
                assert!(
                    n[0].abs() < 1e-3 && n[1].abs() < 1e-3,
                    "pitch {pitch} yaw {yaw} zoom {zoom}: {n:?}"
                );
            }
        }

        #[test]
        fn out_of_range_settings_are_clamped_not_trusted() {
            let c = DioramaCamera {
                pitch_deg: 5.0,
                yaw_deg: 99.0,
                zoom: 40.0,
                follow: Some((f32::NAN, 1.0)),
            }
            .clamped();
            assert_eq!(c.pitch_deg, DioramaCamera::PITCH_MIN);
            assert_eq!(c.yaw_deg, DioramaCamera::YAW_LIMIT);
            assert_eq!(c.zoom, DioramaCamera::ZOOM_MAX);
            assert_eq!(c.follow, None);
        }
    }
}

/// The box around everything the mesh draws (the contact ellipses sit a hair
/// above the floor and add nothing), or `None` for an empty mesh.
fn scene_bounds(vertices: &[Vertex]) -> Option<Aabb> {
    Aabb::around(
        vertices
            .iter()
            .filter(|v| v.kind != crate::diorama_mesh::kind::SHADOW)
            .map(|v| v.pos),
    )
}

/// The vertex ranges that cast into the shadow map: everything except the
/// contact ellipses and a triangle lying flat on the floor, which has
/// nothing to cast onto but itself, and the side view's backdrop planes.
fn caster_runs(vertices: &[Vertex]) -> Vec<std::ops::Range<u32>> {
    use crate::diorama_mesh::kind;
    let casts = |tri: &[Vertex]| {
        tri[0].kind != kind::SHADOW
            && tri[0].kind != kind::PLANE
            && tri[0].kind != kind::BACK
            && !(tri[0].kind == kind::GROUND && tri.iter().all(|v| v.pos[1] < 0.01))
    };
    let mut runs: Vec<std::ops::Range<u32>> = Vec::new();
    for (i, tri) in vertices.chunks_exact(3).enumerate() {
        if casts(tri) {
            let (from, to) = (i as u32 * 3, i as u32 * 3 + 3);
            match runs.last_mut() {
                Some(last) if last.end == from => last.end = to,
                _ => runs.push(from..to),
            }
        }
    }
    runs
}

fn camera_ubo_bytes(view_proj: camera_matrices::Mat4) -> [u8; 64] {
    let mut bytes = [0u8; 64];
    for (i, f) in view_proj.iter().enumerate() {
        bytes[i * 4..i * 4 + 4].copy_from_slice(&f.to_le_bytes());
    }
    bytes
}

/// Pack `verts` into the exact byte layout `DioramaPass::new`'s
/// `VertexAttribute`s declare (offsets 0/12/20/36, module doc) — written
/// field-by-field rather than a `#[repr(C)]` transmute so this stays
/// correct regardless of host endianness (this project only ever runs on
/// little-endian hosts today, but `to_le_bytes` costs nothing and removes
/// the assumption). Fills `out` (cleared first) so the caller can keep one
/// scratch buffer across frames.
fn vertex_bytes_into(verts: &[Vertex], out: &mut Vec<u8>) {
    out.clear();
    out.reserve(std::mem::size_of_val(verts));
    for v in verts {
        for f in v.pos {
            out.extend_from_slice(&f.to_le_bytes());
        }
        for f in v.uv {
            out.extend_from_slice(&f.to_le_bytes());
        }
        for f in v.color {
            out.extend_from_slice(&f.to_le_bytes());
        }
        out.extend_from_slice(&v.kind.to_le_bytes());
    }
}

/// W35-15: where the last `render_with_vp` call spent its time, in
/// microseconds: texture writes, bind groups and uniforms, vertex bytes and
/// upload, encoding, submit, readback collect. For profiling only.
pub static LAST_STAGE_US: [std::sync::atomic::AtomicU64; 6] =
    [const { std::sync::atomic::AtomicU64::new(0) }; 6];

fn stage_us(i: usize, since: std::time::Instant) -> std::time::Instant {
    LAST_STAGE_US[i].store(
        u64::try_from(since.elapsed().as_micros()).unwrap_or(u64::MAX),
        std::sync::atomic::Ordering::Relaxed,
    );
    std::time::Instant::now()
}

/// Colour format of the resolved image the pass reads back.
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// Depth format of the pass's depth attachment (W34-05, D5).
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// Sample count the pass asks for when the device supports it (D5).
pub const MSAA_SAMPLES: u32 = 4;

/// Whether `format` can be rendered at `count` samples on this device.
/// Probed with a validation error scope, as `superfx_gpu` does (the adapter
/// is not kept in [`GpuContext`]).
fn sample_count_ok(device: &wgpu::Device, format: wgpu::TextureFormat, count: u32) -> bool {
    if count == 1 {
        return true;
    }
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let _probe = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("rf-renderer::diorama::sample_probe"),
        size: wgpu::Extent3d {
            width: 4,
            height: 4,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: count,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    pollster::block_on(scope.pop()).is_none()
}

/// Live GPU resources of the pass, counted from construction to drop (W35-18):
/// `resource_counts` only counts creations, so it cannot show that a dropped
/// pass gave its buffers and textures back.
static LIVE_PASSES: AtomicUsize = AtomicUsize::new(0);
static LIVE_READBACKS: AtomicUsize = AtomicUsize::new(0);
static LIVE_SAMPLED: AtomicUsize = AtomicUsize::new(0);

/// What is alive right now, across every [`DioramaPass`] in the process.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LiveResources {
    /// Passes not yet dropped.
    pub passes: usize,
    /// Readback (staging) buffers held by live passes.
    pub readback_buffers: usize,
    /// Kept sampled textures (ground atlas, sprite layer) held by live passes.
    pub sampled_textures: usize,
}

/// The process-wide live counts (W35-18).
#[must_use]
pub fn live_resources() -> LiveResources {
    LiveResources {
        passes: LIVE_PASSES.load(Ordering::Relaxed),
        readback_buffers: LIVE_READBACKS.load(Ordering::Relaxed),
        sampled_textures: LIVE_SAMPLED.load(Ordering::Relaxed),
    }
}

/// Adds `n` to a live counter while it exists.
struct Tally(&'static AtomicUsize, usize);

impl Tally {
    fn new(counter: &'static AtomicUsize, n: usize) -> Self {
        counter.fetch_add(n, Ordering::Relaxed);
        Self(counter, n)
    }
}

impl Drop for Tally {
    fn drop(&mut self) {
        self.0.fetch_sub(self.1, Ordering::Relaxed);
    }
}

/// A texture the pass samples, kept between frames and recreated only when
/// its size changes.
struct SampledTexture {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    size: (u32, u32),
    /// The bytes the texture holds now (W35-15): the next frame uploads only
    /// the rows that differ from them, and nothing when none do.
    held: Vec<u8>,
    _tally: Tally,
}

/// The first and last row (of `row_bytes` each) where `new` differs from
/// `old`, or `None` when they are the same. Both are whole numbers of rows
/// and the same length.
fn dirty_rows(old: &[u8], new: &[u8], row_bytes: usize) -> Option<(usize, usize)> {
    debug_assert_eq!(old.len(), new.len());
    if row_bytes == 0 {
        return None;
    }
    let rows = new.len() / row_bytes;
    fn row(b: &[u8], y: usize, n: usize) -> &[u8] {
        &b[y * n..(y + 1) * n]
    }
    let first = (0..rows).find(|y| row(old, *y, row_bytes) != row(new, *y, row_bytes))?;
    // A row exists at `first`, so the search from the end finds one at or
    // after it; the fallback only keeps the type total.
    let last = (first..rows)
        .rev()
        .find(|y| row(old, *y, row_bytes) != row(new, *y, row_bytes))
        .unwrap_or(first);
    Some((first, last))
}

/// The per-size render targets and readback buffer.
struct Target {
    size: (u32, u32),
    /// The multisampled colour target (absent at one sample).
    msaa_view: Option<wgpu::TextureView>,
    resolved: wgpu::Texture,
    resolved_view: wgpu::TextureView,
    depth_view: wgpu::TextureView,
    /// Two readback buffers: the synchronous path uses the first; the
    /// double-buffered path alternates (one frame in flight).
    readbacks: [wgpu::Buffer; 2],
    padded_bytes_per_row: u32,
    _tally: Tally,
}

/// A readback that has been submitted and mapped but not yet collected
/// (the double-buffered path, [`DioramaPass::set_async_readback`]).
struct PendingReadback {
    slot: usize,
    done: std::sync::mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
}

/// Everything the pass keeps between frames (W34-05, D5/D6): textures,
/// buffers and the bind group are created when a size or capacity changes,
/// not on every call.
#[derive(Default)]
struct Cache {
    ground: Option<SampledTexture>,
    sprite: Option<SampledTexture>,
    bind_group: Option<wgpu::BindGroup>,
    vertex_buffer: Option<(wgpu::Buffer, u64)>,
    /// The bytes last uploaded to `vertex_buffer`: an unchanged mesh is not
    /// uploaded again.
    uploaded: Vec<u8>,
    scratch: Vec<u8>,
    target: Option<Target>,
    /// The sun's shadow map (W34-06); one texel while no map is wanted.
    shadow: Option<ShadowMap>,
    /// The caster pass's bind group (sprite texture, sampler, light).
    caster_bind_group: Option<wgpu::BindGroup>,
    pending: Option<PendingReadback>,
    /// The first double-buffered frame, kept to answer the second call (which
    /// has no earlier frame in flight either).
    primed: Option<Vec<u8>>,
    /// Which readback buffer the next async frame copies into.
    cursor: usize,
}

/// The depth texture the caster pass fills and the main pass samples.
struct ShadowMap {
    view: wgpu::TextureView,
    size: u32,
}

/// How many times the pass created or recreated a GPU resource, by kind
/// (W34-05 tests: a steady scene creates none after the first frame).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ResourceCounts {
    pub textures: u32,
    pub buffers: u32,
    pub bind_groups: u32,
    pub vertex_uploads: u32,
}

/// The Diorama pass's own pipelines/bind-group-layout/sampler (module doc:
/// not a [`crate::shader_chain::ChainStage`] — two input textures, a real
/// vertex buffer), plus the resources it reuses from frame to frame.
pub struct DioramaPass {
    bind_group_layout: wgpu::BindGroupLayout,
    /// Ground, box, side and sprite triangles: depth-tested and depth-written.
    solid_pipeline: wgpu::RenderPipeline,
    /// Contact shadows: depth-tested, never depth-written, nudged toward the
    /// camera so they sit on the surface they are drawn on.
    shadow_pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    camera_buffer: wgpu::Buffer,
    samples: u32,
    cache: Mutex<Cache>,
    counts: Mutex<ResourceCounts>,
    /// Depth-only pass into the shadow map (W34-06).
    caster_layout: wgpu::BindGroupLayout,
    caster_pipeline: wgpu::RenderPipeline,
    shadow_sampler: wgpu::Sampler,
    light_buffer: wgpu::Buffer,
    /// The sun, or `None` for the original unlit look (set by the shell).
    lighting: Mutex<Option<Lighting>>,
    shadow_gate: Mutex<ShadowGate>,
    /// Whether the last render drew with a shadow map.
    shadow_map_used: std::sync::atomic::AtomicBool,
    async_readback: std::sync::atomic::AtomicBool,
    _tally: Tally,
}

impl DioramaPass {
    /// A pass with [`MSAA_SAMPLES`] samples where the device supports them
    /// (one sample otherwise).
    #[must_use]
    pub fn new(gpu: &GpuContext) -> Self {
        Self::with_samples(gpu, MSAA_SAMPLES)
    }

    /// A pass asking for `samples` (1 or 4) multisamples; a count the device
    /// cannot render falls back to 1.
    #[must_use]
    pub fn with_samples(gpu: &GpuContext, samples: u32) -> Self {
        let samples = if samples > 1
            && sample_count_ok(&gpu.device, COLOR_FORMAT, samples)
            && sample_count_ok(&gpu.device, DEPTH_FORMAT, samples)
        {
            samples
        } else {
            1
        };
        let bind_group_layout =
            gpu.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("rf-renderer::diorama::bind_group_layout"),
                    entries: &[
                        wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: wgpu::ShaderStages::VERTEX,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Uniform,
                                has_dynamic_offset: false,
                                min_binding_size: None,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 1,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                                view_dimension: wgpu::TextureViewDimension::D2,
                                multisampled: false,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 2,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                                view_dimension: wgpu::TextureViewDimension::D2,
                                multisampled: false,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 3,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 4,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                sample_type: wgpu::TextureSampleType::Depth,
                                view_dimension: wgpu::TextureViewDimension::D2,
                                multisampled: false,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 5,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 6,
                            visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Uniform,
                                has_dynamic_offset: false,
                                min_binding_size: None,
                            },
                            count: None,
                        },
                    ],
                });
        // The caster pass cannot share that layout: it writes the very map
        // the main layout samples, and one resource cannot be both in a pass.
        let caster_layout = gpu
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("rf-renderer::diorama::caster_layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 6,
                        visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });

        let pipeline_layout = gpu
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("rf-renderer::diorama::pipeline_layout"),
                bind_group_layouts: &[Some(&bind_group_layout)],
                immediate_size: 0,
            });

        let shader = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("rf-renderer::diorama::shader"),
                source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(DIORAMA_SHADER_SRC)),
            });

        let vertex_stride = std::mem::size_of::<Vertex>() as wgpu::BufferAddress;
        let attributes = [
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x3,
                offset: 0,
                shader_location: 0,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: 12,
                shader_location: 1,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x4,
                offset: 20,
                shader_location: 2,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Uint32,
                offset: 36,
                shader_location: 3,
            },
        ];

        let make_pipeline = |label: &str, write_depth: bool, bias: wgpu::DepthBiasState| {
            gpu.device
                .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some(label),
                    layout: Some(&pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: &shader,
                        entry_point: Some("vs_main"),
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        buffers: &[wgpu::VertexBufferLayout {
                            array_stride: vertex_stride,
                            step_mode: wgpu::VertexStepMode::Vertex,
                            attributes: &attributes,
                        }],
                    },
                    // No culling: the quads are one-sided in the mesh and the
                    // oriented builders already drop faces the eye cannot see.
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: Some(wgpu::DepthStencilState {
                        format: DEPTH_FORMAT,
                        depth_write_enabled: Some(write_depth),
                        depth_compare: Some(wgpu::CompareFunction::LessEqual),
                        stencil: wgpu::StencilState::default(),
                        bias,
                    }),
                    multisample: wgpu::MultisampleState {
                        count: samples,
                        mask: !0,
                        alpha_to_coverage_enabled: false,
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &shader,
                        entry_point: Some("fs_main"),
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        targets: &[Some(wgpu::ColorTargetState {
                            format: COLOR_FORMAT,
                            blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                    }),
                    multiview_mask: None,
                    cache: None,
                })
        };
        let solid_pipeline = make_pipeline(
            "rf-renderer::diorama::solid_pipeline",
            true,
            wgpu::DepthBiasState::default(),
        );
        let shadow_pipeline = make_pipeline(
            "rf-renderer::diorama::shadow_pipeline",
            false,
            wgpu::DepthBiasState {
                constant: -4,
                slope_scale: -1.0,
                clamp: 0.0,
            },
        );

        let caster_pipeline_layout =
            gpu.device
                .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("rf-renderer::diorama::caster_pipeline_layout"),
                    bind_group_layouts: &[Some(&caster_layout)],
                    immediate_size: 0,
                });
        let caster_pipeline = gpu
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("rf-renderer::diorama::caster_pipeline"),
                layout: Some(&caster_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_shadow"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: vertex_stride,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &attributes,
                    }],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::LessEqual),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                // A fragment stage with no target: it exists to alpha-test a
                // billboard's silhouette.
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_shadow"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[],
                }),
                multiview_mask: None,
                cache: None,
            });
        let shadow_sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("rf-renderer::diorama::shadow_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });
        let light_buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rf-renderer::diorama::light"),
            size: lighting::LIGHT_UBO_BYTES as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        gpu.queue
            .write_buffer(&light_buffer, 0, &LightUbo::off().to_bytes());

        let sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("rf-renderer::diorama::sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let camera_buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rf-renderer::diorama::camera"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        DioramaPass {
            bind_group_layout,
            solid_pipeline,
            shadow_pipeline,
            sampler,
            camera_buffer,
            samples,
            cache: Mutex::new(Cache::default()),
            counts: Mutex::new(ResourceCounts::default()),
            caster_layout,
            caster_pipeline,
            shadow_sampler,
            light_buffer,
            lighting: Mutex::new(None),
            shadow_gate: Mutex::new(ShadowGate::default()),
            shadow_map_used: std::sync::atomic::AtomicBool::new(false),
            async_readback: std::sync::atomic::AtomicBool::new(false),
            _tally: Tally::new(&LIVE_PASSES, 1),
        }
    }

    /// Light the next frames (W34-06). `None` is the original look, the one
    /// every pinned golden renders; the shell sets a sun from the lens's time
    /// of day before it asks for a frame.
    pub fn set_lighting(&self, lighting: Option<Lighting>) {
        *self.lighting.lock().unwrap_or_else(|e| e.into_inner()) = lighting;
    }

    /// Double-buffer the readback (W34-06): each call submits its frame and
    /// returns the previous call's pixels, so the wait for the GPU overlaps
    /// the caller's other work instead of blocking it. Costs one frame of
    /// latency; the first call after a resize (or after switching this on)
    /// has no earlier frame and waits for its own. Off by default, so goldens
    /// and one-shot callers get the frame they asked for.
    pub fn set_async_readback(&self, on: bool) {
        self.async_readback
            .store(on, std::sync::atomic::Ordering::Relaxed);
    }

    /// Whether the readback is double-buffered.
    #[must_use]
    pub fn async_readback(&self) -> bool {
        self.async_readback
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The light in force.
    #[must_use]
    pub fn lighting(&self) -> Option<Lighting> {
        *self.lighting.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether the last frame was drawn with a shadow map (false: no sun, the
    /// map switched off, the budget fallback, or nothing to shade).
    #[must_use]
    pub fn shadow_map_used(&self) -> bool {
        self.shadow_map_used
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Feed the shadow budget gate one render time (ms); `true` on the call
    /// that trips it. The pass feeds it itself after every frame that used the
    /// map; this is for a shell that measures more than the pass does and for
    /// tests.
    pub fn record_shadow_sample_ms(&self, ms: f64) -> bool {
        self.shadow_gate
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .record_ms(ms)
    }

    /// Whether the budget gate still allows the shadow map.
    #[must_use]
    pub fn shadow_budget_allows_map(&self) -> bool {
        self.shadow_gate
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .allows_map()
    }

    /// Clear the shadow budget gate (the map comes back).
    pub fn reset_shadow_budget(&self) {
        self.shadow_gate
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .reset();
    }

    /// The multisample count in use (4, or 1 where the device has no 4x).
    #[must_use]
    pub fn samples(&self) -> u32 {
        self.samples
    }

    /// How many GPU resources this pass has created so far.
    #[must_use]
    pub fn resource_counts(&self) -> ResourceCounts {
        *self.counts.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Render one diorama frame: `vertices` (already back-to-front,
    /// `crate::diorama_mesh::build_vertices`) against `ground_rgba`
    /// (`ground_w`x`ground_h`) and `sprite_rgba` (`sprite_w`x`sprite_h`,
    /// the sprite-cutout texture billboards sample), into an
    /// `out_width`x`out_height` target with a fixed-pitch camera framing
    /// `tiles_w`x`tiles_h` tiles of `tile_px` each
    /// ([`camera_matrices::view_proj`]).
    ///
    /// Returns straight-alpha RGBA8 (transparent where nothing was
    /// drawn — the caller composites this over the rest of the scene the
    /// same way [`crate::composite::EnhancedCompositor`] composites any
    /// other layer).
    ///
    /// # Errors
    /// Returns `Err` if the GPU readback does not complete within
    /// [`crate::gpu::GPU_WAIT`].
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &self,
        gpu: &GpuContext,
        vertices: &[Vertex],
        ground_rgba: &[u8],
        ground_w: u32,
        ground_h: u32,
        sprite_rgba: &[u8],
        sprite_w: u32,
        sprite_h: u32,
        tiles_w: u32,
        tiles_h: u32,
        tile_px: u32,
        out_width: u32,
        out_height: u32,
    ) -> Result<Vec<u8>, String> {
        self.render_camera(
            gpu,
            vertices,
            ground_rgba,
            ground_w,
            ground_h,
            sprite_rgba,
            sprite_w,
            sprite_h,
            tiles_w,
            tiles_h,
            tile_px,
            out_width,
            out_height,
            &DioramaCamera::default(),
        )
    }

    /// [`Self::render`] through an adjustable [`DioramaCamera`] (W30-05).
    /// `vertices` should come from [`vertices_for_camera`] with the same
    /// camera and output size, so billboards and draw order match the view.
    ///
    /// # Errors
    /// Same as [`Self::render_with_vp`].
    #[allow(clippy::too_many_arguments)]
    pub fn render_camera(
        &self,
        gpu: &GpuContext,
        vertices: &[Vertex],
        ground_rgba: &[u8],
        ground_w: u32,
        ground_h: u32,
        sprite_rgba: &[u8],
        sprite_w: u32,
        sprite_h: u32,
        tiles_w: u32,
        tiles_h: u32,
        tile_px: u32,
        out_width: u32,
        out_height: u32,
        camera: &DioramaCamera,
    ) -> Result<Vec<u8>, String> {
        let aspect = out_width.max(1) as f32 / out_height.max(1) as f32;
        let vp = camera_matrices::view_proj_camera(
            tiles_w.max(1) as f32,
            tiles_h.max(1) as f32,
            tile_px.max(1) as f32,
            aspect,
            FOV_Y_DEG,
            DISTANCE_FACTOR,
            camera,
        );
        self.render_with_vp(
            gpu,
            vertices,
            ground_rgba,
            ground_w,
            ground_h,
            sprite_rgba,
            sprite_w,
            sprite_h,
            vp,
            out_width,
            out_height,
        )
    }

    /// Render `vertices` through the ready-made `vp` matrix (D5): MSAA and a
    /// depth buffer, at exactly `out_width x out_height` (clamped to the
    /// device's texture limit), on resources kept from the previous call
    /// whenever the sizes match.
    ///
    /// # Errors
    /// Returns `Err` if the GPU readback does not complete within
    /// [`crate::gpu::GPU_WAIT`].
    #[allow(clippy::too_many_arguments)]
    pub fn render_with_vp(
        &self,
        gpu: &GpuContext,
        vertices: &[Vertex],
        ground_rgba: &[u8],
        ground_w: u32,
        ground_h: u32,
        sprite_rgba: &[u8],
        sprite_w: u32,
        sprite_h: u32,
        vp: camera_matrices::Mat4,
        out_width: u32,
        out_height: u32,
    ) -> Result<Vec<u8>, String> {
        assert_eq!(
            ground_rgba.len(),
            (ground_w as usize) * (ground_h as usize) * 4,
            "DioramaPass::render: ground buffer size mismatch"
        );
        assert_eq!(
            sprite_rgba.len(),
            (sprite_w as usize) * (sprite_h as usize) * 4,
            "DioramaPass::render: sprite buffer size mismatch"
        );
        let started = std::time::Instant::now();
        let max_dim = gpu.adapter_limits.max_texture_dimension_2d.max(1);
        let (out_w, out_h) = (out_width.clamp(1, max_dim), out_height.clamp(1, max_dim));

        // The sun, and the shadow map fitted over what this camera sees of
        // this mesh (W34-06). No map when there is no sun, the sun asks for
        // none, the budget gate dropped it, or there is nothing to fit.
        let lighting = self.lighting();
        let fit: Option<ShadowFit> = match lighting {
            Some(l) if l.shadow_map && self.shadow_budget_allows_map() => scene_bounds(vertices)
                .and_then(|b| {
                    lighting::fit_shadow_map(
                        &vp,
                        &b,
                        &l.sun,
                        lighting::map_size_for(out_w).min(max_dim),
                    )
                }),
            _ => None,
        };

        let mut guard = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let cache = &mut *guard;
        let mut made = ResourceCounts::default();

        // Textures: recreated only when their size changes, rewritten each
        // call (the live path's picture is new every frame).
        let mut rebind = false;
        let t_stage = std::time::Instant::now();
        for (slot, label, w, h, bytes) in [
            (
                &mut cache.ground,
                "rf-renderer::diorama::ground",
                ground_w,
                ground_h,
                ground_rgba,
            ),
            (
                &mut cache.sprite,
                "rf-renderer::diorama::sprite",
                sprite_w,
                sprite_h,
                sprite_rgba,
            ),
        ] {
            let size = (w.max(1), h.max(1));
            if slot.as_ref().is_none_or(|t| t.size != size) {
                let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: size.0,
                        height: size.1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: COLOR_FORMAT,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
                *slot = Some(SampledTexture {
                    texture,
                    view,
                    size,
                    held: Vec::new(),
                    _tally: Tally::new(&LIVE_SAMPLED, 1),
                });
                made.textures += 1;
                rebind = true;
            }
            if w > 0 && h > 0 {
                if let Some(t) = slot {
                    let row_bytes = w as usize * 4;
                    // A fresh texture holds nothing yet: the whole picture.
                    let (y0, y1) = if t.held.len() == bytes.len() {
                        match dirty_rows(&t.held, bytes, row_bytes) {
                            Some(r) => r,
                            None => continue,
                        }
                    } else {
                        t.held.clear();
                        t.held.resize(bytes.len(), 0);
                        (0, h as usize - 1)
                    };
                    let part = y0 * row_bytes..(y1 + 1) * row_bytes;
                    t.held[part.clone()].copy_from_slice(&bytes[part.clone()]);
                    gpu.queue.write_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture: &t.texture,
                            mip_level: 0,
                            origin: wgpu::Origin3d {
                                x: 0,
                                y: y0 as u32,
                                z: 0,
                            },
                            aspect: wgpu::TextureAspect::All,
                        },
                        &bytes[part],
                        wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(w * 4),
                            rows_per_image: Some((y1 - y0 + 1) as u32),
                        },
                        wgpu::Extent3d {
                            width: w,
                            height: (y1 - y0 + 1) as u32,
                            depth_or_array_layers: 1,
                        },
                    );
                }
            }
        }
        let t_stage = stage_us(0, t_stage);
        // The shadow map: one texel while unused, `map_size` squared when a
        // sun casts into it. Recreated only when that size changes.
        let want_size = fit.as_ref().map_or(1, |f| f.map_size);
        if cache.shadow.as_ref().is_none_or(|m| m.size != want_size) {
            let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("rf-renderer::diorama::shadow_map"),
                size: wgpu::Extent3d {
                    width: want_size,
                    height: want_size,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: DEPTH_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            cache.shadow = Some(ShadowMap {
                view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
                size: want_size,
            });
            made.textures += 1;
            rebind = true;
        }
        if rebind || cache.bind_group.is_none() {
            let (Some(ground), Some(sprite), Some(shadow)) =
                (&cache.ground, &cache.sprite, &cache.shadow)
            else {
                return Err("diorama textures missing".to_string());
            };
            cache.caster_bind_group =
                Some(gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("rf-renderer::diorama::caster_bind_group"),
                    layout: &self.caster_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::TextureView(&sprite.view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: wgpu::BindingResource::Sampler(&self.sampler),
                        },
                        wgpu::BindGroupEntry {
                            binding: 6,
                            resource: self.light_buffer.as_entire_binding(),
                        },
                    ],
                }));
            cache.bind_group = Some(gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("rf-renderer::diorama::bind_group"),
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.camera_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&ground.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&sprite.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(&shadow.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: wgpu::BindingResource::Sampler(&self.shadow_sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 6,
                        resource: self.light_buffer.as_entire_binding(),
                    },
                ],
            }));
            made.bind_groups += 1;
        }
        gpu.queue
            .write_buffer(&self.camera_buffer, 0, &camera_ubo_bytes(vp));
        let light_ubo = match &lighting {
            Some(l) => LightUbo::lit(&l.sun, fit.as_ref()),
            None => LightUbo::off(),
        };
        gpu.queue
            .write_buffer(&self.light_buffer, 0, &light_ubo.to_bytes());

        let t_stage = stage_us(1, t_stage);
        // Vertices: one buffer that only grows; a mesh identical to the last
        // one (a still scene) is not uploaded again.
        vertex_bytes_into(vertices, &mut cache.scratch);
        let need = cache
            .scratch
            .len()
            .max(std::mem::size_of::<Vertex>())
            .next_power_of_two() as u64;
        if cache
            .vertex_buffer
            .as_ref()
            .is_none_or(|(_, cap)| *cap < need)
        {
            cache.vertex_buffer = Some((
                gpu.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("rf-renderer::diorama::vertices"),
                    size: need,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
                need,
            ));
            cache.uploaded.clear();
            made.buffers += 1;
        }
        if !cache.scratch.is_empty() && cache.scratch != cache.uploaded {
            if let Some((buffer, _)) = &cache.vertex_buffer {
                gpu.queue.write_buffer(buffer, 0, &cache.scratch);
            }
            std::mem::swap(&mut cache.scratch, &mut cache.uploaded);
            made.vertex_uploads += 1;
        }

        let t_stage = stage_us(2, t_stage);
        // Targets and readback: rebuilt only when the output size changes.
        if cache
            .target
            .as_ref()
            .is_none_or(|t| t.size != (out_w, out_h))
        {
            cache.target = Some(self.make_target(gpu, out_w, out_h));
            cache.pending = None;
            cache.primed = None;
            cache.cursor = 0;
            made.textures += 3;
            made.buffers += 1;
        }
        let async_mode = self.async_readback();
        if !async_mode {
            cache.primed = None;
            // Switched back to synchronous with a frame in flight: collect
            // and drop it so its buffer is not left mapped.
            if let (Some(p), Some(t)) = (cache.pending.take(), &cache.target) {
                let _ = finish_pending(
                    &gpu.device,
                    &t.readbacks[p.slot],
                    p,
                    out_w,
                    out_h,
                    t.padded_bytes_per_row,
                );
            }
        }
        let slot = if async_mode { cache.cursor } else { 0 };
        let (
            Some(target),
            Some(bind_group),
            Some((vertex_buffer, _)),
            Some(caster_bind_group),
            Some(shadow),
        ) = (
            &cache.target,
            &cache.bind_group,
            &cache.vertex_buffer,
            &cache.caster_bind_group,
            &cache.shadow,
        )
        else {
            return Err("diorama resources missing".to_string());
        };

        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("rf-renderer::diorama::encoder"),
            });
        // Pass 1 (W34-06): the sun's depth map. Boxes and wedges cast solid,
        // billboards by their alpha; the contact ellipses and the flat floor
        // (which only receives) are left out.
        if fit.is_some() {
            let mut sp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rf-renderer::diorama::shadow_pass"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &shadow.view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            sp.set_pipeline(&self.caster_pipeline);
            sp.set_bind_group(0, caster_bind_group, &[]);
            sp.set_vertex_buffer(0, vertex_buffer.slice(..));
            for run in caster_runs(vertices) {
                sp.draw(run, 0..1);
            }
        }
        {
            let (view, resolve_target, store) = match &target.msaa_view {
                Some(mv) => (mv, Some(&target.resolved_view), wgpu::StoreOp::Discard),
                None => (&target.resolved_view, None, wgpu::StoreOp::Store),
            };
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rf-renderer::diorama::render_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target,
                    ops: wgpu::Operations {
                        // Transparent clear: an area the mesh never
                        // covers (outside the framed grid, or a fully
                        // empty scene) composites as "nothing here",
                        // never an opaque colour standing in for one.
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &target.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            rpass.set_bind_group(0, bind_group, &[]);
            rpass.set_vertex_buffer(0, vertex_buffer.slice(..));
            // Shadows draw with their own pipeline (no depth write), so the
            // triangle list is cut into runs of one pipeline each. The mesh
            // order (far to near) is kept: depth settles overlaps, the order
            // settles blending.
            let mut run_start = 0usize;
            let mut run_shadow = false;
            let flush = |rpass: &mut wgpu::RenderPass<'_>, from: usize, to: usize, shadow| {
                if to > from {
                    rpass.set_pipeline(if shadow {
                        &self.shadow_pipeline
                    } else {
                        &self.solid_pipeline
                    });
                    rpass.draw(from as u32..to as u32, 0..1);
                }
            };
            for (i, tri) in vertices.chunks(3).enumerate() {
                let shadow = tri[0].kind == crate::diorama_mesh::kind::SHADOW;
                if shadow != run_shadow {
                    flush(&mut rpass, run_start, i * 3, run_shadow);
                    run_start = i * 3;
                    run_shadow = shadow;
                }
            }
            flush(&mut rpass, run_start, vertices.len(), run_shadow);
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target.resolved,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &target.readbacks[slot],
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(target.padded_bytes_per_row),
                    rows_per_image: Some(out_h),
                },
            },
            wgpu::Extent3d {
                width: out_w,
                height: out_h,
                depth_or_array_layers: 1,
            },
        );
        let t_stage = stage_us(3, t_stage);
        gpu.queue.submit(std::iter::once(encoder.finish()));
        let t_stage = stage_us(4, t_stage);
        let rgba = if async_mode {
            let (tx, done) = std::sync::mpsc::channel();
            target.readbacks[slot]
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |result| {
                    let _ = tx.send(result);
                });
            let this_frame = PendingReadback { slot, done };
            cache.cursor ^= 1;
            // Collect the previous frame. With none in flight this is the
            // first: answer with its own pixels (and keep a copy for the next
            // call, so the lag is one frame from here on).
            let finish = |p: PendingReadback| {
                finish_pending(
                    &gpu.device,
                    &target.readbacks[p.slot],
                    p,
                    out_w,
                    out_h,
                    target.padded_bytes_per_row,
                )
            };
            match cache.pending.replace(this_frame) {
                Some(prev) => finish(prev)?,
                None => match cache.primed.take() {
                    Some(first) => first,
                    None => {
                        let own = cache
                            .pending
                            .take()
                            .ok_or_else(|| "pending readback vanished".to_string())?;
                        let first = finish(own)?;
                        cache.primed = Some(first.clone());
                        first
                    }
                },
            }
        } else {
            read_straight_alpha(
                &gpu.device,
                &target.readbacks[0],
                out_w,
                out_h,
                target.padded_bytes_per_row,
            )?
        };
        stage_us(5, t_stage);
        self.shadow_map_used
            .store(fit.is_some(), std::sync::atomic::Ordering::Relaxed);
        if fit.is_some() {
            self.record_shadow_sample_ms(started.elapsed().as_secs_f64() * 1000.0);
        }
        {
            let mut counts = self.counts.lock().unwrap_or_else(|e| e.into_inner());
            counts.textures += made.textures;
            counts.buffers += made.buffers;
            counts.bind_groups += made.bind_groups;
            counts.vertex_uploads += made.vertex_uploads;
        }
        Ok(rgba)
    }

    fn make_target(&self, gpu: &GpuContext, w: u32, h: u32) -> Target {
        let extent = wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        };
        let make = |label, format, sample_count, usage| {
            gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: extent,
                mip_level_count: 1,
                sample_count,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let resolved = make(
            "rf-renderer::diorama::resolved",
            COLOR_FORMAT,
            1,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        );
        let resolved_view = resolved.create_view(&wgpu::TextureViewDescriptor::default());
        let msaa_view = (self.samples > 1).then(|| {
            make(
                "rf-renderer::diorama::msaa",
                COLOR_FORMAT,
                self.samples,
                wgpu::TextureUsages::RENDER_ATTACHMENT,
            )
            .create_view(&wgpu::TextureViewDescriptor::default())
        });
        let depth_view = make(
            "rf-renderer::diorama::depth",
            DEPTH_FORMAT,
            self.samples,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        )
        .create_view(&wgpu::TextureViewDescriptor::default());
        let padded_bytes_per_row = align_up(w * 4, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let make_readback = || {
            gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("rf-renderer::diorama::readback"),
                size: u64::from(padded_bytes_per_row) * u64::from(h),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            })
        };
        let readbacks = [make_readback(), make_readback()];
        Target {
            size: (w, h),
            msaa_view,
            resolved,
            resolved_view,
            depth_view,
            readbacks,
            padded_bytes_per_row,
            _tally: Tally::new(&LIVE_READBACKS, 2),
        }
    }
}

/// Map `readback`, strip the row padding and turn the premultiplied
/// coverage the blend and the MSAA resolve leave over the transparent clear
/// back into straight alpha (`superfx_gpu` does the same). Opaque and empty
/// pixels, nearly all of them, are copied untouched.
fn read_straight_alpha(
    device: &wgpu::Device,
    readback: &wgpu::Buffer,
    w: u32,
    h: u32,
    padded_bytes_per_row: u32,
) -> Result<Vec<u8>, String> {
    let slice = readback.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = tx.send(result);
    });
    crate::gpu::wait_for_map(device, &rx)?;
    Ok(unpack_mapped(readback, w, h, padded_bytes_per_row))
}

/// Wait for a frame in flight (only for its own submission, which a frame's
/// worth of other work has normally finished long ago) and unpack it.
fn finish_pending(
    device: &wgpu::Device,
    readback: &wgpu::Buffer,
    pending: PendingReadback,
    w: u32,
    h: u32,
    padded_bytes_per_row: u32,
) -> Result<Vec<u8>, String> {
    crate::gpu::wait_for_map(device, &pending.done)?;
    Ok(unpack_mapped(readback, w, h, padded_bytes_per_row))
}

/// Copy a mapped readback out without its row padding, unmap it, and turn
/// the premultiplied edge pixels back into straight alpha.
fn unpack_mapped(readback: &wgpu::Buffer, w: u32, h: u32, padded_bytes_per_row: u32) -> Vec<u8> {
    let slice = readback.slice(..);
    let row_bytes = w as usize * 4;
    let mut rgba = Vec::with_capacity(row_bytes * h as usize);
    {
        let mapped = slice.get_mapped_range();
        for row in mapped
            .chunks(padded_bytes_per_row as usize)
            .take(h as usize)
        {
            rgba.extend_from_slice(&row[..row_bytes]);
        }
    }
    readback.unmap();
    // Almost every pixel is opaque or empty, so test sixteen bytes (four
    // pixels) with one comparison per pixel and only touch the rare edge.
    let straighten = |px: &mut [u8]| {
        let a = u32::from(px[3]);
        for c in &mut px[..3] {
            *c = ((u32::from(*c) * 255 + a / 2) / a).min(255) as u8;
        }
    };
    let mut quads = rgba.chunks_exact_mut(16);
    for quad in &mut quads {
        let edge = |i: usize| quad[i * 4 + 3].wrapping_sub(1) < 254;
        if edge(0) | edge(1) | edge(2) | edge(3) {
            for px in quad.chunks_exact_mut(4) {
                if px[3].wrapping_sub(1) < 254 {
                    straighten(px);
                }
            }
        }
    }
    for px in quads.into_remainder().chunks_exact_mut(4) {
        if px[3].wrapping_sub(1) < 254 {
            straighten(px);
        }
    }
    rgba
}

/// Probe helpers shared with `mode7_plane`'s tests (W30-05b).
#[cfg(test)]
pub(crate) mod probe_support {
    /// Ground texture with four distinct quadrants, in image coordinates
    /// (row 0 on top): top-left red, top-right green, bottom-left blue,
    /// bottom-right yellow.
    pub(crate) fn quadrant_ground(side: u32) -> Vec<u8> {
        let mut px = Vec::with_capacity((side * side * 4) as usize);
        for y in 0..side {
            for x in 0..side {
                let c = match (x < side / 2, y < side / 2) {
                    (true, true) => [255, 0, 0, 255],
                    (false, true) => [0, 255, 0, 255],
                    (true, false) => [0, 0, 255, 255],
                    (false, false) => [255, 255, 0, 255],
                };
                px.extend_from_slice(&c);
            }
        }
        px
    }

    /// Mean screen position `(x, y)` of the pixels equal to `rgb`.
    pub(crate) fn centroid(rgba: &[u8], width: usize, rgb: [u8; 3]) -> (f32, f32) {
        let (mut sx, mut sy, mut n) = (0.0f32, 0.0f32, 0.0f32);
        for (i, px) in rgba.chunks_exact(4).enumerate() {
            if px[3] == 255 && px[..3] == rgb {
                sx += (i % width) as f32;
                sy += (i / width) as f32;
                n += 1.0;
            }
        }
        assert!(n > 0.0, "colour {rgb:?} is not on screen at all");
        (sx / n, sy / n)
    }

    /// The four quadrant colours must land where the level image has them:
    /// left-half colours left of right-half colours, top-half colours above
    /// bottom-half colours.
    pub(crate) fn assert_quadrants_upright(rgba: &[u8], width: usize, what: &str) {
        let red = centroid(rgba, width, [255, 0, 0]);
        let green = centroid(rgba, width, [0, 255, 0]);
        let blue = centroid(rgba, width, [0, 0, 255]);
        let yellow = centroid(rgba, width, [255, 255, 0]);
        assert!(red.0 < green.0, "{what}: level-left (red) must be screen-left of level-right (green): {red:?} {green:?}");
        assert!(
            blue.0 < yellow.0,
            "{what}: bottom row left/right: {blue:?} {yellow:?}"
        );
        assert!(
            red.1 < blue.1,
            "{what}: level-top (red) must be above level-bottom (blue): {red:?} {blue:?}"
        );
        assert!(
            green.1 < yellow.1,
            "{what}: right column top/bottom: {green:?} {yellow:?}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::probe_support::{assert_quadrants_upright, quadrant_ground};
    use super::*;

    fn gpu_or_skip(test_name: &str) -> Option<GpuContext> {
        match GpuContext::request_headless() {
            Ok(gpu) => Some(gpu),
            Err(e) => {
                eprintln!(
                    "SKIP {test_name}: no wgpu adapter in this environment ({e}) -- clean skip"
                );
                None
            }
        }
    }

    #[test]
    fn diorama_pass_renders_a_single_solid_tile_scene() {
        let Some(gpu) = gpu_or_skip("diorama_pass_renders_a_single_solid_tile_scene") else {
            return;
        };
        let pass = DioramaPass::new(&gpu);
        let scene = crate::diorama_mesh::DioramaScene {
            tiles_w: 2,
            tiles_h: 2,
            tile_px: 16.0,
            solid: &[0, 0, 1, 0],
            depth: &[0, 0, 8, 0],
            billboards: &[],
        };
        let verts = crate::diorama_mesh::build_vertices(&scene);
        let ground = vec![128u8; 32 * 32 * 4];
        let sprite = vec![0u8; 4 * 4 * 4];
        let out = pass
            .render(
                &gpu, &verts, &ground, 32, 32, &sprite, 4, 4, 2, 2, 16, 64, 64,
            )
            .expect("diorama pass renders");
        assert_eq!(out.len(), 64 * 64 * 4);
        let any_opaque = out.chunks_exact(4).any(|px| px[3] > 0);
        assert!(
            any_opaque,
            "some pixel must be covered by the ground/box mesh"
        );
    }

    #[test]
    fn diorama_pass_is_deterministic_for_identical_inputs() {
        let Some(gpu) = gpu_or_skip("diorama_pass_is_deterministic_for_identical_inputs") else {
            return;
        };
        let pass = DioramaPass::new(&gpu);
        let scene = crate::diorama_mesh::DioramaScene {
            tiles_w: 1,
            tiles_h: 1,
            tile_px: 16.0,
            solid: &[1],
            depth: &[8],
            billboards: &[],
        };
        let verts = crate::diorama_mesh::build_vertices(&scene);
        let ground = vec![64u8; 16 * 16 * 4];
        let sprite = vec![0u8; 4];
        let a = pass
            .render(
                &gpu, &verts, &ground, 16, 16, &sprite, 1, 1, 1, 1, 16, 32, 32,
            )
            .unwrap();
        let b = pass
            .render(
                &gpu, &verts, &ground, 16, 16, &sprite, 1, 1, 1, 1, 16, 32, 32,
            )
            .unwrap();
        assert_eq!(a, b);
    }

    /// W35-15: only the rows that changed are uploaded; none when none did.
    #[test]
    fn dirty_rows_finds_the_first_and_last_changed_row() {
        let old = vec![7u8; 4 * 5];
        assert_eq!(dirty_rows(&old, &old.clone(), 4), None);
        let mut new = old.clone();
        new[0] = 1;
        assert_eq!(dirty_rows(&old, &new, 4), Some((0, 0)));
        new[19] = 1;
        assert_eq!(dirty_rows(&old, &new, 4), Some((0, 4)));
        let mut mid = old.clone();
        mid[9] = 2;
        assert_eq!(dirty_rows(&old, &mid, 4), Some((2, 2)));
        assert_eq!(dirty_rows(&[], &[], 4), None);
    }

    /// W35-15: a pass that keeps its textures between frames draws every
    /// frame of a changing sequence (a scroll, a one-row edit at either end,
    /// a palette change, a sprite change, a resize and back) exactly as a
    /// fresh pass does.
    #[test]
    fn kept_textures_draw_like_fresh_ones_across_scroll_palette_and_resize() {
        let Some(gpu) = gpu_or_skip("kept_textures_draw_like_fresh_ones") else {
            return;
        };
        let scene = crate::diorama_mesh::DioramaScene {
            tiles_w: 1,
            tiles_h: 1,
            tile_px: 16.0,
            solid: &[1],
            depth: &[8],
            billboards: &[],
        };
        let verts = crate::diorama_mesh::build_vertices(&scene);
        let picture = |shift: usize, tint: u8, n: usize| -> Vec<u8> {
            (0..n * n * 4)
                .map(|i| {
                    let (x, y, c) = ((i / 4) % n, (i / 4) / n, i % 4);
                    if c == 3 {
                        255
                    } else {
                        (((x + shift) * 13 + y * 7) as u8)
                            .wrapping_add(tint.wrapping_mul(c as u8 + 1))
                    }
                })
                .collect()
        };
        let sprite = |v: u8| vec![v; 4 * 4 * 4];
        let edit = |mut g: Vec<u8>, row: usize| {
            g[row * 16 * 4] ^= 0x55;
            g
        };
        let base = picture(0, 0, 16);
        let frames: Vec<(Vec<u8>, u32, Vec<u8>, u32)> = vec![
            (base.clone(), 16, sprite(9), 32),
            (base.clone(), 16, sprite(9), 32),
            (picture(1, 0, 16), 16, sprite(9), 32),
            (picture(2, 0, 16), 16, sprite(9), 32),
            (edit(picture(2, 0, 16), 0), 16, sprite(9), 32),
            (edit(picture(2, 0, 16), 15), 16, sprite(9), 32),
            (picture(2, 40, 16), 16, sprite(9), 32),
            (picture(2, 40, 16), 16, sprite(200), 32),
            (picture(3, 40, 24), 24, sprite(200), 32),
            (picture(3, 40, 24), 24, sprite(200), 48),
            (base.clone(), 16, sprite(9), 32),
        ];
        let kept = DioramaPass::new(&gpu);
        for (i, (ground, n, spr, out)) in frames.iter().enumerate() {
            let draw = |pass: &DioramaPass| {
                pass.render(
                    &gpu, &verts, ground, *n, *n, spr, 4, 4, 1, 1, 16, *out, *out,
                )
                .unwrap()
            };
            let want = draw(&DioramaPass::new(&gpu));
            assert_eq!(draw(&kept), want, "frame {i} differs from a fresh pass");
        }
    }

    /// W30-05b: screen-left is level-left (and screen-top is level-top) at
    /// the default camera and at yaw +-30. Fixed by correcting the camera's
    /// handedness (the eye now looks along -Z), not by flipping the texture.
    #[test]
    fn the_ground_is_not_mirrored_at_default_and_yawed_cameras() {
        let Some(gpu) = gpu_or_skip("the_ground_is_not_mirrored_at_default_and_yawed_cameras")
        else {
            return;
        };
        let pass = DioramaPass::new(&gpu);
        let scene = crate::diorama_mesh::DioramaScene {
            tiles_w: 4,
            tiles_h: 4,
            tile_px: 16.0,
            solid: &[0; 16],
            depth: &[0; 16],
            billboards: &[],
        };
        for yaw in [0.0f32, 30.0, -30.0] {
            let camera = DioramaCamera {
                yaw_deg: yaw,
                ..DioramaCamera::default()
            };
            let verts = vertices_for_camera(&scene, 128, 128, &camera);
            let out = pass
                .render_camera(
                    &gpu,
                    &verts,
                    &quadrant_ground(64),
                    64,
                    64,
                    &[0u8; 4],
                    1,
                    1,
                    4,
                    4,
                    16,
                    128,
                    128,
                    &camera,
                )
                .expect("diorama renders");
            assert_quadrants_upright(&out, 128, &format!("yaw {yaw}"));
        }
    }
}

#[cfg(test)]
mod smoother_tests {
    use super::CameraSmoother;

    #[test]
    fn the_first_step_lands_on_the_target() {
        let mut s = CameraSmoother::default();
        assert_eq!(s.step_to((10.0, 20.0), 100.0), (10.0, 20.0));
    }

    /// D1: a step of the target never overshoots, and the camera arrives.
    #[test]
    fn a_step_response_does_not_overshoot_and_settles() {
        let mut s = CameraSmoother::default();
        s.step_to((0.0, 0.0), 1000.0);
        let mut peak = 0.0_f32;
        let mut p = (0.0, 0.0);
        for _ in 0..90 {
            p = s.step_to((100.0, 50.0), 1000.0);
            peak = peak.max(p.0 - 100.0).max(p.1 - 50.0);
        }
        assert!(peak <= 0.5, "overshoot {peak}");
        assert!(
            (p.0 - 100.0).abs() < 0.5 && (p.1 - 50.0).abs() < 0.5,
            "{p:?}"
        );
    }

    /// A target that walks at 2 px a frame and then stops: the camera lags,
    /// then catches up without passing it.
    #[test]
    fn a_target_that_stops_is_not_overshot() {
        let mut s = CameraSmoother::default();
        let mut x = 0.0_f32;
        s.step_to((x, 0.0), 1000.0);
        for _ in 0..120 {
            x += 2.0;
            let p = s.step_to((x, 0.0), 1000.0);
            assert!(p.0 <= x + 0.01, "ahead of a walking target: {} vs {x}", p.0);
        }
        let mut peak = 0.0_f32;
        for _ in 0..120 {
            let p = s.step_to((x, 0.0), 1000.0);
            peak = peak.max(p.0 - x);
        }
        assert!(peak <= 0.5, "overshoot after stopping {peak}");
    }

    #[test]
    fn the_smoother_says_whether_it_is_on_a_real_target() {
        let mut s = CameraSmoother::default();
        // Never given a target: on the fallback.
        s.step(None, (5.0, 5.0), 100.0);
        assert!(!s.tracking());
        // A target: tracking, and still tracking while it is held ...
        s.step(Some((9.0, 9.0)), (5.0, 5.0), 100.0);
        assert!(s.tracking());
        for _ in 0..CameraSmoother::HOLD_STEPS {
            s.step(None, (5.0, 5.0), 100.0);
            assert!(s.tracking());
        }
        // ... until the hold runs out.
        s.step(None, (5.0, 5.0), 100.0);
        assert!(!s.tracking());
    }

    #[test]
    fn a_jump_larger_than_the_snap_distance_is_taken_at_once() {
        let mut s = CameraSmoother::default();
        s.step_to((0.0, 0.0), 128.0);
        assert_eq!(s.step_to((500.0, 0.0), 128.0), (500.0, 0.0));
        s.reset();
        assert_eq!(s.step_to((7.0, 7.0), 128.0), (7.0, 7.0));
    }

    #[test]
    fn the_same_inputs_replay_the_same_path() {
        let run = || {
            let mut s = CameraSmoother::default();
            (0..50)
                .map(|i| s.step_to((i as f32 * 3.0, 10.0), 500.0))
                .collect::<Vec<_>>()
        };
        assert_eq!(run(), run());
    }

    /// A player the detector loses for a few frames is not a jump to the
    /// fallback and back.
    #[test]
    fn a_dropout_holds_the_last_target_instead_of_jumping_to_the_fallback() {
        let mut s = CameraSmoother::default();
        let fallback = (128.0, 112.0);
        let mut last = (0.0, 0.0);
        for _ in 0..30 {
            last = s.step(Some((40.0, 180.0)), fallback, 1000.0);
        }
        for _ in 0..30 {
            let p = s.step(None, fallback, 1000.0);
            assert!(
                (p.0 - last.0).abs() < 1.0 && (p.1 - last.1).abs() < 1.0,
                "{p:?}"
            );
        }
        let back = s.step(Some((40.0, 180.0)), fallback, 1000.0);
        assert!(
            (back.0 - 40.0).abs() < 1.0 && (back.1 - 180.0).abs() < 1.0,
            "{back:?}"
        );
    }

    #[test]
    fn a_target_lost_for_good_is_given_up_on_for_the_fallback() {
        let mut s = CameraSmoother::default();
        let fallback = (128.0, 112.0);
        for _ in 0..10 {
            s.step(Some((10.0, 10.0)), fallback, 1000.0);
        }
        let mut p = (0.0, 0.0);
        for _ in 0..(CameraSmoother::HOLD_STEPS + 120) {
            p = s.step(None, fallback, 1000.0);
        }
        assert!(
            (p.0 - 128.0).abs() < 1.0 && (p.1 - 112.0).abs() < 1.0,
            "{p:?}"
        );
    }
}
