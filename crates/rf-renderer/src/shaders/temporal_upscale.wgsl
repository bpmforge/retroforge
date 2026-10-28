// Portable temporal upscaler (ticket W39-05; docs in src/temporal.rs).
// Provenance: authored fresh for this ticket. "Reproject the history with the
// motion vector, clamp it to the current neighbourhood, blend" is the
// textbook TAA / TAAU resolve (arithmetic nobody owns); the weights are a
// plain Gaussian over the 3x3 input texels around the jittered sample.
//
// One thread per OUTPUT pixel. Inputs are the low-resolution jittered colour,
// its depth (0 near .. 1 far) and its motion (input pixels, pixel -> where it
// was last frame). The history lives at output resolution in Rgba16Float and
// is stored in the UNJITTERED output pixel grid.

struct U {
    in_size: vec2<f32>,
    out_size: vec2<f32>,
    // Sub-pixel offset the frame was rendered with, input pixels, y down: a
    // surface point at true position P was drawn at P + jitter.
    jitter: vec2<f32>,
    blend: f32,  // weight of the current frame once history is valid
    reset: f32,  // 1: ignore history
}

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var color_tex: texture_2d<f32>;
@group(0) @binding(2) var depth_tex: texture_depth_2d;
@group(0) @binding(3) var motion_tex: texture_2d<f32>;
@group(0) @binding(4) var history_tex: texture_2d<f32>;
@group(0) @binding(5) var samp: sampler;
@group(0) @binding(6) var out_color: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(7) var out_history: texture_storage_2d<rgba16float, write>;

// Gaussian variance over input texels (measured: a wider kernel is visibly softer than a bilinear upscale of the same frame)
// neighbours fill in between the jitter samples).
const SIGMA2: f32 = 0.08;
// History weight is reduced toward this blend when the pixel moves fast.
const FAST_BLEND: f32 = 0.5;
const FAST_PIXELS: f32 = 4.0;

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= u32(u.out_size.x) || gid.y >= u32(u.out_size.y)) {
        return;
    }
    let uv = (vec2<f32>(gid.xy) + 0.5) / u.out_size;
    // Where this output pixel's true position landed in the jittered frame.
    let lr = uv * u.in_size + u.jitter;
    let centre = vec2<i32>(floor(lr));
    let hi = vec2<i32>(u.in_size) - vec2<i32>(1, 1);

    var sum = vec3<f32>(0.0);
    var wsum = 0.0;
    var mn = vec3<f32>(1.0e9);
    var mx = vec3<f32>(-1.0e9);
    var nearest_depth = 2.0;
    var nearest = clamp(centre, vec2<i32>(0, 0), hi);
    for (var j = -1; j <= 1; j = j + 1) {
        for (var i = -1; i <= 1; i = i + 1) {
            let t = clamp(centre + vec2<i32>(i, j), vec2<i32>(0, 0), hi);
            let c = clamp(textureLoad(color_tex, t, 0).rgb, vec3<f32>(0.0), vec3<f32>(1.0));
            let d = vec2<f32>(t) + vec2<f32>(0.5) - lr;
            let w = exp(-dot(d, d) / (2.0 * SIGMA2));
            sum = sum + c * w;
            wsum = wsum + w;
            mn = min(mn, c);
            mx = max(mx, c);
            let z = textureLoad(depth_tex, t, 0);
            if (z < nearest_depth) {
                nearest_depth = z;
                nearest = t;
            }
        }
    }
    let cur = sum / max(wsum, 1.0e-6);

    // Motion of the nearest surface in the neighbourhood (keeps a foreground
    // edge's motion on its own pixels), sanitised against inf/NaN.
    var m = textureLoad(motion_tex, nearest, 0).xy;
    if (!(abs(m.x) < 1.0e4 && abs(m.y) < 1.0e4)) {
        m = vec2<f32>(0.0);
    }
    let hist_uv = uv + m / u.in_size;
    let inside = hist_uv.x >= 0.0 && hist_uv.x <= 1.0 && hist_uv.y >= 0.0 && hist_uv.y <= 1.0;

    var result = cur;
    if (u.reset < 0.5 && inside) {
        let h = textureSampleLevel(history_tex, samp, hist_uv, 0.0).rgb;
        let hc = clamp(h, mn, mx);
        let a = mix(u.blend, FAST_BLEND, clamp(length(m) / FAST_PIXELS, 0.0, 1.0));
        result = mix(hc, cur, a);
    }
    result = clamp(result, vec3<f32>(0.0), vec3<f32>(1.0));
    let p = vec2<i32>(gid.xy);
    textureStore(out_history, p, vec4<f32>(result, 1.0));
    textureStore(out_color, p, vec4<f32>(result, 1.0));
}
