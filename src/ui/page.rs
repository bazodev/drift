//! The main content area: renders any YouTube Music page from the parsed model.

use std::rc::Rc;

use gpui::{
    AnyElement, App, Context, ElementId, ListAlignment, ListScrollEvent, ListState, MouseButton, SharedString,
    Subscription, Window, div, list, prelude::*, px, rgb, rgba,
};

use super::{
    BG, FAINT, HOVER, LINE, MUTED, ON_ACCENT, PANEL, RAISED, SIDEBAR, SURFACE, TEXT, accent, accent_pill, artwork,
    glow, icon, pill, round_button,
};
use crate::{
    model::{self, PageState, PlayerEvent, browse, ctx, item_link, open_menu, run_action},
    ytm::{Action, Body, Chip, Header, Item, Kind, Page, Route, Section, Tile},
};

const PAD: f32 = 32.;
const CARD: f32 = 176.;
const GAP: f32 = 22.;
const PICK: f32 = 340.;

/// The library's own landing page.
pub const LIBRARY_HOME: &str = "FEmusic_library_landing";
/// The library's sections, as chips at the top of each of them.
const LIBRARY_TABS: [(&str, &str); 7] = [
    ("All", LIBRARY_HOME),
    ("Playlists", "FEmusic_liked_playlists"),
    ("Songs", "FEmusic_liked_videos"),
    ("Albums", "FEmusic_liked_albums"),
    ("Artists", "FEmusic_library_corpus_track_artists"),
    ("Subscriptions", "FEmusic_library_corpus_artists"),
    ("History", "FEmusic_history"),
];

pub fn is_library(id: &str) -> bool {
    LIBRARY_TABS.iter().any(|(_, tab)| *tab == id)
}

fn chip_button(id: impl Into<ElementId>, label: SharedString, selected: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h(px(36.))
        .px_4()
        .flex()
        .flex_none()
        .items_center()
        .rounded_full()
        .text_sm()
        .when(selected, |chip| chip.bg(rgba(TEXT)).text_color(rgba(BG)).font_weight(gpui::FontWeight::SEMIBOLD))
        .when(!selected, |chip| chip.bg(rgba(SURFACE)).border_1().border_color(rgba(LINE)).text_color(rgba(TEXT)))
        .cursor_pointer()
        .hover(|style| style.opacity(0.86))
        .child(label)
}

fn library_chips(current: &str) -> AnyElement {
    div()
        .id("library-tabs")
        .flex()
        .gap_2()
        .pt_1()
        .pb_3()
        .overflow_x_scroll()
        .children(LIBRARY_TABS.iter().enumerate().map(|(index, &(label, id))| {
            chip_button(("library-tab", index), label.into(), id == current)
                .on_click(move |_, _, cx| model::open(browse(id), cx))
        }))
        .into_any_element()
}

#[derive(Clone, Copy)]
enum Row {
    Library,
    Header,
    Chips,
    Title(usize),
    Shelf(usize),
    Picks(usize, usize),
    Track(usize, usize),
    Grid(usize, usize),
    Tiles(usize, usize),
    Text(usize),
    End,
}

pub struct PageView {
    list: ListState,
    rows: Vec<Row>,
    page: Option<Rc<Page>>,
    /// The library section on screen, which gets the library chips.
    library: Option<String>,
    current: String,
    columns: usize,
    pick_columns: usize,
    _subscriptions: Vec<Subscription>,
}

pub fn track_menu(item: &Item, window: &Window, cx: &mut App) {
    open_menu(item.title.clone(), item.menu.clone(), item_link(item), window, cx);
}

fn primary(item: &Item) -> Option<Action> {
    if item.is_track() {
        item.play.clone().or_else(|| item.open.clone())
    } else {
        item.open.clone().or_else(|| item.play.clone())
    }
}

fn subtitle(item: &Item, key: usize) -> impl IntoElement {
    div().flex().min_w_0().overflow_hidden().whitespace_nowrap().text_xs().text_color(rgba(MUTED)).children(
        item.subtitle.iter().enumerate().map(|(index, link)| {
            let label = div().id(("sub", key * 16 + index)).flex_none().child(link.text.clone());
            match link.route.clone() {
                Some(route) => {
                    label.cursor_pointer().hover(|style| style.text_color(rgba(TEXT))).on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        model::open(route.clone(), cx);
                    })
                }
                None => label,
            }
        }),
    )
}

