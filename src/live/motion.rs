//! Turns the raw spectrum into smooth motion. Nothing here guesses beats: each value simply
//! follows how loud its part of the sound is right now, within this song's own range.

use super::{BANDS, Frame};

const MIN_SPAN: f32 = 0.08;
const RANGE_FAST: f32 = 1.6;
const RANGE_SLOW: f32 = 0.1;
const ATTACK: f32 = 13.;
const RELEASE: f32 = 3.2;
const QUIET: f32 = 0.002;
const MAX_STEP: f32 = 0.05;
/// The range of the human voice, about 310 to 3600 Hz.
const VOICE_START: usize = 11;
const VOICE_BANDS: usize = 13;
/// A median over 140 ms removes drum hits, which are shorter than a sung syllable.
const VOICE_WINDOW: usize = 13;
const VOICE_SPAN: f32 = 0.05;
const VOICE_ATTACK: f32 = 22.;
const VOICE_HOLD: f32 = 0.07;
const VOICE_RELEASE: f32 = 5.;
const VOICE_DEADBAND: f32 = 0.05;

fn blend(rate: f32, dt: f32) -> f32 {
    1. - (-rate * dt).exp()
}

#[derive(Clone, Copy, Default)]
struct Channel {
    floor: f32,
    ceiling: f32,
    value: f32,
    seeded: bool,
}

impl Channel {
    fn push(&mut self, raw: f32, dt: f32) -> f32 {
        let target = self.level(raw, dt, MIN_SPAN);
        self.settle(target, dt)
    }

    /// Where this value sits within the song's own recent quiet and loud, softly shaped.
    fn level(&mut self, raw: f32, dt: f32, min_span: f32) -> f32 {
        if !self.seeded {
            self.floor = raw;
            self.ceiling = raw + min_span;
            self.seeded = true;
        }
        self.floor += (raw - self.floor) * blend(if raw < self.floor { RANGE_FAST } else { RANGE_SLOW }, dt);
        self.ceiling += (raw - self.ceiling) * blend(if raw > self.ceiling { RANGE_FAST } else { RANGE_SLOW }, dt);
        let span = (self.ceiling - self.floor).max(min_span);
        let level = ((raw - self.floor) / span).clamp(0., 1.);
        level * level * (3. - 2. * level)
    }

    fn settle(&mut self, target: f32, dt: f32) -> f32 {
        let rate = if target > self.value { ATTACK } else { RELEASE };
        self.value += (target - self.value) * blend(rate, dt);
        self.value
    }
}

/// Follows the voice band: rises with a syllable, holds briefly, then falls once.
#[derive(Default)]
struct Voice {
    history: [[f32; VOICE_BANDS]; VOICE_WINDOW],
    next: usize,
    filled: usize,
    range: Channel,
    value: f32,
    wait: f32,
}

impl Voice {
    fn push(&mut self, bands: &[f32; BANDS], dt: f32) -> f32 {
        self.history[self.next].copy_from_slice(&bands[VOICE_START..VOICE_START + VOICE_BANDS]);
        self.next = (self.next + 1) % VOICE_WINDOW;
        self.filled = (self.filled + 1).min(VOICE_WINDOW);
        let mut total = 0.;
        let mut column = [0f32; VOICE_WINDOW];
        for band in 0..VOICE_BANDS {
            for (slot, row) in column[..self.filled].iter_mut().zip(&self.history) {
                *slot = row[band];
            }
            let values = &mut column[..self.filled];
            values.sort_unstable_by(f32::total_cmp);
            total += values[values.len() / 2];
        }
        let target = self.range.level(total / VOICE_BANDS as f32, dt, VOICE_SPAN);
        self.follow(target, dt)
    }

    fn silence(&mut self, dt: f32) -> f32 {
        self.next = 0;
        self.filled = 0;
        self.follow(0., dt)
    }

    fn follow(&mut self, target: f32, dt: f32) -> f32 {
        if target > self.value + VOICE_DEADBAND {
            self.value += (target - self.value) * blend(VOICE_ATTACK, dt);
            self.wait = VOICE_HOLD;
        } else if self.wait > 0. {
            self.wait -= dt;
        } else if target < self.value {
            self.value += (target - self.value) * blend(VOICE_RELEASE, dt);
        }
        self.value
    }
}

#[derive(Default)]
pub struct Motion {
    low: Channel,
    mid: Channel,
    high: Channel,
    voice: Voice,
}

