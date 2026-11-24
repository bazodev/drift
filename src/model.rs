//! Application state: player, navigation, overlays, and the actions that change them.

use std::{rc::Rc, time::Duration};

use gpui::{App, ClipboardItem, Context, Entity, EventEmitter, Global, Pixels, Point, Task, Window};
use serde_json::{Value, json};

use crate::{
    engine::{Client, PlayerState, QueueState, SIGN_IN_URL, SIGN_OUT_URL},
    images::Images,
    prefs::Prefs,
    ytm::{self, Account, Action, Body, BrowseRef, Item, Kind, MenuEntry, Page, PlaylistOption, Route},
};

#[derive(Clone)]
pub struct Ctx {
    pub client: Client,
    pub images: Entity<Images>,
    pub player: Entity<PlayerModel>,
    pub router: Entity<Router>,
    pub ui: Entity<UiState>,
}

impl Global for Ctx {}

pub fn ctx(cx: &App) -> Ctx {
    cx.global::<Ctx>().clone()
}

// ---- Player --------------------------------------------------------------------------

#[allow(clippy::enum_variant_names)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PlayerEvent {
    TrackChanged,
    QueueChanged,
    AuthChanged,
}

#[derive(Default)]
pub struct PlayerModel {
    pub state: PlayerState,
    pub queue: QueueState,
    pub connected: bool,
}

impl EventEmitter<PlayerEvent> for PlayerModel {}

impl PlayerModel {
    pub fn apply_state(&mut self, state: PlayerState, cx: &mut Context<Self>) {
        let track_changed = state.video_id != self.state.video_id;
        let auth_changed = !self.connected || state.signed_in != self.state.signed_in;
        self.state = state;
        self.connected = true;
        if track_changed {
            cx.emit(PlayerEvent::TrackChanged);
        }
        if auth_changed {
            cx.emit(PlayerEvent::AuthChanged);
        }
        cx.notify();
    }

    pub fn apply_queue(&mut self, queue: QueueState, cx: &mut Context<Self>) {
        if queue != self.queue {
            self.queue = queue;
            cx.emit(PlayerEvent::QueueChanged);
            cx.notify();
        }
    }
}

// ---- Navigation ----------------------------------------------------------------------

pub enum PageState {
    Loading,
    Ready(Rc<Page>),
    Failed(String),
}

pub struct Router {
    history: Vec<Route>,
    cursor: usize,
    pub state: PageState,
    generation: u64,
    loading_more: bool,
    task: Option<Task<()>>,
    more_task: Option<Task<()>>,
}

pub fn browse(id: &str) -> Route {
    Route::Browse(BrowseRef { id: id.to_string(), params: None })
}

fn fetch_route(client: &Client, route: &Route) -> impl std::future::Future<Output = Result<Value, String>> + 'static {
    let (endpoint, mut body) = match route {
        Route::Browse(target) => ("browse", json!({ "browseId": target.id })),
        Route::Search { query, .. } => ("search", json!({ "query": query })),
    };
    let params = match route {
        Route::Browse(target) => target.params.as_ref(),
        Route::Search { params, .. } => params.as_ref(),
    };
    if let Some(params) = params {
        body["params"] = json!(params);
    }
    client.api(endpoint, body)
}

impl Router {
    pub fn new() -> Self {
        Self {
            history: vec![browse("FEmusic_home")],
            cursor: 0,
            state: PageState::Loading,
            generation: 0,
            loading_more: false,
            task: None,
            more_task: None,
        }
    }

    pub fn current(&self) -> &Route {
        &self.history[self.cursor]
    }

    pub fn can_go_back(&self) -> bool {
        self.cursor > 0
    }

    pub fn can_go_forward(&self) -> bool {
        self.cursor + 1 < self.history.len()
    }

    pub fn open(&mut self, route: Route, cx: &mut Context<Self>) {
        if *self.current() != route {
            self.history.truncate(self.cursor + 1);
            self.history.push(route);
            self.cursor = self.history.len() - 1;
        }
        self.reload(cx);
    }

    pub fn back(&mut self, cx: &mut Context<Self>) {
        if self.can_go_back() {
            self.cursor -= 1;
            self.reload(cx);
        }
    }

