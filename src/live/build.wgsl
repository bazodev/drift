// Decides where every star is and what it looks like right now.
// The galaxy's stars come first: each owns one pixel of the cover and keeps its color through the whole
// story. After them come the grains of the clip's picture, each owning one spot of the frame.

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read_write> stars: array<Star>;
@group(0) @binding(2) var cover_old: texture_2d<f32>;
@group(0) @binding(3) var cover_new: texture_2d<f32>;
@group(0) @binding(4) var cover_sampler: sampler;
@group(0) @binding(5) var<storage, read> wave: array<f32>;
@group(0) @binding(6) var<storage, read> glyphs: array<vec2f>;
@group(0) @binding(7) var<storage, read> words: array<Word>;
@group(0) @binding(8) var clip_old: texture_2d<f32>;
@group(0) @binding(9) var clip_new: texture_2d<f32>;
/// How much each grain of the clip is stirred by movement in the picture. It cools frame by frame.
@group(0) @binding(10) var<storage, read_write> heat: array<f32>;
/// The last picture before a cut, which bursts apart while the new one gathers.
@group(0) @binding(11) var clip_shard: texture_2d<f32>;
/// How much each word on stage is cut out of the picture behind it, settled over time.
@group(0) @binding(12) var<storage, read_write> word_cut: array<f32>;

/// What the voice did a moment ago: sound spreads outward from the center.
fn ripple(position: f32) -> f32 {
    let at = clamp(position, 0.0, 1.0) * f32(WAVE - 1u);
    let index = u32(at);
    let next = min(index + 1u, WAVE - 1u);
    return mix(wave[index], wave[next], fract(at));
}

/// How far through its crumbling a grain is when it has faded out and can go home unseen.
const DUST_ENDS: f32 = 0.78;

// The clip screen: half its height on screen, its shape, how deep it sits and how its grains glow.
// It fills the whole height, and the width too wherever the screen is no wider than 16:9.
const SCREEN_HALF: f32 = 1.0;
const SCREEN_ASPECT: f32 = 1.7777778;
const SCREEN_DEPTH: f32 = 3.9;
/// Bright parts of the picture come this much closer, so the frame has relief.
const RELIEF: f32 = 0.32;
const SCREEN_GAIN: f32 = 0.9;
/// The radius of one grain on screen: a little less than the spacing, so the picture stays dust.
const SCREEN_GRAIN: f32 = 0.0063;
/// How the picture's lifted light rolls off at the top, so lifted highlights never burn out.
const SHOULDER: f32 = 1.4;
/// Words over the clip. Behind a word brighter than this the word has nothing to add: its letters take light
/// away and are cut out of the picture, fully past the second value.
const CUT_FROM: f32 = 0.3;
const CUT_TO: f32 = 0.55;
/// How quickly a word's cut follows the picture behind it: gently enough that a moving picture never makes
/// the word blink between light and dark.
const CUT_SETTLE: f32 = 2.2;
/// Grains spread this deep along their lines of sight: whole from here, a cloud from anywhere else.
const SCREEN_DEPTH_SPREAD: f32 = 1.8;
/// The galaxy pours into the clip. Departures are spread over this share of the pour, the core first.
const POUR_SPREAD: f32 = 0.5;
/// How far a star turns about the middle of the view on its way, in radians.
const POUR_TURN: f32 = 1.7;
/// How much closer to the middle the stars pass halfway, like water going down a drain.
const POUR_PULL: f32 = 0.34;
/// Stars are this much dimmer halfway, so the crowd in the middle never flashes.
const POUR_DIM: f32 = 0.5;
/// As the pour begins the galaxy spins up like a whirlpool, the middle fastest, by up to this many radians.
const POUR_WHIRL: f32 = 2.4;
/// How far a hot grain breaks loose toward the viewer.
const HEAT_FLIGHT: f32 = 0.28;
/// A cut: how long it lasts, which share of it the old picture bursts, and how far its grains fly.
const CUT_TIME: f32 = 0.85;
const CUT_BURST: f32 = 0.3;
const CUT_FLIGHT: f32 = 0.7;

