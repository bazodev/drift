//! Decides which word of a line is being sung right now.
//! When the catalog knows the time of every word, that is exact. Otherwise each word gets an
//! estimated start from a model learned on 222 songs with real word times.

use crate::lyrics::TimedLine;

/// How many words can be on screen at once, and how many stars each one is made of.
pub const WORD_SLOTS: usize = 16;
pub const STARS_PER_WORD: usize = 6000;
/// A word starts gathering a little early, so it is readable when the voice reaches it.
/// A word snaps into being a moment before it is sung.
const WORD_LEAD: f64 = 0.16;
const LINE_LIMIT: f64 = 8.;
/// How long a line stays after its last word, when the catalog knows where it ends.
const LINGER: f64 = 0.7;
const SHORTEST_WINDOW: f64 = 0.2;
const SHORTEST_HOT_WORD: f64 = 0.2;
/// After it is sung, a word hangs in the air for this long.
const HOLD_SHORTEST: f64 = 1.05;
const HOLD_LONGEST: f64 = 1.75;
/// Fast singing holds each word for less, so about this many words share the screen.
const WORDS_ON_SCREEN: f64 = 6.;
const LAST_WORD_EXTRA: f64 = 0.55;
const HOLD_AFTER_SINGING: f64 = 0.3;
/// A word crumbles slowly, unless the singing is so fast that the dust would pile up.
pub const CRUMBLE_LONGEST: f64 = 3.4;
const CRUMBLE_SHORTEST: f64 = 1.5;

// How long words take relative to each other, learned from real word times.
const UNITS_PER_WORD: f32 = 0.2;
const UNITS_PER_LETTER: f32 = 0.32;
const UNITS_AFTER_PAUSE: f32 = 2.55;
const UNITS_AFTER_STOP: f32 = 1.;
const UNITS_ON_LAST_WORD: f32 = 4.5;
/// How fast a song is sung: this share of its lines is slower.
const RATE_PERCENTILE: f32 = 0.55;
/// Singing never fills more than this share of the time until the next line.
const LINE_FILL: f32 = 0.9;
const TYPICAL_RATE: f32 = 7.8;
const SLOWEST_RATE: f32 = 0.3;

/// The life of one word: when it gathers, is sung, stops being sung and dissolves.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WordTime {
    pub appear: f64,
    pub start: f64,
    pub hot_until: f64,
    pub leave: f64,
    /// How many seconds it takes the word to crumble into dust after it leaves.
    pub crumble: f64,
}

/// Sung syllables in a word: every Russian vowel, or every group of Latin vowels.
fn syllables(word: &str) -> f32 {
    let lower = word.to_lowercase();
    let mut count = 0;
    let mut in_latin_vowels = false;
    for letter in lower.chars() {
        // The ten Cyrillic vowel letters.
        if "\u{430}\u{435}\u{451}\u{438}\u{43e}\u{443}\u{44b}\u{44d}\u{44e}\u{44f}".contains(letter) {
            count += 1;
            in_latin_vowels = false;
        } else if "aeiouy".contains(letter) {
            if !in_latin_vowels {
                count += 1;
            }
            in_latin_vowels = true;
        } else {
            in_latin_vowels = false;
        }
    }
    count.max(1) as f32
}

/// How long this word tends to take compared with others.
fn word_units(word: &str, last: bool) -> f32 {
    let letters = word.chars().filter(|letter| letter.is_alphabetic()).count() as f32;
    let ending = word.trim_end().chars().last();
    let mut units = UNITS_PER_WORD + syllables(word) + UNITS_PER_LETTER * letters;
    if ending.is_some_and(|mark| ",;:—–-".contains(mark)) {
        units += UNITS_AFTER_PAUSE;
    }
    if ending.is_some_and(|mark| ".!?…".contains(mark)) {
        units += UNITS_AFTER_STOP;
    }
    if last {
        units += UNITS_ON_LAST_WORD;
    }
    units
}

