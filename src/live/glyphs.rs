//! Turns a line of lyrics into points for the stars to gather on, using the system font.
//! Every word gets its own cloud of points, so it can appear and live on its own.
//! Runs on its own thread, so drawing text never holds up a frame.

use std::{
    ffi::c_void,
    ptr,
    sync::mpsc::{self, Receiver, Sender},
    thread,
};

use super::{scatter::WordSize, story::GlyphRequest, words::STARS_PER_WORD};

/// Bitmap pixels per half of the screen height.
const UNIT: f64 = 500.;
const CANVAS_WIDTH: usize = 2000;
const CANVAS_HEIGHT: usize = 360;
const FONT_SIZE: f64 = 124.;
const WIDEST_WORD: f64 = 1920.;
const INK: u8 = 130;
const UTF8: u32 = 0x0800_0100;
const EMPHASIZED_SYSTEM_FONT: u32 = 3;
const ALPHA_ONLY: u32 = 7;
/// How much of each letter the stars should cover, and the light of one star's soft disc.
const FILL: f32 = 0.9;
const STAR_AREA: f32 = 1.047;
/// Stars per pixel of ink, so a short word is exactly as bright as a long one.
const STARS_PER_INK: f32 = 0.56;
const FEWEST_STARS: usize = 300;
/// From size on screen to size at the depth where text stands.
const STAR_DEPTH_RATIO: f32 = 3.2 / 1.86;

type Object = *const c_void;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFTypeDictionaryKeyCallBacks: [u8; 0];
    static kCFTypeDictionaryValueCallBacks: [u8; 0];
    fn CFRelease(object: Object);
    fn CFStringCreateWithBytes(
        allocator: Object,
        bytes: *const u8,
        count: isize,
        encoding: u32,
        external: u8,
    ) -> Object;
    fn CFDictionaryCreate(
        allocator: Object,
        keys: *const Object,
        values: *const Object,
        count: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> Object;
    fn CFAttributedStringCreate(allocator: Object, text: Object, attributes: Object) -> Object;
}

#[link(name = "CoreText", kind = "framework")]
unsafe extern "C" {
    static kCTFontAttributeName: Object;
    fn CTFontCreateUIFontForLanguage(kind: u32, size: f64, language: Object) -> Object;
    fn CTLineCreateWithAttributedString(text: Object) -> Object;
    fn CTLineGetTypographicBounds(line: Object, ascent: *mut f64, descent: *mut f64, leading: *mut f64) -> f64;
    fn CTLineDraw(line: Object, context: *mut c_void);
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGBitmapContextCreate(
        data: *mut c_void,
        width: usize,
        height: usize,
        bits: usize,
        row_bytes: usize,
        color_space: Object,
        info: u32,
    ) -> *mut c_void;
    fn CGContextRelease(context: *mut c_void);
    fn CGContextFlush(context: *mut c_void);
    fn CGContextSetTextPosition(context: *mut c_void, x: f64, y: f64);
}

struct Font(Object);

impl Font {
    fn new(size: f64) -> Option<Self> {
        let font = unsafe { CTFontCreateUIFontForLanguage(EMPHASIZED_SYSTEM_FONT, size, ptr::null()) };
        (!font.is_null()).then_some(Self(font))
    }
}

impl Drop for Font {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) };
    }
}

struct Line {
    raw: Object,
    width: f64,
    ascent: f64,
    descent: f64,
}

impl Line {
    fn new(text: &str, font: &Font) -> Option<Self> {
        unsafe {
            let string = CFStringCreateWithBytes(ptr::null(), text.as_ptr(), text.len() as isize, UTF8, 0);
            if string.is_null() {
                return None;
            }
            let keys = [kCTFontAttributeName];
            let values = [font.0];
            let attributes = CFDictionaryCreate(
                ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                1,
                (&raw const kCFTypeDictionaryKeyCallBacks).cast(),
                (&raw const kCFTypeDictionaryValueCallBacks).cast(),
            );
            let styled = CFAttributedStringCreate(ptr::null(), string, attributes);
            CFRelease(string);
            CFRelease(attributes);
            if styled.is_null() {
                return None;
            }
            let raw = CTLineCreateWithAttributedString(styled);
            CFRelease(styled);
            if raw.is_null() {
                return None;
            }
            let (mut ascent, mut descent, mut leading) = (0., 0., 0.);
            let width = CTLineGetTypographicBounds(raw, &mut ascent, &mut descent, &mut leading);
            Some(Self { raw, width, ascent, descent })
        }
    }
}

impl Drop for Line {
    fn drop(&mut self) {
        unsafe { CFRelease(self.raw) };
    }
}

/// One word made of stars: its shape around its own center.
pub struct SpelledWord {
    pub points: Vec<[f32; 2]>,
    pub size: WordSize,
    pub star_size: f32,
}

/// Star size that fills a word evenly, however much ink it has.
fn filling_size(ink: usize, stars: usize) -> f32 {
    let area = ink as f32 / (UNIT * UNIT) as f32;
    let radius = (FILL * area / (stars.max(1) as f32 * STAR_AREA)).sqrt();
    (radius * STAR_DEPTH_RATIO).clamp(0.0026, 0.0075)
}

