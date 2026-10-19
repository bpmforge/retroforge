//! Light for the Diorama pass (ticket W34-06; `docs/design/HD2D_PLAN.md`
//! section 3): one [`Sun`] derived from the lens's time of day, the numbers
//! the shader needs ([`LightUbo`]), and the fit of an orthographic shadow map
//! over what the camera can see ([`fit_shadow_map`]). Pure arithmetic, no
//! GPU: every function here is checked with plain unit tests.
//!
//! ## The sun
//!
//! The day runs from [`SUNRISE`] to [`SUNSET`] (the lens grade's own keys put
//! dawn at 0.27 and dusk at 0.73 to 0.82). Across it the sun's elevation
//! follows `70 sin(pi u)` degrees (`u` = fraction of the day), floored at
//! [`MIN_ELEVATION_DEG`] so a low sun throws a long shadow, not an infinite
//! one, and its azimuth sweeps from +60 to -60 degrees about "screen up":
//! it rises screen-right and sets screen-left. Outside the day a moon at
//! [`MOON_ELEVATION_DEG`] takes over, dim and cool. The plan's formula
//! (`10 + 60 sin(pi t)` over the whole 0..1 day) puts a 45 degree sun at
//! 0.8, which the grade already paints as night; this one follows the grade.
//!
//! The colour is the grade's tint, pulled halfway to white: the lens grade
//! multiplies the finished picture by the same tint, and a full tint here
//! would apply it twice. Weather dims the sun (rain 0.6, snow 0.8).
//!
//! ## The model
//!
//! `lit = colour * (ambient + (1 - ambient) * direct * shadow)` where
//! `direct = intensity * max(n . to_sun, 0) ^ 0.7`. At noon on level ground
//! that is exactly 1, so the picture is as bright as the unlit one; a face
//! turned away from the sun, or a shadowed one, falls to the ambient term.
//! The exponent softens plain Lambert a little so a low sun still reads.
//!
//! ## Side view
//!
//! The camera looks along -Z at an upright picture, so the sun must sit on
//! the camera's side (+Z) or every front face is in the dark. The mirror
//! ([`LightView::Side`]) flips the horizontal component's Z.

use crate::lens::{grade_for, Weather};

/// Time of day (fraction of the day) at which the sun clears the horizon.
pub const SUNRISE: f32 = 0.23;
/// Time of day at which it sets.
pub const SUNSET: f32 = 0.82;
/// The sun's peak elevation, degrees.
pub const NOON_ELEVATION_DEG: f32 = 70.0;
/// Shadows never get longer than this elevation allows, degrees.
pub const MIN_ELEVATION_DEG: f32 = 10.0;
/// The moon's elevation, degrees.
pub const MOON_ELEVATION_DEG: f32 = 20.0;
/// Azimuth half-sweep across the day, degrees.
pub const AZIMUTH_SWEEP_DEG: f32 = 60.0;
/// Ambient light at noon and at night.
pub const AMBIENT_DAY: f32 = 0.45;
pub const AMBIENT_NIGHT: f32 = 0.25;
/// Moon strength next to the sun's 1.0.
pub const MOON_INTENSITY: f32 = 0.35;
/// Exponent that softens Lambert's falloff.
pub const LAMBERT_EXPONENT: f32 = 0.7;
/// Albedo of a box side face (flat, untextured) once lit; the unlit pass
/// bakes `0.55` into the vertex.
pub const SIDE_ALBEDO: f32 = 0.8;
/// Contact-ellipse alpha scale when the shadow map is on (0.25 / 0.45): the
/// ellipse stays as ambient occlusion under a sprite's feet.
pub const CONTACT_AO_SCALE: f32 = 0.25 / 0.45;

/// Which way the picture faces: the sun mirrors for an upright one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LightView {
    /// A floor seen from above: `y` is up, `-Z` is screen-up.
    #[default]
    TopDown,
    /// An upright picture, camera on the `+Z` side.
    Side,
}

