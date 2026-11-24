// The last step: light is fitted to what this screen can show, and the crisp cover goes on top.

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var first: texture_2d<f32>;
@group(0) @binding(2) var second: texture_2d<f32>;
@group(0) @binding(3) var blur_sampler: sampler;

const GLOW: f32 = 0.85;
const KNEE: f32 = 0.62;
const CORNER: f32 = 0.028;

/// Keeps normal brightness as it is and rolls highlights into the screen's real peak.
fn shoulder(value: f32, peak: f32) -> f32 {
    if value <= KNEE {
        return value;
    }
    let room = peak - KNEE;
    return KNEE + room * (1.0 - exp(-(value - KNEE) / room));
}

fn tone(color: vec3f, peak: f32) -> vec3f {
    let top = max(color.r, max(color.g, color.b));
    if top <= 0.00001 {
        return vec3f(0.0);
    }
    let fitted = shoulder(top, peak);
    let white = smoothstep(peak * 0.85, peak * 3.0, top) * 0.8;
    return mix(color * (fitted / top), vec3f(fitted), white);
}

@fragment
fn fs_composite(in: FullOut) -> @location(0) vec4f {
    let uv = vec2f(in.uv.x, 1.0 - in.uv.y);
    let light = textureSampleLevel(first, blur_sampler, uv, 0.0);
    let scene = light.rgb;
    // The scene's alpha is one, except where the letters of a word are cut out of a bright picture.
    let kept = light.a;
    let glow = textureSampleLevel(second, blur_sampler, uv, 0.0).rgb;
    let halo = GLOW * (1.0 - 0.72 * params.card) * (1.0 - 0.75 * params.warp) * (1.0 + 0.16 * params.chorus);
    // Only the finished galaxy uses extra HDR brightness. Hyperspace fills the whole screen.
    let formed = smoothstep(0.55, 1.0, params.twist) * (1.0 - smoothstep(0.0, 0.25, params.warp));
    let peak = mix(1.0, max(params.headroom, 1.0), formed);
    let toned = tone(max(scene + glow * halo, vec3f(0.0)) * kept, peak);
    return vec4f(toned * params.fade, params.fade);
}

struct CardOut {
    @builtin(position) position: vec4f,
    @location(0) uv: vec2f,
}

@vertex
fn vs_card(@builtin(vertex_index) index: u32) -> CardOut {
    var corners = array<vec2f, 4>(vec2f(-1.0, -1.0), vec2f(1.0, -1.0), vec2f(-1.0, 1.0), vec2f(1.0, 1.0));
    let corner = corners[index];
    let world = vec3f(corner.x * CARD, 0.0, corner.y * CARD);
    let view = look_at(to_view(world, params.tilt, params.roll), vec2f(params.look_x, params.look_y));
    let clip = vec2f(view.x * FOCAL / params.aspect, view.y * FOCAL);
    return CardOut(vec4f(clip, 0.0, view.z), vec2f(corner.x * 0.5 + 0.5, 0.5 - corner.y * 0.5));
}

@fragment
fn fs_card(in: CardOut) -> @location(0) vec4f {
    let old_color = textureSampleLevel(first, blur_sampler, in.uv, 0.0).rgb;
    let new_color = textureSampleLevel(second, blur_sampler, in.uv, 0.0).rgb;
    let inside = abs(in.uv - 0.5) - vec2f(0.5 - CORNER);
    let edge = length(max(inside, vec2f(0.0))) - CORNER;
    let alpha = params.card * params.fade * smoothstep(0.0015, -0.0015, edge);
    return vec4f(mix(old_color, new_color, params.blend) * alpha, alpha);
}