/// Draws one word and reads back where its ink is, around the word's own center.
fn read_word(word: &str) -> Option<(Vec<[f32; 2]>, usize)> {
    let mut font = Font::new(FONT_SIZE)?;
    let mut line = Line::new(word, &font)?;
    if line.width > WIDEST_WORD {
        font = Font::new(FONT_SIZE * WIDEST_WORD / line.width)?;
        line = Line::new(word, &font)?;
    }
    let mut canvas = vec![0u8; CANVAS_WIDTH * CANVAS_HEIGHT];
    unsafe {
        let context = CGBitmapContextCreate(
            canvas.as_mut_ptr().cast(),
            CANVAS_WIDTH,
            CANVAS_HEIGHT,
            8,
            CANVAS_WIDTH,
            ptr::null(),
            ALPHA_ONLY,
        );
        if context.is_null() {
            return None;
        }
        let baseline = CANVAS_HEIGHT as f64 / 2. - (line.ascent - line.descent) / 2.;
        CGContextSetTextPosition(context, (CANVAS_WIDTH as f64 - line.width) / 2., baseline);
        CTLineDraw(line.raw, context);
        CGContextFlush(context);
        CGContextRelease(context);
    }
    let mut found = Vec::new();
    let (mut left, mut right, mut top, mut bottom) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
    for (row, pixels) in canvas.as_chunks::<CANVAS_WIDTH>().0.iter().enumerate() {
        for (column, value) in pixels.iter().enumerate() {
            if *value >= INK {
                let (x, y) = (column as f32 + 0.5, row as f32 + 0.5);
                left = left.min(x);
                right = right.max(x);
                top = top.min(y);
                bottom = bottom.max(y);
                found.push([x, y]);
            }
        }
    }
    if found.is_empty() {
        return None;
    }
    let (middle_x, middle_y) = ((left + right) / 2., (top + bottom) / 2.);
    let unit = UNIT as f32;
    for point in &mut found {
        *point = [(point[0] - middle_x) / unit, (middle_y - point[1]) / unit];
    }
    let ink = found.len();
    // Shuffled, so any subset of stars still covers every letter evenly.
    let mut seed = 0x2F6E_2B1Du32;
    for index in (1..found.len()).rev() {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        found.swap(index, seed as usize % (index + 1));
    }
    found.truncate(((ink as f32 * STARS_PER_INK) as usize).clamp(FEWEST_STARS, STARS_PER_WORD));
    Some((found, ink))
}

/// Turns the words of a line into clouds of points, one per word.
pub fn spell(words: &[String]) -> Vec<SpelledWord> {
    words
        .iter()
        .map(|word| {
            let (points, ink) = read_word(word).unwrap_or_default();
            let reach = |axis: usize| points.iter().fold(0f32, |most, point| most.max(point[axis].abs()));
            let size = WordSize {
                width: reach(0) * 2.,
                height: reach(1) * 2.,
                letters: word.chars().filter(|letter| letter.is_alphanumeric()).count(),
            };
            SpelledWord { star_size: filling_size(ink, points.len()), size, points }
        })
        .collect()
}

pub struct GlyphReply {
    pub generation: u32,
    pub words: Vec<SpelledWord>,
}

pub struct GlyphWorker {
    requests: Sender<GlyphRequest>,
    replies: Receiver<GlyphReply>,
}

impl GlyphWorker {
    pub fn start() -> Self {
        let (requests, incoming) = mpsc::channel::<GlyphRequest>();
        let (outgoing, replies) = mpsc::channel();
        let _ = thread::Builder::new().name("drift-glyphs".into()).spawn(move || {
            while let Ok(mut request) = incoming.recv() {
                // Only the newest line matters if several piled up.
                while let Ok(newer) = incoming.try_recv() {
                    request = newer;
                }
                let reply = GlyphReply { generation: request.generation, words: spell(&request.words) };
                if outgoing.send(reply).is_err() {
                    return;
                }
            }
        });
        Self { requests, replies }
    }

    pub fn request(&self, request: GlyphRequest) {
        let _ = self.requests.send(request);
    }

    pub fn poll(&self) -> Option<GlyphReply> {
        self.replies.try_recv().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn every_word_gets_its_own_shape() {
        let spelled = spell(&words("We dreamed of stars"));
        assert_eq!(spelled.len(), 4);
        for word in &spelled {
            assert!(word.points.len() > 500, "{}", word.points.len());
            assert!(word.points.len() <= STARS_PER_WORD);
            assert!(word.star_size > 0. && word.size.width > 0.04);
            for [x, y] in &word.points {
                assert!(x.abs() <= word.size.width / 2. + 0.01 && y.abs() < 0.3, "{x} {y}");
            }
        }
        assert!(spelled[1].size.width > spelled[2].size.width, "a longer word is wider");
        assert_eq!(spelled[1].size.letters, 7);
    }

    #[test]
    fn a_word_is_centered_on_itself() {
        let word = &spell(&words("cat"))[0];
        let count = word.points.len() as f32;
        let middle = word.points.iter().map(|point| point[0]).sum::<f32>() / count;
        assert!(middle.abs() < 0.08, "{middle}");
    }

    #[test]
    fn a_very_long_word_still_fits() {
        let spelled = spell(&words("pneumonoultramicroscopicsilicovolcanoconiosis"));
        assert!(!spelled[0].points.is_empty());
        assert!(spelled[0].size.width < 4.);
    }

    #[test]
    fn empty_text_has_no_words() {
        assert!(spell(&[]).is_empty());
    }

    #[test]
    fn the_worker_answers() {
        let worker = GlyphWorker::start();
        worker.request(GlyphRequest { generation: 7, words: words("Hello everyone") });
        let reply = (0..400).find_map(|_| {
            thread::sleep(std::time::Duration::from_millis(5));
            worker.poll()
        });
        let reply = reply.expect("a reply");
        assert_eq!(reply.generation, 7);
        assert_eq!(reply.words.len(), 2);
        assert!(!reply.words[0].points.is_empty());
    }
}
