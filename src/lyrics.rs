//! Time-synced lyrics: a music video's own subtitles, or LRCLIB, an open lyrics catalog.
//! LRCLIB is only called when the user turns this on, because it sends the track name to that service.

use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::Duration,
};

use serde::Deserialize;

const ENDPOINT: &str = "https://lrclib.net/api/get";
const SEARCH: &str = "https://lrclib.net/api/search";
const TIMEOUT: Duration = Duration::from_secs(8);
const MAX_REPLY_BYTES: u64 = 4 * 1024 * 1024;
/// A search result counts as the same recording only if its length is this close, in seconds.
const SAME_LENGTH: f64 = 6.;
const SAME_LENGTH_FOR_WORDS: f64 = 2.5;
const REMEMBERED_SONGS: usize = 256;

/// A word of a line and the moment it starts, when the catalog knows it.
#[derive(Clone, Debug, PartialEq)]
pub struct TimedWord {
    pub at: f64,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TimedLine {
    pub at: f64,
    pub text: String,
    pub end: Option<f64>,
    pub words: Vec<TimedWord>,
}

impl TimedLine {
    pub fn plain(at: f64, text: impl Into<String>) -> Self {
        Self { at, text: text.into(), end: None, words: Vec::new() }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Reply {
    duration: Option<f64>,
    synced_lyrics: Option<String>,
    has_word_sync: Option<bool>,
    lyricsfile: Option<String>,
}

fn parse_stamp(stamp: &str) -> Option<f64> {
    let (minutes, seconds) = stamp.split_once(':')?;
    Some(minutes.parse::<f64>().ok()? * 60. + seconds.parse::<f64>().ok()?)
}

pub fn parse_lrc(source: &str) -> Vec<TimedLine> {
    let mut lines = Vec::new();
    for raw in source.lines() {
        let mut rest = raw.trim();
        let mut stamps = Vec::new();
        while let Some(body) = rest.strip_prefix('[') {
            let Some((stamp, tail)) = body.split_once(']') else { break };
            let Some(at) = parse_stamp(stamp) else { break };
            stamps.push(at);
            rest = tail;
        }
        let text = rest.trim();
        lines.extend(stamps.into_iter().map(|at| TimedLine::plain(at, text)));
    }
    lines.sort_by(|a, b| a.at.total_cmp(&b.at));
    lines
}

fn sheet_value(raw: &str) -> String {
    let value = raw.strip_prefix(' ').unwrap_or(raw);
    if let Some(body) = value.strip_prefix('\'').and_then(|inner| inner.strip_suffix('\'')) {
        return body.replace("''", "'");
    }
    if let Some(body) = value.strip_prefix('"').and_then(|inner| inner.strip_suffix('"')) {
        return body.replace("\\\"", "\"").replace("\\\\", "\\");
    }
    value.trim_end().to_string()
}

fn sheet_seconds(raw: &str) -> Option<f64> {
    Some(raw.trim().parse::<f64>().ok()? / 1000.)
}

/// The catalog times pieces of text. A word is every piece up to the next space.
fn join_pieces(pieces: Vec<(f64, String)>) -> Vec<TimedWord> {
    let mut words = Vec::new();
    let mut open: Option<TimedWord> = None;
    for (at, piece) in pieces {
        let ends_word = piece.ends_with(char::is_whitespace);
        let part = piece.trim();
        match &mut open {
            Some(word) => word.text.push_str(part),
            None => open = Some(TimedWord { at, text: part.to_string() }),
        }
        if ends_word {
            words.extend(open.take());
        }
    }
    words.extend(open);
    words.retain(|word| !word.text.is_empty());
    words
}

/// Reads the catalog's word timing sheet. Empty unless most lines really have timed words.
fn parse_sheet(source: &str) -> Vec<TimedLine> {
    let mut lines = Vec::new();
    let mut current: Option<(TimedLine, Vec<(f64, String)>)> = None;
    let mut inside = false;
    let mut finish = |current: &mut Option<(TimedLine, Vec<(f64, String)>)>| {
        if let Some((mut line, pieces)) = current.take() {
            line.words = join_pieces(pieces);
            lines.push(line);
        }
    };
    for raw in source.lines() {
        if !inside {
            inside = raw.starts_with("lines:");
            continue;
        }
        if !raw.is_empty() && !raw.starts_with([' ', '-']) {
            break;
        }
        if let Some(rest) = raw.strip_prefix("- text:") {
            finish(&mut current);
            current = Some((TimedLine::plain(0., sheet_value(rest).trim()), Vec::new()));
        } else if let Some((line, pieces)) = &mut current {
            if let Some(rest) = raw.strip_prefix("  - text:") {
                pieces.push((line.at, sheet_value(rest)));
            } else if let Some(rest) = raw.strip_prefix("    start_ms:") {
                if let (Some(piece), Some(at)) = (pieces.last_mut(), sheet_seconds(rest)) {
                    piece.0 = at;
                }
            } else if let Some(rest) = raw.strip_prefix("  start_ms:") {
                line.at = sheet_seconds(rest).unwrap_or(line.at);
            } else if let Some(rest) = raw.strip_prefix("  end_ms:") {
                line.end = sheet_seconds(rest);
            }
        }
    }
    finish(&mut current);
    lines.sort_by(|a, b| a.at.total_cmp(&b.at));
    let sung = lines.iter().filter(|line| !line.text.is_empty()).count();
    let timed = lines.iter().filter(|line| !line.words.is_empty()).count();
    if sung == 0 || timed * 5 < sung * 3 {
        return Vec::new();
    }
    lines
}

/// Index of the line being sung at `position`, if any has started.
pub fn active_line(lines: &[TimedLine], position: f64) -> Option<usize> {
    lines.partition_point(|line| line.at <= position).checked_sub(1)
}

/// `Ok(None)` means the catalog has no such song. `Err` means it could not be asked.
fn ask(url: &str, query: &[(&str, &str)]) -> Result<Option<String>, ureq::Error> {
    let mut request = ureq::get(url);
    for (key, value) in query {
        request = request.query(*key, *value);
    }
    let reply =
        request.header("User-Agent", "Drift music player").config().timeout_global(Some(TIMEOUT)).build().call();
    match reply {
        Ok(mut response) => response.body_mut().with_config().limit(MAX_REPLY_BYTES).read_to_string().map(Some),
        Err(ureq::Error::StatusCode(404)) => Ok(None),
        Err(error) => Err(error),
    }
}

fn timed(source: Option<String>) -> Option<Vec<TimedLine>> {
    let lines = parse_lrc(&source?);
    (!lines.is_empty()).then_some(lines)
}

fn word_lines(reply: &Reply) -> Option<Vec<TimedLine>> {
    if reply.has_word_sync != Some(true) {
        return None;
    }
    let lines = parse_sheet(reply.lyricsfile.as_deref()?);
    (!lines.is_empty()).then_some(lines)
}

/// The closest recording that has the time of every word. A word needs a tighter match than a line.
fn closest_words(replies: &[Reply], duration: f64) -> Option<Vec<TimedLine>> {
    replies
        .iter()
        .filter(|reply| reply.has_word_sync == Some(true))
        .filter_map(|reply| Some(((reply.duration? - duration).abs(), reply)))
        .filter(|(gap, _)| *gap <= SAME_LENGTH_FOR_WORDS)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .and_then(|(_, reply)| word_lines(reply))
}

/// The catalog entry of about the same length that has timed lines.
fn closest(replies: Vec<Reply>, duration: f64) -> Option<Vec<TimedLine>> {
    replies
        .into_iter()
        .filter(|reply| reply.synced_lyrics.is_some())
        .filter_map(|reply| Some(((reply.duration? - duration).abs(), reply.synced_lyrics)))
        .filter(|(gap, _)| *gap <= SAME_LENGTH)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .and_then(|(_, source)| timed(source))
}

fn lookup(title: &str, artist: &str, duration: f64) -> Result<Option<Vec<TimedLine>>, ureq::Error> {
    let seconds = (duration.round() as u64).to_string();
    let exact = ask(ENDPOINT, &[("track_name", title), ("artist_name", artist), ("duration", &seconds)])?;
    let reply = exact.and_then(|body| serde_json::from_str::<Reply>(&body).ok());
    if let Some(lines) = reply.as_ref().and_then(word_lines) {
        return Ok(Some(lines));
    }
    let lines = reply.and_then(|reply| timed(reply.synced_lyrics));
    let found = match ask(SEARCH, &[("track_name", title), ("artist_name", artist)]) {
        Ok(found) => found,
        Err(_) if lines.is_some() => return Ok(lines),
        Err(error) => return Err(error),
    };
    let replies = found.and_then(|body| serde_json::from_str::<Vec<Reply>>(&body).ok()).unwrap_or_default();
    if let Some(words) = closest_words(&replies, duration) {
        return Ok(Some(words));
    }
    Ok(lines.or_else(|| closest(replies, duration)))
}

// ---- A music video's own subtitles ----------------------------------------------------------

/// Fewer lines than this are a credit or a cue, not the words of a song.
const FEWEST_CAPTION_LINES: usize = 3;
/// Cues that subtitles put in parentheses. Anything else in parentheses is sung, like backing vocals.
const CUES: &[&str] = &["music", "applause", "laughter", "instrumental", "inaudible", "cheering", "singing"];

/// What the page found for the music video that is playing.
#[derive(Debug, PartialEq)]
pub enum Captions {
    /// Subtitles someone wrote: the words as they are sung, timed by the line.
    Written(Vec<TimedLine>),
    /// Speech recognition: every word has its moment, but music often makes it mishear.
    Recognized(Vec<TimedLine>),
    None,
}

/// Reads the page's answer about the subtitles of the music video `video`.
pub fn read_captions(reply: &serde_json::Value, video: &str) -> Captions {
    let field = |name: &str| reply.get(name).and_then(serde_json::Value::as_str);
    if field("video") != Some(video) {
        return Captions::None;
    }
    let lines = field("body").map(parse_captions).unwrap_or_default();
    if lines.len() < FEWEST_CAPTION_LINES {
        return Captions::None;
    }
    match field("kind") {
        Some("written") => Captions::Written(lines),
        Some("asr") => Captions::Recognized(lines),
        _ => Captions::None,
    }
}

#[derive(Deserialize)]
struct CaptionSheet {
    #[serde(default)]
    events: Vec<CaptionEvent>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CaptionEvent {
    t_start_ms: Option<f64>,
    d_duration_ms: Option<f64>,
    #[serde(default)]
    segs: Vec<CaptionPiece>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CaptionPiece {
    #[serde(default)]
    utf8: String,
    t_offset_ms: Option<f64>,
}

/// Subtitles in YouTube's json3 format, as timed lines. Speech-recognized ones also time each word.
pub fn parse_captions(source: &str) -> Vec<TimedLine> {
    let Ok(sheet) = serde_json::from_str::<CaptionSheet>(source) else { return Vec::new() };
    let mut lines = Vec::new();
    for event in sheet.events {
        let Some(start) = event.t_start_ms else { continue };
        let raw: String = event.segs.iter().map(|piece| piece.utf8.as_str()).collect();
        let text = clean_caption(&raw);
        if text.is_empty() {
            continue;
        }
        // Speech recognition sends a line word by word, each with its offset from the line's start.
        let timed = event.segs.iter().skip(1).any(|piece| piece.t_offset_ms.is_some());
        let words = if timed {
            event
                .segs
                .iter()
                .filter_map(|piece| {
                    let word = clean_caption(&piece.utf8);
                    let at = (start + piece.t_offset_ms.unwrap_or(0.)) / 1000.;
                    (!word.is_empty()).then_some(TimedWord { at, text: word })
                })
                .collect()
        } else {
            Vec::new()
        };
        let end = event.d_duration_ms.map(|duration| (start + duration) / 1000.);
        lines.push(TimedLine { at: start / 1000., text, end, words });
    }
    lines.sort_by(|a, b| a.at.total_cmp(&b.at));
    // Subtitles linger on screen; a line is over once the next one starts.
    for index in 1..lines.len() {
        let next = lines[index].at;
        let line = &mut lines[index - 1];
        if line.end.is_none_or(|end| end > next) {
            line.end = Some(next);
        }
    }
    lines
}

/// Leaves what is sung: no music notes, no cues in square brackets such as [Music], no cue words
/// in parentheses such as (applause), and no dashes or chevrons that mark who speaks.
fn clean_caption(raw: &str) -> String {
    let mut kept = String::with_capacity(raw.len());
    let mut in_cue = false;
    for ch in raw.chars() {
        match ch {
            '[' => in_cue = true,
            ']' => in_cue = false,
            _ if in_cue => {}
            '♪' | '♫' | '♬' | '♩' | '🎵' | '🎶' => kept.push(' '),
            _ => kept.push(ch),
        }
    }
    let mut text = String::with_capacity(kept.len());
    let mut rest = kept.as_str();
    while let Some(open) = rest.find('(') {
        let Some(close) = rest[open..].find(')').map(|offset| open + offset) else { break };
        text.push_str(&rest[..open]);
        if !CUES.contains(&rest[open + 1..close].trim().to_lowercase().as_str()) {
            text.push_str(&rest[open..=close]);
        }
        rest = &rest[close + 1..];
    }
    text.push_str(rest);
    let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let sung = joined.trim_start_matches(['-', '–', '—', '>', ' ']);
    if sung.chars().any(char::is_alphanumeric) { sung.to_string() } else { String::new() }
}

/// Each song is asked for once per launch, however many parts of the app need its words.
pub fn fetch(title: &str, artist: &str, duration: f64) -> Option<Vec<TimedLine>> {
    static KNOWN: OnceLock<Mutex<HashMap<String, Option<Vec<TimedLine>>>>> = OnceLock::new();
    let known = KNOWN.get_or_init(Default::default);
    let key = format!("{title}\n{artist}\n{}", duration.round() as u64);
    if let Some(answer) = known.lock().ok()?.get(&key) {
        return answer.clone();
    }
    let answer = lookup(title, artist, duration).ok()?;
    let mut known = known.lock().ok()?;
    if known.len() >= REMEMBERED_SONGS {
        known.clear();
    }
    known.insert(key, answer.clone());
    answer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_sorts_lrc() {
        let lines = parse_lrc("[00:18.50] Second\n[00:03.10] First\n[ti:Title]\n[01:00.00][01:30.00] Chorus\n");
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0].text, "First");
        assert!((lines[0].at - 3.1).abs() < 0.001);
        assert_eq!(lines[3].at, 90.);
    }

    #[test]
    fn finds_active_line() {
        let lines = parse_lrc("[00:05.00] a\n[00:10.00] b\n");
        assert_eq!(active_line(&lines, 2.), None);
        assert_eq!(active_line(&lines, 5.), Some(0));
        assert_eq!(active_line(&lines, 99.), Some(1));
    }

    const SHEET: &str = "version: '1.0'\nmetadata:\n  title: Test\n  duration_ms: 90000\nlines:\n- text: 'one two-three'\n  start_ms: 1000\n  end_ms: 3000\n  words:\n  - text: 'one '\n    start_ms: 1000\n  - text: two-\n    start_ms: 1500\n  - text: three\n    start_ms: 1900\n- text: 'it''s me'\n  start_ms: 4000\n  end_ms: 5000\n  words:\n  - text: 'it''s '\n    start_ms: 4000\n  - text: me\n    start_ms: 4400\nplain: |\n  one two-three\n  - text: not a line\n";

    #[test]
    fn reads_word_times_and_joins_pieces_of_a_word() {
        let lines = parse_sheet(SHEET);
        assert_eq!(lines.len(), 2);
        assert_eq!((lines[0].at, lines[0].end), (1., Some(3.)));
        let words: Vec<(&str, f64)> = lines[0].words.iter().map(|word| (word.text.as_str(), word.at)).collect();
        assert_eq!(words, [("one", 1.), ("two-three", 1.5)]);
        assert_eq!(lines[1].text, "it's me");
        assert_eq!(lines[1].words[0].text, "it's");
        assert_eq!(lines[1].words[1].at, 4.4);
    }

    /// Asks the real catalog about a well-known song. Runs only when DRIFT_LRCLIB is set.
    #[test]
    fn the_catalog_gives_word_times() {
        if std::env::var_os("DRIFT_LRCLIB").is_none() {
            return;
        }
        let lines = fetch("Yesterday", "The Beatles", 125.).expect("the song is in the catalog");
        let timed = lines.iter().filter(|line| line.words.len() >= 2).count();
        assert!(timed * 2 > lines.len(), "{timed} of {}", lines.len());
        let line = lines.iter().find(|line| line.words.len() >= 2).unwrap();
        assert!(line.words[1].at > line.words[0].at);
        println!("real catalog: {} lines, {timed} with word times", lines.len());
    }

    #[test]
    fn a_sheet_without_timed_words_is_not_used() {
        assert!(parse_sheet("version: '1.0'\nlines:\n- text: only a line\n  start_ms: 1000\n").is_empty());
    }

    #[test]
    fn prefers_a_close_recording_with_word_times() {
        let entry = |duration: f64, sheet: bool| Reply {
            duration: Some(duration),
            synced_lyrics: Some("[00:01.00] line".into()),
            has_word_sync: Some(sheet),
            lyricsfile: sheet.then(|| SHEET.to_string()),
        };
        assert!(closest_words(&[entry(172., false), entry(174., true)], 172.).is_some());
        assert!(closest_words(&[entry(172., false), entry(180., true)], 172.).is_none());
    }

    const WRITTEN: &str = r#"{"wireMagic":"pb3","events":[
        {"tStartMs":0,"dDurationMs":200000,"id":1,"wpWinPosId":1,"wsWinStyleId":1},
        {"tStartMs":1000,"dDurationMs":2500,"segs":[{"utf8":"♪ First line ♪"}]},
        {"tStartMs":3000,"dDurationMs":1000,"segs":[{"utf8":"[Music]"}]},
        {"tStartMs":5000,"dDurationMs":9000,"segs":[{"utf8":"- Second\nline (oh yeah)"}]},
        {"tStartMs":8000,"dDurationMs":2000,"segs":[{"utf8":"(Applause)"}]},
        {"tStartMs":9000,"dDurationMs":2000,"segs":[{"utf8":"Third "},{"utf8":"line"}]}
    ]}"#;

