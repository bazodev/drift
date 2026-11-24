//! The window: sidebar, top bar, overlays, sign-in gate and engine event loop.

use std::{cell::Cell, rc::Rc, time::Duration};

use futures::{StreamExt, channel::mpsc::UnboundedReceiver};
use gpui::{
    AnyElement, AnyView, App, Bounds, Context, Corner, Entity, FocusHandle, Focusable, MouseButton, Pixels,
    SharedString, StyleRefinement, Subscription, Task, Window, anchored, canvas, deferred, div, linear_color_stop,
    linear_gradient, point, prelude::*, px, rgba,
};
use serde_json::json;

use super::{
    DANGER, Dismiss, FAINT, FocusSearch, GoBack, GoForward, HOVER, LINE, MUTED, NextTrack, ON_ACCENT, PANEL,
    PreviousTrack, RAISED, SCRIM, SIDEBAR, SOLID, SURFACE, TEXT, TOP_BAR, TRANSPORT, TogglePlay, ToggleQueue,
    VolumeDown, VolumeUp, accent_pill, artwork, entry_icon,
    field::{FieldEvent, TextField},
    glow, icon,
    page::PageView,
    panel::PanelView,
    pill, round_button,
    settings::SettingsView,
    transport::TransportView,
};
use crate::{
    engine::{Client, Command, Engine, EngineEvent, HOME, is_consent_page, is_music_page},
    images::Images,
    model::{self, Ctx, Modal, OpenMenu, PanelTab, PlayerEvent, PlayerModel, Router, UiState, browse, ctx},
    ytm::{self, Action, Route, Suggestion},
};

const AUTH_BAR: f32 = 48.;
const VOLUME_STEP: f64 = 5.;
const SEARCH_WIDTH: f32 = 520.;
/// The second color of gradients that start in the song's accent.
const VIOLET: u32 = 0x7c5bb1ff;

const MAIN_NAV: [(&str, &str, &str); 3] = [
    ("Home", "home", "FEmusic_home"),
    ("Explore", "explore", "FEmusic_explore"),
    ("Library", "library", super::page::LIBRARY_HOME),
];

