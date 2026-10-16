// Super FX polygon stream redrawn as GPU triangles (ticket W31-05).
// Provenance: authored fresh for this ticket. A flat-colour triangle pass:
// the vertex stage scales native-pixel coordinates to the render target, the
// fragment stage picks one of two pre-resolved colours by the parity of the
// NATIVE pixel it lies in (the GSU's PLOT dither rule) and discards
// transparent ones. No texture, no depth: painter order is draw order.

struct Params {
    // x native viewport width, y native viewport height, z scale (target
    // pixels per native pixel), w unused.
    v: vec4<f32>,
}

@group(0) @binding(0) var<uniform> p: Params;

struct VsIn {
    // Position in native pixels, y down, origin at the viewport's top-left.
    @location(0) pos: vec2<f32>,
    // Packed RGBA8 (r in the low byte). alpha 0 = this parity draws nothing.
    @location(1) even_rgba: u32,
    @location(2) odd_rgba: u32,
}

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) @interpolate(flat) even_rgba: vec4<f32>,
    @location(1) @interpolate(flat) odd_rgba: vec4<f32>,
}

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    let ndc = vec2<f32>(
        in.pos.x / p.v.x * 2.0 - 1.0,
        1.0 - in.pos.y / p.v.y * 2.0,
    );
    out.clip = vec4<f32>(ndc, 0.0, 1.0);
    out.even_rgba = unpack4x8unorm(in.even_rgba);
    out.odd_rgba = unpack4x8unorm(in.odd_rgba);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // `position` is the pixel centre in target pixels; the native pixel is
    // that divided by the scale. `x ^ y` odd selects the high-nibble colour.
    let native = vec2<u32>(floor(in.clip.xy / p.v.z));
    let odd = ((native.x ^ native.y) & 1u) == 1u;
    let c = select(in.even_rgba, in.odd_rgba, odd);
    if (c.a < 0.5) {
        discard;
    }
    return vec4<f32>(c.rgb, 1.0);
}
