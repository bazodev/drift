//! A Dynamic Island around the MacBook notch: compact while playing, full player on hover.

#[cfg(debug_assertions)]
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, Ordering};
use std::{
    cell::Cell,
    ffi::{c_char, c_void},
    rc::Rc,
    time::{Duration, Instant},
};

use gpui::{
    AnyElement, App, AppContext, Bounds, BoxShadow, Context, Hsla, MouseButton, MouseDownEvent, ObjectFit, Pixels,
    Render, Rgba, ScrollDelta, ScrollWheelEvent, SharedString, Task, TouchPhase, Window, WindowBackgroundAppearance,
    WindowBounds, WindowHandle, WindowKind, WindowOptions, canvas, div, fill, img, linear_color_stop, linear_gradient,
    point, prelude::*, px, rgb, rgba, size,
};
use objc2::{
    class, msg_send,
    runtime::{AnyClass, AnyObject, Bool, Sel},
    sel,
};
use objc2_foundation::{NSEdgeInsets, NSPoint, NSRect, NSSize};

use super::{
    BANDS, Bus,
    palette::{self, Palette},
    wallpaper::{self, Cover, Playback, Wallpaper},
};
use crate::{
    engine::{Command, PlayerState, QueueState},
    images, lyrics,
    model::ctx,
    prefs::Prefs,
    ui::{FAINT, MUTED, RAISED, Shell, TEXT, artwork, icon, round_button},
};

const CANVAS_W: f32 = 560.;
const CANVAS_H: f32 = 248.;
const WING: f32 = 66.;
const OPEN_W: f32 = 500.;
const OPEN_H: f32 = 208.;
const BUTTON_H: f32 = 28.;
/// How far two fingers must travel before a swipe counts, and how clearly along one axis.
const SWIPE_DISTANCE: f32 = 56.;
const SWIPE_CLARITY: f32 = 1.5;
/// While swiping, the player leans after the fingers by this share of their travel, up to a limit.
const LEAN_SHARE: f32 = 0.16;
const LEAN_LIMIT: f32 = 14.;
/// How long the sign of a recognized swipe stays, and how far it floats along the swipe.
const HINT_TIME: f32 = 0.65;
const HINT_TRAVEL: f32 = 16.;
/// The player reports a change with a delay, so a repeated swipe trusts what was just asked.
const ASKED_MEMORY: Duration = Duration::from_millis(1500);
/// Two fingers commit to an axis once they have moved this far.
const AXIS_LOCK: f32 = 8.;
const CARD_SIZE: f32 = 76.;
const CARD_ART: u32 = 152;
const PAGE_GAP: f32 = 18.;
/// Cards travel a little further than the fingers, so a short swipe shows the whole next one.
const STRIP_GAIN: f32 = 1.35;
/// With nothing to swipe to, the strip stretches this far and comes back.
const STRIP_STRETCH: f32 = 34.;
/// If the player has not switched by then, the strip goes back to the track still playing.
const STRIP_PATIENCE: Duration = Duration::from_millis(2500);
const FALLBACK_NOTCH: Notch = Notch { width: 0., height: 30. };
const COMPACT_BARS: usize = 7;
const GROW_WIDTH: f32 = 8.;
const GROW_HEIGHT: f32 = 1.5;
const HELPER_RETRY: Duration = Duration::from_secs(12);
const HELPER_GRACE: Duration = Duration::from_secs(3);
const ACTIVE_TICK: Duration = Duration::from_millis(16);
/// A switch between song and clip the page has not followed is asked for again after this.
const CLIP_RETRY: Duration = Duration::from_secs(4);
/// The page keeps sending frames for a few seconds per ask, so the ask is renewed this often.
const CLIP_RENEW: Duration = Duration::from_secs(2);
const IDLE_TICK: Duration = Duration::from_millis(120);

/// Window flags: all spaces, stationary, outside the window cycle, over fullscreen apps.
const COLLECTION_BEHAVIOR: usize = 1 | (1 << 4) | (1 << 6) | (1 << 8);

#[derive(Clone, Copy, Debug, PartialEq)]
struct Notch {
    width: f32,
    height: f32,
}

/// The parts of the live Mac that can be switched right from the island.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Effect {
    Wallpaper,
    Words,
    Glow,
    Pulse,
    Chorus,
}

impl Effect {
    const ALL: [Self; 5] = [Self::Wallpaper, Self::Words, Self::Chorus, Self::Glow, Self::Pulse];

    fn label(self) -> &'static str {
        match self {
            Self::Wallpaper => "Wallpaper",
            Self::Words => "Words",
            Self::Glow => "Glow",
            Self::Pulse => "Pulse",
            Self::Chorus => "Chorus",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Wallpaper => "sparkles",
            Self::Words => "lyrics",
            Self::Glow => "glow",
            Self::Pulse => "pulse",
            Self::Chorus => "burst",
        }
    }

    fn is_on(self, prefs: &Prefs) -> bool {
        match self {
            Self::Wallpaper => prefs.live_wallpaper,
            Self::Words => prefs.synced_lyrics,
            Self::Glow => prefs.island_glow,
            Self::Pulse => prefs.island_pulse,
            Self::Chorus => prefs.chorus_burst,
        }
    }

    fn set(self, prefs: &mut Prefs, on: bool) {
        match self {
            Self::Wallpaper => prefs.live_wallpaper = on,
            Self::Words => prefs.synced_lyrics = on,
            Self::Glow => prefs.island_glow = on,
            Self::Pulse => prefs.island_pulse = on,
            Self::Chorus => prefs.chorus_burst = on,
        }
    }

    /// Words live in the galaxy, on the desktop or in the window, and the chorus is found in the words.
    fn has_effect(self, prefs: &Prefs) -> bool {
        let galaxy = prefs.live_wallpaper || prefs.window_galaxy;
        match self {
            Self::Words => galaxy,
            Self::Chorus => galaxy && prefs.synced_lyrics,
            _ => true,
        }
    }
}

/// A two-finger swipe, named by where the fingers went.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Swipe {
    Left,
    Right,
    Up,
    Down,
}