/// The light of one frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sun {
    /// Unit vector from the scene toward the light.
    pub to_sun: [f32; 3],
    /// Light colour (the grade tint, halfway to white).
    pub color: [f32; 3],
    /// Direct-light strength, `0..=1` (sun fades at the horizon, moon is dim,
    /// weather dims both).
    pub intensity: f32,
    /// Ambient term, `0.25..=0.45`.
    pub ambient: f32,
    /// Elevation, degrees (after the floor).
    pub elevation_deg: f32,
    /// Azimuth, degrees: positive is screen-right.
    pub azimuth_deg: f32,
    /// A moon, not a sun.
    pub night: bool,
    pub view: LightView,
}

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

impl Sun {
    /// The sun at `time_of_day` (wraps into `0..1`; non-finite is noon).
    #[must_use]
    pub fn for_time(time_of_day: f32, weather: Weather, view: LightView) -> Sun {
        let t = if time_of_day.is_finite() {
            time_of_day.rem_euclid(1.0)
        } else {
            0.5
        };
        let (raw_elevation, azimuth, intensity, night) = if (SUNRISE..=SUNSET).contains(&t) {
            let u = (t - SUNRISE) / (SUNSET - SUNRISE);
            let raw = NOON_ELEVATION_DEG * (std::f32::consts::PI * u).sin();
            (
                raw,
                AZIMUTH_SWEEP_DEG * (1.0 - 2.0 * u),
                smoothstep(0.0, 12.0, raw),
                false,
            )
        } else {
            let into_night = (t - SUNSET).rem_euclid(1.0);
            let night_len = 1.0 - (SUNSET - SUNRISE);
            let v = (into_night / night_len).clamp(0.0, 1.0);
            let edge = into_night.min(night_len - into_night);
            (
                0.0,
                AZIMUTH_SWEEP_DEG * (1.0 - 2.0 * v),
                MOON_INTENSITY * smoothstep(0.0, 0.04, edge),
                true,
            )
        };
        let elevation = if night {
            MOON_ELEVATION_DEG
        } else {
            raw_elevation.max(MIN_ELEVATION_DEG)
        };
        let dim = match weather {
            Weather::None => 1.0,
            Weather::Rain => 0.6,
            Weather::Snow => 0.8,
        };
        let day_strength = if night {
            0.0
        } else {
            smoothstep(0.0, 25.0, raw_elevation)
        };
        let tint = grade_for(t).tint;
        let color = [
            0.5 * (tint[0].min(1.2) + 1.0),
            0.5 * (tint[1].min(1.2) + 1.0),
            0.5 * (tint[2].min(1.2) + 1.0),
        ];
        let (el, az) = (elevation.to_radians(), azimuth.to_radians());
        let z_sign = match view {
            LightView::TopDown => -1.0,
            LightView::Side => 1.0,
        };
        Sun {
            to_sun: [az.sin() * el.cos(), el.sin(), z_sign * az.cos() * el.cos()],
            color,
            intensity: intensity * dim,
            ambient: AMBIENT_NIGHT + (AMBIENT_DAY - AMBIENT_NIGHT) * day_strength,
            elevation_deg: elevation,
            azimuth_deg: azimuth,
            night,
            view,
        }
    }

    /// The direct term for a surface with unit normal `n`.
    #[must_use]
    pub fn direct(&self, n: [f32; 3]) -> f32 {
        let nl = n[0] * self.to_sun[0] + n[1] * self.to_sun[1] + n[2] * self.to_sun[2];
        self.intensity * nl.max(0.0).powf(LAMBERT_EXPONENT)
    }

    /// Brightness (a multiplier on albedo, before colour) of an unshadowed
    /// surface with normal `n`: the shader's model, for tests.
    #[must_use]
    pub fn brightness(&self, n: [f32; 3]) -> f32 {
        self.ambient + (1.0 - self.ambient) * self.direct(n)
    }

    /// The nominal normal sprites take: they stand up facing the camera, so
    /// lighting them by their own (camera-facing) normal would leave them
    /// dark under a sun behind them. They are lit like the ground they stand
    /// on (like the floor and the front faces in a side view).
    #[must_use]
    pub fn sprite_normal(&self) -> [f32; 3] {
        match self.view {
            LightView::TopDown => [0.0, 1.0, 0.0],
            LightView::Side => [0.0, 0.707, 0.707],
        }
    }