    pub fn forward(&mut self, cx: &mut Context<Self>) {
        if self.can_go_forward() {
            self.cursor += 1;
            self.reload(cx);
        }
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        self.state = PageState::Loading;
        self.loading_more = false;
        self.more_task = None;
        cx.notify();
        let call = fetch_route(&ctx(cx).client, self.current());
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |router, cx| {
                if router.generation != generation {
                    return;
                }
                router.state = match result {
                    Ok(json) => PageState::Ready(Rc::new(ytm::parse_page(&json))),
                    Err(error) => PageState::Failed(error),
                };
                cx.notify();
            });
        }));
    }

    pub fn load_more(&mut self, cx: &mut Context<Self>) {
        if self.loading_more {
            return;
        }
        let PageState::Ready(page) = &self.state else { return };
        let section = page.sections.iter().rposition(|section| section.continuation.is_some());
        let Some(token) =
            section.and_then(|index| page.sections[index].continuation.clone()).or_else(|| page.continuation.clone())
        else {
            return;
        };
        self.loading_more = true;
        let generation = self.generation;
        let endpoint = if matches!(self.current(), Route::Search { .. }) { "search" } else { "browse" };
        let call = ctx(cx).client.api(endpoint, json!({ "continuation": token }));
        self.more_task = Some(cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |router, cx| {
                if router.generation != generation {
                    return;
                }
                router.loading_more = false;
                let PageState::Ready(current) = &router.state else { return };
                let mut page = (**current).clone();
                match result {
                    Ok(json) => merge_more(&mut page, section, ytm::parse_continuation(&json)),
                    Err(_) => {
                        if let Some(index) = section {
                            page.sections[index].continuation = None;
                        } else {
                            page.continuation = None;
                        }
                    }
                }
                router.state = PageState::Ready(Rc::new(page));
                cx.notify();
            });
        }));
    }
}

fn merge_more(page: &mut Page, section: Option<usize>, more: ytm::More) {
    if let Some(index) = section {
        let target = &mut page.sections[index];
        match &mut target.body {
            Body::List(items) | Body::Grid(items) | Body::Shelf(items) | Body::Picks(items) => {
                items.extend(more.items);
            }
            _ => {}
        }
        target.continuation = more.next;
        page.sections.extend(more.sections);
    } else {
        page.sections.extend(more.sections);
        page.continuation = more.next;
    }
}

// ---- Overlays ------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PanelTab {
    Queue,
    Lyrics,
    Related,
}

#[derive(Clone)]
pub struct OpenMenu {
    pub title: String,
    pub entries: Vec<MenuEntry>,
    pub link: Option<String>,
    pub position: Point<Pixels>,
}

#[derive(Clone)]
pub enum Modal {
    Confirm {
        label: String,
        action: Action,
    },
    AddToPlaylist {
        video_id: Option<String>,
        playlist_id: Option<String>,
        options: Option<Vec<PlaylistOption>>,
        error: Option<String>,
    },
    NewPlaylist {
        video_id: Option<String>,
        playlist_id: Option<String>,
    },
    SignOut,
}

pub struct UiState {
    pub menu: Option<OpenMenu>,
    pub account_menu: Option<Point<Pixels>>,
    pub modal: Option<Modal>,
    pub panel: Option<PanelTab>,
    pub toast: Option<String>,
    pub auth_open: bool,
    pub settings_open: bool,
    /// The "Now" screen: the song's galaxy with its queue, lyrics and related music beside it.
    pub now_open: bool,
    /// Lyrics over the whole window, with the interface put away.
    pub immersive: bool,
    /// The accent of the playing song, taken from its cover, as 0xRRGGBBAA.
    pub accent: u32,
    pub settings_revision: u64,
    pub prefs: Prefs,
    pub playlists: Vec<Item>,
    pub account: Option<Account>,
    toast_id: u64,
    toast_task: Option<Task<()>>,
    sidebar_task: Option<Task<()>>,
}

impl UiState {
    pub fn new() -> Self {
        Self {
            menu: None,
            account_menu: None,
            modal: None,
            panel: None,
            toast: None,
            auth_open: false,
            settings_open: false,
            now_open: false,
            immersive: false,
            accent: crate::ui::ACCENT,
            settings_revision: 0,
            prefs: Prefs::load(),
            playlists: Vec::new(),
            account: None,
            toast_id: 0,
            toast_task: None,
            sidebar_task: None,
        }
    }

