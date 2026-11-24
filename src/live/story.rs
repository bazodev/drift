//! The life of a song on the wallpaper. Stars arrive through hyperspace, gather into the cover,
//! wind up into a galaxy that lives with the music, and collapse when the track ends.
//! Pure timing and state with no GPU, so every transition can be tested.

use std::collections::VecDeque;

use super::{
    chorus::{self, Section},
    glyphs::SpelledWord,
    scatter::{Area, Place, Scatter},
    words::{self, CRUMBLE_LONGEST, WORD_SLOTS, WordClock, WordTime},
};
use crate::lyrics::{self, TimedLine};

const FACE_ON: f32 = std::f32::consts::FRAC_PI_2;
const ENTER: f32 = 0.9;
const MIN_FLIGHT: f32 = 1.15;
const COVER_WAIT: f32 = 2.4;
const ARRIVE: f32 = 1.2;
const HOLD: f32 = 1.6;
const UNWIND: f32 = 4.4;
const BLEND: f32 = 0.6;
const SPEED: f32 = 24.;
const TUNNEL: f32 = 16.;
pub const FADE: f32 = 2.4;
/// How long the galaxy takes to pour into the clip, and the picture to pour back into the galaxy.
const CLIP_POUR_IN: f32 = 3.4;
const CLIP_POUR_OUT: f32 = 2.6;
/// A new song does not wait for the picture to pour back: the clip simply goes.
const CLIP_LEAVE: f32 = 0.9;
const QUIET: f32 = 0.002;
const COLLAPSE_START: f64 = 6.;
const COLLAPSE_END: f64 = 0.4;
const SHORTEST_COLLAPSING_SONG: f64 = 20.;
/// A line is prepared a moment before it is sung.
const LYRIC_LEAD: f64 = 0.55;
const LATE_COVER: f32 = 0.8;
const SEEK_JUMP: f64 = 0.6;
/// A word snaps together this fast, out of a small puff of dust right where it stands.
const WORD_IN: f32 = 0.32;
/// How far that puff reaches, next to the dust of a crumbling word.
const DUST_ON_ARRIVAL: f32 = 0.3;
/// Words interrupted by a jump in the song clear away faster than words that were sung out.
const WORD_CLEAR: f64 = 1.;
/// Dust that has mostly gone no longer keeps the next word away.
const DUST_IS_NO_OBSTACLE: f32 = 0.55;
/// A word lands this much bigger, then settles to its size in about this many seconds.
const WORD_PUNCH: f32 = 0.2;
const WORD_SETTLE: f32 = 0.16;
/// How fast a hanging word drifts away from the core, and how it swells as it dissolves.
const WORD_FLOAT: f32 = 0.045;
const WORD_SWELL: f32 = 0.07;
const WORD_TURN: f32 = 0.35;
/// The chorus: how early the galaxy starts to lift, and how far it opens, turns and speeds up.
const CHORUS_LEAD: f64 = 0.9;
const CHORUS_OPEN: f32 = 0.15;
const CHORUS_FACE: f32 = 0.24;
const CHORUS_SPIN: f32 = 1.5;
const CHORUS_WORDS: f32 = 1.42;
/// How long the wave of light takes to roll from the core to the rim.
const WAVE_TIME: f32 = 1.9;
/// Numbers per word for the GPU: center, size, tilt, gather, heat, depth, stars, star size, width,
/// leaving, and how far its dust may drift.
pub const WORD_FLOATS: usize = 12;

fn approach(current: f32, target: f32, speed: f32, dt: f32) -> f32 {
    current + (target - current) * (1. - (-speed * dt).exp())
}

fn smooth(edge0: f64, edge1: f64, value: f64) -> f32 {
    let t = ((value - edge0) / (edge1 - edge0)).clamp(0., 1.);
    (t * t * (3. - 2. * t)) as f32
}

fn ease(value: f32) -> f32 {
    let t = value.clamp(0., 1.);
    t * t * t * (t * (t * 6. - 15.) + 10.)
}

/// The look of one song. The same track always gets the same galaxy.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shape {
    pub arms: f32,
    pub winding: f32,
    pub hand: f32,
    pub gather: f32,
    pub extent: f32,
    pub falloff: f32,
    pub thickness: f32,
    pub pitch: f32,
    pub roll: f32,
    pub spin: f32,
}

impl Shape {
    pub fn of(track: &str) -> Self {
        let mut state = 0xcbf2_9ce4_8422_2325u64;
        for byte in track.bytes() {
            state ^= u64::from(byte);
            state = state.wrapping_mul(0x0000_0100_0000_01B3);
        }
        let mut next = move || {
            state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut mixed = state;
            mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            ((mixed ^ (mixed >> 31)) >> 40) as f32 / (1u64 << 24) as f32
        };
        let mut between = |low: f32, high: f32| low + (high - low) * next();
        let arms = between(2., 6.).floor();
        Self {
            arms,
            winding: between(1.75, 3.05),
            hand: if between(0., 1.) < 0.5 { -1. } else { 1. },
            gather: between(0.68, 0.9),
            extent: between(2.6, 3.15),
            falloff: between(1.32, 1.62),
            thickness: between(0.24, 0.44),
            pitch: between(0.86, 1.2),
            roll: between(-0.34, 0.34),
            spin: between(0.038, 0.07),
        }
    }
}