    /// The direction the light travels (the opposite of `to_sun`).
    #[must_use]
    pub fn travel(&self) -> [f32; 3] {
        [-self.to_sun[0], -self.to_sun[1], -self.to_sun[2]]
    }
}

/// Everything the pass needs to light a frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Lighting {
    pub sun: Sun,
    /// Cast a shadow map. `false` keeps the Lambert shading and the full
    /// contact ellipse (the budget fallback).
    pub shadow_map: bool,
}

impl Lighting {
    #[must_use]
    pub fn new(sun: Sun) -> Self {
        Lighting {
            sun,
            shadow_map: true,
        }
    }
}

/// Size in bytes of [`LightUbo`].
pub const LIGHT_UBO_BYTES: usize = 128;

/// The light uniform, `shaders/diorama.wgsl`'s `Light`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LightUbo {
    pub shadow_vp: [f32; 16],
    pub to_sun: [f32; 3],
    pub intensity: f32,
    pub color: [f32; 3],
    pub ambient: f32,
    /// 1.0 when lighting is on.
    pub enabled: f32,
    /// 1.0 when the shadow map is bound and filled this frame.
    pub shadow_on: f32,
    /// Depth bias in map depth units.
    pub bias: f32,
    /// One texel in map UV units.
    pub texel_uv: f32,
    pub contact_scale: f32,
    pub side_albedo: f32,
    /// The sun term sprites use (`direct` of [`Sun::sprite_normal`]).
    pub sprite_direct: f32,
}

impl LightUbo {
    /// Lighting off: the shader takes its original path.
    #[must_use]
    pub fn off() -> Self {
        LightUbo {
            shadow_vp: IDENTITY,
            to_sun: [0.0, 1.0, 0.0],
            intensity: 0.0,
            color: [1.0; 3],
            ambient: 1.0,
            enabled: 0.0,
            shadow_on: 0.0,
            bias: 0.0,
            texel_uv: 0.0,
            contact_scale: 1.0,
            side_albedo: SIDE_ALBEDO,
            sprite_direct: 0.0,
        }
    }

    #[must_use]
    pub fn lit(sun: &Sun, shadow: Option<&ShadowFit>) -> Self {
        let mut u = LightUbo::off();
        u.to_sun = sun.to_sun;
        u.intensity = sun.intensity;
        u.color = sun.color;
        u.ambient = sun.ambient;
        u.enabled = 1.0;
        u.sprite_direct = sun.direct(sun.sprite_normal());
        if let Some(fit) = shadow {
            u.shadow_vp = fit.view_proj;
            u.shadow_on = 1.0;
            u.bias = fit.bias;
            u.texel_uv = 1.0 / fit.map_size as f32;
            u.contact_scale = CONTACT_AO_SCALE;
        }
        u
    }

    #[must_use]
    pub fn to_bytes(&self) -> [u8; LIGHT_UBO_BYTES] {
        let mut floats = [0.0f32; 32];
        floats[..16].copy_from_slice(&self.shadow_vp);
        floats[16..19].copy_from_slice(&self.to_sun);
        floats[19] = self.intensity;
        floats[20..23].copy_from_slice(&self.color);
        floats[23] = self.ambient;
        floats[24] = self.enabled;
        floats[25] = self.shadow_on;
        floats[26] = self.bias;
        floats[27] = self.texel_uv;
        floats[28] = self.contact_scale;
        floats[29] = self.side_albedo;
        floats[30] = self.sprite_direct;
        let mut out = [0u8; LIGHT_UBO_BYTES];
        for (i, f) in floats.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&f.to_le_bytes());
        }
        out
    }
}

const IDENTITY: [f32; 16] = [
    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
];

/// An axis-aligned box in world space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aabb {
    pub min: [f32; 3],
    pub max: [f32; 3],
}

