// Shader chain, "sharp-bilinear" pass (ticket W3-02; docs/design/RENDERER.md
// §4).
//
// Provenance: authored fresh for this ticket from first principles, not
// ported or transcribed from RetroArch/libretro's `sharp-bilinear.glsl`,
// any other shader-preset collection, or any other existing source --
// nobody on this ticket had such a source open while writing this file.
// RENDERER.md §4 notes real "sharp-bilinear" implementations are public
// domain and *may* be ported directly, but this ticket's brief asks for
// authored WGSL with the manifest saying so regardless, so the technique
// below is independently derived: blend the sampled UV toward its nearest
// texel-center UV by a `sharpness` knob, then take one real hardware-
// filtered sample at the blended UV. "Blend toward the nearest texel
// before a filtered sample" is a generic, unowned image-scaling idea, not
// copied code.
//
// Same common chain interface as every pass in `crate::shader_chain`
// (RENDERER.md §4: "tex_in, sampler, params: UBO -> tex_out") -- see
// `chain_nearest.wgsl`'s header for the shared bind group layout. Unlike
// `chain_nearest.wgsl`, this pass uses a *linear*-filtering sampler
// (`ShaderChain::new` builds it that way for this shader specifically) --
// real hardware bilinear filtering, not a hand-rolled `textureLoad` lerp --
// deliberately, because RENDERER.md §7's "not hashable past 1x" warning is
// about exactly this: driver-dependent filtering math. See
// `crate::shader_chain::render_sharp_bilinear_reference` for the CPU oracle
// this pass is checked against with a **nonzero** tolerance (this ticket's
// acceptance criterion 5) -- the residual gap between the GPU's hardware
// bilinear unit and that CPU-side manual bilinear is the real, measured
// noise the tolerance absorbs, not an invented allowance.

struct Params {
    v0: vec4<f32>, // x: sharpness in [0, 1]. yzw unused.
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
    let src_size = vec2<f32>(textureDimensions(source_tex));

    // The continuous source-texel coordinate this output pixel maps to,
    // and the UV of that texel's own center (floor + 0.5, back to [0,1]).
    let texel = uv * src_size;
    let nearest_texel = floor(texel) + vec2<f32>(0.5, 0.5);
    let uv_nearest = nearest_texel / src_size;

    // sharpness = 0: sample exactly at `uv` (pure smooth bilinear).
    // sharpness = 1: sample exactly at a texel center, where bilinear
    // filtering has no neighbor to blend with and so degenerates to a
    // single texel -- a "sharp" result despite still going through the
    // filtering sampler.
    let sharpness = clamp(p.v0.x, 0.0, 1.0);
    let uv_final = mix(uv, uv_nearest, sharpness);

    return textureSample(source_tex, samp, uv_final);
}
