const TAU: f32 = 6.2831853;
const PI: f32 = 3.1415927;
// The camera looks at the galaxy's center from this far away.
const DISTANCE: f32 = 3.95;
const FOCAL: f32 = 1.86;
// Half the side of the cover in world units: it fills 62% of the screen height.
const CARD: f32 = 1.317;
const NEAR: f32 = 0.22;
const TUNNEL: f32 = 16.0;
const TEXT_DEPTH: f32 = 3.2;

struct Params {
    time: f32, voice: f32, fade: f32, lyric: f32,
    twist: f32, tilt: f32, roll: f32, warp: f32,
    flight: f32, speed: f32, card: f32, collapse: f32,
    energy: f32, spin: f32, blend: f32, breath: f32,
    arms: f32, winding: f32, hand: f32, gather: f32,
    extent: f32, falloff: f32, thickness: f32, chorus: f32,
    aspect: f32, pixel: f32, look_x: f32, look_y: f32,
    headroom: f32, shock: f32, clip: f32, clip_blend: f32,
    // Seconds since the clip's last cut and how hard it was, whether frames are still coming,
    // and how much of a grain's heat survives this frame.
    cut_age: f32, cut_strength: f32, motion_gate: f32, heat_keep: f32,
    // How much the picture of the clip is lifted, for dark scenes to show, and the seconds since the last frame.
    exposure: f32, frame_time: f32, unused_b: f32, unused_c: f32,
    color_a: vec4f,
    color_b: vec4f,
}

struct Star {
    view: vec3f,
    size: f32,
    color: vec3f,
    trail: f32,
}

/// One word of the lyrics on screen.
struct Word {
    at: vec2f,
    scale: f32,
    turn: f32,
    gather: f32,
    hot: f32,
    depth: f32,
    count: f32,
    size: f32,
    half_width: f32,
    leaving: f32,
    dust: f32,
}

struct FullOut {
    @builtin(position) position: vec4f,
    @location(0) uv: vec2f,
}

@vertex
fn vs_full(@builtin(vertex_index) index: u32) -> FullOut {
    let uv = vec2f(f32((index << 1u) & 2u), f32(index & 2u));
    return FullOut(vec4f(uv * 2.0 - 1.0, 0.0, 1.0), uv);
}

fn home(index: u32) -> vec2f {
    let cell = vec2f(f32(index % GRID) + 0.5, f32(index / GRID) + 0.5);
    return cell / f32(GRID) * 2.0 - 1.0;
}

fn random(index: u32) -> vec2f {
    var value = index * 2654435761u;
    value ^= value >> 13u;
    value *= 1274126177u;
    value ^= value >> 16u;
    return vec2f(f32(value & 0xffffu), f32(value >> 16u)) / 65535.0;
}

/// The chorus wave: a soft ring that rolls from the core out to the rim and dies away.
fn shock_wave(resting: f32, shock: f32, extent: f32) -> f32 {
    if shock <= 0.0 {
        return 0.0;
    }
    let front = (1.0 - (1.0 - shock) * (1.0 - shock)) * (extent + 0.7);
    let away = (resting - front) / (0.22 + 0.42 * shock);
    return exp(-away * away) * pow(1.0 - shock, 1.4) * smoothstep(0.0, 0.24, shock);
}

fn ease(value: f32) -> f32 {
    let t = clamp(value, 0.0, 1.0);
    return t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
}

fn to_view(world: vec3f, tilt: f32, roll: f32) -> vec3f {
    let cp = cos(tilt);
    let sp = sin(tilt);
    let tilted = vec3f(world.x, cp * world.y + sp * world.z, -sp * world.y + cp * world.z + DISTANCE);
    let cr = cos(roll);
    let sr = sin(roll);
    return vec3f(cr * tilted.x - sr * tilted.y, sr * tilted.x + cr * tilted.y, tilted.z);
}

/// Turns the scene around its center, so near stars shift more than far ones.
fn look_at(view: vec3f, look: vec2f) -> vec3f {
    let p = view - vec3f(0.0, 0.0, DISTANCE);
    let cx = cos(look.x);
    let sx = sin(look.x);
    let a = vec3f(cx * p.x + sx * p.z, p.y, -sx * p.x + cx * p.z);
    let cy = cos(look.y);
    let sy = sin(look.y);
    let b = vec3f(a.x, cy * a.y - sy * a.z, sy * a.y + cy * a.z);
    return b + vec3f(0.0, 0.0, DISTANCE);
}

fn unlook(view: vec3f, look: vec2f) -> vec3f {
    let p = view - vec3f(0.0, 0.0, DISTANCE);
    let cy = cos(look.y);
    let sy = sin(look.y);
    let a = vec3f(p.x, cy * p.y + sy * p.z, -sy * p.y + cy * p.z);
    let cx = cos(look.x);
    let sx = sin(look.x);
    let b = vec3f(cx * a.x - sx * a.z, a.y, sx * a.x + cx * a.z);
    return b + vec3f(0.0, 0.0, DISTANCE);
}
