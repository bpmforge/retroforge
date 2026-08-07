// Original pipeline, palette pass (ticket W3-01; docs/design/RENDERER.md
// §2): indexed frame (R8Uint texture) -> LUT lookup -> RGBA8 target.
//
// Bit-exact by construction (RENDERER.md §7): integer texel load, integer
// LUT index, no filtering, no sRGB math -- so the 1x buffer this pass
// produces hashes identically across backends/drivers, which is exactly
// what makes it safe to golden-hash in CI.

// One RGBA color per NES palette entry ($00-$3F), in [0, 1] float already
// (converted host-side from the u8 RGB triples -- see
// `original_pipeline::lut_bytes`).
@group(0) @binding(0)
var<storage, read> lut: array<vec4<f32>, 64>;

@group(0) @binding(1)
var indexed_tex: texture_2d<u32>;

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
}

// Fullscreen triangle from just the vertex index -- no vertex buffer.
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
    let coord = vec2<i32>(i32(in.clip_pos.x), i32(in.clip_pos.y));
    let palette_index = textureLoad(indexed_tex, coord, 0).r;
    // Mask to 6 bits, same "degrade, never index out of range" stance as
    // the CPU path (`palette::palette_index_to_rgb`).
    return lut[palette_index & 63u];
}