/// Everything the renderer needs to know about this moment.
#[derive(Clone, Copy, Debug)]
pub struct Pose {
    pub fade: f32,
    pub twist: f32,
    pub tilt: f32,
    pub roll: f32,
    pub warp: f32,
    pub flight: f32,
    pub speed: f32,
    pub card: f32,
    pub collapse: f32,
    pub energy: f32,
    pub spin: f32,
    pub blend: f32,
    pub breath: f32,
    pub lyric: f32,
    /// How far into chorus mode the galaxy is, and how far the wave of light has rolled.
    pub chorus: f32,
    pub shock: f32,
    /// How far the song's clip has taken over: a share of the stars forms a screen that shows it.
    pub clip: f32,
    pub words: [[f32; WORD_FLOATS]; WORD_SLOTS],
    pub shape: Shape,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Flight,
    Arrive,
    Hold,
    Unwind,
    Galaxy,
}

#[derive(Clone, Copy, Debug)]
pub struct Input {
    pub active: bool,
    pub position: f64,
    pub duration: f64,
    pub level: f32,
    /// Whether the chorus effect is switched on.
    pub chorus: bool,
    /// Frames of the song's clip are arriving, so the clip may take the stage.
    pub clip: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GlyphRequest {
    pub generation: u32,
    pub words: Vec<String>,
}

/// A word ready to appear at its moment.
struct Prepared {
    time: WordTime,
    shape: SpelledWord,
    closing: bool,
}

/// A word on screen.
struct Live {
    time: WordTime,
    place: Place,
    area: Area,
    count: f32,
    star_size: f32,
    half_width: f32,
    drift: [f32; 2],
    gather: f32,
    hot: f32,
    age: f32,
    leaving: bool,
}

pub struct Story {
    stage: Stage,
    stage_time: f32,
    pose: Pose,
    next_shape: Option<Shape>,
    has_cover: bool,
    cover_ready: bool,
    loudness: f32,
    floor: f32,
    ceiling: f32,
    lines: Vec<TimedLine>,
    rate: f32,
    clock: Option<WordClock>,
    shown: Option<usize>,
    generation: u32,
    request: Option<GlyphRequest>,
    prepared: VecDeque<Prepared>,
    live: [Option<Live>; WORD_SLOTS],
    uploads: Vec<(usize, Vec<[f32; 2]>)>,
    scatter: Scatter,
    last_position: f64,
    sections: Vec<Section>,
    /// The chorus that already got its wave, so each one fires once.
    waved: Option<f64>,
    chorus_allowed: bool,
}

impl Story {
    /// Starts in hyperspace, so the very first song also arrives from it.
    pub fn new() -> Self {
        let shape = Shape::of("");
        Self {
            stage: Stage::Flight,
            stage_time: 0.,
            pose: Pose {
                fade: 0.,
                twist: 0.,
                tilt: FACE_ON,
                roll: 0.,
                warp: 1.,
                flight: 0.,
                speed: SPEED,
                card: 0.,
                collapse: 0.,
                energy: 0.55,
                spin: 0.,
                blend: 1.,
                breath: 1.,
                lyric: 0.,
                chorus: 0.,
                shock: 0.,
                clip: 0.,
                words: [[0.; WORD_FLOATS]; WORD_SLOTS],
                shape,
            },
            next_shape: None,
            has_cover: false,
            cover_ready: false,
            loudness: -24.,
            floor: -24.,
            ceiling: -24.,
            lines: Vec::new(),
            rate: words::song_rate(&[]),
            clock: None,
            shown: None,
            generation: 0,
            request: None,
            prepared: VecDeque::new(),
            live: std::array::from_fn(|_| None),
            uploads: Vec::new(),
            scatter: Scatter::new(1),
            last_position: 0.,
            sections: Vec::new(),
            waved: None,
            chorus_allowed: true,
        }
    }

    pub fn pose(&self) -> &Pose {
        &self.pose
    }

    #[cfg(test)]
    pub fn stage(&self) -> Stage {
        self.stage
    }

    fn enter(&mut self, stage: Stage) {
        self.stage = stage;
        self.stage_time = 0.;
    }

    /// A new song: leave for hyperspace, and take its shape once no star can see the change.
    pub fn new_track(&mut self, id: &str) {
        self.next_shape = Some(Shape::of(id));
        self.cover_ready = false;
        self.lines.clear();
        self.sections.clear();
        self.waved = None;
        self.clock = None;
        self.shown = None;
        self.generation = self.generation.wrapping_add(1);
        self.request = None;
        self.retire_words();
        self.scatter =
            Scatter::new(id.bytes().fold(0x811C_9DC5, |hash, byte| (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)));
        self.enter(Stage::Flight);
    }

    pub fn set_lines(&mut self, lines: Vec<TimedLine>) {
        self.rate = words::song_rate(&lines);
        self.sections = chorus::find(&lines);
        self.waved = None;
        self.lines = lines;
        self.clock = None;
        self.shown = None;
        self.generation = self.generation.wrapping_add(1);
        self.request = None;
        self.retire_words();
    }