fn line_words(line: &TimedLine) -> Vec<String> {
    if line.words.is_empty() {
        line.text.split_whitespace().map(str::to_string).collect()
    } else {
        line.words.iter().map(|word| word.text.clone()).collect()
    }
}

fn line_units(words: &[String]) -> f32 {
    words.iter().enumerate().map(|(index, word)| word_units(word, index + 1 == words.len())).sum()
}

/// How fast this song is sung, judged from all of its lines.
pub fn song_rate(lines: &[TimedLine]) -> f32 {
    let mut rates: Vec<f32> = lines
        .windows(2)
        .filter_map(|pair| {
            let gap = pair[1].at - pair[0].at;
            let words = line_words(&pair[0]);
            (words.len() >= 2 && (0.5..=12.).contains(&gap)).then(|| line_units(&words) / gap.min(LINE_LIMIT) as f32)
        })
        .collect();
    if rates.len() < 3 {
        return TYPICAL_RATE;
    }
    rates.sort_by(f32::total_cmp);
    let place = (rates.len() - 1) as f32 * RATE_PERCENTILE;
    let below = place.floor() as usize;
    let above = (below + 1).min(rates.len() - 1);
    rates[below] + (rates[above] - rates[below]) * place.fract()
}

/// When a line should leave the screen.
pub fn line_end(lines: &[TimedLine], index: usize) -> f64 {
    let line = &lines[index];
    let next = lines.get(index + 1).map_or(f64::INFINITY, |next| next.at);
    match line.end {
        Some(end) => (end + LINGER).min(next),
        None => next.min(line.at + LINE_LIMIT),
    }
}

/// Estimated word starts for a line that only has its own start time.
fn estimate(words: &[String], at: f64, window: f64, rate: f32) -> (Vec<f64>, f64) {
    let total = line_units(words);
    if total <= 0. {
        return (Vec::new(), at);
    }
    let window = window.max(SHORTEST_WINDOW) as f32;
    let per_unit = (1. / rate.max(SLOWEST_RATE)).min(LINE_FILL * window / total);
    let mut passed = 0.;
    let starts = words
        .iter()
        .enumerate()
        .map(|(index, word)| {
            let start = at + f64::from(passed * per_unit);
            passed += word_units(word, index + 1 == words.len());
            start
        })
        .collect();
    (starts, at + f64::from(total * per_unit))
}

/// The word timing of one line.
pub struct WordClock {
    words: Vec<String>,
    starts: Vec<f64>,
    end: f64,
}

impl WordClock {
    pub fn new(lines: &[TimedLine], index: usize, rate: f32) -> Self {
        let line = &lines[index];
        let words = line_words(line);
        let leaves = line_end(lines, index);
        let (starts, end) = if line.words.is_empty() {
            estimate(&words, line.at, leaves - line.at, rate)
        } else {
            (line.words.iter().map(|word| word.at).collect(), line.end.unwrap_or(leaves))
        };
        Self { words, starts, end }
    }

    pub fn words(&self) -> &[String] {
        &self.words
    }