impl Motion {
    /// Replaces the frame's low, mid and high with smooth 0..1 values and adds the voice.
    pub fn apply(&mut self, frame: &mut Frame, dt: f32) {
        let dt = dt.clamp(0., MAX_STEP);
        if frame.level < QUIET {
            frame.low = self.low.settle(0., dt);
            frame.mid = self.mid.settle(0., dt);
            frame.high = self.high.settle(0., dt);
            frame.voice = self.voice.silence(dt);
            return;
        }
        frame.voice = self.voice.push(&frame.bands, dt);
        frame.low = self.low.push(frame.low, dt);
        frame.mid = self.mid.push(frame.mid, dt);
        frame.high = self.high.push(frame.high, dt);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    const DT: f32 = 1. / 93.75;

    fn frame(low: f32, level: f32) -> Frame {
        Frame { bands: [0.; BANDS], level, low, mid: low, high: low, voice: 0., at: Instant::now() }
    }

    #[test]
    fn hard_hits_never_jump() {
        let mut motion = Motion::default();
        let mut previous = 0.;
        let mut highest = 0f32;
        for index in 0..2000 {
            let time = index as f32 * DT;
            let raw = 0.55 + 0.35 * (-(time % 0.5) / 0.04).exp();
            let mut current = frame(raw, 0.2);
            motion.apply(&mut current, DT);
            assert!((0. ..=1.).contains(&current.low));
            assert!((current.low - previous).abs() < 0.14, "jump at {index}");
            previous = current.low;
            highest = highest.max(current.low);
        }
        assert!(highest > 0.3, "still reacts: {highest}");
    }

    #[test]
    fn loud_compressed_music_still_moves() {
        let mut motion = Motion::default();
        let (mut low, mut high) = (1f32, 0f32);
        for index in 0..3000 {
            let raw = 0.8 + 0.05 * (index as f32 * DT * 5.).sin();
            let mut current = frame(raw, 0.3);
            motion.apply(&mut current, DT);
            if index > 1000 {
                low = low.min(current.low);
                high = high.max(current.low);
            }
        }
        assert!(high - low > 0.3, "range {low}..{high}");
    }

    #[test]
    fn silence_fades_out() {
        let mut motion = Motion::default();
        for _ in 0..200 {
            motion.apply(&mut frame(0.9, 0.3), DT);
        }
        let mut current = frame(0., 0.);
        for _ in 0..400 {
            current = frame(0., 0.);
            motion.apply(&mut current, DT);
        }
        assert!(current.low < 0.01);
    }

    fn voice_frame(value: f32) -> Frame {
        let mut current = frame(0.5, 0.2);
        current.bands = [0.4; BANDS];
        for band in &mut current.bands[VOICE_START..VOICE_START + VOICE_BANDS] {
            *band = value;
        }
        current
    }

    #[test]
    fn short_drum_hits_do_not_move_the_voice() {
        let mut motion = Motion::default();
        let mut highest = 0f32;
        for index in 0..1500 {
            let value = if index % 47 < 3 { 0.9 } else { 0.5 };
            let mut current = voice_frame(value);
            motion.apply(&mut current, DT);
            highest = highest.max(current.voice);
        }
        assert!(highest < 0.08, "voice moved to {highest}");
    }

    #[test]
    fn voice_follows_syllables() {
        let mut motion = Motion::default();
        let (mut low, mut high) = (1f32, 0f32);
        for index in 0..2000 {
            let singing = (index as f32 * DT) % 0.8 < 0.4;
            let mut current = voice_frame(if singing { 0.72 } else { 0.5 });
            motion.apply(&mut current, DT);
            if index > 1000 {
                low = low.min(current.voice);
                high = high.max(current.voice);
            }
        }
        assert!(high > 0.7 && low < 0.3, "range {low}..{high}");
    }

    /// Measures smoothness on a recording. DRIFT_RHYTHM is the file.
    #[test]
    fn replay_recording() {
        let Ok(path) = std::env::var("DRIFT_RHYTHM") else { return };
        let data = std::fs::read(path).unwrap();
        let mut motion = Motion::default();
        let (mut previous, mut last_time) = (0f32, None::<f64>);
        let mut steps = Vec::new();
        let mut values = Vec::new();
        let (mut breath, mut breath_steps, mut breath_values) = (0f32, Vec::new(), Vec::new());
        for chunk in data.as_chunks::<164>().0 {
            let time = f64::from_le_bytes(chunk[..8].try_into().unwrap());
            let mut current = crate::live::decode(&chunk[8..], Instant::now()).unwrap();
            let dt = last_time.map_or(DT, |last| (time - last) as f32);
            last_time = Some(time);
            let sounding = current.level >= QUIET;
            motion.apply(&mut current, dt);
            if sounding {
                steps.push((current.low - previous).abs());
                values.push(current.low);
                let before = breath;
                breath += (current.low - breath) * blend(2.6, dt);
                breath_steps.push((breath - before).abs());
                breath_values.push(breath);
            }
            previous = current.low;
        }
        steps.sort_by(f32::total_cmp);
        values.sort_by(f32::total_cmp);
        breath_steps.sort_by(f32::total_cmp);
        breath_values.sort_by(f32::total_cmp);
        let at = |list: &[f32], share: f32| list[((list.len() - 1) as f32 * share) as usize];
        println!(
            "frames {} step per frame: median {:.4}, p99 {:.4}, max {:.4}; value p5 {:.2}, median {:.2}, p95 {:.2}",
            steps.len(),
            at(&steps, 0.5),
            at(&steps, 0.99),
            at(&steps, 1.),
            at(&values, 0.05),
            at(&values, 0.5),
            at(&values, 0.95),
        );
        println!(
            "wallpaper breath: step p99 {:.4}, max {:.4}; value p5 {:.2}, median {:.2}, p95 {:.2}",
            at(&breath_steps, 0.99),
            at(&breath_steps, 1.),
            at(&breath_values, 0.05),
            at(&breath_values, 0.5),
            at(&breath_values, 0.95),
        );
    }
}