pub struct Shell {
    focus: FocusHandle,
    search: Entity<TextField>,
    playlist_name: Entity<TextField>,
    page: Entity<PageView>,
    transport: Entity<TransportView>,
    panel: Entity<PanelView>,
    settings: Entity<SettingsView>,
    suggestions: Vec<Suggestion>,
    suggest_open: bool,
    suggest_index: Option<usize>,
    suggest_task: Option<Task<()>>,
    /// Where the search field is, so suggestions open right under it.
    search_bounds: Rc<Cell<Bounds<Pixels>>>,
    auto_opened: bool,
    _events: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl Shell {
    pub fn build(window: &mut Window, cx: &mut App) -> Entity<Self> {
        let (engine, _tx, rx) = Engine::new(&*window).expect("failed to create the web engine");
        #[cfg(debug_assertions)]
        crate::dev::listen(_tx);
        let client = Client::new(engine, cx.background_executor().clone());
        let images = Images::new(cx);
        let player = cx.new(|_| PlayerModel::default());
        let router = cx.new(|_| Router::new());
        let ui = cx.new(|_| UiState::new());
        cx.set_global(Ctx { client, images, player, router, ui });
        cx.new(|cx| Self::new(rx, window, cx))
    }

    fn new(mut rx: UnboundedReceiver<EngineEvent>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let app = ctx(cx);
        let events = cx.spawn(async move |this, cx| {
            while let Some(event) = rx.next().await {
                if this.update(cx, |shell, cx| shell.handle_engine(event, cx)).is_err() {
                    break;
                }
            }
        });
        let search = cx.new(|cx| TextField::new("Songs, albums, artists, podcasts", cx));
        let playlist_name = cx.new(|cx| TextField::new("Playlist name", cx));
        let page = cx.new(PageView::new);
        let transport = cx.new(TransportView::new);
        let panel = cx.new(PanelView::new);
        let settings = cx.new(SettingsView::new);
        let subscriptions = vec![
            cx.subscribe(&search, Self::on_search),
            cx.subscribe(&playlist_name, Self::on_playlist_name),
            cx.subscribe(&app.player, |this, player, event: &PlayerEvent, cx| {
                if *event == PlayerEvent::AuthChanged {
                    this.on_auth(player.read(cx).state.signed_in, cx);
                }
            }),
            cx.observe(&app.ui, |_, _, cx| cx.notify()),
            cx.observe(&app.router, |_, _, cx| cx.notify()),
            cx.observe(&app.images, |_, _, cx| cx.notify()),
            // "Now" and the full-screen lyrics show the song itself, so they follow the player and its lines.
            cx.observe(&app.player, |_, _, cx| {
                let ui = ctx(cx).ui.read(cx);
                if ui.now_open || ui.immersive {
                    cx.notify();
                }
            }),
            cx.observe(&panel, |_, _, cx| {
                if ctx(cx).ui.read(cx).immersive {
                    cx.notify();
                }
            }),
        ];
        let focus = cx.focus_handle();
        window.focus(&focus);
        Self {
            focus,
            search,
            playlist_name,
            page,
            transport,
            panel,
            settings,
            suggestions: Vec::new(),
            suggest_open: false,
            suggest_index: None,
            suggest_task: None,
            search_bounds: Rc::default(),
            auto_opened: false,
            _events: events,
            _subscriptions: subscriptions,
        }
    }

    fn handle_engine(&mut self, event: EngineEvent, cx: &mut Context<Self>) {
        let app = ctx(cx);
        match event {
            EngineEvent::State(state) => {
                #[cfg(debug_assertions)]
                crate::dev::dump(&state);
                app.client.set_ready(true);
                app.player.update(cx, |player, cx| player.apply_state(*state, cx));
            }
            EngineEvent::Queue(queue) => app.player.update(cx, |player, cx| player.apply_queue(queue, cx)),
            EngineEvent::Reply(id, result) => app.client.resolve(id, result),
            EngineEvent::PageStarted(url) => {
                app.client.set_ready(false);
                app.client.engine().page_started(&url);
                self.follow_page(&url, cx);
            }
            EngineEvent::Navigate(url) => app.client.engine().navigate(&url),
            EngineEvent::OpenExternal(url) => cx.open_url(&url),
            #[cfg(debug_assertions)]
            EngineEvent::Dev(line) => self.run_dev_command(&line, cx),
        }
    }

    #[cfg(debug_assertions)]
    fn run_dev_command(&mut self, line: &str, cx: &mut Context<Self>) {
        let app = ctx(cx);
        let (name, arg) = line.trim().split_once(' ').unwrap_or((line.trim(), ""));
        match name {
            "toggle" => app.client.send(Command::Toggle),
            "next" => app.client.send(Command::Next),
            "previous" => app.client.send(Command::Previous),
            "seek" => app.client.send(Command::Seek(arg.parse().unwrap_or(0.))),
            "volume" => app.client.send(Command::Volume(arg.parse().unwrap_or(100.))),
            "mute" => app.client.send(Command::Mute),
            "eval" => app.client.engine().eval_logged(arg),
            "browse" => model::open(browse(arg), cx),
            "search" => model::open(Route::Search { query: arg.to_string(), params: None }, cx),
            "play" => {
                let endpoint = json!({ "videoId": arg });
                model::run_action(&Action::Play(endpoint), cx);
            }
            "panel" => {
                let tab = match arg {
                    "queue" => Some(PanelTab::Queue),
                    "lyrics" => Some(PanelTab::Lyrics),
                    "related" => Some(PanelTab::Related),
                    _ => None,
                };
                app.ui.update(cx, |ui, cx| {
                    ui.panel = tab;
                    cx.notify();
                });
            }
            "report" => self.report_page(cx),
            "hit" => app.client.engine().report_hit(),
            "settings" => model::set_settings_open(arg != "off", cx),
            "now" => model::set_now_open(arg != "off", cx),
            "immersive" => model::set_immersive(arg != "off", cx),
            "settings-report" => self.settings.read(cx).report(cx),
            // In memory only: the saved preference stays as the user left it.
            "synced" => app.ui.update(cx, |ui, cx| {
                ui.prefs.synced_lyrics = arg == "on";
                cx.notify();
            }),
            // In memory only, like "synced".
            "clip" => app.ui.update(cx, |ui, cx| {
                ui.prefs.clip_particles = arg == "on";
                cx.notify();
            }),
            "island" | "effect" | "main-window" | "swipe-hint" | "strip" | "activity" | "window" => {
                crate::live::dev_command(name, arg, cx)
            }
            "timed-demo" => self.panel.update(cx, |panel, cx| panel.demo_timed(cx)),
            "more" => app.router.update(cx, |router, cx| router.load_more(cx)),
            "select" => app.client.send(Command::Select(arg.parse().unwrap_or(0))),
            "remove" => app.client.send(Command::Remove(arg.parse().unwrap_or(0))),
            "move" => {
                let mut parts = arg.split(' ').filter_map(|part| part.parse().ok());
                if let (Some(from), Some(to)) = (parts.next(), parts.next()) {
                    app.client.send(Command::Move(from, to));
                }
            }
            "shuffle" => app.client.send(Command::Shuffle),
            "repeat" => app.client.send(Command::Repeat(match arg {
                "ALL" => "ALL",
                "ONE" => "ONE",
                _ => "NONE",
            })),
            "queue-add" => {
                let endpoint = json!({ "queueAddEndpoint": {
                    "queueTarget": { "videoId": arg },
                    "queueInsertPosition": "INSERT_AT_END",
                }});
                model::run_action(&Action::Service(endpoint), cx);
            }
            "suggest" => {
                let call = app.client.api("music/get_search_suggestions", json!({ "input": arg }));
                cx.spawn(async move |_, _| {
                    let count = call.await.map(|json| ytm::parse_suggestions(&json).len());
                    eprintln!("suggest: {count:?}");
                })
                .detach();
            }
            "queue-info" => {
                let player = app.player.read(cx);
                eprintln!("queue: {} {}", player.queue.items.len(), player.queue.index);
            }
            _ => {}
        }
    }

    /// Debug only: logs the structure of the open page, without any titles.
    #[cfg(debug_assertions)]
    fn report_page(&self, cx: &mut Context<Self>) {
        use crate::{model::PageState, ytm::Body};
        let app = ctx(cx);
        let router = app.router.read(cx);
        let summary = match &router.state {
            PageState::Loading => "loading".to_string(),
            PageState::Failed(error) => format!("failed: {error}"),
            PageState::Ready(page) => {
                let sections: Vec<String> = page
                    .sections
                    .iter()
                    .map(|section| match &section.body {
                        Body::Shelf(items) => format!("shelf:{}", items.len()),
                        Body::Picks(items) => format!("picks:{}", items.len()),
                        Body::List(items) => format!("list:{}", items.len()),
                        Body::Grid(items) => format!("grid:{}", items.len()),
                        Body::Tiles(items) => format!("tiles:{}", items.len()),
                        Body::Text(_) => "text".to_string(),
                    })
                    .collect();
                let header = page.header.as_ref().map(|h| {
                    format!(
                        "art={} play={} shuffle={} radio={} library={} subscribe={} menu={}",
                        !h.thumbnail.is_empty(),
                        h.play.is_some(),
                        h.shuffle.is_some(),
                        h.radio.is_some(),
                        h.library.is_some(),
                        h.subscribe.is_some(),
                        h.menu.len()
                    )
                });
                format!(
                    "header={header:?} chips={} more={} sections={sections:?}",
                    page.chips.len(),
                    page.continuation.is_some()
                )
            }
        };
        eprintln!("page: {summary}");
    }

    fn follow_page(&mut self, url: &str, cx: &mut Context<Self>) {
        let app = ctx(cx);
        if is_consent_page(url) {
            let opened = !app.ui.read(cx).auth_open;
            self.auto_opened |= opened;
            app.ui.update(cx, |ui, cx| {
                ui.auth_open = true;
                cx.notify();
            });
        } else if self.auto_opened && is_music_page(url) {
            self.auto_opened = false;
            app.ui.update(cx, |ui, cx| {
                ui.auth_open = false;
                cx.notify();
            });
        }
        app.client.engine().set_visible(app.ui.read(cx).auth_open);
    }

    fn on_auth(&mut self, signed_in: bool, cx: &mut Context<Self>) {
        let app = ctx(cx);
        if signed_in {
            app.ui.update(cx, |ui, cx| {
                ui.auth_open = false;
                ui.refresh_library(cx);
            });
            app.client.engine().set_visible(false);
            app.router.update(cx, |router, cx| router.reload(cx));
        }
    }

    fn cancel_sign_in(&mut self, cx: &mut Context<Self>) {
        let app = ctx(cx);
        self.auto_opened = false;
        app.ui.update(cx, |ui, cx| {
            ui.auth_open = false;
            cx.notify();
        });
        app.client.engine().set_visible(false);
        app.client.engine().navigate(HOME);
    }

    // ---- Search --------------------------------------------------------------------

    fn on_search(&mut self, field: Entity<TextField>, event: &FieldEvent, cx: &mut Context<Self>) {
        match event {
            FieldEvent::Changed => {
                self.suggest_index = None;
                let query = field.read(cx).text.trim().to_string();
                if query.is_empty() {
                    self.suggestions.clear();
                    self.suggest_open = false;
                    self.suggest_task = None;
                    cx.notify();
                    return;
                }
                let call = ctx(cx).client.clone();
                self.suggest_task = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(Duration::from_millis(170)).await;
                    let reply = call.api("music/get_search_suggestions", json!({ "input": query })).await;
                    let _ = this.update(cx, |shell, cx| {
                        if let Ok(json) = reply {
                            shell.suggestions = ytm::parse_suggestions(&json);
                            shell.suggest_open = !shell.suggestions.is_empty();
                            cx.notify();
                        }
                    });
                }));
            }
            FieldEvent::Submit => {
                let picked = self.suggest_index.and_then(|index| self.suggestions.get(index).cloned());
                match picked {
                    Some(suggestion) => self.pick_suggestion(&suggestion, cx),
                    None => {
                        let query = field.read(cx).text.trim().to_string();
                        if !query.is_empty() {
                            self.close_suggestions(cx);
                            model::open(Route::Search { query, params: None }, cx);
                        }
                    }
                }
            }
            FieldEvent::Cancel => self.close_suggestions(cx),
            FieldEvent::Down | FieldEvent::Up if self.suggest_open && !self.suggestions.is_empty() => {
                let count = self.suggestions.len();
                self.suggest_index = Some(match (self.suggest_index, event) {
                    (None, FieldEvent::Down) => 0,
                    (None, _) => count - 1,
                    (Some(index), FieldEvent::Down) => (index + 1) % count,
                    (Some(index), _) => (index + count - 1) % count,
                });
                cx.notify();
            }
            _ => {}
        }
    }

    fn close_suggestions(&mut self, cx: &mut Context<Self>) {
        self.suggest_open = false;
        self.suggest_index = None;
        self.suggest_task = None;
        cx.notify();
    }

    fn pick_suggestion(&mut self, suggestion: &Suggestion, cx: &mut Context<Self>) {
        self.close_suggestions(cx);
        match suggestion {
            Suggestion::Query(query) => {
                self.search.update(cx, |field, cx| field.set_text(query.clone(), cx));
                model::open(Route::Search { query: query.clone(), params: None }, cx);
            }
            Suggestion::Item(item) => {
                let action =
                    if item.is_track() { item.play.clone().or_else(|| item.open.clone()) } else { item.open.clone() };
                if let Some(action) = action {
                    model::run_action(&action, cx);
                }
            }
        }
    }

    fn on_playlist_name(&mut self, field: Entity<TextField>, event: &FieldEvent, cx: &mut Context<Self>) {
        if *event == FieldEvent::Submit {
            self.submit_new_playlist(&field, cx);
        }
    }

    fn submit_new_playlist(&mut self, field: &Entity<TextField>, cx: &mut Context<Self>) {
        let name = field.read(cx).text.trim().to_string();
        if name.is_empty() {
            return;
        }
        let modal = ctx(cx).ui.read(cx).modal.clone();
        if let Some(Modal::NewPlaylist { video_id, playlist_id }) = modal {
            field.update(cx, |field, cx| field.set_text("", cx));
            model::create_playlist(name, video_id, playlist_id, cx);
        }
    }

    // ---- Layout --------------------------------------------------------------------

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let app = ctx(cx);
        let (settings_open, now_open, accent) = {
            let ui = app.ui.read(cx);
            (ui.settings_open, ui.now_open, ui.accent)
        };
        let screen_open = settings_open || now_open;
        let current = app.router.read(cx).current().clone();
        let current_id = match &current {
            Route::Browse(target) if !screen_open => target.id.clone(),
            _ => String::new(),
        };
        let playlists = app.ui.read(cx).playlists.clone();
        let rows: Vec<AnyElement> = playlists
            .iter()
            .enumerate()
            .filter(|(_, item)| !matches!(&item.open, Some(Action::Open(Route::Browse(target))) if target.id == "VLLM"))
            .map(|(index, item)| {
                let action = item.open.clone();
                let context = item.clone();
                let active = !screen_open
                    && matches!((&current, &item.open), (route, Some(Action::Open(open))) if route == open);
                div()
                    .id(("side-playlist", index))
                    .h(px(46.))
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_3()
                    .px_2()
                    .rounded(px(12.))
                    .when(active, |row| row.bg(rgba(RAISED)))
                    .cursor_pointer()
                    .hover(|style| style.bg(rgba(HOVER)))
                    .on_click(move |_, _, cx| {
                        if let Some(action) = &action {
                            model::run_action(action, cx);
                        }
                    })
                    .on_mouse_down(MouseButton::Right, move |_, window, cx| {
                        cx.stop_propagation();
                        super::page::track_menu(&context, window, cx);
                    })
                    .child(artwork(&item.thumbnail, 34., 9., cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_weight(if active { gpui::FontWeight::SEMIBOLD } else { gpui::FontWeight::NORMAL })
                            .text_color(rgba(if active { TEXT } else { MUTED }))
                            .child(item.title.clone()),
                    )
                    .into_any_element()
            })
            .collect();

        let liked_active = current_id == "VLLM";
        let liked = div()
            .id("side-liked")
            .h(px(46.))
            .flex()
            .flex_none()
            .items_center()
            .gap_3()
            .px_2()
            .rounded(px(12.))
            .when(liked_active, |row| row.bg(rgba(RAISED)))
            .cursor_pointer()
            .hover(|style| style.bg(rgba(HOVER)))
            .on_click(|_, _, cx| model::open(browse("VLLM"), cx))
            .child(
                div()
                    .size(px(34.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(9.))
                    .bg(linear_gradient(135., linear_color_stop(rgba(accent), 0.), linear_color_stop(rgba(VIOLET), 1.)))
                    .child(icon("like", 16., ON_ACCENT)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .font_weight(if liked_active { gpui::FontWeight::SEMIBOLD } else { gpui::FontWeight::NORMAL })
                    .text_color(rgba(if liked_active { TEXT } else { MUTED }))
                    .child("Liked Music"),
            );

        let main_rows: Vec<AnyElement> = MAIN_NAV
            .iter()
            .map(|&(label, name, id)| {
                let active = if id == super::page::LIBRARY_HOME {
                    super::page::is_library(&current_id)
                } else {
                    current_id == id
                };
                nav_row(label, name, active, accent)
                    .on_click(move |_, _, cx| model::open(browse(id), cx))
                    .into_any_element()
            })
            .collect();

        div()
            .id("sidebar")
            .w(px(SIDEBAR))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .pl_3()
            .pr_2()
            .child(
                div()
                    .id("side-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .pt_1()
                    .pb_3()
                    .child(
                        nav_row("Now", "sparkles", now_open, accent).on_click(|_, _, cx| model::set_now_open(true, cx)),
                    )
                    .children(main_rows)
                    .child(
                        div()
                            .mt_5()
                            .mb_1()
                            .pl_3()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgba(FAINT))
                                    .child("PLAYLISTS"),
                            )
                            .child(round_button("new-playlist", "plus", 28., 15., MUTED).bg(rgba(RAISED)).on_click(
                                |_, _, cx| {
                                    ctx(cx).ui.update(cx, |ui, cx| {
                                        ui.modal = Some(Modal::NewPlaylist { video_id: None, playlist_id: None });
                                        cx.notify();
                                    });
                                },
                            )),
                    )
                    .child(liked)
                    .children(rows),
            )
            .child(div().flex_none().pb_2().child(nav_row("Settings", "settings", settings_open, accent).on_click(
                |_, _, cx| {
                    model::close_overlays(cx);
                    model::set_settings_open(true, cx);
                },
            )))
    }

    fn top_bar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let app = ctx(cx);
        let (screen_open, accent) = {
            let ui = app.ui.read(cx);
            (ui.settings_open || ui.now_open, ui.accent)
        };
        let (can_back, can_forward) = {
            let router = app.router.read(cx);
            (router.can_go_back() || screen_open, router.can_go_forward())
        };
        let focused = self.search.read(cx).focus_handle(cx).is_focused(window);
        let photo = app.ui.read(cx).account.as_ref().map(|value| value.photo.clone()).unwrap_or_default();
        let avatar = artwork(&photo, 36., 18., cx);
        let store = self.search_bounds.clone();
        div()
            .h(px(TOP_BAR))
            .flex_none()
            .flex()
            .items_center()
            .gap_3()
            .pl(px(88.))
            .pr_5()
            .on_mouse_down(MouseButton::Left, |_, _, cx| ctx(cx).client.engine().drag_window())
            .child(
                div()
                    .flex()
                    .gap_2()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(glass_button("back", "back", can_back).on_click(|_, _, cx| model::go_back(cx)))
                    .child(glass_button("forward", "forward", can_forward).on_click(|_, _, cx| model::go_forward(cx))),
            )
            .child(div().flex_1())
            .child(
                div()
                    .id("search-box")
                    .relative()
                    .w(px(SEARCH_WIDTH))
                    .h(px(46.))
                    .flex()
                    .items_center()
                    .gap_3()
                    .pl_4()
                    .pr_2()
                    .rounded(px(16.))
                    .bg(rgba(SURFACE))
                    .border_1()
                    .border_color(rgba(if focused { accent } else { LINE }))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(|this, _, window, cx| {
                        let handle = this.search.read(cx).focus_handle(cx);
                        window.focus(&handle);
                    }))
                    .child(canvas(move |bounds, _, _| store.set(bounds), |_, _, _, _| {}).absolute().size_full())
                    .child(icon("search", 18., MUTED))
                    .child(div().flex_1().min_w_0().child(self.search.clone()))
                    .child(
                        div()
                            .flex_none()
                            .px_2()
                            .py(px(3.))
                            .rounded(px(7.))
                            .bg(rgba(RAISED))
                            .text_xs()
                            .text_color(rgba(MUTED))
                            .child("⌘K"),
                    ),
            )
            .child(div().flex_1())
            .child(
                div()
                    .id("account")
                    .size(px(42.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .border_1()
                    .border_color(rgba(LINE))
                    .bg(rgba(SURFACE))
                    .cursor_pointer()
                    .hover(|style| style.bg(rgba(RAISED)))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(|_, window, cx| {
                        let position = window.mouse_position();
                        ctx(cx).ui.update(cx, |ui, cx| {
                            ui.menu = None;
                            ui.account_menu = Some(position);
                            cx.notify();
                        });
                    })
                    .child(avatar),
            )
    }

    fn suggestions_view(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.suggest_open || self.suggestions.is_empty() {
            return None;
        }
        let rows: Vec<AnyElement> = self
            .suggestions
            .iter()
            .enumerate()
            .map(|(index, suggestion)| {
                let picked = suggestion.clone();
                let selected = self.suggest_index == Some(index);
                let (label, detail, thumbnail) = match suggestion {
                    Suggestion::Query(query) => (query.clone(), String::new(), None),
                    Suggestion::Item(item) => (item.title.clone(), item.subtitle_text(), Some(item.thumbnail.clone())),
                };
                let leading = match &thumbnail {
                    Some(url) => artwork(url, 40., 8., cx),
                    None => div()
                        .size(px(40.))
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .rounded(px(10.))
                        .bg(rgba(RAISED))
                        .child(icon("search", 16., MUTED))
                        .into_any_element(),
                };
                div()
                    .id(("suggestion", index))
                    .min_h(px(52.))
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_2()
                    .rounded(px(12.))
                    .when(selected, |row| row.bg(rgba(RAISED)))
                    .cursor_pointer()
                    .hover(|style| style.bg(rgba(HOVER)))
                    .on_click(cx.listener(move |this, _, _, cx| this.pick_suggestion(&picked, cx)))
                    .child(leading)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().truncate().text_sm().child(label))
                            .when(!detail.is_empty(), |column| {
                                column.child(div().truncate().text_xs().text_color(rgba(MUTED)).child(detail))
                            }),
                    )
                    .into_any_element()
            })
            .collect();
        let bounds = self.search_bounds.get();
        let origin = if bounds.size.width > px(0.) {
            point(bounds.origin.x, bounds.origin.y + bounds.size.height + px(6.))
        } else {
            point(px(SIDEBAR + 40.), px(TOP_BAR))
        };
        let popup = div()
            .id("suggestions")
            .occlude()
            .w(px(SEARCH_WIDTH))
            .max_h(px(460.))
            .overflow_y_scroll()
            .p_1p5()
            .rounded(px(18.))
            .bg(rgba(SOLID))
            .border_1()
            .border_color(rgba(LINE))
            .shadow_lg()
            .text_color(rgba(TEXT))
            .children(rows);
        Some(
            deferred(
                anchored().position(origin).anchor(Corner::TopLeft).snap_to_window_with_margin(px(8.)).child(popup),
            )
            .with_priority(3)
            .into_any_element(),
        )
    }

    fn menu_view(&self, menu: &OpenMenu, cx: &mut Context<Self>) -> AnyElement {
        let mut rows: Vec<AnyElement> = menu
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let value = entry.clone();
                menu_row(("menu", index), entry_icon(entry), entry.label.clone(), entry.destructive)
                    .on_click(move |_, _, cx| model::run_entry(&value, cx))
                    .into_any_element()
            })
            .collect();
        if let Some(link) = menu.link.clone() {
            rows.push(
                menu_row("menu-link", "forward", "Copy link".to_string(), false)
                    .on_click(move |_, _, cx| model::copy_link(&link, cx))
                    .into_any_element(),
            );
        }
        let _ = cx;
        popover(menu.position, Some(menu.title.clone()), rows)
    }

    fn account_view(&self, position: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) -> AnyElement {
        let account = ctx(cx).ui.read(cx).account.clone();
        let name = account.as_ref().map(|value| value.name.clone()).filter(|value| !value.is_empty());
        let rows = vec![
            menu_row("account-history", "history", "History".to_string(), false)
                .on_click(|_, _, cx| model::open(browse("FEmusic_history"), cx))
                .into_any_element(),
            menu_row("account-settings", "settings", "Settings".to_string(), false)
                .on_click(|_, _, cx| {
                    model::close_overlays(cx);
                    model::set_settings_open(true, cx);
                })
                .into_any_element(),
            menu_row("account-switch", "artist", "Switch account".to_string(), false)
                .on_click(|_, _, cx| model::begin_sign_in(cx))
                .into_any_element(),
            menu_row("account-signout", "close", "Sign out".to_string(), true)
                .on_click(|_, _, cx| {
                    ctx(cx).ui.update(cx, |ui, cx| {
                        ui.account_menu = None;
                        ui.modal = Some(Modal::SignOut);
                        cx.notify();
                    });
                })
                .into_any_element(),
        ];
        popover(position, name, rows)
    }

    fn modal_view(&self, modal: &Modal, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let accent = ctx(cx).ui.read(cx).accent;
        let body: AnyElement = match modal {
            Modal::Confirm { label, action } => {
                let action = action.clone();
                dialog(
                    label.clone(),
                    "This changes data in your YouTube Music account.",
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(pill("confirm-no", None, "Cancel", false).on_click(|_, _, cx| model::close_overlays(cx)))
                        .child(
                            pill("confirm-yes", None, "Confirm", true)
                                .on_click(move |_, _, cx| model::run_action(&action, cx)),
                        )
                        .into_any_element(),
                )
            }
            Modal::SignOut => dialog(
                "Sign out?".to_string(),
                "The music will stop, and you will need to sign in again to keep listening.",
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(pill("signout-no", None, "Cancel", false).on_click(|_, _, cx| model::close_overlays(cx)))
                    .child(pill("signout-yes", None, "Sign out", true).on_click(|_, _, cx| model::sign_out(cx)))
                    .into_any_element(),
            ),
            Modal::NewPlaylist { .. } => {
                let handle = self.playlist_name.read(cx).focus_handle(cx);
                if !handle.is_focused(window) {
                    window.focus(&handle);
                }
                let field = self.playlist_name.clone();
                dialog(
                    "New playlist".to_string(),
                    "The playlist will be private. You can change that later.",
                    div()
                        .flex()
                        .flex_col()
                        .gap_4()
                        .child(
                            div()
                                .h(px(46.))
                                .flex()
                                .items_center()
                                .px_3()
                                .rounded(px(12.))
                                .bg(rgba(RAISED))
                                .border_1()
                                .border_color(rgba(accent))
                                .child(self.playlist_name.clone()),
                        )
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(
                                    pill("new-no", None, "Cancel", false)
                                        .on_click(|_, _, cx| model::close_overlays(cx)),
                                )
                                .child(accent_pill("new-yes", None, "Create", accent).on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.submit_new_playlist(&field, cx);
                                    },
                                ))),
                        )
                        .into_any_element(),
                )
            }
            Modal::AddToPlaylist { video_id, playlist_id, options, error } => {
                let rows: AnyElement = match (options, error) {
                    (_, Some(message)) => {
                        div().text_sm().text_color(rgba(DANGER)).child(message.clone()).into_any_element()
                    }
                    (None, _) => {
                        div().py_4().text_sm().text_color(rgba(MUTED)).child("Loading playlists…").into_any_element()
                    }
                    (Some(list), _) => div()
                        .id("playlist-options")
                        .max_h(px(320.))
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .children(list.iter().enumerate().map(|(index, option)| {
                            let (target, video, source) = (option.id.clone(), video_id.clone(), playlist_id.clone());
                            div()
                                .id(("option", index))
                                .min_h(px(48.))
                                .flex()
                                .flex_col()
                                .justify_center()
                                .px_3()
                                .rounded(px(12.))
                                .cursor_pointer()
                                .hover(|style| style.bg(rgba(HOVER)))
                                .on_click(move |_, _, cx| {
                                    model::add_to_playlist(target.clone(), video.clone(), source.clone(), cx)
                                })
                                .child(
                                    div()
                                        .truncate()
                                        .text_sm()
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .child(option.title.clone()),
                                )
                                .when(!option.subtitle.is_empty(), |row| {
                                    row.child(
                                        div()
                                            .truncate()
                                            .text_xs()
                                            .text_color(rgba(MUTED))
                                            .child(option.subtitle.clone()),
                                    )
                                })
                        }))
                        .into_any_element(),
                };
                let (video, source) = (video_id.clone(), playlist_id.clone());
                dialog(
                    "Add to playlist".to_string(),
                    "",
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(rows)
                        .child(
                            div()
                                .flex()
                                .justify_between()
                                .child(pill("add-new", Some("plus"), "New playlist", false).on_click(
                                    move |_, _, cx| {
                                        let (video_id, playlist_id) = (video.clone(), source.clone());
                                        ctx(cx).ui.update(cx, |ui, cx| {
                                            ui.modal = Some(Modal::NewPlaylist { video_id, playlist_id });
                                            cx.notify();
                                        });
                                    },
                                ))
                                .child(
                                    pill("add-close", None, "Close", false)
                                        .on_click(|_, _, cx| model::close_overlays(cx)),
                                ),
                        )
                        .into_any_element(),
                )
            }
        };
        deferred(
            div()
                .id("modal-backdrop")
                .occlude()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x06040cb3))
                .text_color(rgba(TEXT))
                .on_click(|_, _, cx| model::close_overlays(cx))
                .child(body),
        )
        .with_priority(5)
        .into_any_element()
    }

    fn gate(&self, title: &str, detail: &str, button: bool, accent: u32) -> AnyElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(MouseButton::Left, |_, _, cx| ctx(cx).client.engine().drag_window())
            .child(
                div()
                    .w(px(420.))
                    .px_8()
                    .py_8()
                    .rounded(px(28.))
                    .bg(rgba(SOLID))
                    .border_1()
                    .border_color(rgba(LINE))
                    .shadow_lg()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .size(px(68.))
                            .rounded(px(22.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(linear_gradient(
                                135.,
                                linear_color_stop(rgba(accent), 0.),
                                linear_color_stop(rgba(VIOLET), 1.),
                            ))
                            .shadow(glow(accent, 0.5, 34.))
                            .child(icon("sparkles", 34., ON_ACCENT)),
                    )
                    .child(
                        div()
                            .mt_2()
                            .text_2xl()
                            .text_center()
                            .font_weight(gpui::FontWeight::BOLD)
                            .child(title.to_string()),
                    )
                    .child(div().text_sm().text_center().text_color(rgba(MUTED)).child(detail.to_string()))
                    .when(button, |column| {
                        column.child(
                            div().mt_3().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(
                                accent_pill("sign-in", None, "Sign in with Google", accent)
                                    .on_click(|_, _, cx| model::begin_sign_in(cx)),
                            ),
                        )
                    }),
            )
            .into_any_element()
    }

    fn auth_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .h(px(AUTH_BAR))
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .pl(px(92.))
            .pr_3()
            .bg(rgba(SOLID))
            .border_b_1()
            .border_color(rgba(LINE))
            .on_mouse_down(MouseButton::Left, |_, _, cx| ctx(cx).client.engine().drag_window())
            .child(div().text_sm().font_weight(gpui::FontWeight::SEMIBOLD).child("Sign in to your Google account"))
            .child(div().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(
                pill("auth-cancel", None, "Cancel", false).h(px(30.)).on_click(cx.listener(|this, _, _, cx| {
                    this.cancel_sign_in(cx);
                })),
            ))
    }

    /// "Now": the song's galaxy fills the window, its queue, lyrics and related music sit beside it.
    fn now_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let app = ctx(cx);
        let (has_track, is_ad, clip_available) = {
            let state = &app.player.read(cx).state;
            (state.has_track, state.is_ad, state.clip_available)
        };
        let (synced, chorus, window_galaxy, clip_on, column) = {
            let prefs = &app.ui.read(cx).prefs;
            (prefs.synced_lyrics, prefs.chorus_burst, prefs.window_galaxy, prefs.clip_particles, prefs.now_column)
        };
        let galaxy_note = if !window_galaxy {
            Some("The window galaxy is turned off in Settings")
        } else if !has_track {
            Some("Play a song and its galaxy will appear here")
        } else if clip_on && is_ad {
            Some("Ad. The clip comes back after it")
        } else if clip_on && !clip_available {
            Some("This song has no clip, so its galaxy plays")
        } else {
            None
        };
        // Galaxy or clip: with "Clip" the song's music video plays and its frames become stars.
        let view_switch = div()
            .flex_1()
            .flex()
            .gap_1()
            .p(px(4.))
            .rounded(px(14.))
            .bg(rgba(SURFACE))
            .border_1()
            .border_color(rgba(LINE))
            .child(segment("view-galaxy", "sparkles", "Galaxy", !clip_on).on_click(|_, _, cx| {
                set_pref(cx, |prefs| prefs.clip_particles = false);
            }))
            .child(segment("view-clip", "clip", "Clip", clip_on).on_click(|_, _, cx| {
                set_pref(cx, |prefs| prefs.clip_particles = true);
            }));
        let effects = div()
            .flex()
            .flex_wrap()
            .gap_2()
            .child(effect_chip("effect-words", "Words", synced).on_click(|_, _, cx| {
                set_pref(cx, |prefs| prefs.synced_lyrics = !prefs.synced_lyrics);
            }))
            .child(effect_chip("effect-chorus", "Chorus", chorus).on_click(|_, _, cx| {
                set_pref(cx, |prefs| prefs.chorus_burst = !prefs.chorus_burst);
            }))
            .child(
                effect_chip("effect-immersive", "Full screen", false)
                    .on_click(|_, _, cx| model::set_immersive(true, cx)),
            );
        let galaxy_area = div().flex_1().min_w_0().h_full().flex().items_center().justify_center().when_some(
            galaxy_note,
            |area, note| {
                area.child(
                    div().px_5().py_3().rounded_full().bg(rgba(SURFACE)).text_sm().text_color(rgba(MUTED)).child(note),
                )
            },
        );
        // The column can step aside and leave the whole window to the galaxy; a small button brings it back.
        if !column {
            return div()
                .size_full()
                .relative()
                .flex()
                .pr_4()
                .pb_2()
                .child(galaxy_area)
                .child(
                    div().absolute().top_0().right_4().child(
                        glass_button("show-column", "panel_show", true)
                            .size(px(44.))
                            .on_click(|_, _, cx| set_pref(cx, |prefs| prefs.now_column = true)),
                    ),
                )
                .into_any_element();
        }
        div()
            .size_full()
            .flex()
            .gap_5()
            .pr_4()
            .pb_2()
            .child(galaxy_area)
            .child(
                div()
                    .w(px(PANEL))
                    .h_full()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div().flex().items_center().gap_2().child(view_switch).child(
                            glass_button("hide-column", "panel_hide", true)
                                .size(px(44.))
                                .on_click(|_, _, cx| set_pref(cx, |prefs| prefs.now_column = false)),
                        ),
                    )
                    .child(div().flex_1().min_h_0().child(self.panel.clone()))
                    .child(effects),
            )
            .into_any_element()
    }

    /// The lyrics take the window: the galaxy, the line being sung, and the player.
    fn immersive_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let app = ctx(cx);
        let state = app.player.read(cx).state.clone();
        let accent = app.ui.read(cx).accent;
        let lines = self.panel.read(cx).lines_around();
        let chip = state.has_track.then(|| {
            div()
                .h(px(40.))
                .flex()
                .items_center()
                .gap_3()
                .pl(px(5.))
                .pr_4()
                .rounded_full()
                .bg(rgba(SURFACE))
                .child(artwork(&state.artwork, 30., 15., cx))
                .child(div().text_sm().font_weight(gpui::FontWeight::SEMIBOLD).child(state.title.clone()))
                .child(div().text_sm().text_color(rgba(MUTED)).child(state.artist.clone()))
        });
        let words = lines.map(|(before, line, after)| {
            div()
                .max_w(px(720.))
                .pl(px(44.))
                .pb_2()
                .flex()
                .flex_col()
                .gap_2()
                .when_some(before, |column, text| column.child(div().text_lg().text_color(rgba(MUTED)).child(text)))
                .child(
                    div()
                        .text_size(px(38.))
                        .line_height(px(46.))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(rgba(TEXT))
                        .child(line),
                )
                .when_some(after, |column, text| column.child(div().text_lg().text_color(rgba(MUTED)).child(text)))
        });
        div()
            .size_full()
            .flex()
            .flex_col()
            .justify_between()
            .child(
                div()
                    .h(px(TOP_BAR))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_3()
                    .pl(px(88.))
                    .pr_5()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| ctx(cx).client.engine().drag_window())
                    .child(
                        div()
                            .id("leave-immersive")
                            .h(px(40.))
                            .flex()
                            .items_center()
                            .gap_2()
                            .pl_3()
                            .pr_2()
                            .rounded_full()
                            .bg(rgba(SURFACE))
                            .border_1()
                            .border_color(rgba(LINE))
                            .text_sm()
                            .cursor_pointer()
                            .hover(|style| style.bg(rgba(RAISED)))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(|_, _, cx| model::set_immersive(false, cx))
                            .child(icon("down", 16., TEXT))
                            .child("Exit full screen")
                            .child(
                                div()
                                    .px_2()
                                    .py(px(2.))
                                    .rounded(px(6.))
                                    .bg(rgba(RAISED))
                                    .text_xs()
                                    .text_color(rgba(MUTED))
                                    .child("esc"),
                            ),
                    )
                    .child(div().flex_1())
                    .children(chip)
                    .child(div().flex_1())
                    .child(div().w(px(120.))),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .children(words)
                    .when(!app.ui.read(cx).prefs.synced_lyrics, |column| {
                        column.child(
                            div()
                                .pl(px(44.))
                                .pb_3()
                                .text_sm()
                                .text_color(rgba(MUTED))
                                .child("Words appear once \u{201c}Synced lyrics\u{201d} is on in Settings"),
                        )
                    })
                    .child(
                        div().h(px(TRANSPORT)).flex_none().child(
                            AnyView::from(self.transport.clone()).cached(StyleRefinement::default().size_full()),
                        ),
                    ),
            )
            .child(div().hidden().child(icon("note", 1., accent)))
            .into_any_element()
    }
}

