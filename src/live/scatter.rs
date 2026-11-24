//! Finds a spot in the air for each word, around the core of the galaxy, the way seeds sit in a
//! sunflower. Every word turns a golden angle from the last, so they spread without forming a
//! grid, and a word never lands on one that is still hanging there.

use std::f32::consts::TAU;

const GOLDEN_ANGLE: f32 = 2.399_963;
/// Screens are wide, so the words spread wide too.
const STRETCH: f32 = 1.6;
const LIMIT_X: f32 = 1.32;
const LIMIT_Y: f32 = 0.76;
/// Clear air around a word, with room for the small punch it makes as it lands.
const MARGIN: f32 = 0.03;
const BREATHING: f32 = 1.08;
/// How finely the sky is searched for a free spot.
const GRID_STEP: f32 = 0.035;
const MAX_TILT: f32 = 0.1;
/// When the air is full a word first gets smaller, and only then may land on the oldest word.
const SHRINK: f32 = 0.86;
const SHRINK_TRIES: usize = 6;

/// The bright core keeps its place, so words circle around it instead of drowning in it.
const CORE: Area = Area { at: [0., 0.03], half: [0.3, 0.19] };

/// A word's size at scale 1, in half-screen-heights.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WordSize {
    pub width: f32,
    pub height: f32,
    pub letters: usize,
}

/// Where a word stands: its center, size, slight tilt and distance from the camera.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Place {
    pub at: [f32; 2],
    pub scale: f32,
    pub turn: f32,
    pub depth: f32,
}

/// The air a placed word takes up.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Area {
    at: [f32; 2],
    half: [f32; 2],
}

impl Area {
    pub fn of(place: &Place, size: &WordSize) -> Self {
        Self { at: place.at, half: half_size(size, place.scale, place.turn) }
    }
}

fn half_size(size: &WordSize, scale: f32, turn: f32) -> [f32; 2] {
    let (sin, cos) = (turn.sin().abs(), turn.cos().abs());
    let (half_width, half_height) = (size.width * 0.5 * scale * BREATHING, size.height * 0.5 * scale * BREATHING);
    [half_width * cos + half_height * sin + MARGIN, half_width * sin + half_height * cos + MARGIN]
}

fn is_free(spot: [f32; 2], half: [f32; 2], taken: &[Area]) -> bool {
    taken.iter().all(|other| {
        (spot[0] - other.at[0]).abs() >= half[0] + other.half[0]
            || (spot[1] - other.at[1]).abs() >= half[1] + other.half[1]
    })
}

/// The free spot closest to the ideal one, if the word fits anywhere at all.
fn find_spot(ideal: [f32; 2], half: [f32; 2], taken: &[Area]) -> Option<[f32; 2]> {
    let room = [LIMIT_X - half[0], LIMIT_Y - half[1]];
    if room[0] < 0. || room[1] < 0. {
        return None;
    }
    let columns = (room[0] * 2. / GRID_STEP).ceil().max(1.) as u32;
    let rows = (room[1] * 2. / GRID_STEP).ceil().max(1.) as u32;
    let mut best: Option<([f32; 2], f32)> = None;
    for row in 0..=rows {
        for column in 0..=columns {
            let spot = [
                -room[0] + 2. * room[0] * column as f32 / columns as f32,
                -room[1] + 2. * room[1] * row as f32 / rows as f32,
            ];
            let distance = (spot[0] - ideal[0]).powi(2) + (spot[1] - ideal[1]).powi(2);
            if best.is_none_or(|(_, nearest)| distance < nearest)
                && is_free(spot, half, &[CORE])
                && is_free(spot, half, taken)
            {
                best = Some((spot, distance));
            }
        }
    }
    best.map(|(spot, _)| spot)
}

/// Remembers where the last word went, so the next one lands somewhere else.
pub struct Scatter {
    random: u32,
    angle: f32,
}

impl Scatter {
    pub fn new(seed: u32) -> Self {
        let mut scatter = Self { random: seed.wrapping_mul(0x9E37_79B1) | 1, angle: 0. };
        for _ in 0..4 {
            scatter.next();
        }
        scatter.angle = scatter.next() * TAU;
        scatter
    }

    fn next(&mut self) -> f32 {
        self.random ^= self.random << 13;
        self.random ^= self.random >> 17;
        self.random ^= self.random << 5;
        self.random as f32 / u32::MAX as f32
    }