impl Swipe {
    /// Like turning pages: fingers to the right go back, to the left go forward.
    fn icon(self) -> &'static str {
        match self {
            Self::Left => "next",
            Self::Right => "previous",
            Self::Up => "pause",
            Self::Down => "play",
        }
    }

    fn direction(self) -> (f32, f32) {
        match self {
            Self::Left => (-1., 0.),
            Self::Right => (1., 0.),
            Self::Up => (0., -1.),
            Self::Down => (0., 1.),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Axis {
    #[default]
    Undecided,
    Across,
    Upright,
}

/// Finds one swipe per touch. Momentum after the fingers leave the trackpad is ignored.
#[derive(Default)]
struct SwipeTracker {
    touching: bool,
    done: bool,
    axis: Axis,
    travel: (f32, f32),
}

impl SwipeTracker {
    /// Takes the finger movement, positive to the right and down. Pause and play answer at once.
    /// A sideways drag carries the cards, and only counts when the fingers lift.
    fn feed(&mut self, phase: TouchPhase, dx: f32, dy: f32) -> Option<Swipe> {
        match phase {
            TouchPhase::Started => *self = Self { touching: true, ..Self::default() },
            TouchPhase::Ended => {
                let swipe = self.released();
                *self = Self::default();
                return swipe;
            }
            TouchPhase::Moved => {}
        }
        if !self.touching {
            return None;
        }
        self.travel.0 += dx;
        self.travel.1 += dy;
        let (x, y) = self.travel;
        if self.axis == Axis::Undecided && x.abs().max(y.abs()) >= AXIS_LOCK {
            self.axis = if x.abs() >= y.abs() { Axis::Across } else { Axis::Upright };
        }
        if self.axis != Axis::Upright || self.done || y.abs() < SWIPE_DISTANCE || y.abs() < x.abs() * SWIPE_CLARITY {
            return None;
        }
        self.done = true;
        Some(if y > 0. { Swipe::Down } else { Swipe::Up })
    }

    fn released(&self) -> Option<Swipe> {
        let (x, y) = self.travel;
        if self.axis != Axis::Across || x.abs() < SWIPE_DISTANCE || x.abs() < y.abs() * SWIPE_CLARITY {
            return None;
        }
        Some(if x > 0. { Swipe::Right } else { Swipe::Left })
    }

    /// How far the fingers are carrying the cards sideways.
    fn drag(&self) -> f32 {
        if self.touching && self.axis == Axis::Across { self.travel.0 } else { 0. }
    }

    /// How far the player should lean up or down after the fingers right now.
    fn lean(&self) -> f32 {
        if !self.touching || self.done || self.axis != Axis::Upright {
            return 0.;
        }
        (self.travel.1 * LEAN_SHARE).clamp(-LEAN_LIMIT, LEAN_LIMIT)
    }
}

/// Where the playing track sits in the queue and what is on either side of it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Around {
    here: Option<usize>,
    before: Option<usize>,
    after: Option<usize>,
}

/// The queue can report a moment later than the player, so the place comes from the track itself.
fn neighbors(queue: &QueueState, track: &str) -> Around {
    let here = queue
        .items
        .iter()
        .enumerate()
        .filter(|(_, item)| item.video_id == track)
        .min_by_key(|(index, _)| index.abs_diff(queue.index))
        .map(|(index, _)| index);
    let Some(index) = here else { return Around::default() };
    Around { here, before: index.checked_sub(1), after: (index + 1 < queue.items.len()).then_some(index + 1) }
}

/// Where the strip of cards wants to be for this much drag. With nothing on that side it only stretches.
fn strip_target(drag: f32, has_before: bool, has_after: bool, page: f32) -> f32 {
    let open = if drag > 0. { has_before } else { has_after };
    if open { drag.clamp(-page, page) } else { drag.signum() * STRIP_STRETCH * (1. - 1. / (drag.abs() / 90. + 1.)) }
}

/// One track on the strip of cards.
struct Card {
    title: SharedString,
    artist: SharedString,
    art: String,
}

/// Cover art kept at one size in memory, however large it is drawn while the card moves.
fn card_art(urls: [&str; 2], size: f32, cx: &mut App) -> AnyElement {
    let found = urls.iter().filter(|url| !url.is_empty()).find_map(|url| {
        let wanted = images::sized(url, CARD_ART);
        ctx(cx).images.update(cx, |store, _| store.get(&wanted))
    });
    let frame = div()
        .size(px(size))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(12.))
        .overflow_hidden()
        .bg(rgba(RAISED));
    match found {
        Some(image) => {
            frame.child(img(image).size_full().rounded(px(12.)).object_fit(ObjectFit::Cover)).into_any_element()
        }
        None => frame.child(icon("note", size * 0.38, FAINT)).into_any_element(),
    }
}

/// A card shrinks and dims as it moves away from the middle, like the next one in a deck.
#[allow(clippy::too_many_arguments)]
fn track_card(
    card: &Card,
    spare_art: &str,
    x: f32,
    width: f32,
    page: f32,
    wave: Option<Vec<f32>>,
    accent: Rgba,
    cx: &mut App,
) -> AnyElement {
    let away = (x.abs() / page).min(1.);
    let mut info = div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(2.))
        .child(div().truncate().text_base().font_weight(gpui::FontWeight::BOLD).child(card.title.clone()))
        .child(div().truncate().text_sm().text_color(rgba(MUTED)).child(card.artist.clone()));
    if let Some(values) = wave {
        info = info.child(div().mt(px(6.)).h(px(22.)).w_full().child(bars(values, Hsla::from(accent), 2.)));
    }
    div()
        .absolute()
        .left(px(x))
        .top_0()
        .bottom_0()
        .w(px(width))
        .flex()
        .items_center()
        .gap(px(14.))
        .opacity(1. - 0.6 * away)
        .child(div().size(px(CARD_SIZE)).flex_none().flex().items_center().justify_center().child(card_art(
            [&card.art, spare_art],
            CARD_SIZE * (1. - 0.16 * away),
            cx,
        )))
        .child(info)
        .into_any_element()
}

/// Scroll deltas follow the system's scroll direction, and a swipe should follow the fingers.
/// Also says whether the system took this touch away, as when a gesture of its own begins.
fn scroll_event() -> (f32, bool) {
    const SCROLL_WHEEL_EVENT: usize = 22;
    const PHASE_CANCELLED: usize = 1 << 4;
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let event: *mut AnyObject = msg_send![app, currentEvent];
        if event.is_null() {
            return (1., false);
        }
        let kind: usize = msg_send![event, type];
        if kind != SCROLL_WHEEL_EVENT {
            return (1., false);
        }
        let natural: Bool = msg_send![event, isDirectionInvertedFromDevice];
        let phase: usize = msg_send![event, phase];
        (if natural.as_bool() { 1. } else { -1. }, phase == PHASE_CANCELLED)
    }
}

fn switch_effect(effect: Effect, on: bool, cx: &mut App) {
    ctx(cx).ui.update(cx, |ui, cx| {
        effect.set(&mut ui.prefs, on);
        ui.prefs.save();
        cx.notify();
    });
}

fn main_window(cx: &App) -> Option<WindowHandle<Shell>> {
    cx.windows().into_iter().find_map(|window| window.downcast::<Shell>())
}

/// Brings Drift to the front, whether it is behind other apps, minimized or hidden.
fn show_app(cx: &mut App) {
    let Some(main) = main_window(cx) else { return };
    let nothing: *mut AnyObject = std::ptr::null_mut();
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![app, unhide: nothing];
    }
    cx.activate(true);
    let _ = main.update(cx, |_, window, _| {
        if let Some(native) = super::native_window(window) {
            unsafe {
                let minimized: Bool = msg_send![native, isMiniaturized];
                if minimized.as_bool() {
                    let _: () = msg_send![native, deminiaturize: nothing];
                }
                // macOS may refuse to activate the app, and the window should come up anyway.
                let _: () = msg_send![native, orderFrontRegardless];
            }
        }
        window.activate_window();
    });
}

#[cfg(debug_assertions)]
static HELD_OPEN: AtomicBool = AtomicBool::new(false);

#[cfg(debug_assertions)]
static DEV_HINT: AtomicU8 = AtomicU8::new(0);

/// Debug control pipe: holds the strip of cards at a given offset, without touching the music.
#[cfg(debug_assertions)]
static DEV_STRIP: AtomicI32 = AtomicI32::new(i32::MIN);

#[cfg(debug_assertions)]
fn dev_strip() -> Option<f32> {
    let held = DEV_STRIP.load(Ordering::Relaxed);
    (held != i32::MIN).then_some(held as f32)
}

#[cfg(debug_assertions)]
fn held_open() -> bool {
    HELD_OPEN.load(Ordering::Relaxed)
}

#[cfg(not(debug_assertions))]
fn held_open() -> bool {
    false
}