    pub fn show_toast(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.toast = Some(text.into());
        self.toast_id += 1;
        let id = self.toast_id;
        cx.notify();
        self.toast_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(3)).await;
            let _ = this.update(cx, |ui, cx| {
                if ui.toast_id == id {
                    ui.toast = None;
                    cx.notify();
                }
            });
        }));
    }

    /// Loads the user's playlists and account for the sidebar.
    pub fn refresh_library(&mut self, cx: &mut Context<Self>) {
        let client = ctx(cx).client;
        let playlists = client.api("browse", json!({ "browseId": "FEmusic_liked_playlists" }));
        let account = client.api("account/account_menu", json!({}));
        self.sidebar_task = Some(cx.spawn(async move |this, cx| {
            let playlists = playlists.await;
            let account = account.await;
            let _ = this.update(cx, |ui, cx| {
                if let Ok(json) = playlists {
                    ui.playlists = ytm::parse_page(&json)
                        .sections
                        .into_iter()
                        .flat_map(|section| match section.body {
                            Body::Grid(items) | Body::Shelf(items) | Body::List(items) | Body::Picks(items) => items,
                            _ => Vec::new(),
                        })
                        .filter(|item| item.kind == Kind::Playlist)
                        .collect();
                }
                if let Ok(json) = account {
                    ui.account = ytm::parse_account(&json);
                }
                cx.notify();
            });
        }));
    }
}

// ---- Actions -------------------------------------------------------------------------

pub fn toast(text: impl Into<String>, cx: &mut App) {
    let text = text.into();
    ctx(cx).ui.update(cx, |ui, cx| ui.show_toast(text, cx));
}

pub fn close_overlays(cx: &mut App) {
    ctx(cx).ui.update(cx, |ui, cx| {
        ui.menu = None;
        ui.account_menu = None;
        ui.modal = None;
        cx.notify();
    });
}

pub fn open(route: Route, cx: &mut App) {
    close_overlays(cx);
    leave_screens(cx);
    ctx(cx).router.update(cx, |router, cx| router.open(route, cx));
}

pub fn set_settings_open(open: bool, cx: &mut App) {
    ctx(cx).ui.update(cx, |ui, cx| {
        if ui.settings_open != open || (open && (ui.now_open || ui.immersive)) {
            ui.settings_open = open;
            if open {
                ui.now_open = false;
                ui.immersive = false;
            }
            cx.notify();
        }
    });
}

/// Shows or leaves the "Now" screen.
pub fn set_now_open(open: bool, cx: &mut App) {
    ctx(cx).ui.update(cx, |ui, cx| {
        ui.now_open = open;
        ui.immersive = false;
        if open {
            ui.settings_open = false;
        }
        cx.notify();
    });
}

/// Puts the interface away, so the lyrics and the galaxy take the whole window.
pub fn set_immersive(on: bool, cx: &mut App) {
    ctx(cx).ui.update(cx, |ui, cx| {
        ui.immersive = on;
        if on {
            ui.now_open = true;
            ui.settings_open = false;
            ui.panel = Some(PanelTab::Lyrics);
        }
        cx.notify();
    });
}

/// Pages live under settings and "Now": opening one leaves both.
fn leave_screens(cx: &mut App) {
    ctx(cx).ui.update(cx, |ui, cx| {
        if ui.settings_open || ui.now_open || ui.immersive {
            ui.settings_open = false;
            ui.now_open = false;
            ui.immersive = false;
            cx.notify();
        }
    });
}

/// Back first leaves the full-screen lyrics, settings and "Now", then walks the page history.
pub fn go_back(cx: &mut App) {
    let app = ctx(cx);
    let (immersive, settings, now) = {
        let ui = app.ui.read(cx);
        (ui.immersive, ui.settings_open, ui.now_open)
    };
    if immersive {
        set_immersive(false, cx);
    } else if settings || now {
        leave_screens(cx);
    } else {
        app.router.update(cx, |router, cx| router.back(cx));
    }
}

pub fn go_forward(cx: &mut App) {
    leave_screens(cx);
    ctx(cx).router.update(cx, |router, cx| router.forward(cx));
}

