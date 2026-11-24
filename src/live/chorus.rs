//! Finds the chorus of a song in its lyrics: the block of lines that keeps coming back.
//! It reads only the words, so no drum fill can ever set it off by accident.

use std::collections::HashMap;

use super::words;
use crate::lyrics::TimedLine;

/// A repeated block must be at least this many lines, unless one line keeps coming back.
const SHORTEST_BLOCK: usize = 2;
const HOOK_REPEATS: usize = 3;
const SHORTEST_SECTION: f64 = 7.;
/// A chorus is the part that stands out, so it may not take over the whole song.
const LONGEST_SHARE: f64 = 0.62;
const HIGHEST_DEMAND: usize = 6;
/// One short line between two chorus lines still belongs to the chorus.
const BRIDGEABLE: f64 = 4.;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Section {
    pub start: f64,
    pub end: f64,
}

/// The words of a line without case, punctuation or backing vocals in brackets.
fn key(text: &str) -> String {
    let mut depth = 0usize;
    let mut out = String::with_capacity(text.len());
    for letter in text.chars().flat_map(char::to_lowercase) {
        match letter {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            _ if depth > 0 => {}
            // Russian spells one letter two ways, yo and ye; they count as one, so repeats still match.
            '\u{451}' => out.push('\u{435}'),
            _ if letter.is_alphanumeric() => out.push(letter),
            _ if letter.is_whitespace() && !out.is_empty() && !out.ends_with(' ') => out.push(' '),
            _ => {}
        }
    }
    out.trim_end().to_string()
}

/// Lines that are part of a block repeated somewhere else in the song.
fn repeated_lines(keys: &[String]) -> Vec<bool> {
    let count = keys.len();
    let mut marked = vec![false; count];
    let mut below = vec![0usize; count + 2];
    for first in (0..count).rev() {
        let mut here = vec![0usize; count + 2];
        for second in (first + 1..count).rev() {
            if keys[first].is_empty() || keys[first] != keys[second] {
                continue;
            }
            here[second] = below[second + 1] + 1;
            let starts_here = first == 0 || keys[first - 1] != keys[second - 1];
            if starts_here && here[second] >= SHORTEST_BLOCK {
                marked[first..first + here[second]].fill(true);
                marked[second..second + here[second]].fill(true);
            }
        }
        below = here;
    }
    marked
}

fn sections_of(lines: &[TimedLine], keys: &[String], marked: &[bool]) -> Vec<Section> {
    let mut sections = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if !marked[index] {
            index += 1;
            continue;
        }
        let (first, mut last) = (index, index);
        let mut probe = index + 1;
        while probe < lines.len() {
            if marked[probe] {
                last = probe;
                probe += 1;
            } else if probe + 1 < lines.len()
                && marked[probe + 1]
                && lines[probe + 1].at - words::line_end(lines, last) <= BRIDGEABLE
            {
                probe += 1;
            } else {
                break;
            }
        }
        let sung = (first..=last).filter(|line| !keys[*line].is_empty()).count();
        let section = Section { start: lines[first].at, end: words::line_end(lines, last) };
        if sung >= SHORTEST_BLOCK && section.end - section.start >= SHORTEST_SECTION {
            sections.push(section);
        }
        index = last + 1;
    }
    sections
}