/// Debug control pipe: holds the island open and flips effects without touching the pointer.
#[cfg(debug_assertions)]
pub fn dev_command(name: &str, arg: &str, cx: &mut App) {
    match name {
        "island" => HELD_OPEN.store(arg == "open", Ordering::Relaxed),
        "strip" => DEV_STRIP.store(arg.parse().unwrap_or(i32::MIN), Ordering::Relaxed),
        // Shows the sign only: the music is not touched.
        "swipe-hint" => DEV_HINT.store(
            match arg {
                "left" => 1,
                "right" => 2,
                "up" => 3,
                "down" => 4,
                _ => 0,
            },
            Ordering::Relaxed,
        ),
        "activity" => wallpaper::DEV_NO_ACTIVITY.store(arg == "off", Ordering::Relaxed),
        // Minimizes or restores the Drift window, to check that the clip keeps moving on the desktop.
        "window" => {
            let Some(main) = main_window(cx) else { return };
            let minimize = arg == "minimize";
            // Commands arrive while the main window's view is being updated, and GPUI refuses to
            // update it a second time inside that, so the window is reached once that is over.
            cx.defer(move |cx| {
                let _ = main.update(cx, |_, window, _| {
                    let Some(native) = super::native_window(window) else { return };
                    let nothing: *mut AnyObject = std::ptr::null_mut();
                    unsafe {
                        if minimize {
                            let _: () = msg_send![native, miniaturize: nothing];
                        } else {
                            let _: () = msg_send![native, deminiaturize: nothing];
                        }
                    }
                });
            });
        }
        "main-window" => {
            eprintln!("island: main window {}", if main_window(cx).is_some() { "found" } else { "missing" })
        }
        "effect" => {
            let Some((which, state)) = arg.split_once(' ') else { return };
            let effect = match which {
                "wallpaper" => Effect::Wallpaper,
                "words" => Effect::Words,
                "glow" => Effect::Glow,
                "pulse" => Effect::Pulse,
                "chorus" => Effect::Chorus,
                _ => return,
            };
            // In memory only: the saved preference stays as the user left it.
            ctx(cx).ui.update(cx, |ui, cx| {
                effect.set(&mut ui.prefs, state == "on");
                cx.notify();
            });
        }
        _ => {}
    }
}

fn lerp(from: f32, to: f32, amount: f32) -> f32 {
    from + (to - from) * amount
}

/// Width, height and corner radius for the current animation state.
fn island_size(notch: Notch, presence: f32, open: f32) -> (f32, f32, f32) {
    let compact = notch.width + WING * 2. * presence;
    (lerp(compact, OPEN_W, open), lerp(notch.height, OPEN_H, open), lerp(11., 28., open))
}

/// How much the compact island grows with the voice. Steps of a whole point in width move
/// each edge by exactly one Retina pixel, so the shape stays centered and crisp.
fn voice_growth(voice: f32, presence: f32, open: f32) -> (f32, f32) {
    let amount = voice * presence * (1. - open).powi(3);
    ((amount * GROW_WIDTH).round(), (amount * GROW_HEIGHT * 2.).round() / 2.)
}

fn approach(current: f32, target: f32, speed: f32, dt: f32) -> f32 {
    current + (target - current) * (1. - (-speed * dt).exp())
}

fn contains(position: (f32, f32), width: f32, height: f32, margin: f32) -> bool {
    let left = (CANVAS_W - width) / 2.;
    position.0 >= left - margin
        && position.0 <= left + width + margin
        && position.1 >= -2.
        && position.1 <= height + margin
}

fn compact_bars(bands: &[f32; BANDS]) -> [f32; COMPACT_BARS] {
    let mut out = [0.; COMPACT_BARS];
    for (index, slot) in out.iter_mut().enumerate() {
        let start = index * BANDS / COMPACT_BARS;
        let end = ((index + 1) * BANDS / COMPACT_BARS).max(start + 1);
        let slice = &bands[start..end];
        *slot = slice.iter().sum::<f32>() / slice.len() as f32;
    }
    out
}

fn with_alpha(color: Rgba, alpha: f32) -> Rgba {
    Rgba { a: alpha, ..color }
}

fn effect_button(effect: Effect, on: bool, usable: bool, accent: Rgba) -> impl IntoElement {
    let color = if on { TEXT } else { MUTED };
    let white = rgb(0xffffff);
    let (background, edge) = if on {
        (with_alpha(accent, 0.3), with_alpha(accent, 0.65))
    } else {
        (with_alpha(white, 0.05), with_alpha(white, 0.08))
    };
    div()
        .id(("island-effect", effect as usize))
        .h(px(BUTTON_H))
        .pl(px(9.))
        .pr(px(11.))
        .flex()
        .flex_none()
        .items_center()
        .gap(px(5.))
        .rounded_full()
        .border_1()
        .border_color(edge)
        .bg(background)
        .text_color(rgb(color))
        .text_xs()
        .font_weight(gpui::FontWeight::MEDIUM)
        .cursor_pointer()
        .opacity(if usable { 1. } else { 0.4 })
        .hover(|style| style.opacity(0.78))
        .on_click(move |_, _, cx| switch_effect(effect, !on, cx))
        .child(icon(effect.icon(), 14., color))
        .child(effect.label())
}

fn open_app_button() -> impl IntoElement {
    let white = rgb(0xffffff);
    div()
        .id("island-open-app")
        .h(px(BUTTON_H))
        .pl(px(10.))
        .pr(px(12.))
        .flex()
        .flex_none()
        .items_center()
        .gap(px(5.))
        .rounded_full()
        .bg(with_alpha(white, 0.13))
        .text_color(rgba(TEXT))
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .cursor_pointer()
        .hover(move |style| style.bg(with_alpha(white, 0.22)))
        .on_click(|_, _, cx| show_app(cx))
        .child(icon("open", 14., TEXT))
        .child("Open")
}

fn bars(values: Vec<f32>, color: Hsla, gap: f32) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let count = values.len() as f32;
            let (width, height) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
            let bar = ((width - gap * (count - 1.)) / count).max(1.);
            for (index, value) in values.iter().enumerate() {
                let bar_height = (value.clamp(0., 1.) * height).max(bar.min(3.));
                let x = f32::from(bounds.origin.x) + index as f32 * (bar + gap);
                let y = f32::from(bounds.origin.y) + (height - bar_height) / 2.;
                window.paint_quad(
                    fill(Bounds::new(point(px(x), px(y)), size(px(bar), px(bar_height))), color)
                        .corner_radii(px(bar / 2.)),
                );
            }
        },
    )
    .size_full()
}

unsafe fn find_notch() -> (NSRect, Notch) {
    unsafe {
        let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
        let count: usize = msg_send![screens, count];
        for index in 0..count {
            let screen: *mut AnyObject = msg_send![screens, objectAtIndex: index];
            let insets: NSEdgeInsets = msg_send![screen, safeAreaInsets];
            if insets.top > 0. {
                let frame: NSRect = msg_send![screen, frame];
                let left: NSRect = msg_send![screen, auxiliaryTopLeftArea];
                let right: NSRect = msg_send![screen, auxiliaryTopRightArea];
                let width = frame.size.width - left.size.width - right.size.width;
                return (frame, Notch { width: width as f32, height: insets.top as f32 });
            }
        }
        let main: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
        (msg_send![main, frame], FALLBACK_NOTCH)
    }
}

unsafe extern "C" {
    fn object_getClass(object: *const AnyObject) -> *const AnyClass;
    fn class_replaceMethod(
        class: *const AnyClass,
        name: Sel,
        implementation: *const c_void,
        types: *const c_char,
    ) -> *const c_void;
}