pub fn open_menu(title: String, entries: Vec<MenuEntry>, link: Option<String>, window: &Window, cx: &mut App) {
    let position = window.mouse_position();
    ctx(cx).ui.update(cx, |ui, cx| {
        ui.account_menu = None;
        ui.menu = Some(OpenMenu { title, entries, link, position });
        cx.notify();
    });
}

pub fn run_entry(entry: &MenuEntry, cx: &mut App) {
    if entry.destructive {
        let (label, action) = (entry.label.clone(), entry.action.clone());
        ctx(cx).ui.update(cx, |ui, cx| {
            ui.menu = None;
            ui.modal = Some(Modal::Confirm { label, action });
            cx.notify();
        });
    } else {
        run_action(&entry.action, cx);
    }
}

fn changes_library(endpoint: &Value) -> bool {
    ["feedbackEndpoint", "playlistEditEndpoint", "deletePlaylistEndpoint", "subscribeEndpoint", "unsubscribeEndpoint"]
        .iter()
        .any(|key| endpoint.get(key).is_some())
}

pub fn run_action(action: &Action, cx: &mut App) {
    let app = ctx(cx);
    app.ui.update(cx, |ui, cx| {
        ui.menu = None;
        ui.modal = None;
        cx.notify();
    });
    match action {
        Action::Play(endpoint) => {
            let call = app.client.play(endpoint);
            let ui = app.ui.clone();
            cx.spawn(async move |cx| {
                if let Err(error) = call.await {
                    let _ = ui.update(cx, |ui, cx| ui.show_toast(format!("Couldn't play: {error}"), cx));
                }
            })
            .detach();
        }
        Action::Open(route) => {
            leave_screens(cx);
            app.router.update(cx, |router, cx| router.open(route.clone(), cx));
        }
        Action::Service(endpoint) => {
            let call = app.client.service(endpoint);
            let refresh = changes_library(endpoint);
            let queued = endpoint.get("queueAddEndpoint").is_some();
            let (ui, router) = (app.ui.clone(), app.router.clone());
            cx.spawn(async move |cx| {
                let result = call.await;
                let _ = ui.update(cx, |ui, cx| match &result {
                    Ok(_) if queued => ui.show_toast("Added to queue", cx),
                    Ok(_) => {
                        ui.show_toast("Done", cx);
                        ui.settings_revision += 1;
                        if refresh {
                            ui.refresh_library(cx);
                        }
                    }
                    Err(error) => ui.show_toast(format!("Something went wrong: {error}"), cx),
                });
                if refresh && result.is_ok() {
                    let _ = router.update(cx, |router, cx| router.reload(cx));
                }
            })
            .detach();
        }
        Action::AddToPlaylist { video_id, playlist_id } => {
            open_add_to_playlist(video_id.clone(), playlist_id.clone(), cx);
        }
    }
}

fn open_add_to_playlist(video_id: Option<String>, playlist_id: Option<String>, cx: &mut App) {
    let app = ctx(cx);
    let body = match (&video_id, &playlist_id) {
        (Some(video), _) => json!({ "videoIds": [video] }),
        (_, Some(playlist)) => json!({ "playlistId": playlist }),
        _ => return,
    };
    let call = app.client.api("playlist/get_add_to_playlist", body);
    app.ui.update(cx, |ui, cx| {
        ui.modal = Some(Modal::AddToPlaylist {
            video_id: video_id.clone(),
            playlist_id: playlist_id.clone(),
            options: None,
            error: None,
        });
        cx.notify();
    });
    let ui = app.ui.clone();
    cx.spawn(async move |cx| {
        let result = call.await;
        let _ = ui.update(cx, |ui, cx| {
            if let Some(Modal::AddToPlaylist { options, error, .. }) = &mut ui.modal {
                match result {
                    Ok(json) => *options = Some(ytm::parse_add_to_playlist(&json)),
                    Err(message) => *error = Some(message),
                }
                cx.notify();
            }
        });
    })
    .detach();
}