    const RECOGNIZED: &str = r#"{"events":[
        {"tStartMs":0,"dDurationMs":219440,"id":1,"wpWinPosId":1,"wsWinStyleId":1},
        {"tStartMs":14390,"dDurationMs":3920,"wWinId":1,"segs":[{"utf8":"[Music]"}]},
        {"tStartMs":17000,"dDurationMs":5000,"wWinId":1,"segs":[{"utf8":"I","acAsrConf":0},{"utf8":" got","tOffsetMs":240,"acAsrConf":0},{"utf8":" it","tOffsetMs":500,"acAsrConf":0}]},
        {"tStartMs":19000,"wWinId":1,"aAppend":1,"segs":[{"utf8":"\n"}]},
        {"tStartMs":20000,"dDurationMs":4000,"wWinId":1,"segs":[{"utf8":"we"},{"utf8":" go","tOffsetMs":300}]},
        {"tStartMs":23000,"dDurationMs":4000,"wWinId":1,"segs":[{"utf8":"once"},{"utf8":" [Music]","tOffsetMs":200},{"utf8":" more","tOffsetMs":600}]}
    ]}"#;

    #[test]
    fn written_subtitles_keep_what_is_sung() {
        let lines = parse_captions(WRITTEN);
        let texts: Vec<&str> = lines.iter().map(|line| line.text.as_str()).collect();
        assert_eq!(texts, ["First line", "Second line (oh yeah)", "Third line"]);
        assert_eq!((lines[0].at, lines[0].end), (1., Some(3.5)));
        assert_eq!(lines[1].end, Some(9.), "a lingering line ends where the next begins");
        assert!(lines.iter().all(|line| line.words.is_empty()), "written subtitles time lines, not words");
    }

    #[test]
    fn recognized_subtitles_time_every_word() {
        let lines = parse_captions(RECOGNIZED);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].text, "I got it");
        let words: Vec<(&str, f64)> = lines[0].words.iter().map(|word| (word.text.as_str(), word.at)).collect();
        assert_eq!(words.len(), 3);
        assert!(
            words
                .iter()
                .zip([("I", 17.), ("got", 17.24), ("it", 17.5)])
                .all(|(a, b)| a.0 == b.0 && (a.1 - b.1).abs() < 1e-9)
        );
        assert_eq!(lines[0].end, Some(20.));
        assert_eq!(lines[2].text, "once more");
        assert_eq!(lines[2].words.len(), 2, "cues between words are dropped");
    }

    #[test]
    fn takes_only_the_asked_video_and_real_subtitles() {
        let reply =
            |video: &str, kind: &str, body: &str| serde_json::json!({ "video": video, "kind": kind, "body": body });
        assert!(
            matches!(read_captions(&reply("v1", "written", WRITTEN), "v1"), Captions::Written(lines) if lines.len() == 3)
        );
        assert!(matches!(read_captions(&reply("v1", "asr", RECOGNIZED), "v1"), Captions::Recognized(_)));
        assert_eq!(read_captions(&reply("v2", "written", WRITTEN), "v1"), Captions::None, "another video's words");
        let cue_only =
            r#"{"events":[{"tStartMs":0,"segs":[{"utf8":"[Music]"}]},{"tStartMs":9000,"segs":[{"utf8":"♪"}]}]}"#;
        assert_eq!(read_captions(&reply("v1", "written", cue_only), "v1"), Captions::None);
        assert_eq!(read_captions(&serde_json::json!({ "video": "v1", "kind": "none" }), "v1"), Captions::None);
        assert_eq!(read_captions(&serde_json::Value::Null, "v1"), Captions::None);
    }

    #[test]
    fn search_takes_the_closest_recording_with_timed_lines() {
        let reply = |duration: f64, text: Option<&str>| Reply {
            duration: Some(duration),
            synced_lyrics: text.map(str::to_string),
            has_word_sync: None,
            lyricsfile: None,
        };
        let found = closest(
            vec![
                reply(200., Some("[00:01.00] live version")),
                reply(171., None),
                reply(176., Some("[00:01.00] a bit off")),
                reply(173., Some("[00:01.00] studio")),
            ],
            172.,
        )
        .unwrap();
        assert_eq!(found[0].text, "studio");
        assert!(closest(vec![reply(200., Some("[00:01.00] other"))], 172.).is_none());
    }
}
