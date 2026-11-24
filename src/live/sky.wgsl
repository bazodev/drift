// Deep space behind the stars: faint color, distant stars, and glowing gas along the arms.

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> wave: array<f32>;

fn hash(p: vec2f) -> f32 {
    return fract(sin(dot(p, vec2f(127.1, 311.7))) * 43758.5453);
}

fn noise(p: vec2f) -> f32 {
    let cell = floor(p);
    let part = fract(p);
    let curve = part * part * (3.0 - 2.0 * part);
    return mix(
        mix(hash(cell), hash(cell + vec2f(1.0, 0.0)), curve.x),
        mix(hash(cell + vec2f(0.0, 1.0)), hash(cell + vec2f(1.0, 1.0)), curve.x),
        curve.y,
    );
}

fn soft(p: vec2f) -> f32 {
    return (noise(p) + 0.5 * noise(p * 1.9 + vec2f(5.2, 1.3))) / 1.5;
}

fn ripple(position: f32) -> f32 {
    let at = clamp(position, 0.0, 1.0) * f32(WAVE - 1u);
    let index = u32(at);
    let next = min(index + 1u, WAVE - 1u);
    return mix(wave[index], wave[next], fract(at));
}

fn distant_stars(p: vec2f, density: f32) -> f32 {
    let scaled = p * density;
    let cell = floor(scaled);
    let seed = hash(cell);
    if seed < 0.9 {
        return 0.0;
    }
    let spot = vec2f(hash(cell + 3.1), hash(cell + 7.7)) * 0.6 + 0.2;
    let twinkle = 0.62 + 0.38 * sin(params.time * (0.4 + seed * 1.5) + seed * 40.0);
    let brightness = (seed - 0.9) / 0.1;
    return smoothstep(0.17, 0.0, length(scaled - cell - spot)) * brightness * brightness * twinkle;
}

/// The point of the galaxy's plane seen through this screen position.
fn on_disc(ndc: vec2f, look: vec2f) -> vec2f {
    let origin = unlook(vec3f(0.0), look);
    let ahead = unlook(vec3f(ndc * vec2f(params.aspect, 1.0) / FOCAL, 1.0), look) - origin;
    let cr = cos(params.roll);
    let sr = sin(params.roll);
    let o = vec3f(cr * origin.x + sr * origin.y, -sr * origin.x + cr * origin.y, origin.z);
    let d = vec3f(cr * ahead.x + sr * ahead.y, -sr * ahead.x + cr * ahead.y, ahead.z);
    let cp = cos(params.tilt);
    let sp = sin(params.tilt);
    let facing = cp * d.y - sp * d.z;
    if abs(facing) < 0.0005 {
        return vec2f(1000.0);
    }
    let t = (sp * (o.z - DISTANCE) - cp * o.y) / facing;
    if t < 0.0 {
        return vec2f(1000.0);
    }
    let p = o + d * t;
    return vec2f(p.x, sp * p.y + cp * (p.z - DISTANCE));
}

@fragment
fn fs_sky(in: FullOut) -> @location(0) vec4f {
    let ndc = in.uv * 2.0 - 1.0;
    let p = vec2f((in.uv.x - 0.5) * params.aspect, in.uv.y - 0.5);
    let t = params.time * 0.035;
    let look = vec2f(params.look_x, params.look_y);

    let cloud = smoothstep(0.34, 0.76, soft(p * 2.0 + vec2f(t, -t * 0.7)));
    let sky = mix(params.color_a.rgb, params.color_b.rgb, soft(p * 1.25 - vec2f(t * 0.6, t)));
    var color = vec3f(0.0012, 0.0014, 0.0028) + sky * cloud * cloud * (0.006 + 0.01 * params.energy);
    color += vec3f(0.5, 0.56, 0.7) * distant_stars(p + look * 0.012, 150.0) * 0.16;
    color += vec3f(0.62, 0.62, 0.66) * distant_stars(p * 1.7 + look * 0.03 + 9.0, 190.0) * 0.07;

    let hit = on_disc(ndc, look);
    let resting = length(hit) / max(params.breath, 0.06);
    let formed = ease((params.twist - 0.3) / 0.7) * (1.0 - params.warp);
    if formed > 0.001 && resting < 12.0 {
        let angle = atan2(hit.y, hit.x) + params.hand * params.spin;
        let phase = angle - params.hand * log(resting + 0.32) * params.winding;
        let band = pow(0.5 + 0.5 * cos(params.arms * phase), 2.6);
        let dust = soft(hit * 2.1 + vec2f(t * 1.1, -t));
        let rho = pow(clamp(resting / max(params.extent, 0.1), 0.0, 1.0), 1.0 / max(params.falloff, 0.1));
        let crest = ripple(rho);
        let tint = mix(params.color_a.rgb, params.color_b.rgb, 0.5 + 0.5 * sin(resting * 1.9 + phase));
        let gas = band * exp(-resting * 0.62) * (0.25 + 1.2 * dust) * formed;
        let wave = shock_wave(resting, params.shock, params.extent);
        color += tint * gas * (0.055 + 0.04 * params.energy + 0.035 * params.chorus + 0.16 * crest + 0.2 * wave)
            * (1.0 - 0.6 * params.lyric * (1.0 - 0.5 * params.chorus)) * (1.0 - params.clip);
        color += mix(tint, vec3f(1.0), 0.35) * wave * exp(-resting * 0.35) * 0.026 * formed * (1.0 - params.clip);
        let core = exp(-resting * 3.1) * formed;
        let warm = mix(params.color_a.rgb, vec3f(1.0, 0.92, 0.8), 0.62);
        color += warm * core * (0.8 + 0.5 * params.collapse) * (1.0 - 0.85 * params.lyric) * (1.0 - params.clip);
    }

    let streaming = params.warp * params.warp;
    color += mix(params.color_a.rgb, vec3f(0.74, 0.84, 1.0), 0.55) * exp(-length(p) * 3.6) * streaming * 0.07;
    return vec4f(color, 1.0);
}