    /// Finds a spot for the next word. `taken` lists the words in the air, those leaving soonest first.
    pub fn place(&mut self, size: &WordSize, taken: &[Area], closing: bool, boost: f32) -> Place {
        let letters = size.letters.clamp(1, 10) as f32;
        // Short words stand bigger, so a small word still carries weight on a poster.
        let mut scale = (1.12 - 0.045 * letters).max(0.72) * (0.84 + 0.46 * self.next());
        if closing {
            scale *= 1.14;
        }
        scale *= boost;
        let widest = 2. * (LIMIT_X - MARGIN) * 0.9 / (size.width * BREATHING).max(0.05);
        scale = scale.min(widest);
        let turn = (self.next() - 0.5) * 2. * MAX_TILT;
        let depth = -0.45 + 0.8 * self.next();
        self.angle = (self.angle + GOLDEN_ANGLE).rem_euclid(TAU);
        let radius = (0.1 + 0.25 * (taken.len() as f32).sqrt()).min(0.66);
        let ideal = [self.angle.cos() * radius * STRETCH, self.angle.sin() * radius];

        for clear_of in 0..=taken.len() {
            let mut tried = scale;
            for _ in 0..=SHRINK_TRIES {
                let half = half_size(size, tried, turn);
                if let Some(at) = find_spot(ideal, half, &taken[clear_of..]) {
                    return Place { at, scale: tried, turn, depth };
                }
                tried *= SHRINK;
            }
        }
        Place { at: [0., 0.46], scale: scale.min(widest * 0.9), turn: 0., depth }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size(index: usize) -> WordSize {
        WordSize { width: 0.32 + (index % 5) as f32 * 0.17, height: 0.22, letters: 2 + index % 7 }
    }

    fn overlap(a: &Area, b: &Area) -> bool {
        (a.at[0] - b.at[0]).abs() < a.half[0] + b.half[0] - 0.001
            && (a.at[1] - b.at[1]).abs() < a.half[1] + b.half[1] - 0.001
    }

    #[test]
    fn words_never_land_on_each_other_and_stay_on_screen() {
        for seed in 0..40 {
            let mut scatter = Scatter::new(seed);
            let mut air: Vec<Area> = Vec::new();
            for index in 0..150 {
                let word = size(index);
                let place = scatter.place(&word, &air, index % 5 == 4, 1.);
                let area = Area::of(&place, &word);
                assert!(place.at[0].abs() + area.half[0] <= LIMIT_X + 0.001, "seed {seed}: {place:?}");
                assert!(place.at[1].abs() + area.half[1] <= LIMIT_Y + 0.001, "seed {seed}: {place:?}");
                assert!(!overlap(&area, &CORE), "seed {seed} word {index} is on the core");
                for other in &air {
                    assert!(!overlap(&area, other), "seed {seed} word {index}");
                }
                air.push(area);
                if air.len() > 6 {
                    air.remove(0);
                }
            }
        }
    }

    #[test]
    fn words_spread_out_and_differ_in_size() {
        let mut scatter = Scatter::new(7);
        let mut air = Vec::new();
        let mut places = Vec::new();
        for index in 0..6 {
            let word = size(index);
            let place = scatter.place(&word, &air, false, 1.);
            air.push(Area::of(&place, &word));
            places.push(place);
        }
        let heights: Vec<f32> = places.iter().map(|place| place.at[1]).collect();
        let spread = heights.iter().fold(f32::MIN, |a, b| a.max(*b)) - heights.iter().fold(f32::MAX, |a, b| a.min(*b));
        assert!(spread > 0.3, "{heights:?}");
        assert!(places.iter().any(|place| (place.scale - places[0].scale).abs() > 0.05));
    }

    #[test]
    fn a_crowded_sky_still_gives_a_place() {
        let mut scatter = Scatter::new(3);
        let mut air = Vec::new();
        for index in 0..40 {
            let word = size(index);
            let place = scatter.place(&word, &air, false, 1.);
            air.push(Area::of(&place, &word));
        }
        assert_eq!(air.len(), 40);
    }

    #[test]
    fn a_huge_word_still_fits() {
        let huge = WordSize { width: 4.2, height: 0.24, letters: 30 };
        let place = Scatter::new(1).place(&huge, &[], true, 1.4);
        assert!(place.scale * 4.2 <= 2. * LIMIT_X);
    }
}
