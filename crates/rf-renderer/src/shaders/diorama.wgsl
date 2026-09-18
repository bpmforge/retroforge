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
// No depth buffer: crate::diorama_mesh's own module doc explains why
// (painter's algorithm -- vertices already arrive back-to-front).

struct Camera {
    view_proj: mat4x4<f32>,
}

@group(0) @binding(0)
var<uniform> camera: Camera;

@group(0) @binding(1)
var ground_tex: texture_2d<f32>;

@group(0) @binding(2)
var sprite_tex: texture_2d<f32>;

@group(0) @binding(3)
var samp: sampler;

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
}

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    out.clip_position = camera.view_proj * vec4<f32>(in.pos, 1.0);
    out.uv = in.uv;
    out.color = in.color;
    out.kind = in.kind;
    return out;
}

const KIND_GROUND: u32 = 0u;
const KIND_SPRITE: u32 = 1u;
const KIND_SHADOW: u32 = 2u;
const KIND_SIDE: u32 = 3u;

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    if (in.kind == KIND_GROUND) {
        let tex = textureSample(ground_tex, samp, in.uv);
        return vec4<f32>(tex.rgb * in.color.rgb, tex.a * in.color.a);
    }
    if (in.kind == KIND_SPRITE) {
        let tex = textureSample(sprite_tex, samp, in.uv);
        return vec4<f32>(tex.rgb, tex.a * in.color.a);
    }
    if (in.kind == KIND_SHADOW) {
        // Radial falloff from the quad's own centre (uv 0.5,0.5): 1.0 at
        // the centre, 0.0 at/past the quad's edge (uv distance 0.5 from
        // centre on either axis) -- a soft contact-shadow ellipse, not a
        // real shadow map (no light source is modelled at all).
        let centered = (in.uv - vec2<f32>(0.5, 0.5)) * 2.0;
        let falloff = clamp(1.0 - length(centered), 0.0, 1.0);
        return vec4<f32>(in.color.rgb, in.color.a * falloff);
    }
    // KIND_SIDE: flat-shaded, no texture.
    return in.color;
}
