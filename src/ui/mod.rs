//! The native GPUI interface.

mod field;
mod page;
mod panel;
mod settings;
mod shell;
mod transport;

use gpui::{
    AnyElement, App, BoxShadow, Div, Hsla, InteractiveElement, KeyBinding, Menu, MenuItem, ObjectFit, OsAction,
    ParentElement, Rgba, SharedString, Stateful, Styled, StyledImage, Svg, actions, div, img, point, px, rgba, svg,
};

pub use shell::Shell;

use crate::{images, model::ctx, ytm::MenuEntry};

pub const TOP_BAR: f32 = 64.;
pub const SIDEBAR: f32 = 232.;
pub const TRANSPORT: f32 = 104.;
pub const PANEL: f32 = 360.;

// Colors are RGBA, 0xRRGGBBAA: the song's galaxy shows through anything that is not fully opaque.
/// The night under everything, where no galaxy is drawn.
pub const BG: u32 = 0x06040cff;
/// Glass over the galaxy: panels, the player, the search field.
pub const SURFACE: u32 = 0x0e0a1aad;
/// Glass dense enough to read anything on: menus, dialogs, suggestions.
pub const SOLID: u32 = 0x150f26f5;
pub const RAISED: u32 = 0xffffff1f;
pub const HOVER: u32 = 0xffffff12;
pub const LINE: u32 = 0xffffff21;
pub const TEXT: u32 = 0xffffffff;
pub const MUTED: u32 = 0xffffffb3;
pub const FAINT: u32 = 0xffffff6b;
/// Used until the first cover gives its own color.
pub const ACCENT: u32 = 0xff8ac4ff;
/// Text and icons on an accent-colored button.
pub const ON_ACCENT: u32 = 0x1a0a14ff;
pub const DANGER: u32 = 0xff7b7bff;
/// Darkens the galaxy under pages full of text, so the song's flying words stay in the background.
pub const SCRIM: u32 = 0x06040cbd;

actions!(
    drift,
    [
        Quit,
        TogglePlay,
        NextTrack,
        PreviousTrack,
        VolumeUp,
        VolumeDown,
        FocusSearch,
        GoBack,
        GoForward,
        ToggleQueue,
        Dismiss,
        Undo,
        Redo,
        Cut,
        Copy,
        Paste,
        SelectAll,
    ]
);

pub fn init(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-z", Undo, None),
        KeyBinding::new("cmd-shift-z", Redo, None),
        KeyBinding::new("cmd-x", Cut, None),
        KeyBinding::new("cmd-c", Copy, None),
        KeyBinding::new("cmd-v", Paste, None),
        KeyBinding::new("cmd-a", SelectAll, None),
        KeyBinding::new("cmd-f", FocusSearch, None),
        KeyBinding::new("cmd-k", FocusSearch, None),
        KeyBinding::new("cmd-[", GoBack, None),
        KeyBinding::new("cmd-]", GoForward, None),
        KeyBinding::new("cmd-u", ToggleQueue, None),
        KeyBinding::new("escape", Dismiss, Some("Player")),
        KeyBinding::new("space", TogglePlay, Some("Player")),
        KeyBinding::new("cmd-right", NextTrack, Some("Player")),
        KeyBinding::new("cmd-left", PreviousTrack, Some("Player")),
        KeyBinding::new("cmd-up", VolumeUp, Some("Player")),
        KeyBinding::new("cmd-down", VolumeDown, Some("Player")),
    ]);
    // Edit items use native selectors so copy and paste work inside the web sign-in form.
    cx.set_menus(vec![
        Menu { name: "Drift".into(), items: vec![MenuItem::action("Quit Drift", Quit)] },
        Menu {
            name: "Edit".into(),
            items: vec![
                MenuItem::os_action("Undo", Undo, OsAction::Undo),
                MenuItem::os_action("Redo", Redo, OsAction::Redo),
                MenuItem::separator(),
                MenuItem::os_action("Cut", Cut, OsAction::Cut),
                MenuItem::os_action("Copy", Copy, OsAction::Copy),
                MenuItem::os_action("Paste", Paste, OsAction::Paste),
                MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
            ],
        },
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::action("Search", FocusSearch),
                MenuItem::action("Back", GoBack),
                MenuItem::action("Forward", GoForward),
                MenuItem::action("Queue", ToggleQueue),
            ],
        },
    ]);
}

pub fn icon(name: &'static str, size: f32, color: u32) -> Svg {
    svg().path(name).size(px(size)).flex_none().text_color(rgba(color))
}

/// The accent of the song that is playing, taken from its cover.
pub fn accent(cx: &App) -> u32 {
    ctx(cx).ui.read(cx).accent
}

