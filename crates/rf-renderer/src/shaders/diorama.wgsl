// Diorama pass ("walls pop up"; ticket W16-06; docs/design/
// ENHANCEMENT_WAVE_16.md §5). Provenance: authored fresh for this ticket,
// not ported or transcribed from any existing shader -- a per-vertex
// camera transform plus a four-way per-fragment material switch (textured
// top/ground, textured billboard, procedural radial shadow, flat-shaded
// side) is standard forward-rendering vocabulary, not a specific
// implementation this copies (same "arithmetic nobody owns" posture
// crate::shader_chain's module doc states for nearest/scanlines, and
// fog.wgsl's own header states for its multi-octave sampling).
//
// `kind` selects the fragment behaviour (crate::diorama_mesh::kind's own
// four constants, kept in sync with these literals by that module's doc):
//   0 GROUND -- ground plane or a box's top face: sample ground_tex.
//   1 SPRITE -- a billboard: sample sprite_tex, alpha = tex.a * color.a.
//   2 SHADOW -- a contact-shadow quad: no texture, radial falloff from
//     the quad's own UV centre times color.a.
//   3 SIDE   -- a box side face: flat-shaded, no texture.
//
// Depth (W34-05): the pass has a depth buffer and 4x MSAA. The vertices
// still arrive back-to-front, which fixes blending order; depth fixes what
// that order gets wrong (a sprite between two walls). Because a fragment
// that is transparent would still write depth and hide whatever is behind
// it, the fragments with nothing to show are discarded, not blended.
//
// Light (W34-06): `light.enabled == 0` is the original shader, bit for bit
// (every golden that never sets a sun renders through it). With a sun the
// ground, box tops and fronts are Lambert-lit by their screen-space-
// derivative normal, box sides take a flat albedo, sprites are lit like the
// ground they stand on, all scaled by a 3x3 PCF shadow-map lookup; the
// contact ellipse drops to ambient-occlusion strength when the map is on.
// `vs_shadow`/`fs_shadow` are the depth-only caster pass into that map:
// billboards are alpha-tested so a sprite's own silhouette casts.

struct Camera {
    view_proj: mat4x4<f32>,
}

// Lighting uniform; crate::lighting::LightUbo is the Rust twin (128 bytes).
struct Light {
    shadow_vp: mat4x4<f32>,
    to_sun: vec4<f32>,      // xyz toward the light, w intensity
    color: vec4<f32>,       // rgb light colour, w ambient
    params: vec4<f32>,      // x lighting on, y shadow map on, z depth bias, w texel (uv)
    extra: vec4<f32>,       // x contact scale, y side albedo, z sprite direct
}

@group(0) @binding(0)
var<uniform> camera: Camera;

@group(0) @binding(1)
var ground_tex: texture_2d<f32>;

@group(0) @binding(2)
var sprite_tex: texture_2d<f32>;

@group(0) @binding(3)
var samp: sampler;

@group(0) @binding(4)
var shadow_map: texture_depth_2d;

@group(0) @binding(5)
var shadow_samp: sampler_comparison;

@group(0) @binding(6)
var<uniform> light: Light;

struct VsIn {
    @location(0) pos: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) kind: u32,
}

struct VsOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) kind: u32,
    @location(3) world: vec3<f32>,
    @location(4) lpos: vec3<f32>,
}

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    out.clip_position = camera.view_proj * vec4<f32>(in.pos, 1.0);
    out.uv = in.uv;
    out.color = in.color;
    out.kind = in.kind;
    out.world = in.pos;
    out.lpos = (light.shadow_vp * vec4<f32>(in.pos, 1.0)).xyz;
    return out;
}

const KIND_GROUND: u32 = 0u;
const KIND_SPRITE: u32 = 1u;
const KIND_SHADOW: u32 = 2u;
const KIND_SIDE: u32 = 3u;

// Below this a fragment is invisible: it is dropped so it cannot write depth.
const MIN_ALPHA: f32 = 0.02;

// Lambert's exponent (crate::lighting::LAMBERT_EXPONENT).
const LAMBERT_EXPONENT: f32 = 0.7;

// The cross product of the screen-space derivatives of a visible surface's
// position points one fixed way relative to the viewer (the screen axes have
// a fixed handedness), so this sign turns it into the normal facing the
// camera. tests/diorama_light.rs pins it: level ground must come out +Y.
const NORMAL_SIGN: f32 = -1.0;

// 3x3 percentage-closer lookup: 1 lit, 0 in shadow. Outside the map is lit.
fn shadow_lookup(lpos: vec3<f32>, bias: f32) -> f32 {
    let uv = vec2<f32>(lpos.x * 0.5 + 0.5, 0.5 - lpos.y * 0.5);
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0 || lpos.z > 1.0) {
        return 1.0;
    }
    let ref_z = lpos.z - bias;
    let t = light.params.w;
    var sum = 0.0;
    for (var j = -1; j <= 1; j = j + 1) {
        for (var i = -1; i <= 1; i = i + 1) {
            let o = vec2<f32>(f32(i), f32(j)) * t;
            sum = sum + textureSampleCompareLevel(shadow_map, shadow_samp, uv + o, ref_z);
        }
    }
    return sum / 9.0;
}