pub fn track_row(item: &Item, key: usize, current: bool, show_duration: bool, cx: &mut App) -> AnyElement {
    let group: SharedString = format!("track-{key}").into();
    let action = primary(item);
    let menu_item = item.clone();
    let context_item = item.clone();
    let has_menu = !item.menu.is_empty();
    let round = item.kind == Kind::Artist;
    let tint = accent(cx);
    let art = artwork(&item.thumbnail, 44., if round { 22. } else { 8. }, cx);
    div()
        .id(("track", key))
        .group(group.clone())
        .h(px(60.))
        .w_full()
        .flex()
        .items_center()
        .gap_3()
        .px_2()
        .rounded(px(12.))
        .cursor_pointer()
        .when(current, |row| row.bg(rgba(RAISED)))
        .hover(|style| style.bg(rgba(HOVER)))
        .on_click(move |_, _, cx| {
            if let Some(action) = &action {
                run_action(action, cx);
            }
        })
        .on_mouse_down(MouseButton::Right, move |_, window, cx| {
            cx.stop_propagation();
            track_menu(&context_item, window, cx);
        })
        .child(
            div().relative().size(px(44.)).flex_none().child(art).child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(if round { 22. } else { 8. }))
                    .bg(gpui::black().opacity(0.5))
                    .invisible()
                    .group_hover(group.clone(), |style| style.visible())
                    .child(icon("play", 20., TEXT)),
            ),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(1.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .min_w_0()
                        .child(
                            div()
                                .truncate()
                                .text_sm()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(rgba(if current { tint } else { TEXT }))
                                .child(item.title.clone()),
                        )
                        .when(item.explicit, |row| {
                            row.child(
                                div()
                                    .flex_none()
                                    .px_1()
                                    .rounded(px(2.))
                                    .bg(rgba(RAISED))
                                    .text_size(px(9.))
                                    .text_color(rgba(MUTED))
                                    .child("E"),
                            )
                        }),
                )
                .child(subtitle(item, key)),
        )
        .when(show_duration && !item.duration.is_empty(), |row| {
            row.child(div().flex_none().text_xs().text_color(rgba(MUTED)).child(item.duration.clone()))
        })
        .when(has_menu, |row| {
            row.child(div().invisible().group_hover(group.clone(), |style| style.visible()).child(
                round_button(("track-more", key), "more", 32., 18., MUTED).on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    track_menu(&menu_item, window, cx);
                }),
            ))
        })
        .into_any_element()
}

