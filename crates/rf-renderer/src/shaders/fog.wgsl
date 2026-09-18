// Fog/steam post pass (ticket W16-04; docs/design/ENHANCEMENT_WAVE_16.md
// §4/§8). Provenance: authored fresh for this ticket, not ported or
// transcribed from any existing shader -- multi-octave scrolled sampling
// of a density texture with soft accumulation is a standard screen-space
// technique description, not a specific implementation this copies (same
// "arithmetic nobody owns, WGSL text is still original" posture
// `crate::shader_chain`'s module doc states for `nearest`/`scanlines`).
//
// Unlike `crate::shader_chain`'s common one-texture-in interface (RENDERER.md
// §4), this pass reads TWO textures: the already-composited scene (so the
// atmosphere plane's own pixels/mask are still present underneath,
// acceptance criterion 1's "the original plane's mask is kept underneath")
// and a density map built from `SceneLayer::ExtractedBg`'s own pixels
// (`rf_enhance::atmosphere`'s producer) -- resolved to a single density
// channel by the app shell (`rf-renderer` may not depend on `rf-enhance`,
// ARCHITECTURE.md §3; `crates/retroforge/src/enhanced_view.rs` does that
// conversion). This pass never replaces a scene pixel, only blends a fog
// tint over it -- the scene's own alpha/silhouette survives byte-for-byte.

struct Params {
    // x: time in seconds (drives drift). y,z: scroll drift per second,
    // world-space UV units (the atmosphere layer's own scroll telemetry,
    // scaled by the caller). w: overall fog strength/opacity in [0, 1].
    v0: vec4<f32>,
    // x: octave-2 scale, y: octave-3 scale (spatial frequency multipliers
    // relative to the base UV -- RENDERER.md-style "small const number of
    // octaves", never a runtime-sized loop). z: 1/out_width, w: 1/out_height.
    v1: vec4<f32>,
}

@group(0) @binding(0)
var<uniform> p: Params;

@group(0) @binding(1)
var scene_tex: texture_2d<f32>;

@group(0) @binding(2)
var density_tex: texture_2d<f32>;

@group(0) @binding(3)
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

// Fixed at three: the density signal's own low resolution (an 8x8-tile,
// few-distinct-pattern plane per `rf_enhance::atmosphere`'s module doc)
// gives diminishing returns past a handful of octaves, and a compile-time
// constant keeps this a fixed-cost pass under the frame-budget gate
// (law 8: no growth path here at all, host or shader side).
const OCTAVES: u32 = 3;

fn sample_density(uv: vec2<f32>, time: f32, drift: vec2<f32>, scale2: f32, scale3: f32) -> f32 {
    // Octave 1: base frequency, drifting at the caller's own rate.
    let uv1 = uv + drift * time;
    // Octave 2: higher frequency, drifting at a different rate/direction
    // so the accumulated result does not look like one plane just sliding
    // (the "drifting, volumetric-style" look acceptance criterion 1 asks
    // for) -- constants below are fixed multipliers, not measured against
    // any commercial title (same calibration stance
    // `rf_enhance::atmosphere`'s module doc takes for its own thresholds).
    let uv2 = uv * scale2 + drift * (time * -0.6) + vec2<f32>(0.37, 0.11);
    let uv3 = uv * scale3 + drift * (time * 0.35) + vec2<f32>(-0.21, 0.29);

    let d1 = textureSample(density_tex, samp, fract(uv1)).r;
    let d2 = textureSample(density_tex, samp, fract(uv2)).r;
    let d3 = textureSample(density_tex, samp, fract(uv3)).r;

    // Soft accumulation: weighted average (weights sum to 1) rather than a
    // max/sum that could blow past 1.0 and clip hard -- "soft" is the
    // acceptance wording, and a weighted blend is the plain reading of it.
    return d1 * 0.5 + d2 * 0.3 + d3 * 0.2;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let uv = vec2<f32>(
        in.clip_pos.x * p.v1.z,
        in.clip_pos.y * p.v1.w,
    );

    let scene = textureSample(scene_tex, samp, uv);

    let time = p.v0.x;
    let drift = p.v0.yz;
    let strength = clamp(p.v0.w, 0.0, 1.0);
    let scale2 = select(2.3, p.v1.x, p.v1.x > 0.0);
    let scale3 = select(4.1, p.v1.y, p.v1.y > 0.0);

    let density = clamp(sample_density(uv, time, drift, scale2, scale3), 0.0, 1.0);

    // A cool, slightly-blue haze tint -- a plain, unbranded fog colour
    // choice (no game's specific fog palette is referenced), scaled by
    // both the density map and the overall strength knob.
    let fog_tint = vec3<f32>(0.80, 0.85, 0.92);
    let fog_alpha = density * strength;

    // Blend only the colour; the scene's own alpha is untouched, so
    // whatever silhouette/mask the compositor already produced for this
    // pixel is preserved exactly (acceptance criterion 1).
    let out_rgb = mix(scene.rgb, fog_tint, fog_alpha);
    return vec4<f32>(out_rgb, scene.a);
}
