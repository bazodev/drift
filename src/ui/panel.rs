//! The right panel: queue, lyrics and related music for the current track.

use gpui::{
    AnyElement, Context, MouseButton, ScrollHandle, ScrollStrategy, SharedString, Subscription, Task,
    UniformListScrollHandle, Window, div, prelude::*, px, rgba, uniform_list,
};
use serde_json::json;

use super::{
    FAINT, HOVER, LINE, MUTED, PANEL, RAISED, SURFACE, TEXT, accent, artwork, icon, page::track_row, round_button,
};
use crate::{
    engine::Command,
    lyrics::{self, TimedLine},
    model::{PanelTab, PlayerEvent, ctx},
    ytm::{self, Body, Item, Lyrics, WatchTabs},
};

#[derive(Clone, Default)]
enum Loaded<T> {
    #[default]
    Idle,
    Loading,
    Ready(T),
    Empty,
}

pub struct PanelView {
    scroll: UniformListScrollHandle,
    video: String,
    tabs: Option<WatchTabs>,
    lyrics: Loaded<Lyrics>,
    related: Loaded<Vec<Item>>,
    timed: Loaded<Vec<TimedLine>>,
    active_line: Option<usize>,
    lyrics_scroll: ScrollHandle,
    task: Option<Task<()>>,
    timed_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl PanelView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let app = ctx(cx);
        let subscriptions = vec![
            cx.subscribe(&app.player, |this, player, event: &PlayerEvent, cx| match event {
                PlayerEvent::TrackChanged => {
                    this.video = player.read(cx).state.video_id.clone();
                    this.tabs = None;
                    this.lyrics = Loaded::Idle;
                    this.related = Loaded::Idle;
                    this.timed = Loaded::Idle;
                    this.active_line = None;
                    this.task = None;
                    this.timed_task = None;
                    this.reveal_current(cx);
                    this.ensure_loaded(cx);
                    cx.notify();
                }
                PlayerEvent::QueueChanged => {
                    this.reveal_current(cx);
                    cx.notify();
                }
                PlayerEvent::AuthChanged => {}
            }),
            cx.observe(&app.ui, |this, _, cx| {
                this.ensure_loaded(cx);
                cx.notify();
            }),
            cx.observe(&app.images, |_, _, cx| cx.notify()),
            cx.observe(&app.player, |this, _, cx| this.follow_position(cx)),
        ];
        Self {
            scroll: UniformListScrollHandle::new(),
            video: String::new(),
            tabs: None,
            lyrics: Loaded::Idle,
            related: Loaded::Idle,
            timed: Loaded::Idle,
            active_line: None,
            lyrics_scroll: ScrollHandle::new(),
            task: None,
            timed_task: None,
            _subscriptions: subscriptions,
        }
    }

    /// Loads synced lyrics when allowed and keeps the current line in view.
    fn follow_position(&mut self, cx: &mut Context<Self>) {
        let app = ctx(cx);
        let (tab, synced) = {
            let ui = app.ui.read(cx);
            (ui.panel, ui.prefs.synced_lyrics)
        };
        if tab != Some(PanelTab::Lyrics) || !synced {
            return;
        }
        let (position, duration, is_ad) = {
            let state = &app.player.read(cx).state;
            (state.position, state.duration, state.is_ad)
        };
        if matches!(self.timed, Loaded::Idle) && !self.video.is_empty() && duration > 0. && !is_ad {
            let state = app.player.read(cx).state.clone();
            let video = self.video.clone();
            self.timed = Loaded::Loading;
            let search = cx.background_spawn(async move { lyrics::fetch(&state.title, &state.artist, duration) });
            self.timed_task = Some(cx.spawn(async move |this, cx| {
                let found = search.await;
                let _ = this.update(cx, |panel, cx| {
                    if panel.video == video {
                        panel.timed = found.map_or(Loaded::Empty, Loaded::Ready);
                        cx.notify();
                    }
                });
            }));
            return;
        }
        if let Loaded::Ready(lines) = &self.timed {
            let active = lyrics::active_line(lines, position);
            if active != self.active_line {
                self.active_line = active;
                if let Some(index) = active {
                    self.lyrics_scroll.scroll_to_item(index);
                }
                cx.notify();
            }
        }
    }

    /// The line being sung, with the ones around it, once synced lyrics are loaded.
    pub fn lines_around(&self) -> Option<(Option<SharedString>, SharedString, Option<SharedString>)> {
        let Loaded::Ready(lines) = &self.timed else { return None };
        let index = self.active_line?;
        let text = |at: usize| {
            lines.get(at).map(|line| SharedString::from(line.text.clone())).filter(|text| !text.trim().is_empty())
        };
        let current = text(index)?;
        Some((index.checked_sub(1).and_then(text), current, text(index + 1)))
    }

    /// Debug only: shows sample lines, so the view can be checked without playing anything.
    #[cfg(debug_assertions)]
    pub fn demo_timed(&mut self, cx: &mut Context<Self>) {
        self.timed = Loaded::Ready(lyrics::parse_lrc(
            "[00:00.00] This line is being sung now\n[00:04.00] The next line\n[00:08.00] The third line\n[00:12.00] The fourth line",
        ));
        self.active_line = Some(0);
        cx.notify();
    }

    fn timed_view(&self, lines: &[TimedLine], cx: &mut Context<Self>) -> AnyElement {
        let client = ctx(cx).client.clone();
        let active = self.active_line;
        div()
            .id("timed-lyrics")
            .track_scroll(&self.lyrics_scroll)
            .size_full()
            .overflow_y_scroll()
            .px_5()
            .py_4()
            .flex()
            .flex_col()
            .gap_2()
            .children(lines.iter().enumerate().map(|(index, line)| {
                let client = client.clone();
                let at = line.at;
                let is_active = active == Some(index);
                div()
                    .id(("timed-line", index))
                    .flex_none()
                    .min_h(px(16.))
                    .text_lg()
                    .line_height(px(27.))
                    .font_weight(if is_active { gpui::FontWeight::BOLD } else { gpui::FontWeight::NORMAL })
                    .text_color(rgba(if is_active { TEXT } else { FAINT }))
                    .cursor_pointer()
                    .hover(|style| style.text_color(rgba(MUTED)))
                    .on_click(move |_, _, _| client.send(Command::Seek(at)))
                    .child(line.text.clone())
            }))
            .into_any_element()
    }

    fn reveal_current(&self, cx: &mut Context<Self>) {
        let index = ctx(cx).player.read(cx).queue.index;
        self.scroll.scroll_to_item(index, ScrollStrategy::Center);
    }

    fn ensure_loaded(&mut self, cx: &mut Context<Self>) {
        let app = ctx(cx);
        let Some(tab) = app.ui.read(cx).panel else { return };
        if self.video.is_empty() || tab == PanelTab::Queue {
            return;
        }
        let needed = match tab {
            PanelTab::Lyrics => matches!(self.lyrics, Loaded::Idle),
            PanelTab::Related => matches!(self.related, Loaded::Idle),
            PanelTab::Queue => false,
        };
        if !needed {
            return;
        }
        match tab {
            PanelTab::Lyrics => self.lyrics = Loaded::Loading,
            _ => self.related = Loaded::Loading,
        }
        let video = self.video.clone();
        let known = self.tabs.clone();
        let client = app.client.clone();
        self.task = Some(cx.spawn(async move |this, cx| {
            let tabs = match known {
                Some(tabs) => tabs,
                None => {
                    let reply = client
                        .api(
                            "next",
                            json!({ "videoId": video, "isAudioOnly": true, "enablePersistentPlaylistPanel": true }),
                        )
                        .await;
                    reply.map(|json| ytm::parse_watch_tabs(&json)).unwrap_or_default()
                }
            };
            let target = if tab == PanelTab::Lyrics { tabs.lyrics.clone() } else { tabs.related.clone() };
            let reply = match &target {
                Some(target) => {
                    let mut body = json!({ "browseId": target.id });
                    if let Some(params) = &target.params {
                        body["params"] = json!(params);
                    }
                    client.api("browse", body).await.ok()
                }
                None => None,
            };
            let _ = this.update(cx, |panel, cx| {
                if panel.video != video {
                    return;
                }
                panel.tabs = Some(tabs);
                if tab == PanelTab::Lyrics {
                    panel.lyrics = reply.as_ref().and_then(ytm::parse_lyrics).map_or(Loaded::Empty, Loaded::Ready);
                } else {
                    let items: Vec<Item> = reply
                        .as_ref()
                        .map(|json| {
                            ytm::parse_page(json)
                                .sections
                                .into_iter()
                                .flat_map(|section| match section.body {
                                    Body::Shelf(items) | Body::List(items) | Body::Picks(items) | Body::Grid(items) => {
                                        items
                                    }
                                    _ => Vec::new(),
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    panel.related = if items.is_empty() { Loaded::Empty } else { Loaded::Ready(items) };
                }
                cx.notify();
            });
        }));
    }

    fn queue_rows(&mut self, range: std::ops::Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let app = ctx(cx);
        let (items, current) = {
            let player = app.player.read(cx);
            (player.queue.items.get(range.clone()).map(<[_]>::to_vec).unwrap_or_default(), player.queue.index)
        };
        let total = app.player.read(cx).queue.items.len();
        let tint = accent(cx);
        items
            .into_iter()
            .enumerate()
            .map(|(offset, item)| {
                let index = range.start + offset;
                let is_current = index == current;
                let group: SharedString = format!("queue-{index}").into();
                let client = app.client.clone();
                let (up, down, remove) = (app.client.clone(), app.client.clone(), app.client.clone());
                div()
                    .id(("queue", index))
                    .group(group.clone())
                    .h(px(60.))
                    .w_full()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_2()
                    .rounded(px(14.))
                    .cursor_pointer()
                    .when(is_current, |row| row.bg(rgba(RAISED)))
                    .hover(|style| style.bg(rgba(HOVER)))
                    .on_click(move |_, _, _| client.send(Command::Select(index)))
                    .child(artwork(&item.thumbnail, 44., 10., cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .truncate()
                                    .text_sm()
                                    .text_color(rgba(if is_current { tint } else { TEXT }))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child(item.title.clone()),
                            )
                            .child(div().truncate().text_xs().text_color(rgba(MUTED)).child(item.artist.clone())),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(rgba(MUTED))
                            .group_hover(group.clone(), |style| style.invisible())
                            .child(item.duration.clone()),
                    )
                    .child(
                        div()
                            .absolute()
                            .right(px(4.))
                            .flex()
                            .items_center()
                            .invisible()
                            .group_hover(group, |style| style.visible())
                            .when(index > 0, |row| {
                                row.child(round_button(("q-up", index), "up", 26., 15., MUTED).on_click(
                                    move |_, _, cx| {
                                        cx.stop_propagation();
                                        up.send(Command::Move(index, index - 1));
                                    },
                                ))
                            })
                            .when(index + 1 < total, |row| {
                                row.child(round_button(("q-down", index), "down", 26., 15., MUTED).on_click(
                                    move |_, _, cx| {
                                        cx.stop_propagation();
                                        down.send(Command::Move(index, index + 1));
                                    },
                                ))
                            })
                            .when(!is_current, |row| {
                                row.child(round_button(("q-remove", index), "close", 26., 14., MUTED).on_click(
                                    move |_, _, cx| {
                                        cx.stop_propagation();
                                        remove.send(Command::Remove(index));
                                    },
                                ))
                            }),
                    )
                    .relative()
                    .into_any_element()
            })
            .collect()
    }

    fn note(label: &str) -> AnyElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .px_6()
            .text_sm()
            .text_color(rgba(FAINT))
            .child(label.to_string())
            .into_any_element()
    }

    fn body(&mut self, tab: PanelTab, cx: &mut Context<Self>) -> AnyElement {
        match tab {
            PanelTab::Queue => {
                let count = ctx(cx).player.read(cx).queue.items.len();
                if count == 0 {
                    return Self::note("The queue is empty");
                }
                uniform_list("queue-list", count, cx.processor(|this, range, _, cx| this.queue_rows(range, cx)))
                    .track_scroll(self.scroll.clone())
                    .size_full()
                    .px_2()
                    .into_any_element()
            }
            PanelTab::Lyrics if ctx(cx).ui.read(cx).prefs.synced_lyrics && matches!(self.timed, Loaded::Ready(_)) => {
                let Loaded::Ready(lines) = self.timed.clone() else { return Self::note("") };
                self.timed_view(&lines, cx)
            }
            PanelTab::Lyrics => match &self.lyrics {
                Loaded::Ready(lyrics) => div()
                    .id("lyrics")
                    .size_full()
                    .overflow_y_scroll()
                    .px_5()
                    .py_4()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(
                        lyrics
                            .text
                            .lines()
                            .map(|line| div().min_h(px(12.)).text_base().line_height(px(26.)).child(line.to_string())),
                    )
                    .when(!lyrics.source.is_empty(), |column| {
                        column.child(div().mt_4().text_xs().text_color(rgba(FAINT)).child(lyrics.source.clone()))
                    })
                    .into_any_element(),
                Loaded::Empty => Self::note("No lyrics for this track"),
                _ if self.video.is_empty() => Self::note("Play a track to see its lyrics"),
                _ => Self::note("Loading lyrics…"),
            },
            PanelTab::Related => match self.related.clone() {
                Loaded::Ready(items) => {
                    let rows: Vec<AnyElement> = items
                        .iter()
                        .enumerate()
                        .map(|(index, item)| track_row(item, 7_000_000 + index, false, false, cx))
                        .collect();
                    div().id("related").size_full().overflow_y_scroll().px_2().children(rows).into_any_element()
                }
                Loaded::Empty => Self::note("No related music found"),
                _ if self.video.is_empty() => Self::note("Play a track to see related music"),
                _ => Self::note("Loading…"),
            },
        }
    }
}

impl Render for PanelView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let app = ctx(cx);
        let (tab, now_open, tint) = {
            let ui = app.ui.read(cx);
            (ui.panel.unwrap_or(PanelTab::Queue), ui.now_open, ui.accent)
        };
        let tabs = [(PanelTab::Queue, "Up next"), (PanelTab::Lyrics, "Lyrics"), (PanelTab::Related, "Related")];
        let ui = app.ui.clone();
        let header = div()
            .h(px(54.))
            .flex_none()
            .flex()
            .items_center()
            .gap_5()
            .px_5()
            .border_b_1()
            .border_color(rgba(LINE))
            .children(tabs.into_iter().map(|(value, label)| {
                let ui = ui.clone();
                let active = value == tab;
                div()
                    .id(label)
                    .h_full()
                    .flex()
                    .items_center()
                    .text_sm()
                    .font_weight(if active { gpui::FontWeight::SEMIBOLD } else { gpui::FontWeight::NORMAL })
                    .text_color(rgba(if active { TEXT } else { MUTED }))
                    .when(active, |button| button.border_b_2().border_color(rgba(tint)))
                    .cursor_pointer()
                    .hover(|style| style.text_color(rgba(TEXT)))
                    .on_click(move |_, _, cx| {
                        ui.update(cx, |ui, cx| {
                            ui.panel = Some(value);
                            cx.notify();
                        });
                    })
                    .child(label)
            }))
            .child(div().flex_1())
            .when(!now_open, |row| {
                row.child(round_button("close-panel", "close", 30., 16., MUTED).on_click({
                    let ui = app.ui.clone();
                    move |_, _, cx| {
                        ui.update(cx, |ui, cx| {
                            ui.panel = None;
                            cx.notify();
                        });
                    }
                }))
            });
        let body = self.body(tab, cx);
        div()
            .w(px(PANEL))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .rounded(px(22.))
            .bg(rgba(SURFACE))
            .border_1()
            .border_color(rgba(LINE))
            .overflow_hidden()
            .text_color(rgba(TEXT))
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .child(header)
            .child(div().flex_1().min_h_0().py_2().child(body))
            .child(div().hidden().child(icon("note", 1., MUTED)))
    }
}