/// The share of its way a star has poured. `order` says when it leaves: 0 first, 1 last.
fn poured(clip: f32, order: f32) -> f32 {
    return clamp((clip - order * POUR_SPREAD) / (1.0 - POUR_SPREAD), 0.0, 1.0);
}

/// Where a pouring star is, `along` of its way from `origin` to `landing`: it turns about the middle of
/// the view and passes closer to it halfway, then spreads out onto its spot.
fn pour_path(origin: vec3f, landing: vec3f, along: f32) -> vec3f {
    let halfway = sin(along * TAU * 0.5);
    let p = mix(origin, landing, smoothstep(0.0, 1.0, along));
    let turn = -params.hand * POUR_TURN * halfway;
    let ct = cos(turn);
    let st = sin(turn);
    return vec3f(vec2f(ct * p.x - st * p.y, st * p.x + ct * p.y) * (1.0 - POUR_PULL * halfway), p.z);
}

/// How bright the picture is behind a word, from five spots along it.
fn backdrop_of(word: Word) -> f32 {
    let span = vec2f(SCREEN_HALF * SCREEN_ASPECT, SCREEN_HALF);
    let along = vec2f(cos(word.turn), sin(word.turn)) * word.half_width * word.scale;
    var sum = 0.0;
    for (var step = 0; step < 5; step++) {
        let spot = word.at + along * (f32(step) * 0.5 - 1.0);
        let uv = clamp(vec2f(spot.x / span.x * 0.5 + 0.5, 0.5 - spot.y / span.y * 0.5), vec2f(0.0), vec2f(1.0));
        let light = picture_light(textureSampleLevel(clip_new, cover_sampler, uv, 0.0).rgb);
        sum += dot(light, vec3f(0.2126, 0.7152, 0.0722));
    }
    return sum / 5.0;
}

/// Settles, word by word, how much each word on stage is cut out of the picture behind it. The picture changes
/// with every frame, and the decision follows it gently. A word that has just arrived starts where it belongs.
@compute @workgroup_size(16)
fn settle_words(@builtin(global_invocation_id) id: vec3u) {
    let slot = id.x;
    if slot >= WORD_SLOTS {
        return;
    }
    let word = words[slot];
    if word.count <= 0.0 || word.gather <= 0.0 || params.clip <= 0.001 {
        word_cut[slot] = 0.0;
        return;
    }
    let wanted = smoothstep(CUT_FROM, CUT_TO, backdrop_of(word)) * params.clip;
    if word.gather < 0.05 {
        word_cut[slot] = wanted;
        return;
    }
    word_cut[slot] = mix(word_cut[slot], wanted, 1.0 - exp(-params.frame_time * CUT_SETTLE));
}

/// The star is part of a word right now: sung words stay in front of the clip too.
fn in_a_word(rank: u32) -> bool {
    if rank >= WORD_SLOTS * STARS_PER_WORD {
        return false;
    }
    let word = words[rank / STARS_PER_WORD];
    return f32(rank % STARS_PER_WORD) < word.count && word.gather > 0.0;
}

/// A star that is not shown this frame.
fn hidden() -> Star {
    return Star(vec3f(0.0, 0.0, 1.0), 0.0, vec3f(0.0), 0.0);
}

/// The picture's light at a spot. Dark scenes are lifted first; what is still near-black after that gives
/// no light at all, so the screen has no frame and floats in space. The top rolls off softly.
fn picture_light(color: vec3f) -> vec3f {
    let clean = max(color * params.exposure - vec3f(0.014), vec3f(0.0));
    return (vec3f(1.0) - exp(-clean * SHOULDER)) / (1.0 - exp(-SHOULDER));
}

/// A star of the galaxy as the story places it, before words, the clip or hyperspace take it over.
struct Body {
    view: vec3f,
    color: vec3f,
    size: f32,
    rho: f32,
    painted: vec3f,
    tint: vec3f,
    sparkle: f32,
}

