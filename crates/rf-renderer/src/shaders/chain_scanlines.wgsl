// Shader chain, "scanlines" pass (ticket W3-02; docs/design/RENDERER.md
// §4).
//
// Provenance: authored fresh for this ticket, not ported or transcribed
// from any external shader. "Darken every Nth output row" is trivial
// arithmetic nobody can own -- CLAUDE.md law 5 / this ticket's brief still
// requires the WGSL itself be original, which it is.
//
// Same common chain interface as every pass in `crate::shader_chain` --
// see `chain_nearest.wgsl`'s header for the shared bind group layout. Uses
// a nearest-filtering sampler (`ShaderChain::new` builds it that way for
// this shader) since darkening alternating rows has nothing to do with
// resampling quality -- the identity/resize resample here is the same
// technique as `chain_nearest.wgsl`, just with a per-row multiply applied
// afterward.

struct Params {
    v0: vec4<f32>, // x: intensity in [0, 1] (0 = no darkening, 1 = fully
                    // black on darkened rows). y: period in output rows
                    // (>= 1; every row whose index modulo this is 0 is
                    // darkened). zw unused.
    v1: vec4<f32>, // xy: 1/out_width, 1/out_height. zw unused.
}

@group(0) @binding(0)
var<uniform> p: Params;

@group(0) @binding(1)
var source_tex: texture_2d<f32>;

@group(0) @binding(2)
var samp: sampler;

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VsOut {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    var out: VsOut;
    out.clip_pos = vec4<f32>(positions[vertex_index], 0.0, 1.0);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let uv = in.clip_pos.xy * p.v1.xy;
    let color = textureSample(source_tex, samp, uv);

    let intensity = clamp(p.v0.x, 0.0, 1.0);
    let period = max(u32(p.v0.y), 1u);
    let row = u32(in.clip_pos.y);
    let darken = (row % period) == 0u;
    let factor = select(1.0, 1.0 - intensity, darken);

    // RGB only -- alpha (compositing coverage) is untouched by a purely
    // visual darkening effect, same "don't touch alpha for a color-only
    // effect" stance as nothing else in this crate needing to invent one.
    return vec4<f32>(color.rgb * factor, color.a);
}