fn card(item: &Item, key: usize, width: f32, current: &str, cx: &mut App) -> AnyElement {
    let group: SharedString = format!("card-{key}").into();
    let round = item.kind == Kind::Artist;
    let radius = if round { width / 2. } else { 16. };
    let tint = accent(cx);
    let open = primary(item);
    let quick = item.play.clone();
    let context_item = item.clone();
    let menu_item = item.clone();
    let has_menu = !item.menu.is_empty();
    let is_current = item.video_id.as_deref().is_some_and(|id| id == current);
    let art = artwork(&item.thumbnail, width, radius, cx);
    div()
        .id(("card", key))
        .group(group.clone())
        .w(px(width))
        .flex_none()
        .flex()
        .flex_col()
        .gap_1()
        .cursor_pointer()
        .when(round, |card| card.items_center())
        .on_click(move |_, _, cx| {
            if let Some(action) = &open {
                run_action(action, cx);
            }
        })
        .on_mouse_down(MouseButton::Right, move |_, window, cx| {
            cx.stop_propagation();
            track_menu(&context_item, window, cx);
        })
        .child(
            div()
                .relative()
                .size(px(width))
                .mb_1()
                .rounded(px(radius))
                .shadow(glow(0x000000ff, 0.7, 34.))
                .child(art)
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded(px(radius))
                        .bg(gpui::black().opacity(0.28))
                        .invisible()
                        .group_hover(group.clone(), |style| style.visible())
                        .when_some(quick, |overlay, action| {
                            overlay.child(
                                div()
                                    .id(("card-play", key))
                                    .absolute()
                                    .right(px(10.))
                                    .bottom(px(10.))
                                    .size(px(40.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_full()
                                    .bg(rgba(tint))
                                    .shadow(glow(tint, 0.6, 22.))
                                    .hover(|style| style.opacity(0.88))
                                    .on_click(move |_, _, cx| {
                                        cx.stop_propagation();
                                        run_action(&action, cx);
                                    })
                                    .child(icon("play", 20., ON_ACCENT)),
                            )
                        })
                        .when(has_menu, |overlay| {
                            overlay.child(div().absolute().right(px(6.)).top(px(6.)).child(
                                round_button(("card-more", key), "more", 30., 18., TEXT).on_click(
                                    move |_, window, cx| {
                                        cx.stop_propagation();
                                        track_menu(&menu_item, window, cx);
                                    },
                                ),
                            ))
                        }),
                ),
        )
        .child(
            div()
                .w_full()
                .when(round, |title| title.flex().justify_center())
                .line_clamp(2)
                .text_sm()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(rgba(if is_current { tint } else { TEXT }))
                .child(item.title.clone()),
        )
        .child(
            div()
                .w_full()
                .when(round, |line| line.flex().justify_center())
                .line_clamp(2)
                .text_xs()
                .text_color(rgba(MUTED))
                .child(item.subtitle_text()),
        )
        .into_any_element()
}

fn tile(item: &Tile, key: usize, width: f32) -> AnyElement {
    let route = item.route.clone();
    div()
        .id(("tile", key))
        .w(px(width))
        .h(px(52.))
        .flex_none()
        .flex()
        .items_center()
        .rounded(px(12.))
        .overflow_hidden()
        .bg(rgba(SURFACE))
        .border_1()
        .border_color(rgba(LINE))
        .cursor_pointer()
        .hover(|style| style.bg(rgba(RAISED)))
        .on_click(move |_, _, cx| {
            if let Some(route) = &route {
                model::open(route.clone(), cx);
            }
        })
        .child(div().w(px(5.)).h_full().flex_none().bg(rgb(item.color)))
        .child(div().px_3().truncate().text_sm().font_weight(gpui::FontWeight::SEMIBOLD).child(item.title.clone()))
        .into_any_element()
}

fn chips(chips: &[Chip]) -> AnyElement {
    div()
        .id("chips")
        .flex()
        .gap_2()
        .py_3()
        .overflow_x_scroll()
        .children(chips.iter().enumerate().map(|(index, chip)| {
            let route = chip.route.clone();
            chip_button(("chip", index), chip.label.clone().into(), chip.selected).on_click(move |_, _, cx| {
                if let Some(route) = &route {
                    model::open(route.clone(), cx);
                }
            })
        }))
        .into_any_element()
}

fn header(value: &Header, cx: &mut App) -> AnyElement {
    let size = if value.round { 200. } else { 232. };
    let tint = accent(cx);
    let radius = if value.round { size / 2. } else { 22. };
    let art = (!value.thumbnail.is_empty()).then(|| {
        div().flex_none().rounded(px(radius)).shadow(glow(tint, 0.55, 60.)).child(artwork(
            &value.thumbnail,
            size,
            radius,
            cx,
        ))
    });
    let (title_size, title_line) = match value.title.chars().count() {
        0..=18 => (px(60.), px(64.)),
        19..=44 => (px(40.), px(46.)),
        _ => (px(28.), px(34.)),
    };
    let mut buttons: Vec<AnyElement> = Vec::new();
    if let Some(action) = value.play.clone() {
        buttons.push(
            accent_pill("header-play", Some("play"), "Play", tint)
                .on_click(move |_, _, cx| run_action(&action, cx))
                .into_any_element(),
        );
    }
    if let Some(action) = value.shuffle.clone() {
        // Artists have no "Play": then shuffling is the main thing to do.
        let button = if value.play.is_none() {
            accent_pill("header-shuffle", Some("shuffle"), "Shuffle", tint)
        } else {
            pill("header-shuffle", Some("shuffle"), "Shuffle", false)
        };
        buttons.push(button.on_click(move |_, _, cx| run_action(&action, cx)).into_any_element());
    }
    if let Some(action) = value.radio.clone() {
        buttons.push(
            pill("header-radio", Some("related"), "Radio", false)
                .on_click(move |_, _, cx| run_action(&action, cx))
                .into_any_element(),
        );
    }
    if let Some(entry) = value.library.clone() {
        let label = entry.label.clone();
        let saved = entry.destructive;
        buttons.push(
            pill("header-library", Some(if saved { "check" } else { "plus" }), label, false)
                .on_click(move |_, _, cx| model::run_entry(&entry, cx))
                .into_any_element(),
        );
    }
    if let Some(subscribe) = value.subscribe.clone() {
        let label = if subscribe.label.is_empty() {
            if subscribe.subscribed { "Subscribed".to_string() } else { "Subscribe".to_string() }
        } else {
            subscribe.label.clone()
        };
        buttons.push(
            pill("header-subscribe", Some(if subscribe.subscribed { "check" } else { "subscriptions" }), label, false)
                .on_click(move |_, _, cx| model::set_subscribed(&subscribe.channel_id, !subscribe.subscribed, cx))
                .into_any_element(),
        );
    }
    if !value.menu.is_empty() {
        let (title, entries) = (value.title.clone(), value.menu.clone());
        buttons.push(
            round_button("header-more", "more", 38., 20., TEXT)
                .bg(rgba(RAISED))
                .border_1()
                .border_color(rgba(LINE))
                .on_click(move |_, window, cx| open_menu(title.clone(), entries.clone(), None, window, cx))
                .into_any_element(),
        );
    }
    div()
        .flex()
        .items_end()
        .gap_6()
        .pt_4()
        .pb_6()
        .children(art)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .line_clamp(2)
                        .text_size(title_size)
                        .line_height(title_line)
                        .font_weight(gpui::FontWeight::BOLD)
                        .child(value.title.clone()),
                )
                .when(!value.subtitle.is_empty(), |column| {
                    column.child(div().text_sm().text_color(rgba(MUTED)).child(value.subtitle.clone()))
                })
                .when(!value.second.is_empty(), |column| {
                    column.child(div().text_sm().text_color(rgba(MUTED)).child(value.second.clone()))
                })
                .when(!value.description.is_empty(), |column| {
                    column.child(div().line_clamp(3).text_sm().text_color(rgba(MUTED)).child(value.description.clone()))
                })
                .when(!buttons.is_empty(), |column| {
                    column.child(div().mt_2().flex().flex_wrap().gap_2().children(buttons))
                }),
        )
        .into_any_element()
}