pub fn find(lines: &[TimedLine]) -> Vec<Section> {
    let keys: Vec<String> = lines.iter().map(|line| key(&line.text)).collect();
    let mut repeats: HashMap<&str, usize> = HashMap::new();
    for line in keys.iter().filter(|line| !line.is_empty()) {
        *repeats.entry(line).or_default() += 1;
    }
    let times = |index: usize| repeats.get(keys[index].as_str()).copied().unwrap_or(0);

    let mut candidates = repeated_lines(&keys);
    for (index, marked) in candidates.iter_mut().enumerate() {
        *marked |= times(index) >= HOOK_REPEATS;
    }
    let sung: f64 = (0..lines.len())
        .filter(|index| !keys[*index].is_empty())
        .map(|index| words::line_end(lines, index) - lines[index].at)
        .sum();
    let share =
        |sections: &[Section]| sections.iter().map(|section| section.end - section.start).sum::<f64>() / sung.max(1.);

    // When nearly everything repeats, the chorus is what repeats the most.
    let mut best = sections_of(lines, &keys, &candidates);
    for demand in 3..=HIGHEST_DEMAND {
        if share(&best) <= LONGEST_SHARE {
            break;
        }
        let stricter: Vec<bool> =
            candidates.iter().enumerate().map(|(index, marked)| *marked && times(index) >= demand).collect();
        let sections = sections_of(lines, &keys, &stricter);
        if sections.is_empty() {
            break;
        }
        best = sections;
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(texts: &[&str], seconds_per_line: f64) -> Vec<TimedLine> {
        let mut lines: Vec<TimedLine> = texts
            .iter()
            .enumerate()
            .map(|(index, text)| TimedLine::plain(10. + index as f64 * seconds_per_line, *text))
            .collect();
        lines.push(TimedLine::plain(10. + texts.len() as f64 * seconds_per_line, ""));
        lines
    }

    const VERSE_ONE: [&str; 4] =
        ["the city fades outside", "I walk home on my own", "your words are in my head", "and the night is quiet"];
    const VERSE_TWO: [&str; 4] = [
        "the morning paints the ceiling",
        "the coffee cools on the table",
        "the phone is silent again",
        "I am tired of looking for you",
    ];
    const CHORUS: [&str; 4] =
        ["we fly into the dark", "the stars fall in our hands", "I will not let you go", "we burn up in the sky"];

    #[test]
    fn finds_the_block_that_comes_back() {
        let texts: Vec<&str> = [VERSE_ONE, CHORUS, VERSE_TWO, CHORUS].concat();
        let lines = song(&texts, 2.5);
        let found = find(&lines);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].start, lines[4].at);
        assert_eq!(found[1].start, lines[12].at);
        assert!(found[0].end <= lines[8].at + 0.01, "{found:?}");
    }

    #[test]
    fn punctuation_and_backing_vocals_do_not_matter() {
        let varied = [
            "We fly into the dark!",
            "The stars fall in our hands (oh-oh-oh)",
            "I will not, let you go",
            "we burn up in the sky...",
        ];
        let texts: Vec<&str> = [VERSE_ONE, CHORUS, VERSE_TWO, varied].concat();
        assert_eq!(find(&song(&texts, 2.5)).len(), 2);
    }

    #[test]
    fn russian_yo_and_ye_are_one_letter() {
        // "Stars" in Russian, written once with yo and once with ye.
        assert_eq!(
            key("\u{437}\u{432}\u{451}\u{437}\u{434}\u{44b}"),
            key("\u{437}\u{432}\u{435}\u{437}\u{434}\u{44b}")
        );
    }

    #[test]
    fn a_song_with_no_repeats_has_no_chorus() {
        let texts: Vec<&str> = [VERSE_ONE, VERSE_TWO].concat();
        assert!(find(&song(&texts, 2.5)).is_empty());
    }

    #[test]
    fn one_repeated_line_alone_is_not_a_chorus() {
        let texts = ["one", "two", "chorus", "three", "four", "chorus", "five", "six"];
        assert!(find(&song(&texts, 2.5)).is_empty());
    }

    #[test]
    fn a_chorus_never_takes_the_whole_song() {
        let hook = ["our chorus", "here again", "our chorus", "here again"];
        let texts: Vec<&str> = [hook, VERSE_ONE, hook, hook, CHORUS, hook, CHORUS, hook].concat();
        let lines = song(&texts, 2.5);
        let found = find(&lines);
        assert!(!found.is_empty());
        let total: f64 = found.iter().map(|section| section.end - section.start).sum();
        assert!(total < (lines.last().unwrap().at - lines[0].at) * 0.75, "{total}");
    }

    #[test]
    fn too_short_a_repeat_is_ignored() {
        let texts = ["one", "two", "three", "one", "two", "four"];
        assert!(find(&song(&texts, 1.)).is_empty());
    }
}