    /// Words still on screen dissolve, and words that have not appeared never will.
    fn retire_words(&mut self) {
        self.prepared.clear();
        for word in self.live.iter_mut().flatten() {
            word.time.leave = f64::NEG_INFINITY;
            word.time.crumble = word.time.crumble.min(WORD_CLEAR);
        }
    }

    /// New colors wait for hyperspace, unless the song is already playing without them.
    pub fn can_swap_cover(&self, waited: f32) -> bool {
        match self.stage {
            Stage::Flight => self.pose.warp >= 1.,
            _ => waited >= LATE_COVER,
        }
    }

    pub fn cover_swapped(&mut self) {
        self.pose.blend = if self.has_cover { 0. } else { 1. };
        self.has_cover = true;
        self.cover_ready = true;
    }

    pub fn take_request(&mut self) -> Option<GlyphRequest> {
        self.request.take()
    }

    /// The shapes of a line are ready: its words wait for their moments to be sung.
    pub fn glyphs_ready(&mut self, generation: u32, spelled: Vec<SpelledWord>) {
        if generation != self.generation {
            return;
        }
        let Some(clock) = &self.clock else { return };
        let times = clock.timings();
        let last = times.len().saturating_sub(1);
        for (index, (time, shape)) in times.into_iter().zip(spelled).enumerate() {
            if !shape.points.is_empty() {
                self.prepared.push_back(Prepared { time, shape, closing: index == last });
            }
        }
        self.prepared.make_contiguous().sort_by(|a, b| a.time.appear.total_cmp(&b.time.appear));
    }

    /// New word shapes that the GPU needs, each for its place.
    pub fn take_uploads(&mut self) -> Vec<(usize, Vec<[f32; 2]>)> {
        std::mem::take(&mut self.uploads)
    }

    fn begin_song(&mut self, shape: Shape) {
        self.pose.shape = shape;
        self.pose.twist = 0.;
        self.pose.spin = 0.;
        self.pose.collapse = 0.;
        self.pose.energy = 0.55;
        self.floor = self.loudness;
        self.ceiling = self.loudness;
    }

    /// How loud this moment is compared with the rest of the same song.
    fn follow_energy(&mut self, dt: f32, level: f32) {
        if level < QUIET {
            self.pose.energy = approach(self.pose.energy, 0.3, 0.3, dt);
            return;
        }
        let decibels = 20. * level.log10();
        self.loudness = approach(self.loudness, decibels, 2.2, dt);
        let rate = |toward: bool| if toward { 1.5 } else { 0.03 };
        self.floor = approach(self.floor, self.loudness, rate(self.loudness < self.floor), dt);
        self.ceiling = approach(self.ceiling, self.loudness, rate(self.loudness > self.ceiling), dt);
        let span = (self.ceiling - self.floor).max(6.);
        let target = ((self.loudness - self.floor) / span).clamp(0., 1.);
        let speed = if target > self.pose.energy { 0.9 } else { 0.4 };
        self.pose.energy = approach(self.pose.energy, target, speed, dt);
    }

    /// Verses stay calm. As a chorus comes the galaxy opens, turns to the viewer and speeds up,
    /// and one wave of light rolls out of its core.
    fn follow_chorus(&mut self, dt: f32, position: f64, allowed: bool) {
        self.chorus_allowed = allowed;
        if (position - self.last_position).abs() > SEEK_JUMP {
            self.waved = None;
        }
        let here = (allowed && self.stage == Stage::Galaxy)
            .then(|| {
                self.sections.iter().find(|section| position >= section.start - CHORUS_LEAD && position < section.end)
            })
            .flatten()
            .copied();
        let speed = if here.is_some() { 1.2 } else { 0.7 };
        self.pose.chorus = approach(self.pose.chorus, f32::from(u8::from(here.is_some())), speed, dt);
        if let Some(section) = here
            && (0. ..0.5).contains(&(position - section.start))
            && self.waved != Some(section.start)
        {
            self.waved = Some(section.start);
            self.pose.shock = 0.001;
        }
        if self.pose.shock > 0. {
            self.pose.shock += dt / WAVE_TIME;
            if self.pose.shock >= 1. {
                self.pose.shock = 0.;
            }
        }
    }

    fn wanted_line(&self, position: f64) -> Option<usize> {
        let moment = position + LYRIC_LEAD;
        let index = lyrics::active_line(&self.lines, moment)?;
        let line = &self.lines[index];
        if line.text.trim().is_empty() {
            return None;
        }
        (moment < words::line_end(&self.lines, index)).then_some(index)
    }