extern "C" fn keep_frame(_: *mut AnyObject, _: Sel, frame: NSRect, _: *mut AnyObject) -> NSRect {
    frame
}

/// Pins the transparent overlay to the top center of the notched screen.
unsafe fn configure(native: *mut AnyObject, screen: NSRect) {
    // AppKit normally pushes windows below the menu bar, which is exactly where the notch is.
    unsafe {
        class_replaceMethod(
            object_getClass(native),
            sel!(constrainFrameRect:toScreen:),
            keep_frame as *const c_void,
            c"{CGRect={CGPoint=dd}{CGSize=dd}}@:{CGRect={CGPoint=dd}{CGSize=dd}}@".as_ptr(),
        );
    }
    let frame = NSRect::new(
        NSPoint::new(
            screen.origin.x + (screen.size.width - CANVAS_W as f64) / 2.,
            screen.origin.y + screen.size.height - CANVAS_H as f64,
        ),
        NSSize::new(CANVAS_W as f64, CANVAS_H as f64),
    );
    unsafe {
        let clear: *mut AnyObject = msg_send![class!(NSColor), clearColor];
        let _: () = msg_send![native, setBackgroundColor: clear];
        let _: () = msg_send![native, setOpaque: Bool::NO];
        let _: () = msg_send![native, setHasShadow: Bool::NO];
        let _: () = msg_send![native, setCollectionBehavior: COLLECTION_BEHAVIOR];
        let _: () = msg_send![native, setIgnoresMouseEvents: Bool::YES];
        let _: () = msg_send![native, setFrame: frame, display: Bool::YES];
    }
}

pub struct Island {
    bus: Option<Bus>,
    wallpaper: Option<Wallpaper>,
    native: *mut AnyObject,
    notch: Notch,
    presence: f32,
    open: f32,
    pulse: f32,
    /// How much of the glow and of the voice reaction is switched on, so they fade instead of snapping.
    glow_share: f32,
    pulse_share: f32,
    swipe: SwipeTracker,
    lean: f32,
    /// How far the strip of cards is off center, and the track it is on its way to.
    strip: f32,
    going: Option<(f32, Instant)>,
    strip_track: String,
    strip_ids: (Option<String>, Option<String>),
    page_width: Rc<Cell<f32>>,
    hint: Option<Swipe>,
    hint_light: f32,
    /// What the last swipe asked the player to do, until the player has had time to report it.
    asked: Option<(bool, Instant)>,
    bars: [f32; BANDS],
    last_tick: Instant,
    ignoring_mouse: bool,
    playing_since: Option<Instant>,
    helper_started: Option<Instant>,
    cover_url: String,
    palette: Palette,
    palette_task: Option<Task<()>>,
    lyrics_for: String,
    lyrics_task: Option<Task<()>>,
    /// The last time the page was asked to switch between song and clip: for which song, which way, when.
    clip_asked: Option<(String, bool, Instant)>,
    /// The page is asked to send the music video's frames, and when the ask was last renewed.
    clip_streaming: bool,
    clip_renewed: Instant,
    #[cfg(debug_assertions)]
    clip_report: Instant,
    seek_bounds: Rc<Cell<Bounds<Pixels>>>,
    _ticker: Task<()>,
}

pub fn open(bus: Option<Bus>, wallpaper: Option<Wallpaper>, cx: &mut App) {
    let (screen, notch) = unsafe { find_notch() };
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            point(px(0.), px(0.)),
            size(px(CANVAS_W), px(CANVAS_H)),
        ))),
        titlebar: None,
        focus: false,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        window_background: WindowBackgroundAppearance::Transparent,
        ..Default::default()
    };
    let opened = cx.open_window(options, move |window, cx| {
        let native = super::native_window(window).unwrap_or(std::ptr::null_mut());
        if !native.is_null() {
            unsafe { configure(native, screen) };
        }
        cx.new(|cx| Island::new(bus, wallpaper, native, notch, cx))
    });
    if opened.is_err() {
        eprintln!("drift: could not open the island");
    }
}

impl Island {
    fn new(
        bus: Option<Bus>,
        wallpaper: Option<Wallpaper>,
        native: *mut AnyObject,
        notch: Notch,
        cx: &mut Context<Self>,
    ) -> Self {
        let ticker = cx.spawn(async move |this, cx| {
            #[cfg(debug_assertions)]
            let mut pace = super::pace::Pace::new("island");
            loop {
                let Ok(delay) = this.update(cx, |island, cx| {
                    island.tick(cx);
                    cx.notify();
                    if island.is_moving() { ACTIVE_TICK } else { IDLE_TICK }
                }) else {
                    break;
                };
                #[cfg(debug_assertions)]
                if delay == ACTIVE_TICK {
                    pace.frame(ACTIVE_TICK);
                } else {
                    pace.rest();
                }
                cx.background_executor().timer(delay).await;
            }
        });
        Self {
            bus,
            wallpaper,
            native,
            notch,
            presence: 0.,
            open: 0.,
            pulse: 0.,
            glow_share: 1.,
            pulse_share: 1.,
            swipe: SwipeTracker::default(),
            lean: 0.,
            strip: 0.,
            going: None,
            strip_track: String::new(),
            strip_ids: (None, None),
            page_width: Rc::new(Cell::new(328.)),
            hint: None,
            hint_light: 0.,
            asked: None,
            bars: [0.; BANDS],
            last_tick: Instant::now(),
            ignoring_mouse: true,
            playing_since: None,
            helper_started: None,
            cover_url: String::new(),
            palette: Palette::default(),
            palette_task: None,
            lyrics_for: String::new(),
            lyrics_task: None,
            clip_asked: None,
            clip_streaming: false,
            clip_renewed: Instant::now(),
            #[cfg(debug_assertions)]
            clip_report: Instant::now(),
            seek_bounds: Rc::default(),
            _ticker: ticker,
        }
    }

    fn is_moving(&self) -> bool {
        self.playing_since.is_some()
            || self.open > 0.002
            || self.pulse > 0.01
            || self.hint_light > 0.
            || self.lean.abs() > 0.01
            || self.strip.abs() > 0.05
            || self.going.is_some()
            || self.bars.iter().any(|value| *value > 0.01)
    }