impl Aabb {
    /// The box around `points`, or `None` for none.
    #[must_use]
    pub fn around(points: impl IntoIterator<Item = [f32; 3]>) -> Option<Aabb> {
        let mut it = points.into_iter();
        let first = it.next()?;
        let mut b = Aabb {
            min: first,
            max: first,
        };
        for p in it {
            for (k, v) in p.iter().enumerate() {
                b.min[k] = b.min[k].min(*v);
                b.max[k] = b.max[k].max(*v);
            }
        }
        Some(b)
    }

    fn corners(&self) -> [[f32; 3]; 8] {
        let (a, b) = (self.min, self.max);
        [
            [a[0], a[1], a[2]],
            [b[0], a[1], a[2]],
            [a[0], b[1], a[2]],
            [b[0], b[1], a[2]],
            [a[0], a[1], b[2]],
            [b[0], a[1], b[2]],
            [a[0], b[1], b[2]],
            [b[0], b[1], b[2]],
        ]
    }
}

/// An orthographic light camera fitted to a frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShadowFit {
    /// World to the map's clip space (column-major, depth `0..1`, `y` up).
    pub view_proj: [f32; 16],
    pub map_size: u32,
    /// World pixels per map texel.
    pub texel_world: f32,
    /// Depth bias, map depth units.
    pub bias: f32,
}

type V3 = [f64; 3];

fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn norm(a: V3) -> V3 {
    let l = dot(a, a).sqrt().max(1e-12);
    [a[0] / l, a[1] / l, a[2] / l]
}
fn v3(p: [f32; 3]) -> V3 {
    [f64::from(p[0]), f64::from(p[1]), f64::from(p[2])]
}

/// Inverse of a column-major 4x4, or `None` when singular.
#[must_use]
pub fn invert(m: &[f32; 16]) -> Option<[f64; 16]> {
    let a: Vec<f64> = m.iter().map(|v| f64::from(*v)).collect();
    // Gauss-Jordan on [A | I]; 4 pivots, so the loops are bounded ranges.
    let mut aug = [[0.0f64; 8]; 4];
    for r in 0..4 {
        for c in 0..4 {
            aug[r][c] = a[c * 4 + r];
        }
        aug[r][4 + r] = 1.0;
    }
    for col in 0..4 {
        let pivot = (col..4).max_by(|&i, &j| aug[i][col].abs().total_cmp(&aug[j][col].abs()))?;
        if aug[pivot][col].abs() < 1e-14 {
            return None;
        }
        aug.swap(col, pivot);
        let p = aug[col][col];
        for v in &mut aug[col] {
            *v /= p;
        }
        for r in 0..4 {
            if r != col {
                let f = aug[r][col];
                if f != 0.0 {
                    let pivot_row = aug[col];
                    for (v, pv) in aug[r].iter_mut().zip(pivot_row) {
                        *v -= f * pv;
                    }
                }
            }
        }
    }
    let mut out = [0.0f64; 16];
    for r in 0..4 {
        for c in 0..4 {
            out[c * 4 + r] = aug[r][4 + c];
        }
    }
    Some(out)
}

fn unproject(inv: &[f64; 16], x: f64, y: f64, z: f64) -> V3 {
    let v = [x, y, z, 1.0];
    let mut o = [0.0f64; 4];
    for (r, slot) in o.iter_mut().enumerate() {
        *slot = (0..4).map(|c| inv[c * 4 + r] * v[c]).sum();
    }
    let w = if o[3].abs() < 1e-12 { 1e-12 } else { o[3] };
    [o[0] / w, o[1] / w, o[2] / w]
}