fn galaxy_body(index: u32) -> Body {
    let cell = home(index);
    let a = random(index);
    let b = random(index + 7919u);
    let c = random(index + 131u);
    let rho = min(length(cell) / 1.4142136, 1.0);
    let base = atan2(cell.y, cell.x);

    // Three kinds of stars: dust close to the camera, a soft disc and the spiral arms.
    let is_dust = a.y < 0.035;
    let is_disc = a.y < 0.3;

    let resting = params.extent * pow(rho, params.falloff) + 0.012;
    let wave = shock_wave(resting, params.shock, params.extent);
    let radius = resting * params.breath * (1.0 + 0.14 * wave);
    let sector = TAU / params.arms;
    let arm = round(base / sector);
    let offset = (base - arm * sector) / (sector * 0.5);
    let bunched = sign(offset) * pow(abs(offset), 1.0 + 1.6 * params.gather) * mix(1.0, 0.34, params.gather);
    let across = select(bunched, offset, is_disc) * sector * 0.5;
    let wind = params.hand * log(resting + 0.32) * params.winding;
    let scatter = (b.x - 0.5) * 0.17 / (resting + 0.45);
    let drift = sin(params.time * (0.11 + a.x * 0.2) + b.y * TAU) * 0.012;
    let whirl = POUR_WHIRL * params.clip * params.clip * exp(-resting * 0.8);
    let orbit = arm * sector + across + wind + scatter + drift - params.hand * (params.spin + whirl);
    let height = (b.y - 0.5) * params.thickness * exp(-resting * 0.72) + (c.x - 0.5) * 0.52 * exp(-resting * 3.4);

    // The cover winds up from its center outward.
    let wound = ease((params.twist - (rho * 0.4 + a.x * 0.07)) / 0.53);
    let flat_spot = vec3f(cell.x * CARD, 0.0, cell.y * CARD);
    let turned = base + (orbit - base) * wound;
    let reach = mix(length(cell) * CARD, radius, wound);
    var world = vec3f(cos(turned) * reach, height * wound, sin(turned) * reach);
    if is_dust {
        let around = b.x * TAU - params.hand * params.spin * 0.6;
        let far_out = 0.7 + 3.5 * sqrt(c.x);
        let cloud = vec3f(cos(around) * far_out, -0.5 + 2.75 * b.y, sin(around) * far_out);
        world = mix(flat_spot, cloud, wound);
    }
    var view = to_view(world, params.tilt, params.roll);

    // Color comes from this star's own pixel, with a little of the song's palette.
    let uv = vec2f(cell.x * 0.5 + 0.5, 0.5 - cell.y * 0.5);
    let pick = clamp(params.blend * 1.6 - a.x * 0.6, 0.0, 1.0);
    let pixel = mix(
        textureSampleLevel(cover_old, cover_sampler, uv, 0.0).rgb,
        textureSampleLevel(cover_new, cover_sampler, uv, 0.0).rgb,
        pick,
    );
    let shade = dot(pixel, vec3f(0.2126, 0.7152, 0.0722));
    let vivid = max(mix(vec3f(shade), pixel, 1.8), vec3f(0.0));
    let tint = mix(params.color_a.rgb, params.color_b.rgb, 0.5 + 0.5 * sin(resting * 1.9 + arm * 1.7));
    let painted = mix(vivid, tint * (0.2 + shade * 1.5), 0.42);

    let sparkle = pow(b.x, 24.0);
    let core = exp(-resting * 2.1);
    let crest = ripple(rho);
    let body = select(1.0, 0.42, is_disc) * (0.13 + 0.09 * params.energy + 0.5 * crest * (1.0 - rho * 0.45));
    let light = (body + 2.2 * sparkle + core * 0.5) * (1.0 - 0.78 * params.collapse) * (1.0 + 0.26 * params.chorus + 0.95 * wave);
    let star_color = mix(painted, vec3f(1.0, 0.92, 0.82), core * 0.85) * light;
    var color = mix(pixel, star_color, wound);
    var size = mix(0.0052, 0.0027 + 0.0074 * pow(b.x, 8.0), wound);
    if is_dust {
        size = mix(0.0052, 0.005 + 0.008 * c.y, wound);
        color = mix(pixel, painted * 0.15, wound);
    }

    size *= 1.0 + 0.28 * wave * wound;

    // Words borrow their light from the galaxy, which dims and clears its core for them.
    color *= 1.0 - params.lyric * (0.3 + 0.5 * exp(-resting * 1.15)) * (1.0 - 0.45 * params.chorus);
    return Body(view, color, size, rho, painted, tint, sparkle);
}