    /// Every word lives on its own, so each one can hang a little after it is sung.
    pub fn timings(&self) -> Vec<WordTime> {
        let count = self.starts.len();
        let typical =
            if count > 1 { (self.starts[count - 1] - self.starts[0]) / (count - 1) as f64 } else { HOLD_LONGEST };
        let hold = (typical * WORDS_ON_SCREEN).clamp(HOLD_SHORTEST, HOLD_LONGEST);
        let crumble = (typical * (WORD_SLOTS - 2) as f64 - hold).clamp(CRUMBLE_SHORTEST, CRUMBLE_LONGEST);
        self.starts
            .iter()
            .enumerate()
            .map(|(index, &start)| {
                let next = self.starts.get(index + 1).copied();
                let hot_until = next.unwrap_or(self.end).max(start + SHORTEST_HOT_WORD);
                let extra = if next.is_none() { LAST_WORD_EXTRA } else { 0. };
                WordTime {
                    appear: start - WORD_LEAD,
                    start,
                    hot_until,
                    leave: (start + hold + extra).max(hot_until + HOLD_AFTER_SINGING),
                    crumble,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::TimedWord;

    fn line(at: f64, text: &str) -> TimedLine {
        TimedLine::plain(at, text)
    }

    fn starts(lines: &[TimedLine], index: usize) -> Vec<f64> {
        WordClock::new(lines, index, song_rate(lines)).starts
    }

    #[test]
    fn counts_sung_syllables() {
        // "Cat" in Russian: two vowels, two syllables.
        assert_eq!(syllables("\u{43a}\u{43e}\u{442}\u{430}"), 2.);
        assert_eq!(syllables("hmm"), 1.);
        assert_eq!(syllables("Yesterday"), 3.);
    }

    #[test]
    fn exact_words_follow_the_catalog() {
        let mut sung = line(10., "one two three");
        sung.end = Some(12.);
        sung.words = vec![
            TimedWord { at: 10., text: "one".into() },
            TimedWord { at: 10.8, text: "two".into() },
            TimedWord { at: 11.4, text: "three".into() },
        ];
        let times = WordClock::new(&[sung], 0, TYPICAL_RATE).timings();
        assert_eq!(times.iter().map(|time| time.start).collect::<Vec<_>>(), [10., 10.8, 11.4]);
        assert!((times[1].appear - (10.8 - WORD_LEAD)).abs() < 1e-9, "appears just before it is sung");
        assert_eq!(times[0].hot_until, 10.8);
        assert_eq!(times[2].hot_until, 12.);
    }

    #[test]
    fn each_word_hangs_after_it_is_sung() {
        let lines = vec![line(10., "one two three four"), line(14., "onward")];
        let times = WordClock::new(&lines, 0, TYPICAL_RATE).timings();
        for time in &times {
            assert!(time.leave - time.start >= HOLD_SHORTEST, "{time:?}");
            assert!(time.leave > time.hot_until, "{time:?}");
        }
        let last = times.last().unwrap();
        let before = times[times.len() - 2];
        assert!(last.leave - last.start > before.leave - before.start, "the last word stays longest");
    }

    #[test]
    fn fast_singing_keeps_the_screen_from_filling_up() {
        let mut fast = line(10., "");
        fast.words =
            (0..40).map(|index| TimedWord { at: 10. + f64::from(index) * 0.25, text: "yeah".into() }).collect();
        let times = WordClock::new(&[fast], 0, TYPICAL_RATE).timings();
        let moment = times[30].start;
        let alive = times.iter().filter(|time| time.appear <= moment && moment < time.leave + time.crumble).count();
        assert!(alive <= WORD_SLOTS, "{alive}");
    }

    #[test]
    fn words_crumble_slowly() {
        let lines = vec![line(10., "one two three four"), line(14., "onward")];
        for time in WordClock::new(&lines, 0, TYPICAL_RATE).timings() {
            assert!(time.crumble >= CRUMBLE_SHORTEST && time.crumble <= CRUMBLE_LONGEST, "{time:?}");
        }
        let mut calm = line(10., "");
        calm.words = (0..4).map(|index| TimedWord { at: 10. + f64::from(index) * 0.6, text: "word".into() }).collect();
        let times = WordClock::new(&[calm], 0, TYPICAL_RATE).timings();
        assert_eq!(times[0].crumble, CRUMBLE_LONGEST);
    }

    #[test]
    fn the_rate_comes_from_the_whole_song() {
        let slow: Vec<TimedLine> =
            (0..12).map(|index| line(f64::from(index) * 4., "one two three four five six")).collect();
        let fast: Vec<TimedLine> =
            (0..12).map(|index| line(f64::from(index) * 2., "one two three four five six")).collect();
        assert!((song_rate(&fast) / song_rate(&slow) - 2.).abs() < 0.01);
        assert_eq!(song_rate(&fast[..2]), TYPICAL_RATE);
    }

    #[test]
    fn line_end_is_the_next_line_or_a_limit() {
        let lines = vec![line(10., "a"), line(12., "b"), line(40., "c")];
        assert_eq!(line_end(&lines, 0), 12.);
        assert_eq!(line_end(&lines, 1), 20.);
        assert_eq!(line_end(&lines, 2), 48.);
    }

    #[test]
    fn estimated_words_come_in_order_before_the_next_line() {
        let lines: Vec<TimedLine> =
            (0..8).map(|index| line(f64::from(index) * 3., "one two three four five")).collect();
        let times = starts(&lines, 2);
        assert_eq!(times[0], 6.);
        assert!(times.windows(2).all(|pair| pair[0] < pair[1]), "{times:?}");
        assert!(*times.last().unwrap() < 9., "{times:?}");
    }

    #[test]
    fn a_comma_leaves_room_for_a_breath() {
        let plain: Vec<TimedLine> = (0..8).map(|index| line(f64::from(index) * 3., "one two three four")).collect();
        let breath: Vec<TimedLine> = (0..8).map(|index| line(f64::from(index) * 3., "one, two three four")).collect();
        let (a, b) = (starts(&plain, 2), starts(&breath, 2));
        assert!(b[1] - b[0] > (a[1] - a[0]) * 1.2, "{a:?} {b:?}");
    }

    #[test]
    fn a_long_gap_after_a_line_does_not_stretch_its_words() {
        let mut lines: Vec<TimedLine> =
            (0..8).map(|index| line(f64::from(index) * 2.5, "one two three four five")).collect();
        lines[5] = line(12.5, "one two three four five");
        lines[6] = line(30., "one two three four five");
        lines[7] = line(32.5, "one two three four five");
        let normal = starts(&lines, 2);
        let before_break = starts(&lines, 5);
        let normal_span = normal.last().unwrap() - normal[0];
        let break_span = before_break.last().unwrap() - before_break[0];
        assert!((break_span - normal_span).abs() < 0.5, "{normal_span} {break_span}");
    }

    /// Measures the estimate on songs with real word times. DRIFT_WORD_TRUTH is the file.
    #[test]
    fn measure_against_real_word_times() {
        let Ok(path) = std::env::var("DRIFT_WORD_TRUTH") else { return };
        let songs: Vec<serde_json::Value> = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let mut errors = Vec::new();
        for song in &songs {
            let real: Vec<(f64, Vec<(f64, String)>)> = song["lines"]
                .as_array()
                .unwrap()
                .iter()
                .map(|entry| {
                    let words = entry["words"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|word| (word[0].as_f64().unwrap(), word[1].as_str().unwrap().to_string()));
                    (entry["at"].as_f64().unwrap(), words.collect())
                })
                .collect();
            let plain: Vec<TimedLine> = real
                .iter()
                .map(|(at, words)| line(*at, &words.iter().map(|word| word.1.as_str()).collect::<Vec<_>>().join(" ")))
                .collect();
            let rate = song_rate(&plain);
            for index in 0..plain.len().saturating_sub(1) {
                let gap = plain[index + 1].at - plain[index].at;
                if real[index].1.len() < 2 || !(0.5..=12.).contains(&gap) {
                    continue;
                }
                let clock = WordClock::new(&plain, index, rate);
                for (start, (when, _)) in clock.starts.iter().zip(&real[index].1) {
                    errors.push((start - when).abs());
                }
            }
        }
        errors.sort_by(f64::total_cmp);
        let share = |limit: f64| errors.partition_point(|error| *error <= limit) as f64 / errors.len() as f64 * 100.;
        println!(
            "{} words: median {:.3} s, mean {:.3} s, within 0.15 s {:.0}%, within 0.3 s {:.0}%",
            errors.len(),
            errors[errors.len() / 2],
            errors.iter().sum::<f64>() / errors.len() as f64,
            share(0.15),
            share(0.3),
        );
    }

    #[test]
    fn empty_lines_have_no_words() {
        let lines = vec![line(1., ""), line(2., "a b")];
        let clock = WordClock::new(&lines, 0, TYPICAL_RATE);
        assert!(clock.words().is_empty());
        assert!(clock.timings().is_empty());
    }
}
