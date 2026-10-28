// Diorama G-buffer pass (ticket W39-05; docs in src/diorama_gbuffer.rs).
// Provenance: authored fresh for this ticket -- a depth + motion-vector
// pre-pass is the standard input of any temporal upscaler (arithmetic
// nobody owns). It draws the same mesh as diorama.wgsl, with the same
// alpha-test discards, but writes only what the upscaler needs:
//   * depth (the Depth32Float attachment, 0 near / 1 far), and
//   * a motion vector per pixel (location 0, Rg16Float), in INPUT pixels,
//     pointing from the pixel to where its surface point was last frame
//     (an object that moved 10 px down-right has (-10, -10); MetalFX's
//     motionVectorScale = 1 convention).
// The position is rasterised with the JITTERED matrix; the motion vector is
// built from the UNJITTERED current and previous matrices, so the jitter
// never leaks into the motion (that would blur forever).

struct Cam {
    vp_jit: mat4x4<f32>,
    vp_cur: mat4x4<f32>,
    vp_prev: mat4x4<f32>,
    size: vec4<f32>, // xy: input size in pixels
}

@group(0) @binding(0) var<uniform> cam: Cam;
@group(0) @binding(1) var ground_tex: texture_2d<f32>;
@group(0) @binding(2) var sprite_tex: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;

struct VsIn {
    @location(0) pos: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) kind: u32,
    @location(4) prev_pos: vec3<f32>,
}

struct VsOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color_a: f32,
    @location(2) @interpolate(flat) kind: u32,
    @location(3) cur: vec4<f32>,
    @location(4) prev: vec4<f32>,
}

const KIND_SPRITE: u32 = 1u;
const KIND_SHADOW: u32 = 2u;
const KIND_SIDE: u32 = 3u;
const MIN_ALPHA: f32 = 0.02;

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    out.clip_position = cam.vp_jit * vec4<f32>(in.pos, 1.0);
    out.uv = in.uv;
    out.color_a = in.color.a;
    out.kind = in.kind;
    out.cur = cam.vp_cur * vec4<f32>(in.pos, 1.0);
    out.prev = cam.vp_prev * vec4<f32>(in.prev_pos, 1.0);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec2<f32> {
    // Contact shadows are decals: they never write depth.
    if (in.kind == KIND_SHADOW) {
        discard;
    }
    // Explicit-level samples (no derivatives): legal after the discard above.
    let g = textureSampleLevel(ground_tex, samp, in.uv, 0.0);
    let s = textureSampleLevel(sprite_tex, samp, in.uv, 0.0);
    if (in.kind == KIND_SPRITE) {
        if (s.a * in.color_a < MIN_ALPHA) {
            discard;
        }
    } else if (in.kind != KIND_SIDE) {
        // GROUND, PLANE and BACK sample the ground texture.
        if (g.a * in.color_a < MIN_ALPHA) {
            discard;
        }
    }
    // A point at or behind the eye plane has no defined projection: no motion.
    if (in.cur.w < 1e-5 || in.prev.w < 1e-5) {
        return vec2<f32>(0.0, 0.0);
    }
    let d = in.prev.xy / in.prev.w - in.cur.xy / in.cur.w;
    // NDC is y-up, pixels are y-down.
    let px = vec2<f32>(d.x * 0.5 * cam.size.x, -d.y * 0.5 * cam.size.y);
    // Interpolating two clip positions leaves ~1e-5 px of rounding noise on a
    // static point; below 1/256 px nothing moved, so say exactly zero (a
    // static scene then reprojects without any resampling blur).
    return select(px, vec2<f32>(0.0), abs(px) < vec2<f32>(0.004));
}