@compute @workgroup_size(128)
fn build(@builtin(global_invocation_id) id: vec3u) {
    let index = id.x;
    if index >= GRID * GRID + SCREEN_W * SCREEN_H {
        return;
    }
    if index >= GRID * GRID {
        build_grain(index - GRID * GRID, index);
        return;
    }
    // Each star of the galaxy has a rank, and the first ranks serve the words.
    let rank = (index * 1009u) % (GRID * GRID);
    // Once the galaxy has poured into the clip it is gone: its stars wait unseen and cost nothing.
    if params.clip >= 1.0 && params.warp <= 0.0 && !in_a_word(rank) {
        stars[index] = hidden();
        return;
    }
    let a = random(index);
    let b = random(index + 7919u);
    let c = random(index + 131u);
    let star = galaxy_body(index);
    var view = star.view;
    var color = star.color;
    var size = star.size;

    // A share of the stars belongs to word places. They leave whenever their word is sung.
    var held = false;
    var cut = 0.0;
    if rank < WORD_SLOTS * STARS_PER_WORD {
        let slot = rank / STARS_PER_WORD;
        let word = words[slot];
        let grain = rank % STARS_PER_WORD;
        if f32(grain) < word.count && word.gather > 0.0 {
            let local = glyphs[slot * STARS_PER_WORD + grain];
            let along = clamp(local.x / max(word.half_width, 0.02) * 0.5 + 0.5, 0.0, 1.0);
            let ct = cos(word.turn);
            let st = sin(word.turn);
            let placed = word.at + vec2f(ct * local.x - st * local.y, st * local.x + ct * local.y) * word.scale;
            let depth = TEXT_DEPTH + word.depth;
            let letter_size = word.size * word.scale * depth / TEXT_DEPTH;
            // The word being sung burns white. Once sung it cools into the colors of the song.
            let cooled = mix(max(star.tint, vec3f(0.3)), vec3f(1.0, 0.97, 0.93), 0.4);
            let lit = mix(cooled, vec3f(1.0, 0.98, 0.95), word.hot) * (0.4 + 0.12 * params.chorus + 0.05 * params.voice + 0.4 * word.hot * (1.0 - 0.38 * params.chorus));
            // A word snaps together from its first letter and comes apart from its first letter too.
            let order = mix(along, 1.0 - along, word.leaving);
            let gone = 1.0 - clamp((word.gather - order * 0.28 - c.y * 0.12) / 0.6, 0.0, 1.0);
            if gone < DUST_ENDS {
                // Grains only ever move a short way around their letter. The star itself comes
                // and goes from its place in the arm unseen, so nothing streaks across the screen.
                let fading = gone / DUST_ENDS;
                let away = placed / max(length(placed), 0.05) * (word.leaving * 2.0 - 1.0);
                let gust = vec2f(a.x, b.y) - 0.5;
                let phase = c.x * TAU;
                let sway = vec2f(sin(phase + fading * 3.4) - sin(phase), cos(phase * 1.7 + fading * 2.8) - cos(phase * 1.7));
                let drift = ((away * 0.13 + gust * 0.32) * pow(fading, 1.4) + sway * 0.016) * word.dust;
                view = vec3f((placed + drift) * depth / FOCAL, depth);
                // Most of the dust stays dim, and a few grains are lit as sparks.
                let glint = mix(1.0, 0.55 + 2.2 * pow(b.x, 5.0), smoothstep(0.0, 0.4, fading));
                // Over a bright picture a glowing word has nothing to add: it takes light away instead,
                // and its letters are cut out of the picture.
                if params.clip > 0.001 {
                    cut = word_cut[slot] * pow(1.0 - fading, 1.2);
                }
                color = lit * glint * pow(1.0 - fading, mix(1.2, 1.6, word.leaving)) * (1.0 - cut);
                size = letter_size * (1.0 - 0.2 * fading);
                held = true;
            } else {
                color *= smoothstep(DUST_ENDS, 1.0, gone);
            }
        }
    }

    // A cut-out letter's star is marked by its trail, from -2 to -3 with how much light it takes away.
    var trail = select(0.0, -2.0 - cut, held && cut > 0.001);
    // The galaxy pours into the clip: every star leaves in turn, the core first and the rim last. It lands
    // on a spot of its own and melts into the picture there, while the picture's own grains take over.
    let along = poured(params.clip, star.rho * 0.7 + c.y * 0.3);
    if params.clip > 0.001 && !held && along > 0.0 {
        let spot = vec2f(a.y, c.x);
        let span = vec2f(SCREEN_HALF * SCREEN_ASPECT, SCREEN_HALF);
        let flat = (vec2f(spot.x, 1.0 - spot.y) * 2.0 - 1.0) * span;
        let depth = SCREEN_DEPTH + (b.y - 0.5) * SCREEN_DEPTH_SPREAD;
        let there = picture_light(textureSampleLevel(clip_new, cover_sampler, spot, 0.0).rgb) * SCREEN_GAIN;
        view = pour_path(view, vec3f(flat * depth / FOCAL, depth), along);
        color = mix(color, there, smoothstep(0.25, 0.9, along)) * (1.0 - POUR_DIM * sin(along * TAU * 0.5))
            * (1.0 - smoothstep(0.6, 1.0, along));
        size *= 1.0 - smoothstep(0.7, 1.0, along);
    }

    // Hyperspace: stars join a tunnel and stream past the camera.
    let entering = ease((params.warp - a.x * 0.3) / 0.7);
    if entering > 0.0 {
        let lane = c.y * TAU;
        let away = 0.24 + 3.25 * sqrt(b.y);
        let deep = NEAR + TUNNEL * fract(a.x * 7.31 - params.flight);
        let tube = vec3f(cos(lane) * away, sin(lane) * away, deep);
        view = mix(view, tube, entering);
        trail = params.speed * 0.05 * entering;
        let streak = mix(mix(max(star.painted, vec3f(0.04)), star.tint, 0.4), vec3f(0.84, 0.9, 1.0), 0.14) * (0.15 + 0.75 * star.sparkle);
        color = mix(color, streak, entering);
        size = mix(size, 0.0033, entering);
        // Only one star in nine streaks. The rest fade first, so the screen never floods with light.
        if a.x > 0.11 {
            size *= 1.0 - smoothstep(0.0, 0.5, entering);
            trail = 0.0;
        }
    }

    stars[index] = Star(view, size, color, trail);
}