/// Parameter range `[t0, t1]` (clamped to `0..=1`) where the segment
/// `a + t (b - a)` is inside `bx`, or `None`.
fn slab(a: V3, b: V3, bx: &Aabb) -> Option<(f64, f64)> {
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for k in 0..3 {
        let d = b[k] - a[k];
        // A frame edge that lies exactly on the scene's edge must still hit.
        let eps = 1e-3 * (f64::from(bx.max[k] - bx.min[k]) + 1.0);
        let (lo, hi) = (f64::from(bx.min[k]) - eps, f64::from(bx.max[k]) + eps);
        if d.abs() < 1e-12 {
            if a[k] < lo || a[k] > hi {
                return None;
            }
        } else {
            let (mut s, mut e) = ((lo - a[k]) / d, (hi - a[k]) / d);
            if s > e {
                std::mem::swap(&mut s, &mut e);
            }
            t0 = t0.max(s);
            t1 = t1.min(e);
            if t0 > t1 {
                return None;
            }
        }
    }
    Some((t0, t1))
}

/// Fit an orthographic shadow map of `map_size` texels a side over what the
/// camera `view_proj` sees of `scene`.
///
/// The four corner rays of the frame are cut against the scene's box; where
/// they enter and leave bounds the part of the scene that can be seen, and
/// the map covers that, snapped to whole texels so it does not shimmer as
/// the camera glides. Anything that shades the visible part lies upsun of it
/// on the same light ray, so its position across the light is already inside
/// those bounds; only the depth range needs the whole scene. A frame whose
/// rays miss the box (a camera looking at nothing) falls back to the whole
/// box. `None` for a singular matrix or an empty light.
#[must_use]
pub fn fit_shadow_map(
    view_proj: &[f32; 16],
    scene: &Aabb,
    sun: &Sun,
    map_size: u32,
) -> Option<ShadowFit> {
    let inv = invert(view_proj)?;
    let f = norm(v3(sun.travel()));
    // Not parallel to `up`: the sun is never straight overhead (the floor on
    // its elevation is below 90), but be safe for a hand-built Sun.
    let up = if f[1].abs() > 0.999 {
        [0.0, 0.0, -1.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    let s = norm(cross(f, up));
    let u = cross(s, f);
    let to_light = |p: V3| (dot(s, p), dot(u, p), dot(f, p));

    let mut lateral: Vec<(f64, f64)> = Vec::with_capacity(8);
    for (x, y) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
        let near = unproject(&inv, x, y, 0.0);
        let far = unproject(&inv, x, y, 1.0);
        if let Some((t0, t1)) = slab(near, far, scene) {
            for t in [t0, t1] {
                let p = [
                    near[0] + (far[0] - near[0]) * t,
                    near[1] + (far[1] - near[1]) * t,
                    near[2] + (far[2] - near[2]) * t,
                ];
                let (lx, ly, _) = to_light(p);
                lateral.push((lx, ly));
            }
        }
    }
    let mut depth = (f64::MAX, f64::MIN);
    let rays_hit = !lateral.is_empty();
    for c in scene.corners() {
        let (lx, ly, ld) = to_light(v3(c));
        depth.0 = depth.0.min(ld);
        depth.1 = depth.1.max(ld);
        if !rays_hit {
            // The camera sees none of the scene's box: cover all of it.
            lateral.push((lx, ly));
        }
    }
    let (mut x0, mut x1, mut y0, mut y1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
    for (x, y) in &lateral {
        x0 = x0.min(*x);
        x1 = x1.max(*x);
        y0 = y0.min(*y);
        y1 = y1.max(*y);
    }
    // A square map keeps one texel size; a little slack keeps the PCF
    // kernel's border inside it.
    let n = f64::from(map_size.max(1));
    let half = (((x1 - x0).max(y1 - y0)) * 0.5 * 1.02).max(1.0);
    let texel = 2.0 * half / n;
    let snap = |c: f64| (c / texel).round() * texel;
    let (cx, cy) = (snap((x0 + x1) * 0.5), snap((y0 + y1) * 0.5));
    let margin = 2.0 * texel + 1.0;
    let (d0, d1) = (depth.0 - margin, depth.1 + margin);
    let dr = (d1 - d0).max(1e-6);

    // clip = (x - cx) / half, (y - cy) / half, (d - d0) / dr
    let m = [
        s[0] / half,
        u[0] / half,
        f[0] / dr,
        0.0,
        s[1] / half,
        u[1] / half,
        f[1] / dr,
        0.0,
        s[2] / half,
        u[2] / half,
        f[2] / dr,
        0.0,
        -cx / half,
        -cy / half,
        -d0 / dr,
        1.0,
    ];
    let mut view_proj_out = [0.0f32; 16];
    for (o, v) in view_proj_out.iter_mut().zip(m) {
        *o = v as f32;
    }
    // Bias grows with the texel (a coarse map needs more) and with how
    // grazing the light is (the shader adds the slope term).
    let bias_world = texel * 1.5 + 0.75;
    Some(ShadowFit {
        view_proj: view_proj_out,
        map_size,
        texel_world: texel as f32,
        bias: (bias_world / dr) as f32,
    })
}

/// Shadow map size for a render target of `out_width` pixels: 1024 below 3x
/// the SNES width, 2048 from there.
#[must_use]
pub fn map_size_for(out_width: u32) -> u32 {
    if out_width >= 768 {
        2048
    } else {
        1024
    }
}

/// Budget gate of the shadow map: when a render's p95 over the last
/// [`SHADOW_GATE_WINDOW`] frames passes [`SHADOW_BUDGET_P95_MS`] the map
/// switches off and the pass draws contact shadows only, until [`Self::reset`].
#[derive(Debug, Default)]
pub struct ShadowGate {
    window: std::collections::VecDeque<f64>,
    tripped: bool,
}

/// Frames the shadow gate looks at.
pub const SHADOW_GATE_WINDOW: usize = 120;
/// Whole-render p95 (ms) above which the shadow map is dropped: half a 60 Hz
/// frame, leaving the rest for the app, the lens and the UI. The pass's own
/// disable gate (`crate::fog::DISABLE_P95_MS`) stays the last resort.
pub const SHADOW_BUDGET_P95_MS: f64 = 8.0;

impl ShadowGate {
    /// Record one render's time. Returns `true` on the call that trips.
    pub fn record_ms(&mut self, ms: f64) -> bool {
        if self.tripped {
            return false;
        }
        self.window.push_back(ms);
        if self.window.len() > SHADOW_GATE_WINDOW {
            self.window.pop_front();
        }
        if self.window.len() < SHADOW_GATE_WINDOW {
            return false;
        }
        let mut sorted: Vec<f64> = self.window.iter().copied().collect();
        sorted.sort_by(f64::total_cmp);
        let p95 = sorted[(sorted.len() - 1) * 95 / 100];
        if p95 > SHADOW_BUDGET_P95_MS {
            self.tripped = true;
        }
        self.tripped
    }

    #[must_use]
    pub fn allows_map(&self) -> bool {
        !self.tripped
    }

    pub fn reset(&mut self) {
        self.window.clear();
        self.tripped = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sun(t: f32) -> Sun {
        Sun::for_time(t, Weather::None, LightView::TopDown)
    }

    #[test]
    fn the_sun_rises_screen_right_and_sets_screen_left() {
        let (am, noon, pm) = (sun(0.3), sun(0.5), sun(0.78));
        assert!(am.to_sun[0] > 0.2, "morning sun is screen-right: {am:?}");
        assert!(noon.to_sun[0].abs() < 0.1, "noon is centred: {noon:?}");
        assert!(pm.to_sun[0] < -0.2, "evening sun is screen-left: {pm:?}");
        assert!(am.to_sun[2] < 0.0, "top-down: the sun is toward screen-up");
    }

    #[test]
    fn elevation_peaks_at_noon_and_shadows_lengthen_toward_dusk() {
        let (am, noon, dusk) = (sun(0.3), sun(0.5), sun(0.8));
        assert!(noon.elevation_deg > am.elevation_deg);
        assert!(am.elevation_deg > dusk.elevation_deg);
        assert!(dusk.elevation_deg >= MIN_ELEVATION_DEG - 1e-3);
        assert!((noon.elevation_deg - 70.0).abs() < 3.0);
    }

    #[test]
    fn to_sun_is_a_unit_vector_at_every_hour() {
        for i in 0..=100 {
            let t = i as f32 / 100.0;
            for view in [LightView::TopDown, LightView::Side] {
                let s = Sun::for_time(t, Weather::None, view);
                let l: f32 = s.to_sun.iter().map(|v| v * v).sum::<f32>().sqrt();
                assert!((l - 1.0).abs() < 1e-4, "t={t}: {l}");
                assert!(s.to_sun[1] > 0.0, "the light is always above the floor");
            }
        }
    }

    #[test]
    fn night_is_a_dim_cool_moon_with_more_ambient_than_nothing() {
        let n = sun(0.05);
        assert!(n.night);
        assert!(n.intensity <= MOON_INTENSITY + 1e-6);
        assert!(n.color[2] > n.color[0], "cool: {:?}", n.color);
        assert!((n.ambient - AMBIENT_NIGHT).abs() < 1e-6);
        assert!((sun(0.5).ambient - AMBIENT_DAY).abs() < 1e-3);
    }

    #[test]
    fn noon_on_level_ground_is_as_bright_as_the_unlit_picture() {
        let b = sun(0.5).brightness([0.0, 1.0, 0.0]);
        assert!(b > 0.97 && b <= 1.0 + 1e-4, "{b}");
    }

    #[test]
    fn a_face_turned_from_the_sun_falls_to_ambient() {
        let s = sun(0.3);
        let away = [-s.to_sun[0], 0.0, -s.to_sun[2]];
        assert!((s.brightness(away) - s.ambient).abs() < 1e-6);
        assert!(s.brightness([0.0, 1.0, 0.0]) > s.ambient + 0.1);
    }

    #[test]
    fn weather_dims_the_sun() {
        let clear = Sun::for_time(0.5, Weather::None, LightView::TopDown).intensity;
        let rain = Sun::for_time(0.5, Weather::Rain, LightView::TopDown).intensity;
        let snow = Sun::for_time(0.5, Weather::Snow, LightView::TopDown).intensity;
        assert!((rain / clear - 0.6).abs() < 1e-4 && (snow / clear - 0.8).abs() < 1e-4);
    }

    #[test]
    fn the_side_view_sun_is_on_the_camera_side() {
        for t in [0.3, 0.5, 0.78] {
            let s = Sun::for_time(t, Weather::None, LightView::Side);
            assert!(s.to_sun[2] > 0.0, "t={t}: front faces must see the sun");
            assert!(s.brightness([0.0, 0.0, 1.0]) > s.ambient);
        }
    }

    #[test]
    fn a_non_finite_time_is_noon() {
        assert_eq!(sun(f32::NAN), sun(0.5));
    }

    #[test]
    fn the_light_uniform_is_128_bytes_and_off_means_off() {
        let off = LightUbo::off().to_bytes();
        assert_eq!(off.len(), LIGHT_UBO_BYTES);
        let enabled = f32::from_le_bytes(off[96..100].try_into().unwrap());
        assert_eq!(enabled, 0.0);
        let lit = LightUbo::lit(&sun(0.5), None).to_bytes();
        assert_eq!(f32::from_le_bytes(lit[96..100].try_into().unwrap()), 1.0);
        assert_eq!(f32::from_le_bytes(lit[100..104].try_into().unwrap()), 0.0);
    }

    #[test]
    fn the_inverse_undoes_the_matrix() {
        let m: [f32; 16] = [
            2.0, 0.0, 0.0, 0.0, 0.0, 1.5, 0.3, 0.0, 0.0, -0.2, 1.0, -1.0, 5.0, 3.0, 2.0, 1.0,
        ];
        let inv = invert(&m).expect("invertible");
        let p = [1.0f64, 2.0, 3.0, 1.0];
        let mut q = [0.0f64; 4];
        for (r, slot) in q.iter_mut().enumerate() {
            *slot = (0..4).map(|c| f64::from(m[c * 4 + r]) * p[c]).sum();
        }
        let mut back = [0.0f64; 4];
        for (r, slot) in back.iter_mut().enumerate() {
            *slot = (0..4).map(|c| inv[c * 4 + r] * q[c]).sum();
        }
        for k in 0..4 {
            assert!((back[k] - p[k]).abs() < 1e-9, "{back:?}");
        }
        assert!(invert(&[0.0; 16]).is_none());
    }

    /// A top-down orthographic-ish camera over a 100 x 100 floor: maps world
    /// x,z in [0,100] to the frame.
    fn overhead(span: f32) -> [f32; 16] {
        // x_ndc = (x - 50) / span, y_ndc = (50 - z) / span, z_ndc = (h)/200
        [
            1.0 / span,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            1.0 / 200.0,
            0.0,
            0.0,
            -1.0 / span,
            0.0,
            0.0,
            -50.0 / span,
            50.0 / span,
            0.0,
            1.0,
        ]
    }

    #[test]
    fn the_fit_covers_the_visible_floor_and_snaps_to_texels() {
        let scene = Aabb {
            min: [0.0, 0.0, 0.0],
            max: [100.0, 16.0, 100.0],
        };
        let s = sun(0.3);
        let fit = fit_shadow_map(&overhead(50.0), &scene, &s, 1024).expect("fits");
        // Every visible floor corner lands inside the map.
        for (x, z) in [(0.0, 0.0), (100.0, 0.0), (0.0, 100.0), (100.0, 100.0)] {
            let m = fit.view_proj;
            let clip = |k: usize| m[k] * x + m[4 + k] * 0.0 + m[8 + k] * z + m[12 + k];
            let (cx, cy, cz) = (clip(0), clip(1), clip(2));
            assert!(cx.abs() <= 1.0 && cy.abs() <= 1.0, "({x},{z}) -> {cx},{cy}");
            assert!((0.0..=1.0).contains(&cz), "depth {cz}");
        }
        assert!(fit.texel_world > 0.0 && fit.texel_world < 0.5);
        // A high box shifts the depth range, not the footprint.
        let tall = Aabb {
            max: [100.0, 80.0, 100.0],
            ..scene
        };
        let t = fit_shadow_map(&overhead(50.0), &tall, &s, 1024).expect("fits");
        assert!((t.texel_world - fit.texel_world).abs() < fit.texel_world);
    }

    #[test]
    fn a_closer_view_gets_a_finer_map() {
        let scene = Aabb {
            min: [0.0, 0.0, 0.0],
            max: [100.0, 16.0, 100.0],
        };
        let s = sun(0.5);
        let wide = fit_shadow_map(&overhead(50.0), &scene, &s, 1024).unwrap();
        let near = fit_shadow_map(&overhead(20.0), &scene, &s, 1024).unwrap();
        assert!(near.texel_world < wide.texel_world);
    }

    #[test]
    fn a_singular_camera_has_no_fit() {
        let scene = Aabb {
            min: [0.0; 3],
            max: [1.0; 3],
        };
        assert!(fit_shadow_map(&[0.0; 16], &scene, &sun(0.5), 1024).is_none());
    }

    #[test]
    fn the_shadow_gate_trips_once_on_a_slow_p95_and_stays_off_until_reset() {
        let mut g = ShadowGate::default();
        for _ in 0..SHADOW_GATE_WINDOW * 2 {
            assert!(!g.record_ms(2.0));
        }
        assert!(g.allows_map());
        let mut trips = 0;
        for _ in 0..SHADOW_GATE_WINDOW * 2 {
            if g.record_ms(30.0) {
                trips += 1;
            }
        }
        assert!(trips >= 1 && !g.allows_map());
        g.reset();
        assert!(g.allows_map());
    }

    #[test]
    fn the_map_is_bigger_from_3x() {
        assert_eq!(map_size_for(256), 1024);
        assert_eq!(map_size_for(1024), 2048);
    }

    #[test]
    fn aabb_around_nothing_is_none() {
        assert!(Aabb::around(std::iter::empty()).is_none());
        let b = Aabb::around([[1.0, 2.0, 3.0], [-1.0, 5.0, 0.0]]).unwrap();
        assert_eq!((b.min, b.max), ([-1.0, 2.0, 0.0], [1.0, 5.0, 3.0]));
    }
}
