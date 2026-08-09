// Original pipeline, scale pass (ticket W3-01b; docs/design/RENDERER.md
// §2): overscan crop + integer-locked vertical scale + PAR-corrected
// horizontal scale, nearest-neighbor. Same bit-exactness technique as
// palette.wgsl: `@builtin(position)` is spec-guaranteed to be the pixel
// center in window (target-texture) coordinates, not a driver-defined
// value, and `textureLoad` (not `textureSample`) picks an exact texel with
// no sampler/filtering involved. See `crate::scale`'s module doc for why
// this is still not treated as literally bit-identical across every
// backend (WGSL float division/floor is not guaranteed bit-for-bit
// portable the way Rust's is) -- that residual gap is what
// `rf-harness`'s reference-image-with-tolerance test exists to absorb,
// not a sign this pass is doing anything approximate on purpose.

struct Params {
    // x: 1 / x_scale, y: 1 / y_scale, z: crop_top (source pixel rows), w: unused.
    v: vec4<f32>,
}

@group(0) @binding(0)
var<uniform> p: Params;

@group(0) @binding(1)
var source_tex: texture_2d<f32>;

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
}

// Fullscreen triangle from just the vertex index -- no vertex buffer. The
// render pass's color attachment is the *output* (scaled) texture, so
// `clip_pos` here ranges over the output's pixel space, not the source's.
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
    let out_x = i32(in.clip_pos.x);
    let out_y = i32(in.clip_pos.y);
    let dims = vec2<i32>(textureDimensions(source_tex));
    // floor(out_coord * inv_scale) -- identical formula, identical f32
    // precision, to `crate::scale::render_scaled_reference`'s CPU oracle.
    let src_x_f = floor(f32(out_x) * p.v.x);
    let src_y_f = floor(f32(out_y) * p.v.y) + p.v.z;
    let src_x = clamp(i32(src_x_f), 0, dims.x - 1);
    let src_y = clamp(i32(src_y_f), 0, dims.y - 1);
    return textureLoad(source_tex, vec2<i32>(src_x, src_y), 0);
}