    fn pointer(&self) -> Option<(f32, f32)> {
        if self.native.is_null() {
            return None;
        }
        unsafe {
            let mouse: NSPoint = msg_send![class!(NSEvent), mouseLocation];
            let frame: NSRect = msg_send![self.native, frame];
            Some(((mouse.x - frame.origin.x) as f32, (frame.origin.y + frame.size.height - mouse.y) as f32))
        }
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f32().min(0.08);
        self.last_tick = now;

        let app = ctx(cx);
        let state = app.player.read(cx).state.clone();
        let (has_track, playing, artwork_url, is_ad) =
            (state.signed_in && state.has_track, state.playing, state.artwork.clone(), state.is_ad);
        let (
            show_island,
            show_wallpaper,
            synced_lyrics,
            island_glow,
            island_pulse,
            chorus_burst,
            window_galaxy,
            clip_particles,
        ) = {
            let prefs = &app.ui.read(cx).prefs;
            (
                prefs.live_island,
                prefs.live_wallpaper,
                prefs.synced_lyrics,
                prefs.island_glow,
                prefs.island_pulse,
                prefs.chorus_burst,
                prefs.window_galaxy,
                prefs.clip_particles,
            )
        };
        let clip_wanted = clip_particles && (show_wallpaper || window_galaxy);
        self.glow_share = approach(self.glow_share, f32::from(u8::from(island_glow)), 7., dt);
        self.pulse_share = approach(self.pulse_share, f32::from(u8::from(island_pulse)), 7., dt);
        if let Some(wallpaper) = &mut self.wallpaper {
            wallpaper.update(Playback {
                active: playing && show_wallpaper,
                playing,
                loaded: has_track && !is_ad,
                window: window_galaxy,
                clip: clip_wanted && state.clip_mode && !is_ad,
                track: state.video_id.clone(),
                position: state.position,
                duration: state.duration,
                chorus: chorus_burst,
            });
        }
        self.follow_lyrics(&state, (show_wallpaper || window_galaxy) && synced_lyrics, cx);
        self.follow_clip(&state, clip_wanted, clip_particles && show_wallpaper, cx);
        let has_track = has_track && show_island;

        let (width, height, _) = island_size(self.notch, self.presence, self.open);
        let inside = self.pointer().is_some_and(|position| contains(position, width, height, 8.));
        let hovering = inside && has_track;
        let wants_open = hovering || (has_track && held_open());
        if !hovering {
            self.swipe = SwipeTracker::default();
        }
        self.lean = approach(self.lean, self.swipe.lean(), 18., dt);
        self.follow_strip(&state, &app.player.read(cx).queue, dt);
        self.hint_light = (self.hint_light - dt / HINT_TIME).max(0.);
        #[cfg(debug_assertions)]
        if let Some(swipe) = match DEV_HINT.swap(0, Ordering::Relaxed) {
            1 => Some(Swipe::Left),
            2 => Some(Swipe::Right),
            3 => Some(Swipe::Up),
            4 => Some(Swipe::Down),
            _ => None,
        } {
            self.hint = Some(swipe);
            self.hint_light = 1.;
        }
        self.presence = approach(self.presence, if has_track { 1. } else { 0. }, 9., dt);
        self.open = approach(self.open, if wants_open { 1. } else { 0. }, if wants_open { 13. } else { 9. }, dt);
        if self.open < 0.001 && !wants_open {
            self.open = 0.;
        }
        self.set_ignores_mouse(!hovering);

        let frame = self.bus.as_ref().and_then(Bus::fresh).filter(|_| playing);
        for (index, value) in self.bars.iter_mut().enumerate() {
            let target = frame.map_or(0., |frame| {
                let bands = &frame.bands;
                let soft = 0.25 * bands[index.saturating_sub(1)]
                    + 0.5 * bands[index]
                    + 0.25 * bands[(index + 1).min(BANDS - 1)];
                ((soft - 0.28) / 0.62).clamp(0., 1.).powf(1.35)
            });
            *value = approach(*value, target, if target > *value { 20. } else { 6.5 }, dt);
        }
        let (voice, speed) = frame.map_or((0., 6.), |frame| (frame.voice, 45.));
        self.pulse = approach(self.pulse, voice, speed, dt);

        self.playing_since = match (playing, self.playing_since) {
            (true, Some(since)) => Some(since),
            (true, None) => Some(now),
            (false, _) => None,
        };
        if !is_ad {
            self.keep_helper_running(frame.is_some(), now, cx);
        }
        self.follow_cover(&artwork_url, cx);
    }

    fn set_ignores_mouse(&mut self, ignore: bool) {
        if ignore != self.ignoring_mouse && !self.native.is_null() {
            self.ignoring_mouse = ignore;
            let value = Bool::new(ignore);
            let _: () = unsafe { msg_send![self.native, setIgnoresMouseEvents: value] };
        }
    }

    fn keep_helper_running(&mut self, has_frames: bool, now: Instant, cx: &mut Context<Self>) {
        let Some(bus) = &self.bus else { return };
        let Some(since) = self.playing_since else { return };
        if has_frames || now.duration_since(since) < HELPER_GRACE {
            return;
        }
        if self.helper_started.is_some_and(|at| now.duration_since(at) < HELPER_RETRY) {
            return;
        }
        if let Some(pid) = ctx(cx).client.engine().audio_process() {
            self.helper_started = Some(now);
            bus.launch_helper(pid);
        }
    }

    fn follow_cover(&mut self, url: &str, cx: &mut Context<Self>) {
        if url == self.cover_url {
            return;
        }
        let wanted = images::sized(url, wallpaper::COVER_SIZE);
        let Some(image) = ctx(cx).images.update(cx, |images, _| images.get(&wanted)) else { return };
        self.cover_url = url.to_string();
        let work =
            cx.background_executor().spawn(async move { palette::from_image(&image.bytes, wallpaper::COVER_SIZE) });
        self.palette_task = Some(cx.spawn(async move |this, cx| {
            let found = work.await;
            let _ = this.update(cx, |island, cx| {
                let Some((palette, rgba)) = found else {
                    island.palette = Palette::default();
                    return;
                };
                island.palette = palette;
                // The window's accent follows the cover, like its galaxy does.
                let accent = crate::ui::readable_accent(palette.accent);
                ctx(cx).ui.update(cx, |ui, cx| {
                    if ui.accent != accent {
                        ui.accent = accent;
                        cx.notify();
                    }
                });
                if let Some(wallpaper) = &island.wallpaper {
                    wallpaper.set_cover(Cover { rgba, palette });
                }
            });
        }));
    }

    /// Keeps YouTube Music's Song / Video switch in step with "Clip" and feeds the galaxy the clip's frames.
    /// `on_desktop`: the clip's stars also show on the desktop, not only in the Drift window.
    fn follow_clip(&mut self, state: &PlayerState, wanted: bool, on_desktop: bool, cx: &mut Context<Self>) {
        if state.is_ad || !state.has_track {
            return;
        }
        // One nudge per song and direction: the page remembers the choice for the songs after it.
        // If the page did not follow, the nudge comes again after a while.
        let video = wanted && state.clip_available;
        ctx(cx).client.engine().set_watching(wanted && state.clip_mode, on_desktop);
        #[cfg(debug_assertions)]
        if self.clip_report.elapsed() >= Duration::from_secs(2) {
            self.clip_report = Instant::now();
            let (frames, pictures) = super::clip_counts();
            if wanted && state.clip_mode {
                eprintln!("clip: {frames} frames, {pictures} new pictures in 2 s");
            }
        }
        if video != state.clip_mode {
            let fresh = !self
                .clip_asked
                .as_ref()
                .is_some_and(|(track, way, at)| *track == state.video_id && *way == video && at.elapsed() < CLIP_RETRY);
            if fresh {
                self.clip_asked = Some((state.video_id.clone(), video, Instant::now()));
                ctx(cx).client.send(Command::Clip(video));
            }
        }
        // The page sends frames on its own while asked. The ask is renewed, so a reloaded page picks it up again.
        let streaming = wanted && state.clip_mode && state.playing;
        if streaming != self.clip_streaming || (streaming && self.clip_renewed.elapsed() >= CLIP_RENEW) {
            self.clip_streaming = streaming;
            self.clip_renewed = Instant::now();
            ctx(cx).client.send(Command::Stream(streaming));
        }
    }

