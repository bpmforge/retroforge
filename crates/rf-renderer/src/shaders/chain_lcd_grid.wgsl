// Shader chain, "LCD-grid" pass (ticket W3-02a; docs/design/RENDERER.md
// §4, design review G-42).
//
// ============================ PROVENANCE ============================
//
// Licence: MIT OR Apache-2.0 (this file is original RetroForge work).
// Basis: NONE. Not based on, ported from, or transcribed from any
// existing shader -- specifically NOT libretro's `lcd-grid`/`lcd3x` or any
// other GPL or unlicensed shader. RENDERER.md §4 classes the LCD look as
// **behaviour-spec clean-room**; §4 separately notes lcd3x is public
// domain and *may* be ported directly, but this file is not a port of it
// either. The UI label is "LCD-grid-style".
//
// AUTHORSHIP CAVEAT: identical to `chain_crt.wgsl`'s -- see that file's
// header for the full statement. In short: written by an LLM plausibly
// trained on GPL shader sources, so the text below derives each term from
// stated geometry rather than asserting an unverifiable "sources closed".
//
// ========================= WHAT IT COMPUTES =========================
//
// An LCD panel differs from a CRT in that its pixels are DISCRETE cells
// with visible, non-emitting gaps between them, and each cell is built
// from three vertical subpixel stripes. Two effects, both pure geometry:
//
// 1. CELL GRID. Each source pixel maps to a rectangular block of output
//    pixels. Near that block's edge the panel shows the dark gap between
//    cells rather than the cell itself. Modelled by taking the fragment's
//    fractional position within its source pixel, measuring the distance
//    to the nearest cell edge on each axis, and darkening when that
//    distance is under `gap` (v0.y, as a fraction of the cell). Both axes
//    are treated identically because an LCD grid is symmetric -- unlike
//    the CRT above, where only the vertical axis carries the beam.
//
// 2. SUBPIXEL STRIPES. Within a cell, the horizontal third the fragment
//    falls into favours R, G or B. This is the same "tint by phase"
//    arithmetic as the CRT's aperture mask, but keyed to position WITHIN
//    THE SOURCE PIXEL rather than to the output column -- which is the
//    real difference between the two looks, and the reason both files
//    exist rather than one with a flag.
//
// No gamma round-trip here, deliberately, and the omission is the honest
// kind: the grid is a geometric occlusion (part of the panel is simply not
// lit) rather than a modulation of emitted brightness, so applying it in
// display space is the more defensible of the two choices. If a later
// ticket measures otherwise, this comment is what should be revisited.

struct Params {
    v0: vec4<f32>, // x: grid strength [0,1], y: gap width as a fraction of
                    // a cell [0,0.5], z: subpixel strength [0,1],
                    // w: unused.
    v1: vec4<f32>, // xy: 1/out_width, 1/out_height.
                    // zw: source width, source height in pixels.
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

    let grid_strength = clamp(p.v0.x, 0.0, 1.0);
    let gap = clamp(p.v0.y, 0.0, 0.5);
    let subpixel_strength = clamp(p.v0.z, 0.0, 1.0);
    let source_size = max(p.v1.zw, vec2<f32>(1.0, 1.0));

    // Position within this fragment's source pixel, on both axes.
    let cell = fract(uv * source_size);

    // (1) Cell grid. Distance to the nearest edge of the cell is
    // min(cell, 1 - cell) per axis; the smaller of the two axes decides,
    // so corners are darkest -- which is what a rectangular gap looks
    // like.
    let edge_dist = min(cell, vec2<f32>(1.0) - cell);
    let nearest_edge = min(edge_dist.x, edge_dist.y);
    // `smoothstep` rather than a hard cutoff so the gap has a soft
    // shoulder instead of aliasing badly at non-integer scale factors.
    let lit = smoothstep(0.0, max(gap, 0.0001), nearest_edge);
    let grid = mix(1.0, lit, grid_strength);

    // (2) Subpixel stripes, by horizontal third WITHIN the cell.
    var stripe: vec3<f32>;
    let third = u32(cell.x * 3.0);
    if (third == 0u) {
        stripe = vec3<f32>(1.0, 0.0, 0.0);
    } else if (third == 1u) {
        stripe = vec3<f32>(0.0, 1.0, 0.0);
    } else {
        stripe = vec3<f32>(0.0, 0.0, 1.0);
    }
    let tint = mix(vec3<f32>(1.0, 1.0, 1.0), stripe, subpixel_strength);

    return vec4<f32>(color.rgb * grid * tint, color.a);
}