/// A grain of the clip's picture. It owns one spot of the frame and sits at its own depth on the line from
/// the eye through that spot: from here the picture is whole, from anywhere else it is a cloud of stars.
fn build_grain(grain: u32, index: u32) {
    if params.clip <= 0.001 {
        stars[index] = hidden();
        return;
    }
    let a = random(index);
    let b = random(index + 7919u);
    let c = random(index + 131u);
    let grid = vec2f(f32(SCREEN_W), f32(SCREEN_H));
    let cell_at = (vec2f(f32(grain % SCREEN_W), f32(grain / SCREEN_W)) + 0.5) / grid;
    // Each grain looks a little off its cell, so the picture reads as dust, not as a grid.
    let frame_uv = cell_at + (vec2f(a.y, c.x) - 0.5) / grid * 0.7;
    let before = textureSampleLevel(clip_old, cover_sampler, frame_uv, 0.0).rgb;
    let after = textureSampleLevel(clip_new, cover_sampler, frame_uv, 0.0).rgb;
    let shown = picture_light(mix(before, after, params.clip_blend));
    let level = dot(shown, vec3f(0.2126, 0.7152, 0.0722));

    // Movement heats a grain; heat cools frame by frame. Only strong change in a bright spot counts,
    // and only while the rest of the picture holds still: a pan or a flash is not a spark.
    let stirred = smoothstep(0.18, 0.5, length(after - before)) * smoothstep(0.06, 0.35, level) * params.motion_gate;
    let warm = max(heat[grain] * params.heat_keep, stirred);
    heat[grain] = warm;

    // The grain comes from a star of the galaxy, picked at random so neighbors never fly in lines, and
    // leaves when that star's turn comes.
    let source = min(u32(random(index + 4099u).x * f32(GRID * GRID)), GRID * GRID - 1u);
    let along = poured(params.clip, min(length(home(source)) / 1.4142136, 1.0) * 0.7 + c.y * 0.3);
    if along <= 0.0 {
        stars[index] = hidden();
        return;
    }

    let span = vec2f(SCREEN_HALF * SCREEN_ASPECT, SCREEN_HALF);
    // Grains sit a little off their cells too, so the screen is dust and never a grid of lamps.
    let shake = (vec2f(b.x, a.y) - 0.5) * span * 2.0 / grid * 0.8;
    let flat = (vec2f(cell_at.x, 1.0 - cell_at.y) * 2.0 - 1.0) * span + shake;
    // The chorus wave rolls across the picture too, pushing it toward the viewer.
    let ring = shock_wave(length(flat) * 1.4, params.shock, params.extent);
    let depth = SCREEN_DEPTH + (c.x - 0.5) * SCREEN_DEPTH_SPREAD - level * RELIEF - ring * 0.3;
    var placed = vec3f(flat * depth / FOCAL, depth);
    var lit = shown * SCREEN_GAIN * (1.0 + 0.8 * ring) * (1.0 + 0.15 * params.voice);
    // Grains keep their size on screen wherever they fly, so none swells into a blot.
    let grain_size = (0.55 + 0.65 * sqrt(level)) * (1.0 + 0.2 * ring);
    var streak = -1.0;

    // Hot grains break loose, mostly sideways and a little toward the viewer, and flare.
    // A few of the hottest leave a short streak.
    let toward = normalize(vec3f(flat * 0.9 + (vec2f(a.x, c.y) - 0.5) * 1.2, -0.45));
    placed += toward * warm * HEAT_FLIGHT;
    lit *= 1.0 + 0.8 * warm;
    var swell = 1.0 + 0.3 * warm;
    if b.x < 0.12 && warm > 0.4 {
        streak = warm * 0.25;
    }

    // A cut: the old picture bursts toward the viewer, then its grains gather into the new one.
    if params.cut_age < CUT_TIME {
        let personal = clamp(params.cut_age / CUT_TIME * 1.3 - c.y * 0.3, 0.0, 1.0);
        let burst = normalize(vec3f(flat + (vec2f(c.x, a.y) - 0.5) * 0.8, -0.5));
        if personal < CUT_BURST {
            let flung = ease(personal / CUT_BURST);
            let old_picture = picture_light(textureSampleLevel(clip_shard, cover_sampler, frame_uv, 0.0).rgb);
            placed += burst * flung * CUT_FLIGHT * params.cut_strength;
            lit = old_picture * SCREEN_GAIN * (1.0 + 1.5 * (1.0 - flung)) * (1.0 - 0.6 * flung);
        } else {
            let back = 1.0 - ease((personal - CUT_BURST) / (1.0 - CUT_BURST));
            placed += burst * back * CUT_FLIGHT * params.cut_strength;
            lit *= 1.0 - 0.6 * back;
        }
        streak = -1.0;
    }
    let landed = SCREEN_GRAIN * placed.z / FOCAL * grain_size * swell;
    if along >= 1.0 {
        stars[index] = Star(placed, landed, lit, streak);
        return;
    }
    // On the way it starts as a faint copy of its star and takes on the picture's color as it nears.
    // The copy stays dim, so the galaxy does not brighten while the grains leave it.
    let origin = galaxy_body(source);
    let near = smoothstep(0.25, 0.95, along);
    let appear = smoothstep(0.0, 0.3, along) * 0.55;
    stars[index] = Star(
        pour_path(origin.view, placed, along),
        mix(origin.size, landed, near),
        mix(origin.color * appear, lit, near) * (1.0 - POUR_DIM * sin(along * TAU * 0.5)),
        0.0,
    );
}
