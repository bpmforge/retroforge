// Shader chain, "CRT-class" pass (ticket W3-02a; docs/design/RENDERER.md
// §4, design review G-42).
//
// ============================ PROVENANCE ============================
//
// Licence: MIT OR Apache-2.0 (this file is original RetroForge work).
// Basis: NONE. Not based on, ported from, or transcribed from any
// existing shader -- specifically NOT crt-easymode, NOT any libretro
// GPL shader, NOT any RetroArch .slang/.glsl preset. RENDERER.md §4
// classes the CRT look as **behaviour-spec clean-room**, and the UI label
// is "CRT-class", never a claim to BE crt-easymode.
//
// AUTHORSHIP CAVEAT, recorded deliberately (ticket W3-02a, Brad's ruling
// 2026-08-17): this WGSL was written by an LLM which may have been trained
// on GPL-licensed shader sources. "Did not have the sources open" -- G-42's
// literal test -- does not mean for such an author what it means for a
// human, so instead of asserting the test is met, this file DERIVES every
// term below from stated physical/mathematical reasoning that a reviewer
// can check line by line. If any expression here looks like a transcription
// rather than a derivation, that is a bug in the provenance and should be
// raised. The derivations are deliberately simple and generic -- gaussian
// falloff, a periodic mask, a gamma round-trip -- precisely so that
// "someone derived this" is a credible reading of the text.
//
// ========================= WHAT IT COMPUTES =========================
//
// Three independent, individually-motivated effects, in the order the
// physical CRT applies them:
//
// 1. BEAM PROFILE (vertical). A CRT's electron beam paints a line whose
//    brightness falls off away from the line centre. Modelled as a
//    gaussian in the distance from the scanline centre:
//        w(d) = exp(-(d/sigma)^2)
//    where d is the fractional distance of this output row from the
//    centre of the source row it belongs to, in source-pixel units.
//    Gaussian because that is the standard model for a focused beam's
//    cross-section; `sigma` (v0.x) is the focus knob -- small sigma is a
//    tight, obviously-scanlined beam, large sigma washes the scanlines
//    out.
//
// 2. APERTURE MASK (horizontal). A colour CRT's shadow mask / aperture
//    grille lets each phosphor stripe be lit by only one gun, so adjacent
//    output columns favour R, G and B in turn. Modelled as a per-column
//    multiplier selecting one of three tints by `column mod 3`, blended
//    toward white by (1 - strength) so the effect is tunable to nothing.
//    v0.y is the strength.
//
// 3. GAMMA. Both effects above are multiplicative on light, but the
//    incoming texture is display-referred (roughly gamma-encoded), so
//    multiplying it directly darkens more than physics says. The pass
//    therefore decodes with pow(c, gamma), applies the multipliers in
//    linear light, and re-encodes with pow(c, 1/gamma). v0.z is gamma.
//    Without this round-trip a 50% beam weight looks like ~22% brightness
//    rather than 50%; with it, the arithmetic means what it says.

struct Params {
    v0: vec4<f32>, // x: beam sigma (focus), y: mask strength [0,1],
                    // z: gamma, w: unused.
    v1: vec4<f32>, // xy: 1/out_width, 1/out_height.
                    // zw: source width, source height in pixels (the beam
                    // falls off across a SOURCE scanline, so the pass needs
                    // to know how many output rows one source row spans).
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

// The three aperture-grille tints, one per phosphor stripe. Full strength
// is a pure primary; `mask_strength` blends toward white (1,1,1).
fn mask_tint(column: u32, strength: f32) -> vec3<f32> {
    var tint: vec3<f32>;
    let phase = column % 3u;
    if (phase == 0u) {
        tint = vec3<f32>(1.0, 0.0, 0.0);
    } else if (phase == 1u) {
        tint = vec3<f32>(0.0, 1.0, 0.0);
    } else {
        tint = vec3<f32>(0.0, 0.0, 1.0);
    }
    return mix(vec3<f32>(1.0, 1.0, 1.0), tint, clamp(strength, 0.0, 1.0));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let uv = in.clip_pos.xy * p.v1.xy;
    let color = textureSample(source_tex, samp, uv);

    let sigma = max(p.v0.x, 0.0001);
    let mask_strength = clamp(p.v0.y, 0.0, 1.0);
    let gamma = max(p.v0.z, 0.0001);
    let source_height = max(p.v1.w, 1.0);

    // (1) Beam profile. `uv.y * source_height` is this fragment's position
    // in SOURCE-pixel units; its fractional part says where inside the
    // source row we are. Distance from the row's centre (0.5) is what the
    // beam falls off with.
    let src_y = uv.y * source_height;
    let dist_from_centre = fract(src_y) - 0.5;
    let t = dist_from_centre / sigma;
    let beam = exp(-(t * t));

    // (2) Aperture mask, by output column.
    let tint = mask_tint(u32(in.clip_pos.x), mask_strength);

    // (3) Gamma round-trip: decode, apply in linear light, re-encode.
    let linear = pow(max(color.rgb, vec3<f32>(0.0)), vec3<f32>(gamma));
    let lit = linear * beam * tint;
    let encoded = pow(max(lit, vec3<f32>(0.0)), vec3<f32>(1.0 / gamma));

    return vec4<f32>(encoded, color.a);
}