pub fn add_to_playlist(target: String, video_id: Option<String>, source: Option<String>, cx: &mut App) {
    let app = ctx(cx);
    let action = match (&video_id, &source) {
        (Some(video), _) => {
            json!({ "action": "ACTION_ADD_VIDEO", "addedVideoId": video, "dedupeOption": "DEDUPE_OPTION_SKIP" })
        }
        (_, Some(list)) => json!({ "action": "ACTION_ADD_PLAYLIST", "addedFullListId": list }),
        _ => return,
    };
    let call = app.client.api("browse/edit_playlist", json!({ "playlistId": target, "actions": [action] }));
    close_overlays(cx);
    let ui = app.ui.clone();
    cx.spawn(async move |cx| {
        let result = call.await;
        let _ = ui.update(cx, |ui, cx| match result {
            Ok(_) => ui.show_toast("Added to playlist", cx),
            Err(error) => ui.show_toast(format!("Something went wrong: {error}"), cx),
        });
    })
    .detach();
}

pub fn create_playlist(title: String, video_id: Option<String>, source: Option<String>, cx: &mut App) {
    let app = ctx(cx);
    let mut body = json!({ "title": title, "description": "", "privacyStatus": "PRIVATE" });
    if let Some(video) = video_id {
        body["videoIds"] = json!([video]);
    }
    if let Some(source) = source {
        body["sourcePlaylistId"] = json!(source);
    }
    let call = app.client.api("playlist/create", body);
    close_overlays(cx);
    let ui = app.ui.clone();
    cx.spawn(async move |cx| {
        let result = call.await;
        let _ = ui.update(cx, |ui, cx| match result {
            Ok(_) => {
                ui.show_toast("Playlist created", cx);
                ui.refresh_library(cx);
            }
            Err(error) => ui.show_toast(format!("Something went wrong: {error}"), cx),
        });
    })
    .detach();
}

/// Sets the like status of a track: LIKE, DISLIKE or INDIFFERENT.
pub fn set_like(video_id: &str, status: &str, cx: &mut App) {
    if video_id.is_empty() {
        return;
    }
    let endpoint = json!({ "likeEndpoint": { "status": status, "target": { "videoId": video_id } } });
    let app = ctx(cx);
    let call = app.client.service(&endpoint);
    app.player.update(cx, |player, cx| {
        if player.state.video_id == video_id {
            player.state.like = status.to_string();
            cx.notify();
        }
    });
    let ui = app.ui.clone();
    cx.spawn(async move |cx| {
        if let Err(error) = call.await {
            let _ = ui.update(cx, |ui, cx| ui.show_toast(format!("Something went wrong: {error}"), cx));
        }
    })
    .detach();
}

pub fn set_subscribed(channel_id: &str, subscribe: bool, cx: &mut App) {
    let key = if subscribe { "subscribeEndpoint" } else { "unsubscribeEndpoint" };
    let endpoint = json!({ key: { "channelIds": [channel_id] } });
    run_action(&Action::Service(endpoint), cx);
}

pub fn begin_sign_in(cx: &mut App) {
    let app = ctx(cx);
    app.client.engine().navigate(SIGN_IN_URL);
    app.ui.update(cx, |ui, cx| {
        ui.auth_open = true;
        ui.menu = None;
        ui.account_menu = None;
        ui.modal = None;
        cx.notify();
    });
}

pub fn sign_out(cx: &mut App) {
    let app = ctx(cx);
    app.client.engine().navigate(SIGN_OUT_URL);
    app.ui.update(cx, |ui, cx| {
        ui.auth_open = true;
        ui.playlists.clear();
        ui.account = None;
        ui.account_menu = None;
        ui.modal = None;
        cx.notify();
    });
}

pub fn item_link(item: &Item) -> Option<String> {
    if let Some(video) = &item.video_id {
        return Some(format!("https://music.youtube.com/watch?v={video}"));
    }
    let Some(Action::Open(Route::Browse(target))) = &item.open else { return None };
    let id = &target.id;
    Some(match id.strip_prefix("VL") {
        Some(list) => format!("https://music.youtube.com/playlist?list={list}"),
        None if id.starts_with("UC") => format!("https://music.youtube.com/channel/{id}"),
        None => format!("https://music.youtube.com/browse/{id}"),
    })
}

pub fn copy_link(link: &str, cx: &mut App) {
    cx.write_to_clipboard(ClipboardItem::new_string(link.to_string()));
    close_overlays(cx);
    toast("Link copied", cx);
}