    /// Gives the galaxy the words of this song, only while the user allows the lyrics lookup.
    fn follow_lyrics(&mut self, state: &PlayerState, allowed: bool, cx: &mut Context<Self>) {
        if !allowed {
            if !self.lyrics_for.is_empty() {
                self.lyrics_for.clear();
                self.lyrics_task = None;
                if let Some(wallpaper) = &self.wallpaper {
                    wallpaper.set_lyrics(state.video_id.clone(), Vec::new());
                }
            }
            return;
        }
        if state.is_ad || state.video_id.is_empty() || state.duration <= 0. || state.video_id == self.lyrics_for {
            return;
        }
        self.lyrics_for.clone_from(&state.video_id);
        let (track, title, artist, duration) =
            (state.video_id.clone(), state.title.clone(), state.artist.clone(), state.duration);
        // A music video's own written subtitles come first: they match the clip to the moment and
        // need no outside service. Speech-recognized ones only stand in when the catalog has nothing.
        let page = state.clip_mode.then(|| ctx(cx).client.clone());
        let executor = cx.background_executor().clone();
        self.lyrics_task = Some(cx.spawn(async move |this, cx| {
            let (mut lines, mut heard) = (None, None);
            if let Some(page) = page {
                let reply = page.captions(&track).await.unwrap_or_default();
                let video = track.clone();
                match executor.spawn(async move { lyrics::read_captions(&reply, &video) }).await {
                    lyrics::Captions::Written(written) => lines = Some(("the clip's subtitles", written)),
                    lyrics::Captions::Recognized(recognized) => {
                        heard = Some(("speech-recognized subtitles", recognized))
                    }
                    lyrics::Captions::None => {}
                }
            }
            if lines.is_none() {
                let found = executor.spawn(async move { lyrics::fetch(&title, &artist, duration) }).await;
                lines = found.map(|found| ("LRCLIB", found)).or(heard);
            }
            let Some((_source, lines)) = lines else { return };
            #[cfg(debug_assertions)]
            if std::env::var_os("DRIFT_DEV_FIFO").is_some() {
                let timed = lines.iter().filter(|line| !line.words.is_empty()).count();
                eprintln!("lyrics: {} lines from {_source}, {timed} with word times", lines.len());
            }
            let _ = this.update(cx, |island, _| {
                if island.lyrics_for == track
                    && let Some(wallpaper) = &island.wallpaper
                {
                    wallpaper.set_lyrics(track, lines);
                }
            });
        }));
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        // A regular mouse wheel has no touch to follow, so only trackpads swipe.
        let ScrollDelta::Pixels(delta) = event.delta else { return };
        let (sign, cancelled) = scroll_event();
        if cancelled {
            self.swipe = SwipeTracker::default();
            return;
        }
        let (dx, dy) = (f32::from(delta.x) * sign, f32::from(delta.y) * sign);
        if let Some(swipe) = self.swipe.feed(event.touch_phase, dx, dy) {
            self.run_swipe(swipe, cx);
        }
    }

    fn run_swipe(&mut self, swipe: Swipe, cx: &mut Context<Self>) {
        #[cfg(debug_assertions)]
        eprintln!("island: swipe {swipe:?}");
        let app = ctx(cx);
        let (has_track, is_ad, playing, around) = {
            let player = app.player.read(cx);
            let state = &player.state;
            (state.has_track, state.is_ad, state.playing, neighbors(&player.queue, &state.video_id))
        };
        let now = Instant::now();
        match swipe {
            Swipe::Left | Swipe::Right => {
                // Ads play to the end, the same as with the buttons in the app.
                if !has_track || is_ad {
                    return;
                }
                if swipe == Swipe::Left {
                    app.client.send(Command::Next);
                    if around.after.is_some() {
                        self.going = Some((1., now));
                    }
                } else if let Some(index) = around.before {
                    // The card on screen is the track before, so go there even mid-song.
                    app.client.send(Command::Select(index));
                    self.going = Some((-1., now));
                }
                return;
            }
            Swipe::Up | Swipe::Down => {
                let wanted = swipe == Swipe::Down;
                let current = match self.asked {
                    Some((asked, at)) if now.duration_since(at) < ASKED_MEMORY => asked,
                    _ => playing,
                };
                if current != wanted {
                    app.client.send(Command::Toggle);
                    self.asked = Some((wanted, now));
                }
            }
        }
        self.hint = Some(swipe);
        self.hint_light = 1.;
    }

    /// Keeps the strip of covers under the fingers. A real track change reshuffles the cards
    /// without moving what is on screen, so the slide simply carries on.
    fn follow_strip(&mut self, state: &PlayerState, queue: &QueueState, dt: f32) {
        let page = self.page_width.get() + PAGE_GAP;
        if state.video_id != self.strip_track {
            let track = Some(state.video_id.as_str());
            let step = if self.strip_ids.1.as_deref() == track {
                1.
            } else if self.strip_ids.0.as_deref() == track {
                -1.
            } else {
                0.
            };
            self.strip = if step == 0. { 0. } else { self.strip + step * page };
            self.going = None;
            self.strip_track.clone_from(&state.video_id);
        }
        let around = neighbors(queue, &state.video_id);
        let id = |index: Option<usize>| index.map(|index| queue.items[index].video_id.clone());
        self.strip_ids = (id(around.before), id(around.after));

        if self.going.is_some_and(|(_, at)| at.elapsed() > STRIP_PATIENCE) {
            self.going = None;
        }
        let dragging = self.swipe.drag() * STRIP_GAIN;
        let target = match self.going {
            Some((step, _)) => -step * page,
            None => strip_target(dragging, around.before.is_some(), around.after.is_some(), page),
        };
        #[cfg(debug_assertions)]
        let target = dev_strip().unwrap_or(target);
        let speed = if self.going.is_some() {
            13.
        } else if dragging != 0. {
            38.
        } else {
            11.
        };
        self.strip = approach(self.strip, target, speed, dt);
        if target == 0. && self.strip.abs() < 0.05 {
            self.strip = 0.;
        }
    }

    /// The cover and title on a strip of cards, with the tracks before and after it just out of view.
    fn carousel(&self, state: &PlayerState, accent: Rgba, cx: &mut Context<Self>) -> AnyElement {
        let (before, after, spare_art) = {
            let queue = &ctx(cx).player.read(cx).queue;
            let around = neighbors(queue, &state.video_id);
            let card = |index: usize| {
                let item = &queue.items[index];
                Card {
                    title: item.title.clone().into(),
                    artist: item.artist.clone().into(),
                    art: item.thumbnail.clone(),
                }
            };
            let spare = around.here.map_or_else(String::new, |index| queue.items[index].thumbnail.clone());
            (around.before.map(card), around.after.map(card), spare)
        };
        let current =
            Card { title: state.title.clone().into(), artist: state.artist.clone().into(), art: state.artwork.clone() };
        let width = self.page_width.get();
        let page = width + PAGE_GAP;
        let store = self.page_width.clone();
        let mut strip = div().relative().flex_1().min_w_0().h(px(CARD_SIZE)).overflow_hidden().child(
            canvas(move |bounds, _, _| store.set(f32::from(bounds.size.width).max(1.)), |_, _, _, _| {})
                .absolute()
                .size_full(),
        );
        if let Some(card) = &before {
            strip = strip.child(track_card(card, "", self.strip - page, width, page, None, accent, cx));
        }
        strip = strip.child(track_card(
            &current,
            &spare_art,
            self.strip,
            width,
            page,
            Some(self.bars.to_vec()),
            accent,
            cx,
        ));
        if let Some(card) = &after {
            strip = strip.child(track_card(card, "", self.strip + page, width, page, None, accent, cx));
        }
        // Moving cards fade at the edges instead of being cut off.
        let edge = (self.strip.abs() / 30.).min(1.);
        if edge > 0.01 {
            let fade = |angle: f32| {
                linear_gradient(
                    angle,
                    linear_color_stop(gpui::black(), 0.),
                    linear_color_stop(gpui::transparent_black(), 1.),
                )
            };
            strip = strip
                .child(div().absolute().left_0().top_0().bottom_0().w(px(18.)).opacity(edge).bg(fade(90.)))
                .child(div().absolute().right_0().top_0().bottom_0().w(px(18.)).opacity(edge).bg(fade(270.)));
        }
        strip.into_any_element()
    }