// Everything lit: colour multiplier = light colour * (ambient + (1 - ambient)
// * direct * shadow). `direct` is the unshadowed sun term for this surface.
fn lit(direct: f32, shadow: f32) -> vec3<f32> {
    let ambient = light.color.w;
    return light.color.rgb * (ambient + (1.0 - ambient) * direct * shadow);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Derivatives need uniform control flow: take them before any discard.
    let dx = dpdx(in.world);
    let dy = dpdy(in.world);
    if (light.params.x < 0.5) {
        // Lighting off: the original shader.
        if (in.kind == KIND_GROUND) {
            let tex = textureSample(ground_tex, samp, in.uv);
            if (tex.a * in.color.a < MIN_ALPHA) {
                discard;
            }
            return vec4<f32>(tex.rgb * in.color.rgb, tex.a * in.color.a);
        }
        if (in.kind == KIND_SPRITE) {
            let tex = textureSample(sprite_tex, samp, in.uv);
            if (tex.a * in.color.a < MIN_ALPHA) {
                discard;
            }
            return vec4<f32>(tex.rgb, tex.a * in.color.a);
        }
        if (in.kind == KIND_SHADOW) {
            // Radial falloff from the quad's own centre (uv 0.5,0.5): 1.0 at
            // the centre, 0.0 at/past the quad's edge -- a soft contact-shadow
            // ellipse.
            let centered = (in.uv - vec2<f32>(0.5, 0.5)) * 2.0;
            let falloff = clamp(1.0 - length(centered), 0.0, 1.0);
            if (in.color.a * falloff < MIN_ALPHA) {
                discard;
            }
            return vec4<f32>(in.color.rgb, in.color.a * falloff);
        }
        // KIND_SIDE: flat-shaded, no texture.
        return in.color;
    }

    // Lit.
    var n = normalize(cross(dx, dy)) * NORMAL_SIGN;
    let to_sun = light.to_sun.xyz;
    let intensity = light.to_sun.w;
    let nl = dot(n, to_sun);
    let direct = intensity * pow(max(nl, 0.0), LAMBERT_EXPONENT);
    // Slope-scaled bias: a surface at a grazing angle to the light needs more.
    let bias = light.params.z * (1.0 + 4.0 * (1.0 - clamp(nl, 0.0, 1.0)));
    var shadow = 1.0;
    if (light.params.y > 0.5 && direct > 0.0) {
        shadow = shadow_lookup(in.lpos, bias);
    }

    if (in.kind == KIND_GROUND) {
        let tex = textureSample(ground_tex, samp, in.uv);
        if (tex.a * in.color.a < MIN_ALPHA) {
            discard;
        }
        return vec4<f32>(tex.rgb * lit(direct, shadow), tex.a * in.color.a);
    }
    if (in.kind == KIND_SPRITE) {
        let tex = textureSample(sprite_tex, samp, in.uv);
        if (tex.a * in.color.a < MIN_ALPHA) {
            discard;
        }
        // Sprites are lit like the ground under them; only the shadow is
        // looked up at the sprite's own position.
        var s = 1.0;
        if (light.params.y > 0.5 && light.extra.z > 0.0) {
            s = shadow_lookup(in.lpos, light.params.z * 3.0);
        }
        return vec4<f32>(tex.rgb * lit(light.extra.z, s), tex.a * in.color.a);
    }
    if (in.kind == KIND_SHADOW) {
        let centered = (in.uv - vec2<f32>(0.5, 0.5)) * 2.0;
        let falloff = clamp(1.0 - length(centered), 0.0, 1.0);
        let a = in.color.a * light.extra.x * falloff;
        if (a < MIN_ALPHA) {
            discard;
        }
        return vec4<f32>(in.color.rgb, a);
    }
    // KIND_SIDE: flat albedo, lit.
    return vec4<f32>(vec3<f32>(light.extra.y) * lit(direct, shadow), in.color.a);
}

// ---- shadow map caster pass (depth only) ----------------------------------

struct ShVsOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) alpha: f32,
    @location(2) @interpolate(flat) kind: u32,
}

@vertex
fn vs_shadow(in: VsIn) -> ShVsOut {
    var out: ShVsOut;
    out.clip_position = light.shadow_vp * vec4<f32>(in.pos, 1.0);
    out.uv = in.uv;
    out.alpha = in.color.a;
    out.kind = in.kind;
    return out;
}

@fragment
fn fs_shadow(in: ShVsOut) {
    // Only a billboard has a silhouette to test; boxes are solid.
    if (in.kind == KIND_SPRITE) {
        let tex = textureSampleLevel(sprite_tex, samp, in.uv, 0.0);
        if (tex.a * in.alpha < 0.5) {
            discard;
        }
    }
}