fn nav_row(label: &'static str, name: &'static str, active: bool, accent: u32) -> gpui::Stateful<gpui::Div> {
    div()
        .id(label)
        .h(px(44.))
        .flex()
        .flex_none()
        .items_center()
        .gap_3()
        .px_3()
        .rounded(px(12.))
        .text_size(px(15.))
        .font_weight(if active { gpui::FontWeight::SEMIBOLD } else { gpui::FontWeight::MEDIUM })
        .text_color(rgba(if active { TEXT } else { MUTED }))
        .when(active, |row| row.bg(rgba(RAISED)))
        .cursor_pointer()
        .hover(|style| style.bg(rgba(HOVER)).text_color(rgba(TEXT)))
        .child(icon(name, 19., if active { accent } else { MUTED }))
        .child(label)
}

fn segment(id: &'static str, name: &'static str, label: &'static str, on: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_1()
        .h(px(36.))
        .flex()
        .items_center()
        .justify_center()
        .gap_2()
        .rounded(px(10.))
        .text_sm()
        .font_weight(if on { gpui::FontWeight::SEMIBOLD } else { gpui::FontWeight::NORMAL })
        .text_color(rgba(if on { TEXT } else { MUTED }))
        .when(on, |segment| segment.bg(rgba(0xffffff29)))
        .cursor_pointer()
        .hover(|style| style.text_color(rgba(TEXT)))
        .child(icon(name, 15., if on { TEXT } else { MUTED }))
        .child(label)
}