    fn seek(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        let bounds = self.seek_bounds.get();
        if bounds.size.width <= px(0.) {
            return;
        }
        let fraction = ((event.position.x - bounds.origin.x) / bounds.size.width).clamp(0., 1.);
        let app = ctx(cx);
        let duration = app.player.read(cx).state.duration;
        if duration > 0. {
            app.client.send(Command::Seek(f64::from(fraction) * duration));
        }
    }
}

fn clock(seconds: f64) -> SharedString {
    let total = seconds.max(0.) as u64;
    format!("{}:{:02}", total / 60, total % 60).into()
}

impl Render for Island {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let app = ctx(cx);
        let state = app.player.read(cx).state.clone();
        let (width, height, radius) = island_size(self.notch, self.presence, self.open);
        let prefs = app.ui.read(cx).prefs.clone();
        let voice = self.pulse * self.pulse_share;
        let (grow_x, grow_y) = voice_growth(voice, self.presence, self.open);
        let (width, height) = (width + grow_x, height + grow_y);
        let compact = ((1. - self.open * 2.6).clamp(0., 1.) * self.presence * 1.6 - 0.6).clamp(0., 1.);
        let detail = ((self.open - 0.42) / 0.58).clamp(0., 1.);
        // While a clip plays, the island glows in the colors of its current scene.
        let accent = super::clip_palette().map_or(self.palette.accent, |colors| colors.accent);
        let glow = self.presence * self.glow_share * (0.14 + 0.3 * voice);
        let progress = if state.duration > 0. { (state.position / state.duration).clamp(0., 1.) as f32 } else { 0. };
        let can_skip = state.has_track && !state.is_ad;
        let playing = state.playing;
        let client = app.client.clone();
        let command = move |value: Command| {
            let client = client.clone();
            move |_: &gpui::ClickEvent, _: &mut Window, _: &mut App| client.send(value)
        };
        let seek_store = self.seek_bounds.clone();
        let wing_center = (self.notch.height - 24.) / 2.;

        let mut island = div()
            .relative()
            .w(px(width))
            .h(px(height))
            .flex_none()
            .overflow_hidden()
            .bg(gpui::black())
            .rounded_bl(px(radius))
            .rounded_br(px(radius))
            .on_scroll_wheel(cx.listener(|island, event: &ScrollWheelEvent, _, cx| island.on_scroll(event, cx)));
        if glow > 0.01 {
            island = island.shadow(vec![BoxShadow {
                color: with_alpha(accent, glow).into(),
                offset: point(px(0.), px(2.)),
                blur_radius: px(18. + voice * 6.),
                spread_radius: px(-1.),
            }]);
        }
        if compact > 0.01 {
            island = island
                .child(
                    div()
                        .absolute()
                        .left(px(13. + grow_x / 2.))
                        .top(px(wing_center))
                        .size(px(24.))
                        .opacity(compact)
                        .child(artwork(&state.artwork, 24., 6., cx)),
                )
                .child(
                    div()
                        .absolute()
                        .right(px(12. + grow_x / 2.))
                        .top(px(wing_center))
                        .w(px(38.))
                        .h(px(24.))
                        .opacity(compact)
                        .child(bars(compact_bars(&self.bars).to_vec(), Hsla::from(accent), 2.)),
                );
        }
        if detail > 0.01 {
            let play_icon = if playing { "pause" } else { "play" };
            island = island.child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(px(self.notch.height + 8. + self.lean))
                    .bottom(px(14. - self.lean))
                    .px(px(20.))
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .opacity(detail)
                    .text_color(rgba(TEXT))
                    .font_family(".SystemUIFont")
                    .child(
                        div().flex().items_center().gap(px(14.)).child(self.carousel(&state, accent, cx)).child(
                            div()
                                .flex_none()
                                .flex()
                                .items_center()
                                .gap_1()
                                .child(
                                    round_button(
                                        "island-previous",
                                        "previous",
                                        34.,
                                        19.,
                                        if can_skip { TEXT } else { MUTED },
                                    )
                                    .on_click(command(Command::Previous)),
                                )
                                .child(
                                    div()
                                        .id("island-play")
                                        .size(px(42.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded_full()
                                        .bg(rgba(TEXT))
                                        .cursor_pointer()
                                        .hover(|style| style.opacity(0.85))
                                        .on_click(command(Command::Toggle))
                                        .child(icon(play_icon, 22., 0x0e1013)),
                                )
                                .child(
                                    round_button("island-next", "next", 34., 19., if can_skip { TEXT } else { MUTED })
                                        .on_click(command(Command::Next)),
                                ),
                        ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .text_xs()
                            .text_color(rgba(MUTED))
                            .child(div().w(px(32.)).flex().justify_end().child(clock(state.position)))
                            .child(
                                div()
                                    .id("island-seek")
                                    .relative()
                                    .flex_1()
                                    .h(px(14.))
                                    .flex()
                                    .items_center()
                                    .cursor_pointer()
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|island, event: &MouseDownEvent, _, cx| island.seek(event, cx)),
                                    )
                                    .child(
                                        canvas(move |bounds, _, _| seek_store.set(bounds), |_, _, _, _| {})
                                            .absolute()
                                            .size_full(),
                                    )
                                    .child(
                                        div().w_full().h(px(4.)).rounded_full().bg(rgb(0x2b2e34)).child(
                                            div()
                                                .h_full()
                                                .w(gpui::relative(progress))
                                                .rounded_full()
                                                .bg(Hsla::from(accent)),
                                        ),
                                    ),
                            )
                            .child(div().w(px(32.)).child(clock(state.duration))),
                    )
                    .child(div().flex().items_center().justify_between().children(
                        Effect::ALL.map(|effect| {
                            effect_button(effect, effect.is_on(&prefs), effect.has_effect(&prefs), accent)
                        }),
                    )),
            );
            island = island.child(div().absolute().top(px(5.)).right(px(18.)).opacity(detail).child(open_app_button()));
        }

        if let Some(swipe) = self.hint.filter(|_| self.hint_light > 0.01 && detail > 0.01) {
            let (along_x, along_y) = swipe.direction();
            let floated = (1. - self.hint_light) * HINT_TRAVEL;
            island = island.child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(px(self.notch.height))
                    .bottom_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .relative()
                            .left(px(along_x * floated))
                            .top(px(along_y * floated))
                            .size(px(64.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .border_1()
                            .border_color(with_alpha(accent, 0.8))
                            .bg(with_alpha(rgb(0x0b0c0f), 0.86))
                            .opacity((self.hint_light * 1.8).min(1.) * detail)
                            .child(icon(swipe.icon(), 30., TEXT)),
                    ),
            );
        }

        div().size_full().flex().justify_center().items_start().child(island)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTCH: Notch = Notch { width: 220., height: 38. };

    #[test]
    fn idle_island_is_exactly_the_notch() {
        assert_eq!(island_size(NOTCH, 0., 0.).0, 220.);
        assert_eq!(island_size(NOTCH, 0., 0.).1, 38.);
    }

    #[test]
    fn open_island_fits_the_overlay() {
        let (width, height, _) = island_size(NOTCH, 1., 1.);
        assert!(width < CANVAS_W && height < CANVAS_H);
    }