    /// Puts a word on stage, somewhere clear of the words still hanging there.
    fn launch(&mut self, word: Prepared) {
        // Words closest to being gone come first: crumbling ones, then those about to crumble.
        let nearly_gone = |word: &Live| if word.leaving { f64::from(word.gather) - 2. } else { word.time.leave };
        let rank = |slot: usize, live: &[Option<Live>; WORD_SLOTS]| live[slot].as_ref().map_or(f64::MIN, nearly_gone);
        let slot = (0..WORD_SLOTS).find(|slot| self.live[*slot].is_none()).unwrap_or_else(|| {
            (0..WORD_SLOTS).min_by(|a, b| rank(*a, &self.live).total_cmp(&rank(*b, &self.live))).unwrap_or(0)
        });
        let mut hanging: Vec<(f64, Area)> = (0..WORD_SLOTS)
            .filter(|other| *other != slot)
            .filter_map(|other| self.live[other].as_ref())
            .filter(|word| !word.leaving || word.gather > DUST_IS_NO_OBSTACLE)
            .map(|word| (nearly_gone(word), word.area))
            .collect();
        hanging.sort_by(|a, b| a.0.total_cmp(&b.0));
        let taken: Vec<Area> = hanging.into_iter().map(|(_, area)| area).collect();

        let Prepared { time, shape, closing } = word;
        let in_chorus = self.chorus_allowed
            && self.sections.iter().any(|section| time.start >= section.start && time.start < section.end);
        let place = self.scatter.place(&shape.size, &taken, closing, if in_chorus { CHORUS_WORDS } else { 1. });
        let [x, y] = place.at;
        let reach = x.hypot(y).max(0.05);
        self.live[slot] = Some(Live {
            time,
            place,
            area: Area::of(&place, &shape.size),
            count: shape.points.len() as f32,
            star_size: shape.star_size,
            half_width: shape.size.width / 2.,
            drift: [x / reach, y / reach],
            gather: 0.,
            hot: 0.,
            age: 0.,
            leaving: false,
        });
        self.uploads.push((slot, shape.points));
    }

    fn follow_lyrics(&mut self, dt: f32, position: f64) {
        if (position - self.last_position).abs() > SEEK_JUMP {
            self.retire_words();
            self.shown = None;
            self.clock = None;
        }
        // Words move on their own clock: smooth between position updates, and still on pause.
        let moved = if position == self.last_position { 0. } else { dt };
        self.last_position = position;

        let formed = self.stage == Stage::Galaxy || (self.stage == Stage::Unwind && self.pose.twist > 0.8);
        let wanted = if formed { self.wanted_line(position) } else { None };
        if wanted != self.shown {
            self.shown = wanted;
            self.generation = self.generation.wrapping_add(1);
            self.request = None;
            self.clock = None;
            if let Some(index) = wanted {
                let clock = WordClock::new(&self.lines, index, self.rate);
                self.request = Some(GlyphRequest { generation: self.generation, words: clock.words().to_vec() });
                self.clock = Some(clock);
            }
        }

        while self.prepared.front().is_some_and(|word| word.time.appear <= position) {
            if let Some(word) = self.prepared.pop_front()
                && position < word.time.leave
            {
                self.launch(word);
            }
        }

        let mut any_on_stage = false;
        for slot in 0..WORD_SLOTS {
            let Some(word) = &mut self.live[slot] else {
                self.pose.words[slot] = [0.; WORD_FLOATS];
                continue;
            };
            let on_stage = formed && position >= word.time.appear && position < word.time.leave;
            word.leaving = !on_stage;
            any_on_stage |= on_stage || word.gather > 0.4;
            let step = if on_stage { dt / WORD_IN } else { -dt / word.time.crumble as f32 };
            word.gather = (word.gather + step).clamp(0., 1.);
            if word.gather <= 0. && !on_stage {
                self.live[slot] = None;
                self.pose.words[slot] = [0.; WORD_FLOATS];
                continue;
            }
            let sung = position >= word.time.start && position < word.time.hot_until;
            // Big chorus words catch fire a little slower, so they never flash the whole screen.
            let ignite = 18. - 9. * self.pose.chorus;
            word.hot = approach(word.hot, f32::from(u8::from(sung)), if sung { ignite } else { 2.6 }, dt);

            // A word lands big, settles, hangs in the air drifting outward, and swells as it goes.
            word.age += moved;
            let age = (word.age - (word.time.start - word.time.appear) as f32).max(0.);
            let landing = (-age / WORD_SETTLE).exp();
            let going = if on_stage { 0. } else { 1. - word.gather };
            let scale = word.place.scale * (1. + WORD_PUNCH * landing + WORD_SWELL * going);
            let float = age * WORD_FLOAT;
            let turn = word.place.turn * (1. + age * WORD_TURN);
            let dust = if word.leaving { (word.time.crumble / CRUMBLE_LONGEST) as f32 } else { DUST_ON_ARRIVAL };
            self.pose.words[slot] = [
                word.place.at[0] + word.drift[0] * float,
                word.place.at[1] + word.drift[1] * float,
                scale,
                turn,
                word.gather,
                word.hot,
                word.place.depth,
                word.count,
                word.star_size,
                word.half_width,
                f32::from(u8::from(word.leaving)),
                dust,
            ];
        }
        let target = f32::from(u8::from(any_on_stage));
        self.pose.lyric = approach(self.pose.lyric, target, if any_on_stage { 5. } else { 1.4 }, dt);
    }

