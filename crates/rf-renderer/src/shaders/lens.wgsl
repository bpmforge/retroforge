// HD-2D lens and lighting post pass (ticket W30-04). Provenance: authored
// fresh for this ticket. Bright-pass bloom, a banded blur ("tilt-shift"),
// blurred-alpha drop shadows, a colour tint with an exposure change, and a
// hashed rain/snow overlay are all plain screen-space techniques described
// in general terms; nothing here is ported or transcribed from a specific
// shader. The 5-tap blur uses the standard binomial-derived Gaussian
// weights with bilinear offsets (1.3846153846, 3.2307692308), a published
// arithmetic table, not anyone's code.
//
// Three entry-point families share one bind group layout:
//   fs_down  -- scene (+ sprite layer) -> three small textures:
//               a = scene, b = bright-pass of the scene, c = sprite alpha
//   fs_blur  -- one separable blur step over a, b and c at once
//   fs_final -- full resolution: composes every enabled effect over the
//               untouched scene pixel (textureLoad, so strength 0 for
//               every effect is the identity, byte for byte)
//
// Every loop below has a compile-time bound (CLAUDE.md law 8).

struct Params {
    // x bloom strength, y bloom threshold, z tilt-shift strength,
    // w tilt-shift focus centre (0..1 of the height).
    v0: vec4<f32>,
    // x tilt-shift band height (0..1), y shadow strength,
    // zw shadow offset in UV (down-right).
    v1: vec4<f32>,
    // xyz grade tint, w grade strength.
    v2: vec4<f32>,
    // x grade exposure, y weather kind (0 none, 1 rain, 2 snow),
    // z weather density, w frame counter (as f32).
    v3: vec4<f32>,
    // xy 1/full size, z aspect (w/h), w sprite layer present (0 or 1).
    v4: vec4<f32>,
    // xy 1/small size, zw UV size of one small texel (down-sample footprint).
    v5: vec4<f32>,
    // xy UV step of the blur taps (one small texel along the axis).
    v6: vec4<f32>,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var tex_a: texture_2d<f32>;
@group(0) @binding(2) var tex_b: texture_2d<f32>;
@group(0) @binding(3) var tex_c: texture_2d<f32>;
@group(0) @binding(4) var tex_d: texture_2d<f32>;
@group(0) @binding(5) var tex_e: texture_2d<f32>;
@group(0) @binding(6) var samp: sampler;

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

struct Triple {
    @location(0) a: vec4<f32>,
    @location(1) b: vec4<f32>,
    @location(2) c: vec4<f32>,
}

// ---- down-sample + bright-pass -------------------------------------

// 4x4 taps spread over the footprint of one small texel; each tap is a
// bilinear fetch, so at 4x4 taps over an 8x8 pixel footprint every source
// pixel is counted once.
@fragment
fn fs_down(in: VsOut) -> Triple {
    let uv = in.clip_pos.xy * p.v5.xy;
    let thr = clamp(p.v0.y, 0.0, 0.99);
    var acc_s = vec3<f32>(0.0);
    var acc_b = vec3<f32>(0.0);
    var acc_a = 0.0;
    for (var iy = 0; iy < 4; iy = iy + 1) {
        for (var ix = 0; ix < 4; ix = ix + 1) {
            let o = (vec2<f32>(f32(ix), f32(iy)) + vec2<f32>(0.5)) / 4.0 - vec2<f32>(0.5);
            let suv = uv + o * p.v5.zw;
            let s = textureSampleLevel(tex_a, samp, suv, 0.0).rgb;
            let luma = dot(s, vec3<f32>(0.2126, 0.7152, 0.0722));
            let k = clamp((luma - thr) / max(1.0 - thr, 0.001), 0.0, 1.0);
            acc_s = acc_s + s;
            acc_b = acc_b + s * k;
            if (p.v4.w > 0.5) {
                acc_a = acc_a + textureSampleLevel(tex_b, samp, suv, 0.0).a;
            }
        }
    }
    var out: Triple;
    out.a = vec4<f32>(acc_s / 16.0, 1.0);
    out.b = vec4<f32>(acc_b / 16.0, 1.0);
    out.c = vec4<f32>(acc_a / 16.0, 0.0, 0.0, 1.0);
    return out;
}

// ---- separable blur -------------------------------------------------

fn blur5(tex: texture_2d<f32>, uv: vec2<f32>) -> vec4<f32> {
    let step = p.v6.xy;
    var c = textureSampleLevel(tex, samp, uv, 0.0) * 0.2270270270;
    c = c + (textureSampleLevel(tex, samp, uv + step * 1.3846153846, 0.0)
        + textureSampleLevel(tex, samp, uv - step * 1.3846153846, 0.0)) * 0.3162162162;
    c = c + (textureSampleLevel(tex, samp, uv + step * 3.2307692308, 0.0)
        + textureSampleLevel(tex, samp, uv - step * 3.2307692308, 0.0)) * 0.0702702703;
    return c;
}

@fragment
fn fs_blur(in: VsOut) -> Triple {
    let uv = in.clip_pos.xy * p.v5.xy;
    var out: Triple;
    out.a = blur5(tex_a, uv);
    out.b = blur5(tex_b, uv);
    out.c = blur5(tex_c, uv);
    return out;
}

// ---- weather ---------------------------------------------------------

// PCG integer hash: identical results on every backend, unlike the usual
// sin-based hashes, so the overlay is reproducible from the frame counter.
fn pcg(v: u32) -> u32 {
    let s = v * 747796405u + 2891336453u;
    let w = ((s >> ((s >> 28u) + 4u)) ^ s) * 277803737u;
    return (w >> 22u) ^ w;
}

fn hash2(a: i32, b: i32) -> f32 {
    let h = pcg(bitcast<u32>(a) ^ pcg(bitcast<u32>(b) + 0x9e3779b9u));
    return f32(h >> 8u) / 16777215.0;
}

// Slanted falling streaks: three depth layers, each a set of thin columns,
// a column is "raining" when its hash is under the density.
fn rain_mask(q: vec2<f32>, density: f32, frame: f32) -> f32 {
    var a = 0.0;
    for (var l = 0; l < 3; l = l + 1) {
        let fl = f32(l);
        let cols = 70.0 + 45.0 * fl;
        let x = (q.x + q.y * 0.18) * cols;
        let cx = i32(floor(x));
        let fx = x - floor(x);
        let h = hash2(cx, l);
        if (hash2(cx, l + 10) < density * 0.45) {
            let speed = 0.035 + 0.02 * h + 0.012 * fl;
            let y = fract(q.y * 0.5 + h * 7.0 - frame * speed);
            let len = 0.10;
            if (y < len) {
                let width = 1.0 - smoothstep(0.08, 0.22, abs(fx - 0.5));
                let tail = 1.0 - y / len;
                a = max(a, width * tail * (0.6 - 0.12 * fl));
            }
        }
    }
    return a;
}

// Soft round flakes drifting down with a gentle sideways sway.
fn snow_mask(q: vec2<f32>, density: f32, frame: f32) -> f32 {
    var a = 0.0;
    for (var l = 0; l < 3; l = l + 1) {
        let fl = f32(l);
        let cells = 22.0 + 16.0 * fl;
        let fall = frame * (0.0035 + 0.0025 * fl);
        let pos = vec2<f32>(q.x * cells, (q.y - fall) * cells);
        let cell = floor(pos);
        let f = pos - cell;
        let cxi = i32(cell.x);
        let cyi = i32(cell.y);
        let h = hash2(cxi, cyi + l * 131);
        if (h < density * 0.35) {
            let sway = 0.3 * sin(frame * 0.03 + h * 40.0);
            let centre = vec2<f32>(
                0.5 + sway,
                0.5 + 0.3 * (hash2(cxi + 7, cyi + 3) * 2.0 - 1.0),
            );
            let r = 0.2 - 0.03 * fl;
            let d = length(f - centre);
            a = max(a, (1.0 - smoothstep(r * 0.4, r, d)) * (0.9 - 0.15 * fl));
        }
    }
    return a;
}

// ---- final composite -------------------------------------------------

@fragment
fn fs_final(in: VsOut) -> @location(0) vec4<f32> {
    let pix = vec2<i32>(in.clip_pos.xy);
    let uv = in.clip_pos.xy * p.v4.xy;
    let scene = textureLoad(tex_a, pix, 0);
    var rgb = scene.rgb;

    // Soft sprite shadow: the blurred sprite alpha, looked up from up-left
    // so the shadow falls down-right, darkening the scene only where the
    // sprite itself is not.
    let shadow_s = clamp(p.v1.y, 0.0, 1.0);
    if (shadow_s > 0.0 && p.v4.w > 0.5) {
        let sa = textureSampleLevel(tex_e, samp, uv - p.v1.zw, 0.0).r;
        let here = textureSampleLevel(tex_b, samp, uv, 0.0).a;
        rgb = rgb * (1.0 - clamp(sa * shadow_s * 0.7, 0.0, 1.0) * (1.0 - here));
    }

    // Tilt-shift: the blur fades in with the distance from the focus band.
    let tilt_s = clamp(p.v0.z, 0.0, 1.0);
    if (tilt_s > 0.0) {
        let d = abs(uv.y - p.v0.w);
        let w = smoothstep(0.0, 0.35, max(d - p.v1.x * 0.5, 0.0));
        rgb = mix(rgb, textureSampleLevel(tex_c, samp, uv, 0.0).rgb, w * tilt_s);
    }

    // Bloom: add the blurred bright-pass.
    let bloom_s = clamp(p.v0.x, 0.0, 1.0);
    if (bloom_s > 0.0) {
        rgb = rgb + textureSampleLevel(tex_d, samp, uv, 0.0).rgb * (bloom_s * 1.6);
    }

    // Day/night grading: tint and exposure from the CPU's gradient table.
    let grade_s = clamp(p.v2.w, 0.0, 1.0);
    if (grade_s > 0.0) {
        rgb = rgb * mix(vec3<f32>(1.0), p.v2.xyz, grade_s) * mix(1.0, p.v3.x, grade_s);
    }

    // Weather, in aspect-corrected screen space so it looks the same at
    // every output size.
    let kind = i32(p.v3.y + 0.5);
    let density = clamp(p.v3.z, 0.0, 1.0);
    if (density > 0.0 && kind != 0) {
        let q = vec2<f32>(uv.x * p.v4.z, uv.y);
        if (kind == 1) {
            rgb = mix(rgb, vec3<f32>(0.78, 0.86, 1.0), rain_mask(q, density, p.v3.w));
        } else {
            rgb = mix(rgb, vec3<f32>(1.0), snow_mask(q, density, p.v3.w));
        }
    }

    return vec4<f32>(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)), scene.a);
}