fn section_title(section: &Section, index: usize) -> AnyElement {
    let more = section.more.clone();
    div()
        .pt_6()
        .pb_3()
        .flex()
        .items_end()
        .justify_between()
        .gap_4()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .when(!section.strapline.is_empty(), |column| {
                    column.child(div().truncate().text_xs().text_color(rgba(MUTED)).child(section.strapline.clone()))
                })
                .child(
                    div()
                        .truncate()
                        .text_size(px(22.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .child(section.title.clone()),
                ),
        )
        .when_some(more, |row, route| {
            row.child(
                div()
                    .id(("more", index))
                    .flex_none()
                    .h(px(32.))
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_1()
                    .rounded_full()
                    .text_sm()
                    .text_color(rgba(MUTED))
                    .bg(rgba(SURFACE))
                    .border_1()
                    .border_color(rgba(LINE))
                    .cursor_pointer()
                    .hover(|style| style.bg(rgba(RAISED)).text_color(rgba(TEXT)))
                    .on_click(move |_, _, cx| model::open(route.clone(), cx))
                    .child("See all")
                    .child(icon("forward", 14., MUTED)),
            )
        })
        .into_any_element()
}

impl PageView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let app = ctx(cx);
        let list_state = ListState::new(0, ListAlignment::Top, px(700.));
        list_state.set_scroll_handler(cx.listener(|this: &mut Self, event: &ListScrollEvent, _, cx| {
            if event.visible_range.end + 8 >= this.rows.len() && this.has_more() {
                ctx(cx).router.update(cx, |router, cx| router.load_more(cx));
            }
        }));
        let subscriptions = vec![
            cx.observe(&app.router, |this, _, cx| this.sync(cx)),
            cx.observe(&app.images, |_, _, cx| cx.notify()),
            cx.subscribe(&app.player, |this, player, event: &PlayerEvent, cx| {
                if *event == PlayerEvent::TrackChanged {
                    this.current = player.read(cx).state.video_id.clone();
                    cx.notify();
                }
            }),
        ];
        Self {
            list: list_state,
            rows: Vec::new(),
            page: None,
            library: None,
            current: String::new(),
            columns: 4,
            pick_columns: 2,
            _subscriptions: subscriptions,
        }
    }

    fn has_more(&self) -> bool {
        self.page.as_ref().is_some_and(|page| {
            page.continuation.is_some() || page.sections.iter().any(|section| section.continuation.is_some())
        })
    }

    fn sync(&mut self, cx: &mut Context<Self>) {
        self.library = match ctx(cx).router.read(cx).current() {
            Route::Browse(target) if is_library(&target.id) => Some(target.id.clone()),
            _ => None,
        };
        let state = &ctx(cx).router.read(cx).state;
        match state {
            PageState::Ready(page) => {
                let appended = self.page.as_ref().is_some_and(|old| {
                    !Rc::ptr_eq(old, page) && old.sections.len() <= page.sections.len() && same_start(old, page)
                });
                if self.page.as_ref().is_some_and(|old| Rc::ptr_eq(old, page)) {
                    cx.notify();
                    return;
                }
                self.page = Some(page.clone());
                let previous = self.rows.len();
                self.rebuild();
                if appended && self.rows.len() >= previous && previous > 0 {
                    self.list.splice(previous - 1..previous, self.rows.len() - previous + 1);
                } else {
                    self.list.reset(self.rows.len());
                }
            }
            _ => {
                self.page = None;
                self.rows.clear();
                self.list.reset(0);
            }
        }
        cx.notify();
    }

    fn rebuild(&mut self) {
        self.rows.clear();
        let Some(page) = self.page.clone() else { return };
        if self.library.is_some() {
            self.rows.push(Row::Library);
        }
        if page.header.is_some() {
            self.rows.push(Row::Header);
        }
        if !page.chips.is_empty() {
            self.rows.push(Row::Chips);
        }
        for (index, section) in page.sections.iter().enumerate() {
            if !section.title.is_empty() {
                self.rows.push(Row::Title(index));
            }
            match &section.body {
                Body::Shelf(_) => self.rows.push(Row::Shelf(index)),
                Body::Picks(items) => {
                    for start in (0..items.len()).step_by(self.pick_columns) {
                        self.rows.push(Row::Picks(index, start));
                    }
                }
                Body::List(items) => {
                    self.rows.extend((0..items.len()).map(|item| Row::Track(index, item)));
                }
                Body::Grid(items) => {
                    for start in (0..items.len()).step_by(self.columns) {
                        self.rows.push(Row::Grid(index, start));
                    }
                }
                Body::Tiles(items) => {
                    for start in (0..items.len()).step_by(self.columns) {
                        self.rows.push(Row::Tiles(index, start));
                    }
                }
                Body::Text(_) => self.rows.push(Row::Text(index)),
            }
        }
        self.rows.push(Row::End);
    }

    fn render_row(&mut self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let (Some(page), Some(row)) = (self.page.clone(), self.rows.get(index).copied()) else {
            return div().into_any_element();
        };
        let current = self.current.clone();
        let columns = self.columns;
        let pick_columns = self.pick_columns;
        let content: AnyElement = match row {
            Row::Library => library_chips(self.library.as_deref().unwrap_or(LIBRARY_HOME)),
            Row::Header => page.header.as_ref().map_or_else(|| div().into_any_element(), |value| header(value, cx)),
            Row::Chips => chips(&page.chips),
            Row::Title(section) => section_title(&page.sections[section], section),
            Row::Shelf(section) => {
                let Body::Shelf(items) = &page.sections[section].body else { return div().into_any_element() };
                let cards: Vec<AnyElement> = items
                    .iter()
                    .enumerate()
                    .map(|(item, value)| card(value, section * 1000 + item, CARD, &current, cx))
                    .collect();
                div()
                    .id(("shelf", section))
                    .flex()
                    .gap(px(GAP))
                    .pb_2()
                    .overflow_x_scroll()
                    .children(cards)
                    .into_any_element()
            }
            Row::Picks(section, start) => {
                let Body::Picks(items) = &page.sections[section].body else { return div().into_any_element() };
                let cells: Vec<AnyElement> = items
                    .iter()
                    .enumerate()
                    .skip(start)
                    .take(pick_columns)
                    .map(|(item, value)| {
                        let is_current = value.video_id.as_deref() == Some(current.as_str());
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(track_row(value, section * 1000 + item, is_current, false, cx))
                            .into_any_element()
                    })
                    .collect();
                let missing = pick_columns.saturating_sub(cells.len());
                div().flex().gap_2().children(cells).children((0..missing).map(|_| div().flex_1())).into_any_element()
            }
            Row::Track(section, item) => {
                let Body::List(items) = &page.sections[section].body else { return div().into_any_element() };
                let value = &items[item];
                let is_current = value.video_id.as_deref() == Some(current.as_str());
                track_row(value, section * 100_000 + item, is_current, true, cx)
            }
            Row::Grid(section, start) => {
                let Body::Grid(items) = &page.sections[section].body else { return div().into_any_element() };
                let cards: Vec<AnyElement> = items
                    .iter()
                    .enumerate()
                    .skip(start)
                    .take(columns)
                    .map(|(item, value)| card(value, section * 1000 + item, CARD, &current, cx))
                    .collect();
                div().flex().gap(px(GAP)).pb_5().children(cards).into_any_element()
            }
            Row::Tiles(section, start) => {
                let Body::Tiles(items) = &page.sections[section].body else { return div().into_any_element() };
                let tiles: Vec<AnyElement> = items
                    .iter()
                    .enumerate()
                    .skip(start)
                    .take(columns)
                    .map(|(item, value)| tile(value, section * 1000 + item, CARD))
                    .collect();
                div().flex().gap(px(GAP)).pb_3().children(tiles).into_any_element()
            }
            Row::Text(section) => {
                let Body::Text(value) = &page.sections[section].body else { return div().into_any_element() };
                div().pb_4().text_sm().text_color(rgba(MUTED)).child(value.clone()).into_any_element()
            }
            Row::End => div()
                .h(px(56.))
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(rgba(FAINT))
                .when(self.has_more(), |row| row.child("Loading more…"))
                .when(page.sections.is_empty(), |row| row.text_sm().text_color(rgba(MUTED)).child("Nothing here yet"))
                .into_any_element(),
        };
        div().w_full().px(px(PAD)).child(content).into_any_element()
    }
}