    #[test]
    fn hit_test_follows_the_visible_shape() {
        let (width, height, _) = island_size(NOTCH, 1., 0.);
        assert!(contains((CANVAS_W / 2., 10.), width, height, 0.));
        assert!(!contains((5., 10.), width, height, 0.));
        assert!(!contains((CANVAS_W / 2., 120.), width, height, 0.));
    }

    #[test]
    fn voice_growth_is_subtle_and_stops_when_open() {
        assert_eq!(voice_growth(0., 1., 0.), (0., 0.));
        assert_eq!(voice_growth(1., 1., 0.), (GROW_WIDTH, GROW_HEIGHT));
        assert_eq!(voice_growth(1., 1., 1.), (0., 0.));
        let (width, _) = voice_growth(0.37, 1., 0.);
        assert_eq!(width, width.round());
    }

    #[test]
    fn every_effect_switches_only_itself() {
        for effect in Effect::ALL {
            let mut prefs = Prefs { synced_lyrics: true, ..Prefs::default() };
            effect.set(&mut prefs, false);
            assert!(!effect.is_on(&prefs), "{effect:?}");
            for other in Effect::ALL {
                if other != effect {
                    assert!(other.is_on(&prefs), "{effect:?} also changed {other:?}");
                }
            }
            effect.set(&mut prefs, true);
            assert!(effect.is_on(&prefs), "{effect:?}");
        }
    }

    #[test]
    fn words_need_a_galaxy() {
        let mut prefs = Prefs { synced_lyrics: true, ..Prefs::default() };
        assert!(Effect::Words.has_effect(&prefs));
        prefs.live_wallpaper = false;
        assert!(Effect::Words.has_effect(&prefs), "the window still has its galaxy");
        prefs.window_galaxy = false;
        assert!(!Effect::Words.has_effect(&prefs));
        assert!(!Effect::Chorus.has_effect(&prefs), "the chorus lives in the galaxy too");
        assert!(Effect::Glow.has_effect(&prefs) && Effect::Pulse.has_effect(&prefs));
    }

    fn swipe_of(dx: f32, dy: f32) -> Option<Swipe> {
        let mut tracker = SwipeTracker::default();
        tracker.feed(TouchPhase::Started, 0., 0.);
        (0..10)
            .find_map(|_| tracker.feed(TouchPhase::Moved, dx / 10., dy / 10.))
            .or_else(|| tracker.feed(TouchPhase::Ended, 0., 0.))
    }

    #[test]
    fn each_direction_is_its_own_swipe() {
        assert_eq!(swipe_of(90., 4.), Some(Swipe::Right));
        assert_eq!(swipe_of(-90., 4.), Some(Swipe::Left));
        assert_eq!(swipe_of(5., -90.), Some(Swipe::Up));
        assert_eq!(swipe_of(5., 90.), Some(Swipe::Down));
    }

    #[test]
    fn small_or_diagonal_movement_does_nothing() {
        assert_eq!(swipe_of(30., 5.), None);
        assert_eq!(swipe_of(90., 80.), None);
    }

    #[test]
    fn one_touch_is_one_swipe() {
        let mut tracker = SwipeTracker::default();
        tracker.feed(TouchPhase::Started, 0., 0.);
        assert_eq!(tracker.feed(TouchPhase::Moved, 0., -100.), Some(Swipe::Up), "pause does not wait");
        assert_eq!(tracker.feed(TouchPhase::Moved, 0., -100.), None);
        assert_eq!(tracker.feed(TouchPhase::Moved, 0., 400.), None, "changing direction mid-touch does nothing");
        assert_eq!(tracker.feed(TouchPhase::Ended, 0., 0.), None);
        tracker.feed(TouchPhase::Started, 0., 0.);
        assert_eq!(tracker.feed(TouchPhase::Moved, 0., 100.), Some(Swipe::Down), "the next touch works again");
    }

    #[test]
    fn a_sideways_drag_counts_only_when_the_fingers_lift() {
        let mut tracker = SwipeTracker::default();
        tracker.feed(TouchPhase::Started, 0., 0.);
        assert_eq!(tracker.feed(TouchPhase::Moved, -120., 0.), None);
        assert_eq!(tracker.drag(), -120., "the cards follow the fingers meanwhile");
        assert_eq!(tracker.feed(TouchPhase::Ended, 0., 0.), Some(Swipe::Left));
        assert_eq!(tracker.drag(), 0.);

        tracker.feed(TouchPhase::Started, 0., 0.);
        tracker.feed(TouchPhase::Moved, 120., 0.);
        tracker.feed(TouchPhase::Moved, -100., 0.);
        assert_eq!(tracker.feed(TouchPhase::Ended, 0., 0.), None, "dragged back: nothing happens");
    }

    fn queue_of(ids: &[&str], index: usize) -> QueueState {
        QueueState {
            index,
            items: ids
                .iter()
                .map(|id| crate::engine::QueueItem { video_id: (*id).to_string(), ..Default::default() })
                .collect(),
        }
    }

    #[test]
    fn neighbors_come_from_the_playing_track() {
        let queue = queue_of(&["a", "b", "c"], 1);
        assert_eq!(neighbors(&queue, "b"), Around { here: Some(1), before: Some(0), after: Some(2) });
        assert_eq!(
            neighbors(&queue, "a"),
            Around { here: Some(0), before: None, after: Some(1) },
            "the queue index may lag"
        );
        assert_eq!(neighbors(&queue, "c"), Around { here: Some(2), before: Some(1), after: None });
        assert_eq!(neighbors(&queue, "z"), Around::default());
    }

    #[test]
    fn a_repeated_track_uses_the_copy_nearest_the_queue() {
        let queue = queue_of(&["a", "b", "a", "c"], 2);
        assert_eq!(neighbors(&queue, "a"), Around { here: Some(2), before: Some(1), after: Some(3) });
    }

    #[test]
    fn with_no_card_on_that_side_the_strip_only_stretches() {
        assert_eq!(strip_target(120., true, true, 346.), 120.);
        assert_eq!(strip_target(900., true, true, 346.), 346.);
        let stretched = strip_target(900., false, true, 346.);
        assert!(stretched > 0. && stretched < STRIP_STRETCH, "{stretched}");
        assert!(strip_target(-900., true, false, 346.) > -STRIP_STRETCH);
    }

    #[test]
    fn momentum_after_the_fingers_leave_is_ignored() {
        let mut tracker = SwipeTracker::default();
        tracker.feed(TouchPhase::Started, 0., 0.);
        tracker.feed(TouchPhase::Moved, 20., 0.);
        tracker.feed(TouchPhase::Ended, 0., 0.);
        for _ in 0..30 {
            assert_eq!(tracker.feed(TouchPhase::Moved, 40., 0.), None);
        }
    }

    #[test]
    fn the_player_leans_a_little_and_lets_go() {
        let mut tracker = SwipeTracker::default();
        tracker.feed(TouchPhase::Started, 0., 0.);
        tracker.feed(TouchPhase::Moved, 0., 40.);
        assert!(tracker.lean() > 0. && tracker.lean() <= LEAN_LIMIT);
        tracker.feed(TouchPhase::Moved, 0., 40.);
        assert_eq!(tracker.lean(), 0., "once the swipe has fired it springs back");
    }

    #[test]
    fn compact_bars_average_all_bands() {
        assert_eq!(compact_bars(&[0.5; BANDS]), [0.5; COMPACT_BARS]);
    }
}