    pub fn advance(&mut self, dt: f32, input: &Input) {
        self.pose.fade = approach(self.pose.fade, if input.active { 1. } else { 0. }, FADE, dt);
        self.pose.blend = (self.pose.blend + dt / BLEND).min(1.);
        self.follow_energy(dt, input.level);

        // With nothing on screen the story waits, so the first song still gets its arrival.
        let on_screen = input.active || self.pose.fade > 0.02;
        if on_screen {
            self.stage_time += dt;
        }
        match self.stage {
            _ if !on_screen => {}
            Stage::Flight => {
                self.pose.warp = (self.pose.warp + dt / ENTER).min(1.);
                if self.pose.warp >= 1. {
                    if let Some(shape) = self.next_shape.take() {
                        self.begin_song(shape);
                    }
                    let cover_known = self.cover_ready || (self.has_cover && self.stage_time >= COVER_WAIT);
                    if self.stage_time >= MIN_FLIGHT && cover_known {
                        self.enter(Stage::Arrive);
                    }
                }
            }
            Stage::Arrive => {
                self.pose.warp = (self.pose.warp - dt / ARRIVE).max(0.);
                if self.pose.warp <= 0. {
                    self.enter(Stage::Hold);
                }
            }
            Stage::Hold => {
                if self.stage_time >= HOLD {
                    self.enter(Stage::Unwind);
                }
            }
            Stage::Unwind => {
                self.pose.twist = (self.pose.twist + dt / UNWIND).min(1.);
                if self.pose.twist >= 1. {
                    self.enter(Stage::Galaxy);
                }
            }
            Stage::Galaxy => {}
        }

        // The clip only takes the stage once the galaxy has formed, and leaves before the next song flies in.
        // The pour runs on a steady clock, so each star leaves at its own set moment.
        let wants_clip = input.clip && self.stage == Stage::Galaxy;
        let seconds = match (wants_clip, self.stage) {
            (true, _) => CLIP_POUR_IN,
            (false, Stage::Galaxy) => CLIP_POUR_OUT,
            (false, _) => CLIP_LEAVE,
        };
        let step = dt / seconds;
        self.pose.clip = if wants_clip { (self.pose.clip + step).min(1.) } else { (self.pose.clip - step).max(0.) };

        let cruising = match self.stage {
            Stage::Flight => SPEED * ease(self.pose.warp),
            Stage::Arrive => SPEED * self.pose.warp * self.pose.warp,
            _ => 0.,
        };
        self.pose.speed = approach(self.pose.speed, cruising, 5., dt);
        self.pose.flight = (self.pose.flight + self.pose.speed / TUNNEL * dt).fract();

        let shown_card = match self.stage {
            Stage::Arrive => self.pose.warp < 0.32,
            Stage::Hold => true,
            Stage::Unwind => self.pose.twist < 0.03,
            _ => false,
        };
        self.pose.card =
            approach(self.pose.card, f32::from(u8::from(shown_card)), if shown_card { 6. } else { 7. }, dt);

        let ending = match self.stage {
            Stage::Galaxy if input.duration >= SHORTEST_COLLAPSING_SONG => {
                smooth(input.duration - COLLAPSE_START, input.duration - COLLAPSE_END, input.position)
            }
            Stage::Flight => self.pose.collapse,
            _ => 0.,
        };
        self.pose.collapse = approach(self.pose.collapse, ending, 3., dt);

        if matches!(self.stage, Stage::Unwind | Stage::Galaxy | Stage::Flight) {
            let pace =
                (0.7 + 0.9 * self.pose.energy) * (1. + 5. * self.pose.collapse) * (1. + CHORUS_SPIN * self.pose.chorus);
            self.pose.spin += self.pose.shape.spin * pace * dt;
        }
        let wound = ease(self.pose.twist);
        let pitch = self.pose.shape.pitch + (FACE_ON - self.pose.shape.pitch) * CHORUS_FACE * self.pose.chorus;
        self.pose.tilt = FACE_ON + (pitch - FACE_ON) * wound;
        self.pose.roll = self.pose.shape.roll * wound;
        self.pose.breath =
            (0.84 + 0.24 * self.pose.energy) * (1. - 0.9 * self.pose.collapse) * (1. + CHORUS_OPEN * self.pose.chorus);

        self.follow_chorus(dt, input.position, input.chorus);
        self.follow_lyrics(dt, input.position);
    }
}

#[cfg(test)]
mod tests {
    use super::super::scatter::WordSize;
    use super::*;
    use crate::lyrics::TimedWord;

    const DT: f32 = 1. / 60.;

    fn input(position: f64, duration: f64) -> Input {
        Input { active: true, position, duration, level: 0.12, chorus: true, clip: false }
    }

    fn run(story: &mut Story, seconds: f32, position: f64, duration: f64) {
        for frame in 0..(seconds / DT) as u32 {
            let moment = position + f64::from(frame) * f64::from(DT);
            story.advance(DT, &input(moment, duration));
            if story.can_swap_cover(10.) && !story.cover_ready && story.pose.warp >= 1. {
                story.cover_swapped();
            }
        }
    }

    fn in_galaxy() -> Story {
        let mut story = Story::new();
        story.new_track("first");
        run(&mut story, 12., 0., 200.);
        story
    }

