// Shader chain, "xBR-class" edge-directed upscaler (ticket W3-02a;
// docs/design/RENDERER.md §4, design review G-42).
//
// ============================ PROVENANCE ============================
//
// Licence: MIT OR Apache-2.0 (this file is original RetroForge work).
//
// Basis: the PUBLISHED CLASS OF TECHNIQUE -- diagonal edge-directed
// interpolation, as used by the xBR family (Hyllian, MIT) and by the older
// EPX/Eagle/Scale2x lineage. It is explicitly **NOT**:
//   * xBRZ (Zenju, GPL-3.0) -- forbidden outright by G-42;
//   * any libretro GPL .slang/.glsl xBR port -- forbidden outright;
//   * a transcription of Hyllian's MIT xBR either. MIT would have PERMITTED
//     a credited port, but a port is not what this is, and claiming one
//     would misstate the provenance in the opposite direction.
//
// This shader implements the IDEA the family shares -- detect which
// diagonal across a 2x2 neighbourhood is the real edge, and interpolate
// along it rather than across it -- with a rule set derived below and
// deliberately SIMPLER than xBR's. Real xBR evaluates a larger neighbour-
// hood with a multi-level weighted rule table; this evaluates one 2x2 with
// a single comparison. The UI label is "xBR-class", never "xBR", exactly
// as §4 requires, and the difference is real rather than cosmetic.
//
// AUTHORSHIP CAVEAT: identical to `chain_crt.wgsl`'s -- written by an LLM
// plausibly trained on GPL shader sources. That is why this file states
// its rule as arithmetic derived in the comments below rather than
// asserting an unverifiable "sources closed", and why the rule is a
// simplification a reviewer can check in full rather than a rule table
// whose provenance would be impossible to audit.
//
// ========================= WHAT IT COMPUTES =========================
//
// For an output fragment landing inside source texel `P`, the three
// neighbours that share its quadrant are fetched -- call them, for a
// fragment in the lower-right quadrant of P:
//
//     P   B          P = the texel we are in
//     C   D          B = right, C = below, D = the diagonal one
//
// The two diagonals across that 2x2 are (P,D) and (B,C). An edge runs
// along whichever diagonal joins the SIMILAR pair:
//
//   * if d(P,D) << d(B,C), P and D are the same surface and B/C are the
//     edge -> interpolate along the P-D diagonal;
//   * if d(B,C) << d(P,D), the mirror case;
//   * if neither dominates, there is no confident edge -> keep P
//     (nearest-neighbour), which is the right default for pixel art: an
//     unnecessary blend is a visible artefact, a missed blend is merely
//     the un-upscaled original.
//
// "<<" is the tunable part: `d_small * (1 + threshold) < d_large`, so
// `threshold` (v0.x) is "how much more different must the other diagonal
// be before I believe this is an edge", 0 meaning any difference at all
// counts and larger values demanding more confidence.
//
// `d` is a luma-weighted colour distance using the Rec. 601 luma
// coefficients (0.299, 0.587, 0.114) -- chosen because edge detection
// should follow PERCEIVED brightness, and two colours differing only in
// blue are far less visibly an edge than two differing in green. The
// weights are the standard published luma coefficients, not anything
// specific to any shader.
//
// `strength` (v0.y) scales how far toward the blended value the result
// moves, so the effect is tunable to nothing (0 = plain nearest).

struct Params {
    v0: vec4<f32>, // x: edge threshold (>= 0), y: blend strength [0,1],
                    // zw: unused.
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

// Luma-weighted colour distance (Rec. 601 coefficients) -- see header for
// why perceived brightness rather than raw RGB distance.
fn colour_distance(a: vec3<f32>, b: vec3<f32>) -> f32 {
    let luma = vec3<f32>(0.299, 0.587, 0.114);
    return dot(abs(a - b), luma);
}

// Fetch source texel at integer coordinates, clamped to the edge so the
// border does not sample garbage.
fn texel(coord: vec2<i32>, size: vec2<i32>) -> vec4<f32> {
    let c = clamp(coord, vec2<i32>(0, 0), size - vec2<i32>(1, 1));
    return textureLoad(source_tex, c, 0);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let uv = in.clip_pos.xy * p.v1.xy;
    let source_size = max(p.v1.zw, vec2<f32>(1.0, 1.0));
    let isize = vec2<i32>(source_size);

    let threshold = max(p.v0.x, 0.0);
    let strength = clamp(p.v0.y, 0.0, 1.0);

    // Which source texel are we in, and which quadrant of it?
    let src_pos = uv * source_size;
    let base = vec2<i32>(floor(src_pos));
    let frac_pos = fract(src_pos);

    // Step toward the neighbours on the side of the texel this fragment
    // is on: right/left and down/up depending on the quadrant. This is
    // what makes the 2x2 the fragment's OWN quadrant rather than a fixed
    // one, and is why the shader upscales rather than merely filtering.
    let step_x = select(-1, 1, frac_pos.x >= 0.5);
    let step_y = select(-1, 1, frac_pos.y >= 0.5);

    let pp = texel(base, isize);                                    // P
    let bb = texel(base + vec2<i32>(step_x, 0), isize);             // B
    let cc = texel(base + vec2<i32>(0, step_y), isize);             // C
    let dd = texel(base + vec2<i32>(step_x, step_y), isize);        // D

    let d_pd = colour_distance(pp.rgb, dd.rgb);
    let d_bc = colour_distance(bb.rgb, cc.rgb);

    // Default: no confident edge, keep P (see header for why nearest is
    // the right default for pixel art).
    var blended = pp;

    if (d_pd * (1.0 + threshold) < d_bc) {
        // P and D are the same surface; the edge runs along that
        // diagonal, so interpolate along it.
        blended = mix(pp, dd, 0.5);
    } else if (d_bc * (1.0 + threshold) < d_pd) {
        // Mirror case: B and C are the same surface.
        blended = mix(bb, cc, 0.5);
    }

    let result = mix(pp, blended, strength);
    return vec4<f32>(result.rgb, result.a);
}
