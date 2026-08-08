// Enhanced pipeline, layer compositor (ticket W4-03c; docs/design/RENDERER.md
// §3): one textured quad per `crate::composite::CompositeLayer`, positioned
// in the target's pixel space and blended "over" the accumulated result so
// far -- back-to-front draw order (host side, `EnhancedCompositor::composite`)
// is what makes later draws land on top, this shader has no notion of order
// itself.
//
// `rect` is a clip-space (NDC) bounding box, computed host-side from the
// layer's `dst_x`/`dst_y`/`width`/`height` and the target's own size --
// keeping the pixel->NDC math on the CPU (one division per layer, not per
// fragment) and the shader itself trivial: mix the four corners, sample.

struct Rect {
    // (x0, y0, x1, y1) in NDC: (x0, y0) is the layer's top-left corner,
    // (x1, y1) its bottom-right, after the host's pixel->NDC conversion
    // (which itself flips Y -- NDC +Y is up, pixel +Y is down).
    rect: vec4<f32>,
}

@group(0) @binding(0)
var<uniform> u: Rect;

@group(0) @binding(1)
var layer_tex: texture_2d<f32>;

@group(0) @binding(2)
var layer_sampler: sampler;

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

// Two triangles (6 vertices, no vertex/index buffer) forming the layer's
// quad. `t` sweeps (0,0)..(1,1) across the quad; `uv` reuses it directly --
// `t.y == 0` is the quad's top edge (`u.rect.y`, i.e. `dst_y`) and maps to
// `uv.y == 0`, which is texel row 0 in the layer's row-major upload
// (`EnhancedCompositor::composite`'s per-layer `write_texture` call) --
// so texture row 0 lands at the layer's top edge, matching every other
// row-major RGBA buffer in this crate (top-to-bottom).
@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VsOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
    );
    let t = corners[vertex_index];
    var out: VsOut;
    out.clip_pos = vec4<f32>(mix(u.rect.x, u.rect.z, t.x), mix(u.rect.y, u.rect.w, t.y), 0.0, 1.0);
    out.uv = t;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Straight (non-premultiplied) alpha out -- the pipeline's `BlendState`
    // (`EnhancedCompositor::new`) does the "over" compositing against
    // whatever is already in the target attachment; this pass only
    // resolves the layer's own texel.
    return textureSample(layer_tex, layer_sampler, in.uv);
}
