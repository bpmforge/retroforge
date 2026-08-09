// Shader chain, "nearest" pass (ticket W3-02; docs/design/RENDERER.md §4).
//
// Provenance: authored fresh for this ticket, not ported or transcribed
// from any external shader. Nearest-neighbor texture resampling is
// arithmetically trivial (one texel fetch, no blend) -- nobody owns this
// technique -- but the WGSL text itself is original RetroForge work, per
// this ticket's licensing brief. See `crate::shader_chain`'s module doc
// for the full provenance statement recorded in this shader's manifest.
//
// Common chain interface (RENDERER.md §4: "tex_in, sampler, params: UBO ->
// tex_out"), shared byte-for-byte across every pass in `crate::shader_chain`
// (present and future -- W3-02a's shaders reuse this exact bind group
// layout): binding 0 is a 32-byte Params UBO, binding 1 the input texture,
// binding 2 a sampler. This pass ignores `p.v0` (no tunable parameters --
// see the manifest's empty `params` list) and only reads `p.v1.xy`, the
// reciprocal output size every chain pass needs to turn a fragment's
// window-space position into a [0,1] UV.

struct Params {
    v0: vec4<f32>, // unused by this shader.
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

// Fullscreen triangle from just the vertex index -- no vertex buffer, same
// technique as every other pass in this crate (`palette.wgsl`/`scale.wgsl`/
// `composite.wgsl`).
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
    // `@builtin(position)` is spec-guaranteed to be the pixel center in
    // window (output-texture) coordinates, so this needs no extra +0.5 --
    // multiplying directly by the reciprocal output size lands exactly on
    // each output pixel's own UV center.
    let uv = in.clip_pos.xy * p.v1.xy;
    // `samp` is created with nearest min/mag filtering by
    // `ShaderChain::new` for this pass specifically (`crate::shader_chain`)
    // -- the bind group layout's `Filtering` sampler-binding type only
    // declares what a bound sampler is *permitted* to do, same fact
    // `crate::composite`'s own sampler already relies on; the actual
    // nearest-vs-linear behavior comes from the sampler object itself.
    return textureSample(source_tex, samp, uv);
}
