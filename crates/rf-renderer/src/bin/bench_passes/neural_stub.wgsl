// Stub "neural" compute pass (ticket W16-01; docs/design/ENHANCEMENT_WAVE_16.md
// §7-8). NOT a trained model and NOT a claim about any real network's
// accuracy or output -- a stand-in workload so the benchmark harness has a
// GPU compute pass to time even before W16-02/W16-07/W16-08 land a real
// upscaler. Shape is deliberately similar to a shallow ESRGAN-class block:
// a fixed number of 5x5-neighborhood accumulation passes per output texel,
// at a 4x upscale factor (Real-ESRGAN's own scale) -- "4x conv-like
// workload" per the ticket's acceptance criteria, not an attempt to model
// any specific architecture's FLOP count.

struct Params {
    width: u32,
    height: u32,
    scale: u32,
    _pad: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> src: array<u32>;
@group(0) @binding(2) var<storage, read_write> dst: array<u32>;

fn unpack_rgba(p: u32) -> vec4<f32> {
    return vec4<f32>(
        f32(p & 0xffu),
        f32((p >> 8u) & 0xffu),
        f32((p >> 16u) & 0xffu),
        f32((p >> 24u) & 0xffu),
    ) / 255.0;
}

fn pack_rgba(c: vec4<f32>) -> u32 {
    let cu = vec4<u32>(clamp(c, vec4<f32>(0.0), vec4<f32>(1.0)) * 255.0 + vec4<f32>(0.5));
    return cu.x | (cu.y << 8u) | (cu.z << 16u) | (cu.w << 24u);
}

const TAPS: f32 = 25.0; // 5x5 neighborhood
const CONV_PASSES: u32 = 4u; // fixed workload multiplier -- "4x conv-like"

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let out_w = params.width * params.scale;
    let out_h = params.height * params.scale;
    if (gid.x >= out_w || gid.y >= out_h) {
        return;
    }
    let sx = i32(gid.x / params.scale);
    let sy = i32(gid.y / params.scale);

    var acc = vec4<f32>(0.0);
    for (var p: u32 = 0u; p < CONV_PASSES; p = p + 1u) {
        var sum = vec4<f32>(0.0);
        for (var dy: i32 = -2; dy <= 2; dy = dy + 1) {
            for (var dx: i32 = -2; dx <= 2; dx = dx + 1) {
                let xx = clamp(sx + dx, 0, i32(params.width) - 1);
                let yy = clamp(sy + dy, 0, i32(params.height) - 1);
                let idx = u32(yy) * params.width + u32(xx);
                sum = sum + unpack_rgba(src[idx]) * (1.0 / TAPS);
            }
        }
        acc = acc + sum;
    }
    acc = acc / f32(CONV_PASSES);

    let out_idx = gid.y * out_w + gid.x;
    dst[out_idx] = pack_rgba(acc);
}