/// Turns a cover's most vivid color into an accent that reads on dark glass.
pub fn readable_accent(color: Rgba) -> u32 {
    let mut hsla: Hsla = color.into();
    hsla.s = hsla.s.min(0.92);
    hsla.l = hsla.l.clamp(0.68, 0.82);
    hsla.a = 1.;
    let color = hsla.to_rgb();
    let channel = |value: f32| (value.clamp(0., 1.) * 255.).round() as u32;
    channel(color.r) << 24 | channel(color.g) << 16 | channel(color.b) << 8 | 0xff
}

/// A soft colored glow under covers and primary buttons.
pub fn glow(color: u32, strength: f32, blur: f32) -> Vec<BoxShadow> {
    let mut tint: Hsla = rgba(color).into();
    tint.a = strength;
    vec![BoxShadow {
        color: tint,
        offset: point(px(0.), px(blur * 0.4)),
        blur_radius: px(blur),
        spread_radius: px(-blur * 0.35),
    }]
}

pub fn artwork(url: &str, size: f32, radius: f32, cx: &mut App) -> AnyElement {
    use gpui::IntoElement;
    let wanted = images::sized(url, (size * 2.).max(120.) as u32);
    let image = ctx(cx).images.update(cx, |images, _| images.get(&wanted));
    let frame = div()
        .size(px(size))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(radius))
        .overflow_hidden()
        .bg(rgba(RAISED));
    match image {
        Some(image) => {
            frame.child(img(image).size_full().rounded(px(radius)).object_fit(ObjectFit::Cover)).into_any_element()
        }
        None => frame.child(icon("note", size * 0.38, FAINT)).into_any_element(),
    }
}

pub fn round_button(
    id: impl Into<gpui::ElementId>,
    name: &'static str,
    box_size: f32,
    icon_size: f32,
    color: u32,
) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(box_size))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .cursor_pointer()
        .hover(|style| style.bg(rgba(RAISED)))
        .child(icon(name, icon_size, color))
}

pub fn pill(
    id: impl Into<gpui::ElementId>,
    name: Option<&'static str>,
    label: impl Into<SharedString>,
    primary: bool,
) -> Stateful<Div> {
    let (bg, fg) = if primary { (TEXT, BG) } else { (RAISED, TEXT) };
    let mut button = div()
        .id(id)
        .h(px(38.))
        .px_4()
        .flex()
        .flex_none()
        .items_center()
        .gap_2()
        .rounded_full()
        .bg(rgba(bg))
        .text_color(rgba(fg))
        .text_sm()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .cursor_pointer()
        .hover(|style| style.opacity(0.86));
    if !primary {
        button = button.border_1().border_color(rgba(LINE));
    }
    if let Some(name) = name {
        button = button.child(icon(name, 16., fg));
    }
    button.child(label.into())
}

/// The one button on a screen that matters most, in the song's color.
pub fn accent_pill(
    id: impl Into<gpui::ElementId>,
    name: Option<&'static str>,
    label: impl Into<SharedString>,
    color: u32,
) -> Stateful<Div> {
    let mut button = div()
        .id(id)
        .h(px(42.))
        .px_5()
        .flex()
        .flex_none()
        .items_center()
        .gap_2()
        .rounded_full()
        .bg(rgba(color))
        .shadow(glow(color, 0.55, 28.))
        .text_color(rgba(ON_ACCENT))
        .text_sm()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .cursor_pointer()
        .hover(|style| style.opacity(0.88));
    if let Some(name) = name {
        button = button.child(icon(name, 16., ON_ACCENT));
    }
    button.child(label.into())
}

/// Maps YouTube's icon names to the app's icon set.
pub fn entry_icon(entry: &MenuEntry) -> &'static str {
    match entry.icon.as_str() {
        "MIX" => "related",
        "QUEUE_PLAY_NEXT" | "ADD_TO_REMOTE_QUEUE" | "QUEUE_MUSIC" => "queue",
        "ADD_TO_PLAYLIST" | "PLAYLIST_ADD" => "plus",
        "ALBUM" => "album",
        "ARTIST" | "PERSON" => "artist",
        "LIBRARY_ADD" | "BOOKMARK_BORDER" | "LIBRARY_MUSIC" => "library",
        "LIBRARY_SAVED" | "LIBRARY_REMOVE" | "DELETE" | "REMOVE_FROM_HISTORY" | "REMOVE_FROM_PLAYLIST" => "close",
        "MUSIC_SHUFFLE" => "shuffle",
        "FAVORITE" | "UNFAVORITE" | "LIKE" => "like_outline",
        "DISLIKE" => "dislike_outline",
        "SUBSCRIBE" | "SUBSCRIPTIONS" => "subscriptions",
        "CHECK" => "check",
        "BLANK" => "blank",
        _ => "note",
    }
}