fn glass_button(id: &'static str, name: &'static str, enabled: bool) -> gpui::Stateful<gpui::Div> {
    round_button(id, name, 36., 18., if enabled { TEXT } else { FAINT })
        .bg(rgba(SURFACE))
        .border_1()
        .border_color(rgba(LINE))
}

fn effect_chip(id: &'static str, label: &'static str, on: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h(px(34.))
        .px_3()
        .flex()
        .items_center()
        .rounded_full()
        .border_1()
        .border_color(rgba(if on { 0xffffff47 } else { LINE }))
        .bg(rgba(if on { 0xffffff26 } else { SURFACE }))
        .text_sm()
        .text_color(rgba(if on { TEXT } else { MUTED }))
        .cursor_pointer()
        .hover(|style| style.bg(rgba(RAISED)))
        .child(label)
}

fn set_pref(cx: &mut App, change: impl FnOnce(&mut crate::prefs::Prefs)) {
    ctx(cx).ui.update(cx, |ui, cx| {
        change(&mut ui.prefs);
        ui.prefs.save();
        cx.notify();
    });
}

fn menu_row(
    id: impl Into<gpui::ElementId>,
    name: &'static str,
    label: String,
    danger: bool,
) -> gpui::Stateful<gpui::Div> {
    let color = if danger { DANGER } else { TEXT };
    div()
        .id(id)
        .min_h(px(38.))
        .flex()
        .items_center()
        .gap_3()
        .px_3()
        .rounded(px(10.))
        .text_sm()
        .text_color(rgba(color))
        .cursor_pointer()
        .hover(|style| style.bg(rgba(HOVER)))
        .child(icon(name, 16., if danger { DANGER } else { MUTED }))
        .child(div().flex_1().min_w_0().truncate().child(label))
}