fn same_start(old: &Page, new: &Page) -> bool {
    match (old.header.as_ref(), new.header.as_ref()) {
        (Some(a), Some(b)) if a.title != b.title => return false,
        (None, Some(_)) | (Some(_), None) => return false,
        _ => {}
    }
    old.sections.first().map(|s| &s.title) == new.sections.first().map(|s| &s.title)
}

fn centered(label: impl Into<SharedString>) -> gpui::Div {
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_3()
        .text_sm()
        .text_color(rgba(MUTED))
        .child(label.into())
}

impl Render for PageView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let app = ctx(cx);
        let panel_open = app.ui.read(cx).panel.is_some();
        let width =
            f32::from(window.viewport_size().width) - SIDEBAR - if panel_open { PANEL + 24. } else { 0. } - PAD * 2.;
        let columns = (((width + GAP) / (CARD + GAP)).floor() as usize).max(2);
        let pick_columns = (((width + 8.) / (PICK + 8.)).floor() as usize).clamp(1, 3);
        if columns != self.columns || pick_columns != self.pick_columns {
            self.columns = columns;
            self.pick_columns = pick_columns;
            if self.page.is_some() {
                self.rebuild();
                let count = self.list.item_count();
                self.list.splice(0..count, self.rows.len());
            }
        }

        match &app.router.read(cx).state {
            PageState::Loading => centered("Loading…").into_any_element(),
            PageState::Failed(error) => centered(format!("Couldn't open this page: {error}"))
                .child(pill("retry", None, "Try again", true).on_click(|_, _, cx| {
                    ctx(cx).router.update(cx, |router, cx| router.reload(cx));
                }))
                .into_any_element(),
            PageState::Ready(page) if page.sections.is_empty() && page.header.is_none() && page.chips.is_empty() => {
                centered("Nothing here yet").into_any_element()
            }
            PageState::Ready(_) => {
                list(self.list.clone(), cx.processor(|this, index, _, cx| this.render_row(index, cx)))
                    .size_full()
                    .into_any_element()
            }
        }
    }
}