    fn clip_run(story: &mut Story, seconds: f32, clip: bool) {
        for _ in 0..(seconds / DT) as u32 {
            story.advance(DT, &Input { clip, ..input(30., 200.) });
            if story.can_swap_cover(10.) && !story.cover_ready && story.pose.warp >= 1. {
                story.cover_swapped();
            }
        }
    }

    /// The clip takes the stage only from a formed galaxy, gives it back when its frames stop,
    /// and is gone before the next song flies in.
    #[test]
    fn clip_follows_the_galaxy() {
        let mut story = Story::new();
        story.new_track("first");
        clip_run(&mut story, 1., true);
        assert!(story.pose().clip < 0.01, "no clip while the song arrives: {}", story.pose().clip);
        clip_run(&mut story, 12., true);
        assert_eq!(story.stage(), Stage::Galaxy);
        assert!(story.pose().clip > 0.95, "the clip took the stage: {}", story.pose().clip);
        clip_run(&mut story, 3., false);
        assert!(story.pose().clip < 0.05, "the galaxy is back: {}", story.pose().clip);
        clip_run(&mut story, 6., true);
        story.new_track("second");
        clip_run(&mut story, 2., true);
        assert!(story.pose().clip < 0.1, "the clip leaves before the next song: {}", story.pose().clip);
    }

    #[test]
    fn a_song_arrives_as_cover_and_winds_into_a_galaxy() {
        let mut story = Story::new();
        story.new_track("song");
        run(&mut story, 2., 0., 200.);
        assert_eq!(story.stage(), Stage::Arrive);
        run(&mut story, 1.3, 2., 200.);
        assert_eq!(story.stage(), Stage::Hold);
        assert!(story.pose.card > 0.9, "the cover is on screen: {}", story.pose.card);
        assert!((story.pose.tilt - FACE_ON).abs() < 0.001);
        run(&mut story, 3., 3.3, 200.);
        assert_eq!(story.stage(), Stage::Unwind);
        assert!(story.pose.card < 0.05);
        run(&mut story, 4., 6.3, 200.);
        assert_eq!(story.stage(), Stage::Galaxy);
        assert!((story.pose.tilt - story.pose.shape.pitch).abs() < 0.001);
    }

    #[test]
    fn waits_in_hyperspace_for_the_first_cover() {
        let mut story = Story::new();
        story.new_track("song");
        for _ in 0..600 {
            story.advance(DT, &input(0., 200.));
        }
        assert_eq!(story.stage(), Stage::Flight);
        story.cover_swapped();
        run(&mut story, 0.1, 0., 200.);
        assert_eq!(story.stage(), Stage::Arrive);
    }

    #[test]
    fn the_arrival_waits_until_the_music_starts() {
        let mut story = Story::new();
        story.new_track("song");
        story.cover_swapped();
        let paused = Input { active: false, position: 0., duration: 200., level: 0., chorus: true, clip: false };
        for _ in 0..1200 {
            story.advance(DT, &paused);
        }
        assert_eq!(story.stage(), Stage::Flight);
        run(&mut story, 1., 0., 200.);
        assert_eq!(story.stage(), Stage::Flight, "the viewer sees hyperspace first");
        run(&mut story, 0.5, 1., 200.);
        assert_eq!(story.stage(), Stage::Arrive);
    }

    #[test]
    fn the_shape_changes_only_inside_hyperspace() {
        let mut story = in_galaxy();
        let old = story.pose.shape;
        story.new_track("another song");
        story.advance(DT, &input(0., 200.));
        assert_eq!(story.pose.shape, old, "still the old galaxy while leaving");
        run(&mut story, 1., 0., 200.);
        assert_eq!(story.pose.shape, Shape::of("another song"));
        assert_eq!(story.pose.twist, 0.);
    }

    #[test]
    fn a_track_skipped_mid_animation_flies_again_without_jumps() {
        let mut story = Story::new();
        story.new_track("a");
        run(&mut story, 2.6, 0., 200.);
        let warp = story.pose.warp;
        story.new_track("b");
        story.advance(DT, &input(0., 200.));
        assert_eq!(story.stage(), Stage::Flight);
        assert!(story.pose.warp - warp < 0.03, "warp rises smoothly");
    }

    #[test]
    fn the_same_track_always_gets_the_same_galaxy() {
        assert_eq!(Shape::of("DFWOLUHYj0s"), Shape::of("DFWOLUHYj0s"));
        assert_ne!(Shape::of("DFWOLUHYj0s"), Shape::of("DFWOLUHYj0t"));
        for id in ["a", "b", "c", "dQw4w9WgXcQ", "x"] {
            let shape = Shape::of(id);
            assert!((2. ..=5.).contains(&shape.arms), "{shape:?}");
            assert!(shape.hand.abs() == 1.);
        }
    }

    #[test]
    fn the_galaxy_collapses_only_at_the_end() {
        let mut story = in_galaxy();
        run(&mut story, 2., 100., 200.);
        assert!(story.pose.collapse < 0.01);
        run(&mut story, 7., 192.5, 200.);
        assert!(story.pose.collapse > 0.8, "{}", story.pose.collapse);
        run(&mut story, 3., 40., 200.);
        assert!(story.pose.collapse < 0.05, "seeking back restores it");
    }