fn popover(position: gpui::Point<gpui::Pixels>, title: Option<String>, rows: Vec<AnyElement>) -> AnyElement {
    div()
        .child(
            deferred(
                div()
                    .id("popover-backdrop")
                    .occlude()
                    .absolute()
                    .inset_0()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| model::close_overlays(cx))
                    .on_mouse_down(MouseButton::Right, |_, _, cx| model::close_overlays(cx)),
            )
            .with_priority(3),
        )
        .child(
            deferred(
                anchored().position(position).snap_to_window_with_margin(px(8.)).child(
                    div()
                        .id("popover")
                        .occlude()
                        .w(px(280.))
                        .max_h(px(480.))
                        .overflow_y_scroll()
                        .p_1p5()
                        .rounded(px(16.))
                        .bg(rgba(SOLID))
                        .border_1()
                        .border_color(rgba(LINE))
                        .shadow_lg()
                        .text_color(rgba(TEXT))
                        .when_some(title, |menu, title| {
                            menu.child(
                                div()
                                    .px_3()
                                    .py_2()
                                    .truncate()
                                    .text_xs()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgba(MUTED))
                                    .child(title),
                            )
                        })
                        .children(rows),
                ),
            )
            .with_priority(4),
        )
        .into_any_element()
}

fn dialog(title: String, detail: &str, content: AnyElement) -> AnyElement {
    div()
        .id("dialog")
        .occlude()
        .w(px(420.))
        .flex()
        .flex_col()
        .gap_3()
        .p_5()
        .rounded(px(22.))
        .bg(rgba(SOLID))
        .border_1()
        .border_color(rgba(LINE))
        .shadow_lg()
        .on_click(|_, _, cx| cx.stop_propagation())
        .child(div().text_lg().font_weight(gpui::FontWeight::BOLD).child(title))
        .when(!detail.is_empty(), |column| {
            column.child(div().text_sm().text_color(rgba(MUTED)).child(SharedString::from(detail.to_string())))
        })
        .child(content)
        .into_any_element()
}

