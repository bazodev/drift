// Draws each star as a soft point, or as a streak while it flies through hyperspace.

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read> stars: array<Star>;

struct StarOut {
    @builtin(position) position: vec4f,
    @location(0) local: vec2f,
    @location(1) color: vec3f,
    @location(2) stretch: f32,
}

@vertex
fn vs_star(@builtin(vertex_index) corner_index: u32, @builtin(instance_index) instance: u32) -> StarOut {
    var corners = array<vec2f, 4>(vec2f(-1.0, -1.0), vec2f(1.0, -1.0), vec2f(-1.0, 1.0), vec2f(1.0, 1.0));
    let corner = corners[corner_index];
    let star = stars[instance];
    var out: StarOut;
    out.local = corner;
    out.color = vec3f(0.0);
    out.stretch = 0.0;
    out.position = vec4f(2.0, 2.0, 0.0, 1.0);
    if star.size <= 0.00001 {
        return out;
    }
    let look = vec2f(params.look_x, params.look_y);
    let head = look_at(star.view, look);
    if head.z < NEAR {
        return out;
    }

    // Stars away from the focus plane spread into soft discs and keep their total light.
    let sharp = max(star.size * FOCAL / head.z, params.pixel * 0.85);
    let streaming = star.trail > 0.001;
    // Grains of the clip are marked with a negative trail: they stay sharp at any depth.
    let crisp = star.trail < -0.5;
    let blur = select(clamp(abs(1.0 / head.z - 1.0 / DISTANCE) * 0.05, 0.0, 0.015), 0.0, streaming || crisp);
    let radius = select(sharp + blur, min(sharp, 0.0032), streaming);
    let kept = min(sharp / (sharp + blur), 1.0);

    var center = head.xy * FOCAL / head.z;
    var offset = corner * radius;
    if streaming {
        let behind = look_at(star.view + vec3f(0.0, 0.0, star.trail), look);
        let tail = behind.xy * FOCAL / max(behind.z, NEAR);
        let span = center - tail;
        let half = length(span) * 0.5;
        if half > radius * 0.25 {
            let along = span / (half * 2.0);
            let side = vec2f(-along.y, along.x);
            center = (center + tail) * 0.5;
            offset = along * corner.x * (half + radius) + side * corner.y * radius;
            out.stretch = half / radius;
        }
    }
    out.position = vec4f((center + offset) / vec2f(params.aspect, 1.0), 0.0, 1.0);
    out.color = star.color * kept * kept;
    return out;
}

/// Letters cut out of a bright picture: their stars take light away, over a disc this much wider than they
/// shine, so the letters close up, and at most this deep.
const CUT_SPREAD: f32 = 1.6;
const CUT_DEPTH: f32 = 0.96;

@vertex
fn vs_cut(@builtin(vertex_index) corner_index: u32, @builtin(instance_index) instance: u32) -> StarOut {
    var corners = array<vec2f, 4>(vec2f(-1.0, -1.0), vec2f(1.0, -1.0), vec2f(-1.0, 1.0), vec2f(1.0, 1.0));
    let corner = corners[corner_index];
    let star = stars[instance];
    var out: StarOut;
    out.local = corner;
    out.color = vec3f(0.0);
    out.stretch = 0.0;
    out.position = vec4f(2.0, 2.0, 0.0, 1.0);
    let cut = -2.0 - star.trail;
    if star.trail > -1.5 || cut <= 0.0 || star.size <= 0.00001 {
        return out;
    }
    let head = look_at(star.view, vec2f(params.look_x, params.look_y));
    if head.z < NEAR {
        return out;
    }
    let radius = max(star.size * FOCAL / head.z, params.pixel * 0.85) * CUT_SPREAD;
    out.position = vec4f((head.xy * FOCAL / head.z + corner * radius) / vec2f(params.aspect, 1.0), 0.0, 1.0);
    out.color = vec3f(min(cut, 1.0) * CUT_DEPTH);
    return out;
}

/// How much light this spot of a cut-out letter takes away. The blend multiplies what is there by one minus it.
/// The disc is solid in the middle and soft only at its rim, so the letters come out dark and clean.
@fragment
fn fs_cut(in: StarOut) -> @location(0) vec4f {
    let falloff = 1.0 - smoothstep(0.45, 1.0, length(in.local));
    return vec4f(in.color * falloff, in.color.x * falloff);
}

@fragment
fn fs_star(in: StarOut) -> @location(0) vec4f {
    let along = max(abs(in.local.x) * (1.0 + in.stretch) - in.stretch, 0.0);
    let falloff = max(1.0 - along * along - in.local.y * in.local.y, 0.0);
    return vec4f(in.color * falloff * falloff, 0.0);
}