    fn line(at: f64, text: &str) -> TimedLine {
        TimedLine::plain(at, text)
    }

    fn shapes(count: usize) -> Vec<SpelledWord> {
        (0..count)
            .map(|index| SpelledWord {
                points: vec![[0.01, 0.02]; 10],
                size: WordSize { width: 0.3 + index as f32 * 0.02, height: 0.2, letters: 4 },
                star_size: 0.004,
            })
            .collect()
    }

    fn timed_line(at: f64, count: usize, gap: f64) -> TimedLine {
        let mut sung = line(at, "");
        sung.text = vec!["word"; count].join(" ");
        sung.end = Some(at + gap * count as f64);
        sung.words = (0..count).map(|index| TimedWord { at: at + gap * index as f64, text: "word".into() }).collect();
        sung
    }

    fn visible(story: &Story) -> usize {
        story.pose.words.iter().filter(|word| word[4] > 0.5).count()
    }

    /// Plays frames and answers the text request at once, as the worker thread would.
    fn sing(story: &mut Story, from: f64, seconds: f64) {
        for frame in 0..(seconds * 60.) as u32 {
            story.advance(DT, &input(from + f64::from(frame) / 60., 200.));
            if let Some(request) = story.take_request() {
                story.glyphs_ready(request.generation, shapes(request.words.len()));
            }
        }
    }

    #[test]
    fn a_line_is_requested_ahead_of_the_voice() {
        let mut story = in_galaxy();
        story.set_lines(vec![line(20., "first line"), line(24., "second"), line(26., "")]);
        run(&mut story, 0.1, 19., 200.);
        assert!(story.take_request().is_none());
        run(&mut story, 0.1, 19.5, 200.);
        let request = story.take_request().expect("asked for the first line");
        assert_eq!(request.words, ["first", "line"]);
    }

    #[test]
    fn words_appear_one_by_one_and_each_hangs_after_it_is_sung() {
        let mut story = in_galaxy();
        story.set_lines(vec![timed_line(20., 4, 0.5), line(30., "onward")]);
        sing(&mut story, 19., 1.3);
        assert_eq!(visible(&story), 1, "only the first word so far");
        sing(&mut story, 20.3, 0.5);
        assert_eq!(visible(&story), 2);
        sing(&mut story, 20.8, 1.);
        assert_eq!(visible(&story), 4, "the earlier words are still hanging");
        assert_eq!(story.take_uploads().len(), 4, "each word brought its own shape");
        sing(&mut story, 21.8, 7.);
        assert_eq!(visible(&story), 0, "and then they all dissolve");
        assert!(story.live.iter().all(Option::is_none));
    }

    #[test]
    fn the_word_being_sung_is_the_hot_one() {
        let mut story = in_galaxy();
        story.set_lines(vec![timed_line(20., 3, 0.6), line(30., "onward")]);
        sing(&mut story, 19., 2.4);
        let hot: Vec<f32> = story.pose.words.iter().filter(|word| word[4] > 0.).map(|word| word[5]).collect();
        assert_eq!(hot.len(), 3);
        assert!(hot[2] > 0.9 && hot[0] < 0.5, "{hot:?}");
    }

    #[test]
    fn a_word_lands_big_and_settles() {
        let mut story = in_galaxy();
        story.set_lines(vec![timed_line(20., 2, 1.), line(30., "onward")]);
        sing(&mut story, 19., 1.02);
        let landing = story.pose.words[0][2];
        sing(&mut story, 20.02, 0.6);
        let settled = story.pose.words[0][2];
        assert!(landing > settled * 1.12, "{landing} {settled}");
    }

    #[test]
    fn a_word_snaps_together_just_before_it_is_sung() {
        let mut story = in_galaxy();
        story.set_lines(vec![timed_line(20., 3, 0.5), line(30., "onward")]);
        sing(&mut story, 19., 0.8);
        assert_eq!(story.pose.words[0][4], 0., "nothing shows ahead of time");
        sing(&mut story, 19.8, 0.45);
        assert!(story.pose.words[0][4] > 0.99, "and it is whole right as it is sung");
    }

    fn song_with_choruses() -> Vec<TimedLine> {
        let chorus =
            ["we fly into the dark", "the stars fall in our hands", "I will not let you go", "we burn up in the sky"];
        let first =
            ["the city fades outside", "I walk home on my own", "your words are in my head", "and the night is quiet"];
        let second = [
            "the morning paints the ceiling",
            "the coffee cools on the table",
            "the phone is silent again",
            "I am tired of looking for you",
        ];
        let texts: Vec<&str> = [first, chorus, second, chorus].concat();
        let mut lines: Vec<TimedLine> =
            texts.iter().enumerate().map(|(index, text)| line(10. + index as f64 * 2.5, text)).collect();
        lines.push(line(50., ""));
        lines
    }