/// Darkens the galaxy where text needs it: everywhere under pages, only at the edges on "Now".
fn scrim(full: bool) -> AnyElement {
    let fade = |angle: f32, strength: u32| {
        linear_gradient(
            angle,
            linear_color_stop(rgba(0x06040c00 | strength), 0.),
            linear_color_stop(rgba(0x06040c00), 1.),
        )
    };
    if full {
        return div().absolute().inset_0().bg(rgba(SCRIM)).into_any_element();
    }
    div()
        .absolute()
        .inset_0()
        .child(div().absolute().top_0().bottom_0().left_0().w(px(SIDEBAR + 12.)).bg(rgba(0x06040cc8)))
        .child(div().absolute().top_0().bottom_0().left(px(SIDEBAR + 12.)).w(px(180.)).bg(fade(90., 0xc8)))
        .child(div().absolute().top_0().bottom_0().right_0().w(px(PANEL + 160.)).bg(fade(270., 0xb0)))
        .child(div().absolute().left_0().right_0().bottom_0().h(px(TRANSPORT + 120.)).bg(fade(0., 0xb8)))
        .child(div().absolute().left_0().right_0().top_0().h(px(TOP_BAR + 40.)).bg(fade(180., 0x80)))
        .into_any_element()
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let app = ctx(cx);
        let viewport = window.viewport_size();
        let (width, height) = (f32::from(viewport.width), f32::from(viewport.height));
        let (connected, signed_in) = {
            let player = app.player.read(cx);
            (player.connected, player.state.signed_in)
        };
        let ui = app.ui.read(cx);
        let (auth_open, panel_open, settings_open, now_open, immersive, accent) =
            (ui.auth_open, ui.panel.is_some(), ui.settings_open, ui.now_open, ui.immersive, ui.accent);
        let (menu, account_menu, modal, toast) = (ui.menu.clone(), ui.account_menu, ui.modal.clone(), ui.toast.clone());

        {
            let mut engine = app.client.engine();
            engine.place(0., AUTH_BAR, width, (height - AUTH_BAR).max(1.));
            engine.set_visible(auth_open);
            engine.keep_keys();
        }

        let ready = connected && signed_in && !auth_open;
        let content: AnyElement = if auth_open {
            div().size_full().flex().flex_col().child(self.auth_bar(cx)).into_any_element()
        } else if !connected {
            self.gate("Connecting to YouTube Music", "This takes a couple of seconds", false, accent)
        } else if !signed_in {
            self.gate("Sign in", "You only need to sign in once, then Drift remembers you", true, accent)
        } else if immersive {
            self.immersive_view(cx)
        } else {
            let sidebar = self.sidebar(cx).into_any_element();
            let top_bar = self.top_bar(window, cx).into_any_element();
            let main_area: AnyElement = if now_open {
                self.now_view(cx)
            } else if settings_open {
                AnyView::from(self.settings.clone()).into_any_element()
            } else {
                AnyView::from(self.page.clone()).cached(StyleRefinement::default().size_full()).into_any_element()
            };
            div()
                .size_full()
                .flex()
                .flex_col()
                .child(top_bar)
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .child(sidebar)
                        .child(div().flex_1().min_w_0().h_full().child(main_area))
                        .when(panel_open && !now_open, |row| {
                            row.child(div().h_full().flex_none().pl_2().pr_4().pb_2().child(self.panel.clone()))
                        }),
                )
                .child(
                    div()
                        .h(px(TRANSPORT))
                        .flex_none()
                        .child(AnyView::from(self.transport.clone()).cached(StyleRefinement::default().size_full())),
                )
                .into_any_element()
        };

        let suggestions = if !auth_open { self.suggestions_view(cx) } else { None };
        let menu_element = menu.as_ref().map(|value| self.menu_view(value, cx));
        let account_element = account_menu.map(|position| self.account_view(position, cx));
        let modal_element = modal.as_ref().map(|value| self.modal_view(value, window, cx));

        div()
            .key_context("Player")
            .track_focus(&self.focus)
            .on_action(cx.listener(|_, _: &TogglePlay, _, cx| {
                let app = ctx(cx);
                if app.player.read(cx).state.has_track {
                    app.client.send(Command::Toggle);
                }
            }))
            .on_action(|_: &NextTrack, _, cx| ctx(cx).client.send(Command::Next))
            .on_action(|_: &PreviousTrack, _, cx| ctx(cx).client.send(Command::Previous))
            .on_action(|_: &VolumeUp, _, cx| change_volume(VOLUME_STEP, cx))
            .on_action(|_: &VolumeDown, _, cx| change_volume(-VOLUME_STEP, cx))
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                if ctx(cx).ui.read(cx).immersive {
                    model::set_immersive(false, cx);
                }
                let handle = this.search.read(cx).focus_handle(cx);
                window.focus(&handle);
            }))
            .on_action(|_: &GoBack, _, cx| model::go_back(cx))
            .on_action(|_: &GoForward, _, cx| model::go_forward(cx))
            .on_action(|_: &ToggleQueue, _, cx| {
                ctx(cx).ui.update(cx, |ui, cx| {
                    if ui.now_open {
                        // On "Now" the queue lives in the side column, so the shortcut shows and hides the column.
                        ui.prefs.now_column = !ui.prefs.now_column;
                        ui.prefs.save();
                        if ui.prefs.now_column {
                            ui.panel = Some(PanelTab::Queue);
                        }
                    } else {
                        ui.panel = if ui.panel.is_some() { None } else { Some(PanelTab::Queue) };
                    }
                    cx.notify();
                });
            })
            .on_action(cx.listener(|this, _: &Dismiss, window, cx| {
                let immersive = ctx(cx).ui.read(cx).immersive;
                this.close_suggestions(cx);
                model::close_overlays(cx);
                if immersive {
                    model::set_immersive(false, cx);
                }
                window.focus(&this.focus);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    if this.suggest_open {
                        this.close_suggestions(cx);
                    }
                    let search = this.search.read(cx).focus_handle(cx);
                    if search.is_focused(window) {
                        window.focus(&this.focus);
                    }
                }),
            )
            .relative()
            .size_full()
            .font_family(".SystemUIFont")
            .text_color(rgba(TEXT))
            .when(!auth_open, |root| root.child(scrim(!ready || !(now_open || immersive))))
            .child(content)
            .children(suggestions)
            .children(menu_element)
            .children(account_element)
            .children(modal_element)
            .when_some(toast, |root, text| {
                root.child(
                    deferred(
                        div().absolute().bottom(px(TRANSPORT + 12.)).left_0().right_0().flex().justify_center().child(
                            div()
                                .h(px(42.))
                                .flex()
                                .items_center()
                                .gap_2()
                                .pl_3()
                                .pr_4()
                                .rounded_full()
                                .bg(rgba(SOLID))
                                .border_1()
                                .border_color(rgba(LINE))
                                .shadow_lg()
                                .text_sm()
                                .text_color(rgba(TEXT))
                                .child(icon("check", 16., accent))
                                .child(text),
                        ),
                    )
                    .with_priority(6),
                )
            })
    }
}

fn change_volume(delta: f64, cx: &mut App) {
    let app = ctx(cx);
    let level = (app.player.read(cx).state.volume + delta).clamp(0., 100.);
    app.client.send(Command::Volume(level));
    app.player.update(cx, |player, cx| {
        player.state.volume = level;
        cx.notify();
    });
}
