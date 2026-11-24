// Real glow: bright light is shrunk step by step and spread back over the image.

@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var blur: sampler;

struct FullOut {
    @builtin(position) position: vec4f,
    @location(0) uv: vec2f,
}

@vertex
fn vs_full(@builtin(vertex_index) index: u32) -> FullOut {
    let uv = vec2f(f32((index << 1u) & 2u), f32(index & 2u));
    return FullOut(vec4f(uv * 2.0 - 1.0, 0.0, 1.0), uv);
}

fn tap(uv: vec2f, x: f32, y: f32, texel: vec2f) -> vec3f {
    return textureSampleLevel(source, blur, uv + vec2f(x, y) * texel, 0.0).rgb;
}

fn shrink(uv: vec2f) -> vec3f {
    let texel = 1.0 / vec2f(textureDimensions(source));
    let corners = tap(uv, -1.0, -1.0, texel) + tap(uv, 1.0, -1.0, texel) + tap(uv, -1.0, 1.0, texel) + tap(uv, 1.0, 1.0, texel);
    return (tap(uv, 0.0, 0.0, texel) * 4.0 + corners) / 8.0;
}

@fragment
fn fs_bright(in: FullOut) -> @location(0) vec4f {
    let color = min(shrink(vec2f(in.uv.x, 1.0 - in.uv.y)), vec3f(24.0));
    let top = max(color.r, max(color.g, color.b));
    return vec4f(color * smoothstep(0.24, 0.95, top), 1.0);
}

@fragment
fn fs_down(in: FullOut) -> @location(0) vec4f {
    return vec4f(shrink(vec2f(in.uv.x, 1.0 - in.uv.y)), 1.0);
}

@fragment
fn fs_up(in: FullOut) -> @location(0) vec4f {
    let uv = vec2f(in.uv.x, 1.0 - in.uv.y);
    let texel = 0.5 / vec2f(textureDimensions(source));
    let edges = tap(uv, -2.0, 0.0, texel) + tap(uv, 2.0, 0.0, texel) + tap(uv, 0.0, -2.0, texel) + tap(uv, 0.0, 2.0, texel);
    let corners = tap(uv, -1.0, 1.0, texel) + tap(uv, 1.0, 1.0, texel) + tap(uv, 1.0, -1.0, texel) + tap(uv, -1.0, -1.0, texel);
    return vec4f((edges + corners * 2.0) / 12.0, 1.0);
}