    #[test]
    fn a_chorus_lifts_the_galaxy_and_fires_one_wave() {
        let mut story = in_galaxy();
        story.set_lines(song_with_choruses());
        run(&mut story, 2., 16., 200.);
        let verse = story.pose;
        assert!(verse.chorus < 0.01 && verse.shock == 0., "calm in the verse");
        run(&mut story, 1.2, 19., 200.);
        assert!(story.pose.chorus > 0.3, "it starts lifting just before: {}", story.pose.chorus);
        assert!(story.pose.shock > 0., "the wave leaves as the chorus starts");
        run(&mut story, 4., 20.2, 200.);
        assert_eq!(story.pose.shock, 0., "the wave has rolled out and does not repeat");
        assert!(story.pose.chorus > 0.95);
        assert!(story.pose.breath > verse.breath && story.pose.tilt > verse.tilt, "the galaxy opens toward the viewer");
        run(&mut story, 7., 32., 200.);
        assert!(story.pose.chorus < 0.1, "and calms down in the next verse: {}", story.pose.chorus);
        run(&mut story, 1.2, 39.5, 200.);
        assert!(story.pose.shock > 0., "the next chorus gets its own wave");
    }

    #[test]
    fn the_chorus_can_be_switched_off() {
        let mut story = in_galaxy();
        story.set_lines(song_with_choruses());
        for frame in 0..360u32 {
            let moment = 19. + f64::from(frame) / 60.;
            story.advance(
                DT,
                &Input { active: true, position: moment, duration: 200., level: 0.12, chorus: false, clip: false },
            );
            assert!(story.pose.chorus < 0.001 && story.pose.shock == 0.);
        }
    }

    #[test]
    fn chorus_words_are_bigger() {
        let mut story = in_galaxy();
        story.set_lines(song_with_choruses());
        sing(&mut story, 12., 1.);
        let verse = story.pose.words.iter().find(|word| word[4] > 0.).map(|word| word[2]);
        sing(&mut story, 20.3, 0.1);
        sing(&mut story, 20.4, 0.9);
        let biggest = story.pose.words.iter().map(|word| word[2]).fold(0f32, f32::max);
        assert!(biggest > verse.unwrap() * 1.15, "{biggest} {verse:?}");
    }

    /// Words glide. Nothing about them may change abruptly from one frame to the next.
    #[test]
    fn words_never_jump() {
        let mut story = in_galaxy();
        story.set_lines(vec![timed_line(20., 5, 0.35), timed_line(22.2, 4, 0.4), line(30., "onward")]);
        let mut before = story.pose.words;
        for frame in 0..420u32 {
            story.advance(DT, &input(19. + f64::from(frame) / 60., 200.));
            if let Some(request) = story.take_request() {
                story.glyphs_ready(request.generation, shapes(request.words.len()));
            }
            for (now, old) in story.pose.words.iter().zip(&before) {
                if old[4] > 0. && now[4] > 0. {
                    assert!(
                        (now[0] - old[0]).abs() < 0.004 && (now[1] - old[1]).abs() < 0.004,
                        "moved at frame {frame}"
                    );
                    assert!((now[2] - old[2]).abs() < old[2] * 0.03, "resized at frame {frame}: {} {}", old[2], now[2]);
                    assert!((now[3] - old[3]).abs() < 0.004, "turned at frame {frame}");
                    assert!((now[4] - old[4]).abs() < 0.06, "gathered at frame {frame}");
                }
            }
            before = story.pose.words;
        }
    }

    #[test]
    fn the_last_words_of_a_line_overlap_the_next_line() {
        let mut story = in_galaxy();
        story.set_lines(vec![timed_line(20., 3, 0.4), timed_line(21.4, 3, 0.4), line(30., "onward")]);
        sing(&mut story, 19., 2.6);
        assert!(visible(&story) >= 4, "{}", visible(&story));
    }

    #[test]
    fn more_words_than_places_replace_the_oldest() {
        let mut story = in_galaxy();
        story.set_lines(vec![timed_line(20., WORD_SLOTS + 8, 0.05), line(30., "onward")]);
        sing(&mut story, 19., 2.);
        assert!(story.live.iter().flatten().count() <= WORD_SLOTS);
        assert!(story.take_uploads().len() > WORD_SLOTS);
    }

    #[test]
    fn a_new_track_clears_the_words() {
        let mut story = in_galaxy();
        story.set_lines(vec![timed_line(20., 3, 0.4), line(30., "onward")]);
        sing(&mut story, 19., 2.);
        assert!(visible(&story) > 0);
        story.new_track("next");
        sing(&mut story, 0., 1.2);
        assert_eq!(visible(&story), 0);
        assert!(story.prepared.is_empty());
    }

    #[test]
    fn seeking_clears_the_words() {
        let mut story = in_galaxy();
        story.set_lines(vec![timed_line(20., 3, 0.4), line(30., "onward")]);
        sing(&mut story, 19., 2.);
        sing(&mut story, 80., 1.);
        assert_eq!(visible(&story), 0);
    }

    #[test]
    fn no_text_until_the_galaxy_is_formed() {
        let mut story = Story::new();
        story.new_track("song");
        story.set_lines(vec![line(0.5, "early")]);
        run(&mut story, 3., 0.6, 200.);
        assert!(story.take_request().is_none());
    }
}
