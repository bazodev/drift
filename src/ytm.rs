//! Parses YouTube Music API responses into the app's own model.

use serde_json::Value;

#[derive(Clone, Debug, PartialEq)]
pub struct BrowseRef {
    pub id: String,
    pub params: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Route {
    Browse(BrowseRef),
    Search { query: String, params: Option<String> },
}

#[derive(Clone, Debug)]
pub enum Action {
    Play(Value),
    Open(Route),
    Service(Value),
    AddToPlaylist { video_id: Option<String>, playlist_id: Option<String> },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Song,
    Video,
    Album,
    Artist,
    Playlist,
    Podcast,
    Episode,
    Other,
}

#[derive(Clone, Debug)]
pub struct MenuEntry {
    pub label: String,
    pub icon: String,
    pub action: Action,
    pub destructive: bool,
}

#[derive(Clone, Debug)]
pub struct Link {
    pub text: String,
    pub route: Option<Route>,
}

#[derive(Clone, Debug)]
pub struct Item {
    pub kind: Kind,
    pub title: String,
    pub subtitle: Vec<Link>,
    pub thumbnail: String,
    pub duration: String,
    pub explicit: bool,
    pub video_id: Option<String>,
    pub open: Option<Action>,
    pub play: Option<Action>,
    pub menu: Vec<MenuEntry>,
}

impl Item {
    pub fn subtitle_text(&self) -> String {
        self.subtitle.iter().map(|link| link.text.as_str()).collect()
    }

    pub fn is_track(&self) -> bool {
        self.video_id.is_some() && matches!(self.kind, Kind::Song | Kind::Video | Kind::Episode)
    }
}

#[derive(Clone, Debug)]
pub struct Tile {
    pub title: String,
    pub color: u32,
    pub route: Option<Route>,
}

#[derive(Clone, Debug)]
pub enum Body {
    Shelf(Vec<Item>),
    /// Compact multi-column track rows, like "quick picks".
    Picks(Vec<Item>),
    List(Vec<Item>),
    Grid(Vec<Item>),
    Tiles(Vec<Tile>),
    Text(String),
}

#[derive(Clone, Debug)]
pub struct Section {
    pub title: String,
    pub strapline: String,
    pub more: Option<Route>,
    pub body: Body,
    pub continuation: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Chip {
    pub label: String,
    pub route: Option<Route>,
    pub selected: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Header {
    pub title: String,
    pub subtitle: String,
    pub second: String,
    pub description: String,
    pub thumbnail: String,
    pub round: bool,
    pub play: Option<Action>,
    pub shuffle: Option<Action>,
    pub radio: Option<Action>,
    pub library: Option<MenuEntry>,
    pub subscribe: Option<Subscribe>,
    pub menu: Vec<MenuEntry>,
}

#[derive(Clone, Debug)]
pub struct Subscribe {
    pub channel_id: String,
    pub subscribed: bool,
    pub label: String,
}

#[derive(Clone, Debug, Default)]
pub struct Page {
    pub header: Option<Header>,
    pub chips: Vec<Chip>,
    pub sections: Vec<Section>,
    pub continuation: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct WatchTabs {
    pub lyrics: Option<BrowseRef>,
    pub related: Option<BrowseRef>,
}

#[derive(Clone, Debug)]
pub struct Lyrics {
    pub text: String,
    pub source: String,
}

// Only a handful of suggestions exist at a time, so the size of an item does not matter.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub enum Suggestion {
    Query(String),
    Item(Item),
}

#[derive(Clone, Debug)]
pub struct PlaylistOption {
    pub id: String,
    pub title: String,
    pub subtitle: String,
}

#[derive(Clone, Debug, Default)]
pub struct Account {
    pub name: String,
    pub photo: String,
}

// ---- Value helpers -------------------------------------------------------------------

fn at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(value, |current, key| current.get(key))
}

fn items_of(value: Option<&Value>) -> &[Value] {
    value.and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
}

fn string_at(value: &Value, path: &[&str]) -> Option<String> {
    at(value, path).and_then(Value::as_str).map(str::to_string)
}

pub fn text(value: Option<&Value>) -> String {
    let Some(value) = value else { return String::new() };
    if let Some(simple) = value.get("simpleText").and_then(Value::as_str) {
        return simple.to_string();
    }
    items_of(value.get("runs")).iter().filter_map(|run| run.get("text").and_then(Value::as_str)).collect()
}

fn find_key<'a>(value: &'a Value, key: &str, depth: usize) -> Option<&'a Value> {
    if depth == 0 {
        return None;
    }
    match value {
        Value::Object(map) => map.get(key).or_else(|| map.values().find_map(|child| find_key(child, key, depth - 1))),
        Value::Array(list) => list.iter().find_map(|child| find_key(child, key, depth - 1)),
        _ => None,
    }
}

fn thumbnail(value: Option<&Value>) -> String {
    let Some(value) = value else { return String::new() };
    find_key(value, "thumbnails", 7)
        .and_then(Value::as_array)
        .and_then(|list| list.last())
        .and_then(|thumb| thumb.get("url"))
        .and_then(Value::as_str)
        .map(|url| if url.starts_with("//") { format!("https:{url}") } else { url.to_string() })
        .unwrap_or_default()
}

fn page_type(endpoint: &Value) -> &str {
    at(
        endpoint,
        &["browseEndpoint", "browseEndpointContextSupportedConfigs", "browseEndpointContextMusicConfig", "pageType"],
    )
    .and_then(Value::as_str)
    .unwrap_or("")
}

fn kind_of_page(page: &str) -> Kind {
    match page {
        "MUSIC_PAGE_TYPE_ALBUM" | "MUSIC_PAGE_TYPE_AUDIOBOOK" => Kind::Album,
        "MUSIC_PAGE_TYPE_ARTIST" | "MUSIC_PAGE_TYPE_USER_CHANNEL" | "MUSIC_PAGE_TYPE_LIBRARY_ARTIST" => Kind::Artist,
        "MUSIC_PAGE_TYPE_PLAYLIST" => Kind::Playlist,
        "MUSIC_PAGE_TYPE_PODCAST_SHOW_DETAIL_PAGE" => Kind::Podcast,
        "MUSIC_PAGE_TYPE_NON_MUSIC_AUDIO_TRACK_PAGE" => Kind::Episode,
        _ => Kind::Other,
    }
}

fn kind_of_watch(endpoint: &Value) -> Kind {
    let music_type = at(
        endpoint,
        &["watchEndpoint", "watchEndpointMusicSupportedConfigs", "watchEndpointMusicConfig", "musicVideoType"],
    )
    .and_then(Value::as_str)
    .unwrap_or("");
    match music_type {
        "MUSIC_VIDEO_TYPE_ATV" => Kind::Song,
        "MUSIC_VIDEO_TYPE_PODCAST_EPISODE" => Kind::Episode,
        "" => Kind::Song,
        _ => Kind::Video,
    }
}

fn route_of(endpoint: &Value) -> Option<Route> {
    if let Some(browse) = endpoint.get("browseEndpoint") {
        return Some(Route::Browse(BrowseRef {
            id: browse.get("browseId")?.as_str()?.to_string(),
            params: browse.get("params").and_then(Value::as_str).map(str::to_string),
        }));
    }
    let search = endpoint.get("searchEndpoint")?;
    Some(Route::Search {
        query: search.get("query")?.as_str()?.to_string(),
        params: search.get("params").and_then(Value::as_str).map(str::to_string),
    })
}

fn is_service(endpoint: &Value) -> bool {
    [
        "queueAddEndpoint",
        "feedbackEndpoint",
        "likeEndpoint",
        "playlistEditEndpoint",
        "subscribeEndpoint",
        "unsubscribeEndpoint",
        "deletePlaylistEndpoint",
        "setSettingEndpoint",
        "setClientSettingEndpoint",
    ]
    .iter()
    .any(|key| endpoint.get(key).is_some())
}

fn action_of(endpoint: &Value) -> Option<Action> {
    if let Some(watch) = endpoint.get("watchEndpoint").or_else(|| endpoint.get("watchPlaylistEndpoint")) {
        return Some(Action::Play(watch.clone()));
    }
    if let Some(route) = route_of(endpoint) {
        return Some(Action::Open(route));
    }
    if let Some(add) = endpoint.get("addToPlaylistEndpoint") {
        return Some(Action::AddToPlaylist {
            video_id: string_at(add, &["videoId"]),
            playlist_id: string_at(add, &["playlistId"]),
        });
    }
    is_service(endpoint).then(|| Action::Service(endpoint.clone()))
}

fn is_destructive(endpoint: &Value, icon: &str) -> bool {
    endpoint.get("deletePlaylistEndpoint").is_some()
        || endpoint.get("playlistEditEndpoint").is_some()
        || matches!(icon, "DELETE" | "REMOVE_FROM_HISTORY" | "LIBRARY_REMOVE" | "REMOVE_FROM_PLAYLIST")
}

fn menu_entry(item: &Value) -> Option<MenuEntry> {
    let (renderer, text_key, icon_key, endpoint_key) = if let Some(r) = item.get("menuNavigationItemRenderer") {
        (r, "text", "icon", "navigationEndpoint")
    } else if let Some(r) = item.get("menuServiceItemRenderer") {
        (r, "text", "icon", "serviceEndpoint")
    } else {
        let r = item.get("toggleMenuServiceItemRenderer")?;
        (r, "defaultText", "defaultIcon", "defaultServiceEndpoint")
    };
    let endpoint = renderer.get(endpoint_key)?;
    let icon = string_at(renderer, &[icon_key, "iconType"]).unwrap_or_default();
    let endpoint = at(endpoint, &["confirmDialogEndpoint", "content"])
        .and_then(|_| find_key(endpoint, "serviceEndpoint", 8))
        .unwrap_or(endpoint);
    Some(MenuEntry {
        label: text(renderer.get(text_key)),
        destructive: is_destructive(endpoint, &icon),
        action: action_of(endpoint)?,
        icon,
    })
}

fn menu_of(renderer: &Value) -> Vec<MenuEntry> {
    items_of(at(renderer, &["menu", "menuRenderer", "items"])).iter().filter_map(menu_entry).collect()
}

fn links_of(value: Option<&Value>) -> Vec<Link> {
    let Some(value) = value else { return Vec::new() };
    if let Some(simple) = value.get("simpleText").and_then(Value::as_str) {
        return vec![Link { text: simple.to_string(), route: None }];
    }
    items_of(value.get("runs"))
        .iter()
        .filter_map(|run| {
            Some(Link {
                text: run.get("text")?.as_str()?.to_string(),
                route: run.get("navigationEndpoint").and_then(route_of),
            })
        })
        .collect()
}

fn overlay_play(renderer: &Value) -> Option<Action> {
    let overlay = renderer.get("overlay").or_else(|| renderer.get("thumbnailOverlay"))?;
    let endpoint = at(
        overlay,
        &["musicItemThumbnailOverlayRenderer", "content", "musicPlayButtonRenderer", "playNavigationEndpoint"],
    )?;
    action_of(endpoint)
}

fn explicit(renderer: &Value) -> bool {
    items_of(renderer.get("badges")).iter().any(|badge| {
        string_at(badge, &["musicInlineBadgeRenderer", "icon", "iconType"]).as_deref() == Some("MUSIC_EXPLICIT_BADGE")
    }) || items_of(renderer.get("subtitleBadges")).iter().any(|badge| {
        string_at(badge, &["musicInlineBadgeRenderer", "icon", "iconType"]).as_deref() == Some("MUSIC_EXPLICIT_BADGE")
    })
}

// ---- Items ---------------------------------------------------------------------------

fn two_row_item(renderer: &Value) -> Option<Item> {
    let endpoint = renderer.get("navigationEndpoint")?;
    let is_watch = endpoint.get("watchEndpoint").is_some();
    let kind = if is_watch { kind_of_watch(endpoint) } else { kind_of_page(page_type(endpoint)) };
    Some(Item {
        kind,
        title: text(renderer.get("title")),
        subtitle: links_of(renderer.get("subtitle")),
        thumbnail: thumbnail(renderer.get("thumbnailRenderer")),
        duration: String::new(),
        explicit: explicit(renderer),
        video_id: string_at(endpoint, &["watchEndpoint", "videoId"]),
        open: action_of(endpoint),
        play: overlay_play(renderer),
        menu: menu_of(renderer),
    })
}

fn flex_column(renderer: &Value, index: usize) -> Option<&Value> {
    items_of(renderer.get("flexColumns")).get(index)?.get("musicResponsiveListItemFlexColumnRenderer")?.get("text")
}

fn is_clock(value: &str) -> bool {
    let value = value.trim();
    value.contains(':') && value.chars().all(|c| c.is_ascii_digit() || c == ':')
}

fn responsive_item(renderer: &Value) -> Option<Item> {
    let title_value = flex_column(renderer, 0);
    let title = text(title_value);
    if title.is_empty() {
        return None;
    }
    let mut subtitle = Vec::new();
    for index in 1..items_of(renderer.get("flexColumns")).len() {
        let links = links_of(flex_column(renderer, index));
        if links.is_empty() {
            continue;
        }
        if !subtitle.is_empty() {
            subtitle.push(Link { text: " • ".into(), route: None });
        }
        subtitle.extend(links);
    }

    let fixed = items_of(renderer.get("fixedColumns"))
        .first()
        .and_then(|column| column.get("musicResponsiveListItemFixedColumnRenderer"))
        .map(|column| text(column.get("text")))
        .unwrap_or_default();
    let mut duration = if is_clock(&fixed) { fixed } else { String::new() };
    if duration.is_empty() && subtitle.last().is_some_and(|link| is_clock(&link.text)) {
        duration = subtitle.pop().map(|link| link.text).unwrap_or_default();
        while subtitle.last().is_some_and(|link| link.text.trim() == "•") {
            subtitle.pop();
        }
    }

    let title_endpoint =
        title_value.and_then(|value| items_of(value.get("runs")).first()).and_then(|run| run.get("navigationEndpoint"));
    let own_endpoint = renderer.get("navigationEndpoint");
    let play = overlay_play(renderer);
    let video_id = string_at(renderer, &["playlistItemData", "videoId"])
        .or_else(|| title_endpoint.and_then(|endpoint| string_at(endpoint, &["watchEndpoint", "videoId"])));

    let endpoint = own_endpoint.or(title_endpoint);
    let kind = match endpoint {
        Some(endpoint) if endpoint.get("browseEndpoint").is_some() => kind_of_page(page_type(endpoint)),
        Some(endpoint) => kind_of_watch(endpoint),
        None if video_id.is_some() => Kind::Song,
        None => Kind::Other,
    };
    let open = endpoint.and_then(action_of).or_else(|| play.clone());

    Some(Item {
        kind,
        title,
        subtitle,
        thumbnail: thumbnail(renderer.get("thumbnail")),
        duration,
        explicit: explicit(renderer),
        video_id,
        open,
        play,
        menu: menu_of(renderer),
    })
}

fn card_item(renderer: &Value) -> Option<Item> {
    let title_value = renderer.get("title")?;
    let endpoint =
        renderer.get("onTap").or_else(|| items_of(title_value.get("runs")).first()?.get("navigationEndpoint"))?;
    let kind = if endpoint.get("browseEndpoint").is_some() {
        kind_of_page(page_type(endpoint))
    } else {
        kind_of_watch(endpoint)
    };
    Some(Item {
        kind,
        title: text(Some(title_value)),
        subtitle: links_of(renderer.get("subtitle")),
        thumbnail: thumbnail(renderer.get("thumbnail")),
        duration: String::new(),
        explicit: explicit(renderer),
        video_id: string_at(endpoint, &["watchEndpoint", "videoId"]),
        open: action_of(endpoint),
        play: overlay_play(renderer),
        menu: menu_of(renderer),
    })
}

fn item_of(entry: &Value) -> Option<Item> {
    if let Some(r) = entry.get("musicTwoRowItemRenderer") {
        two_row_item(r)
    } else if let Some(r) = entry.get("musicResponsiveListItemRenderer") {
        responsive_item(r)
    } else {
        None
    }
}

fn tile_of(entry: &Value) -> Option<Tile> {
    let renderer = entry.get("musicNavigationButtonRenderer")?;
    let color = renderer
        .pointer("/solid/leftStripeColor")
        .and_then(Value::as_u64)
        .map_or(0x3a3f47, |argb| (argb & 0xff_ffff) as u32);
    Some(Tile {
        title: text(renderer.get("buttonText")),
        color,
        route: renderer.get("clickCommand").and_then(route_of),
    })
}

// ---- Sections ------------------------------------------------------------------------

fn continuation_token(renderer: &Value) -> Option<String> {
    if let Some(token) = items_of(renderer.get("continuations")).first().and_then(|entry| {
        entry
            .pointer("/nextContinuationData/continuation")
            .or_else(|| entry.pointer("/reloadContinuationData/continuation"))
    }) {
        return token.as_str().map(str::to_string);
    }
    items_of(renderer.get("contents"))
        .iter()
        .chain(items_of(renderer.get("items")))
        .rev()
        .find_map(|entry| entry.pointer("/continuationItemRenderer/continuationEndpoint/continuationCommand/token"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn items_in(list: &[Value]) -> Vec<Item> {
    list.iter().filter_map(item_of).collect()
}

fn parse_grid(renderer: &Value, fallback_title: String) -> Option<Section> {
    let entries = items_of(renderer.get("items"));
    let title = at(renderer, &["header", "gridHeaderRenderer", "title"])
        .map(|value| text(Some(value)))
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback_title);
    let tiles: Vec<Tile> = entries.iter().filter_map(tile_of).collect();
    let body = if !tiles.is_empty() { Body::Tiles(tiles) } else { Body::Grid(items_in(entries)) };
    Some(Section { title, strapline: String::new(), more: None, continuation: continuation_token(renderer), body })
}

fn section_of(entry: &Value, out: &mut Vec<Section>) {
    if let Some(r) =
        entry.get("musicCarouselShelfRenderer").or_else(|| entry.get("musicImmersiveCarouselShelfRenderer"))
    {
        let header = r
            .pointer("/header/musicCarouselShelfBasicHeaderRenderer")
            .or_else(|| r.pointer("/header/musicImmersiveCarouselShelfBasicHeaderRenderer"));
        let contents = items_of(r.get("contents"));
        let tiles: Vec<Tile> = contents.iter().filter_map(tile_of).collect();
        let items = items_in(contents);
        let body = if !tiles.is_empty() {
            Body::Tiles(tiles)
        } else if contents.iter().any(|c| c.get("musicResponsiveListItemRenderer").is_some()) {
            Body::Picks(items)
        } else {
            Body::Shelf(items)
        };
        let more = header
            .and_then(|h| h.pointer("/moreContentButton/buttonRenderer/navigationEndpoint"))
            .and_then(route_of)
            .or_else(|| {
                header
                    .and_then(|h| h.get("title"))
                    .and_then(|t| items_of(t.get("runs")).first())
                    .and_then(|run| run.get("navigationEndpoint"))
                    .and_then(route_of)
            });
        out.push(Section {
            title: text(header.and_then(|h| h.get("title"))),
            strapline: text(header.and_then(|h| h.get("strapline"))),
            more,
            body,
            continuation: None,
        });
    } else if let Some(r) = entry.get("musicShelfRenderer").or_else(|| entry.get("musicPlaylistShelfRenderer")) {
        let more = r
            .pointer("/bottomEndpoint")
            .and_then(route_of)
            .or_else(|| r.pointer("/title/runs/0/navigationEndpoint").and_then(route_of));
        out.push(Section {
            title: text(r.get("title")),
            strapline: String::new(),
            more,
            continuation: continuation_token(r),
            body: Body::List(items_in(items_of(r.get("contents")))),
        });
    } else if let Some(r) = entry.get("gridRenderer") {
        out.extend(parse_grid(r, String::new()));
    } else if let Some(r) = entry.get("musicDescriptionShelfRenderer") {
        let description = text(r.get("description"));
        if !description.is_empty() {
            out.push(Section {
                title: text(r.get("header")),
                strapline: String::new(),
                more: None,
                continuation: None,
                body: Body::Text(description),
            });
        }
    } else if let Some(r) = entry.get("musicCardShelfRenderer") {
        let mut items: Vec<Item> = card_item(r).into_iter().collect();
        items.extend(items_in(items_of(r.get("contents"))));
        out.push(Section {
            title: text(r.pointer("/header/musicCardShelfHeaderBasicRenderer/title")),
            strapline: String::new(),
            more: None,
            continuation: None,
            body: Body::List(items),
        });
    } else if let Some(r) = entry.get("itemSectionRenderer") {
        let mut rows = Vec::new();
        for child in items_of(r.get("contents")) {
            match item_of(child) {
                Some(item) => rows.push(item),
                None => section_of(child, out),
            }
        }
        if !rows.is_empty() {
            out.push(Section {
                title: String::new(),
                strapline: String::new(),
                more: None,
                continuation: None,
                body: Body::List(rows),
            });
        }
    }
}

/// Search returns each row in its own untitled section: join them back into one list.
fn merge_lists(sections: &mut Vec<Section>) {
    let mut merged: Vec<Section> = Vec::with_capacity(sections.len());
    for section in std::mem::take(sections) {
        if section.title.is_empty()
            && section.more.is_none()
            && let (Some(Section { body: Body::List(target), .. }), Body::List(items)) =
                (merged.last_mut(), &section.body)
        {
            target.extend(items.iter().cloned());
            continue;
        }
        merged.push(section);
    }
    *sections = merged;
}

fn chips_of(section_list: &Value) -> Vec<Chip> {
    let chips = section_list
        .pointer("/header/chipCloudRenderer/chips")
        .or_else(|| section_list.pointer("/header/musicSideAlignedItemRenderer/startItems/0/chipCloudRenderer/chips"));
    items_of(chips)
        .iter()
        .filter_map(|chip| {
            let renderer = chip.get("chipCloudChipRenderer")?;
            Some(Chip {
                label: text(renderer.get("text")),
                route: renderer.get("navigationEndpoint").and_then(route_of),
                selected: renderer.get("isSelected").and_then(Value::as_bool).unwrap_or(false),
            })
        })
        .collect()
}

fn parse_header(root: &Value, section_list: Option<&Value>) -> Option<Header> {
    let responsive = section_list.into_iter().flat_map(|list| items_of(list.get("contents"))).find_map(|entry| {
        entry.get("musicResponsiveHeaderRenderer").or_else(|| {
            entry.pointer("/musicEditablePlaylistDetailHeaderRenderer/header/musicResponsiveHeaderRenderer")
        })
    });
    let header = root.get("header");
    let (renderer, round) = if let Some(r) = responsive {
        (r, false)
    } else if let Some(r) = header.and_then(|h| h.get("musicImmersiveHeaderRenderer")) {
        (r, true)
    } else if let Some(r) = header.and_then(|h| h.get("musicVisualHeaderRenderer")) {
        (r, true)
    } else if let Some(r) = header.and_then(|h| h.get("musicDetailHeaderRenderer")) {
        (r, false)
    } else {
        return header
            .and_then(|h| h.get("musicHeaderRenderer"))
            .map(|r| Header { title: text(r.get("title")), ..Header::default() })
            .filter(|h| !h.title.is_empty());
    };

    let mut out = Header {
        title: text(renderer.get("title")),
        subtitle: [text(renderer.get("subtitle")), text(renderer.get("straplineTextOne"))]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" · "),
        second: text(renderer.get("secondSubtitle")),
        description: text(renderer.get("description")).chars().take(600).collect::<String>(),
        thumbnail: thumbnail(renderer.get("thumbnail").or_else(|| renderer.get("foregroundThumbnail"))),
        round,
        ..Header::default()
    };
    if out.description.is_empty() {
        out.description =
            text(find_key(renderer, "musicDescriptionShelfRenderer", 4).and_then(|d| d.get("description")))
                .chars()
                .take(600)
                .collect();
    }

    let mut menus: Vec<MenuEntry> = menu_of(renderer);
    for button in items_of(renderer.get("buttons")) {
        if let Some(play) = button.pointer("/musicPlayButtonRenderer/playNavigationEndpoint").and_then(action_of) {
            out.play = Some(play);
        } else if let Some(items) = button.pointer("/menuRenderer/items") {
            menus.extend(items_of(Some(items)).iter().filter_map(menu_entry));
        } else if let Some(toggle) = button.get("toggleButtonRenderer") {
            let toggled = toggle.get("isToggled").and_then(Value::as_bool).unwrap_or(false);
            let (text_key, icon_key, endpoint_key) = if toggled {
                ("toggledText", "toggledIcon", "toggledServiceEndpoint")
            } else {
                ("defaultText", "defaultIcon", "defaultServiceEndpoint")
            };
            if let Some(action) = toggle.get(endpoint_key).and_then(action_of) {
                out.library = Some(MenuEntry {
                    label: text(toggle.get(text_key)),
                    icon: string_at(toggle, &[icon_key, "iconType"]).unwrap_or_default(),
                    action,
                    destructive: toggled,
                });
            }
        }
    }
    for (key, slot) in [("playButton", 0), ("startRadioButton", 1)] {
        if let Some(action) = renderer.pointer(&format!("/{key}/buttonRenderer/navigationEndpoint")).and_then(action_of)
        {
            if slot == 0 { out.shuffle = Some(action) } else { out.radio = Some(action) }
        }
    }
    if let Some(subscribe) = renderer.pointer("/subscriptionButton/subscribeButtonRenderer")
        && let Some(channel_id) = string_at(subscribe, &["channelId"])
    {
        let subscribed = subscribe.get("subscribed").and_then(Value::as_bool).unwrap_or(false);
        out.subscribe = Some(Subscribe {
            channel_id,
            subscribed,
            label: text(subscribe.get(if subscribed { "subscribedButtonText" } else { "unsubscribedButtonText" })),
        });
    }
    menus.retain(|entry| match entry.icon.as_str() {
        "MUSIC_SHUFFLE" => {
            out.shuffle.get_or_insert_with(|| entry.action.clone());
            false
        }
        "MIX" => {
            out.radio.get_or_insert_with(|| entry.action.clone());
            false
        }
        _ => true,
    });
    out.menu = menus;
    Some(out)
}

fn section_lists(root: &Value) -> Vec<&Value> {
    let contents = root.get("contents");
    let mut lists = Vec::new();
    for key in ["singleColumnBrowseResultsRenderer", "twoColumnBrowseResultsRenderer", "tabbedSearchResultsRenderer"] {
        if let Some(container) = contents.and_then(|c| c.get(key)) {
            for tab in items_of(container.get("tabs")) {
                let selected = tab.pointer("/tabRenderer/selected").and_then(Value::as_bool);
                if selected == Some(false) {
                    continue;
                }
                if let Some(list) = tab.pointer("/tabRenderer/content/sectionListRenderer") {
                    lists.push(list);
                    break;
                }
            }
            if let Some(list) = container.pointer("/secondaryContents/sectionListRenderer") {
                lists.push(list);
            }
        }
    }
    if let Some(list) = contents.and_then(|c| c.get("sectionListRenderer")) {
        lists.push(list);
    }
    lists
}

pub fn parse_page(root: &Value) -> Page {
    let lists = section_lists(root);
    let mut page = Page { header: parse_header(root, lists.first().copied()), ..Page::default() };
    for list in lists {
        if page.chips.is_empty() {
            page.chips = chips_of(list);
        }
        for entry in items_of(list.get("contents")) {
            section_of(entry, &mut page.sections);
        }
        if page.continuation.is_none() {
            page.continuation = continuation_token(list);
        }
    }
    merge_lists(&mut page.sections);
    page.sections.retain(|section| match &section.body {
        Body::Shelf(items) | Body::Picks(items) | Body::List(items) | Body::Grid(items) => !items.is_empty(),
        Body::Tiles(tiles) => !tiles.is_empty(),
        Body::Text(value) => !value.is_empty(),
    });
    page
}

/// Result of loading more: items for the open list, or whole new sections.
#[derive(Debug, Default)]
pub struct More {
    pub items: Vec<Item>,
    pub sections: Vec<Section>,
    pub next: Option<String>,
}

pub fn parse_continuation(root: &Value) -> More {
    let mut more = More::default();
    if let Some(contents) = root.get("continuationContents") {
        if let Some(list) = contents.get("sectionListContinuation") {
            for entry in items_of(list.get("contents")) {
                section_of(entry, &mut more.sections);
            }
            more.next = continuation_token(list);
        }
        for key in ["musicShelfContinuation", "musicPlaylistShelfContinuation"] {
            if let Some(shelf) = contents.get(key) {
                more.items = items_in(items_of(shelf.get("contents")));
                more.next = continuation_token(shelf);
            }
        }
        if let Some(grid) = contents.get("gridContinuation") {
            more.items = items_in(items_of(grid.get("items")));
            more.next = continuation_token(grid);
        }
    }
    for action in items_of(root.get("onResponseReceivedActions")) {
        if let Some(entries) = action.pointer("/appendContinuationItemsAction/continuationItems") {
            more.items.extend(items_in(items_of(Some(entries))));
            more.next = items_of(Some(entries))
                .iter()
                .rev()
                .find_map(|entry| {
                    entry.pointer("/continuationItemRenderer/continuationEndpoint/continuationCommand/token")
                })
                .and_then(Value::as_str)
                .map(str::to_string);
        }
    }
    more
}

pub fn parse_watch_tabs(root: &Value) -> WatchTabs {
    let tabs = root.pointer(
        "/contents/singleColumnMusicWatchNextResultsRenderer/tabbedRenderer/watchNextTabbedResultsRenderer/tabs",
    );
    let mut result = WatchTabs::default();
    for tab in items_of(tabs) {
        let Some(browse) = tab.pointer("/tabRenderer/endpoint/browseEndpoint") else { continue };
        let Some(id) = browse.get("browseId").and_then(Value::as_str) else { continue };
        let target =
            BrowseRef { id: id.to_string(), params: browse.get("params").and_then(Value::as_str).map(str::to_string) };
        if id.starts_with("MPLY") {
            result.lyrics = Some(target);
        } else if id.starts_with("MPTR") {
            result.related = Some(target);
        }
    }
    result
}

pub fn parse_lyrics(root: &Value) -> Option<Lyrics> {
    let shelf = find_key(root.get("contents")?, "musicDescriptionShelfRenderer", 10)?;
    let value = text(shelf.get("description"));
    (!value.is_empty()).then(|| Lyrics { text: value, source: text(shelf.get("footer")) })
}

pub fn parse_suggestions(root: &Value) -> Vec<Suggestion> {
    let mut out = Vec::new();
    for group in items_of(root.get("contents")) {
        for entry in items_of(group.pointer("/searchSuggestionsSectionRenderer/contents")) {
            if let Some(renderer) = entry.get("searchSuggestionRenderer") {
                let query = string_at(renderer, &["navigationEndpoint", "searchEndpoint", "query"])
                    .unwrap_or_else(|| text(renderer.get("suggestion")));
                out.push(Suggestion::Query(query));
            } else if let Some(item) = item_of(entry) {
                out.push(Suggestion::Item(item));
            }
        }
    }
    out
}

pub fn parse_add_to_playlist(root: &Value) -> Vec<PlaylistOption> {
    let list =
        items_of(root.get("contents")).iter().find_map(|entry| entry.pointer("/addToPlaylistRenderer/playlists"));
    items_of(list)
        .iter()
        .filter_map(|entry| {
            let renderer = entry.get("playlistAddToOptionRenderer")?;
            Some(PlaylistOption {
                id: renderer.get("playlistId")?.as_str()?.to_string(),
                title: text(renderer.get("title")),
                subtitle: text(renderer.get("shortBylineText")),
            })
        })
        .collect()
}

pub fn parse_account(root: &Value) -> Option<Account> {
    let header = find_key(root, "activeAccountHeaderRenderer", 9)?;
    Some(Account { name: text(header.get("accountName")), photo: thumbnail(header.get("accountPhoto")) })
}

// ---- Settings ------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct SettingOption {
    pub name: String,
    pub selected: bool,
    pub endpoint: Value,
}

#[derive(Clone, Debug)]
pub enum SettingItem {
    Toggle { title: String, summary: String, enabled: bool, enable: Value, disable: Value },
    Choice { title: String, summary: String, options: Vec<SettingOption> },
    Action { title: String, summary: String, endpoint: Value, confirm: String },
    Info { title: String, summary: String, url: Option<String> },
}

#[derive(Clone, Debug)]
pub struct SettingCategory {
    pub title: String,
    pub items: Vec<SettingItem>,
}

/// True for settings stored in the page's cookie, which reload the player when changed.
pub fn reloads_player(endpoint: &Value) -> bool {
    endpoint.get("setClientSettingEndpoint").is_some()
}

pub fn is_safety_toggle(endpoint: &Value) -> bool {
    endpoint.pointer("/setClientSettingEndpoint/settingDatas/0/clientSettingEnum/item").and_then(Value::as_str)
        == Some("SAFETY_MODE")
}

/// YouTube does not mark the active language or country, so compare with the page's values.
pub fn is_current_locale(endpoint: &Value, language: &str, region: &str) -> bool {
    let Some(data) = endpoint.pointer("/setClientSettingEndpoint/settingDatas/0") else {
        return false;
    };
    let value = data.get("stringValue").and_then(Value::as_str);
    match data.pointer("/clientSettingEnum/item").and_then(Value::as_str) {
        Some("I18N_LANGUAGE") => value == Some(language),
        Some("I18N_REGION") => value == Some(region),
        _ => false,
    }
}

fn setting_item(entry: &Value) -> Option<SettingItem> {
    if let Some(r) = entry.get("settingBooleanRenderer") {
        return Some(SettingItem::Toggle {
            title: text(r.get("title")),
            summary: text(r.get("summary")),
            enabled: r.get("enabled").and_then(Value::as_bool).unwrap_or(false),
            enable: r.get("enableServiceEndpoint")?.clone(),
            disable: r.get("disableServiceEndpoint")?.clone(),
        });
    }
    if let Some(r) = entry.get("settingSingleOptionMenuRenderer") {
        let options = items_of(r.get("items"))
            .iter()
            .filter_map(|option| {
                let option = option.get("settingMenuItemRenderer")?;
                let name =
                    option.get("name").and_then(Value::as_str).map_or_else(|| text(option.get("name")), str::to_string);
                Some(SettingOption {
                    name,
                    selected: option.get("selected").and_then(Value::as_bool).unwrap_or(false),
                    endpoint: option.get("updateServiceEndpoint")?.clone(),
                })
            })
            .collect();
        return Some(SettingItem::Choice { title: text(r.get("title")), summary: text(r.get("summary")), options });
    }
    if let Some(r) = entry.get("settingActionRenderer") {
        let confirm = r
            .pointer("/confirmDialog/confirmDialogRenderer/dialogMessages/0")
            .map(|message| text(Some(message)))
            .unwrap_or_default();
        return Some(SettingItem::Action {
            title: text(r.get("title")),
            summary: text(r.get("summary")),
            endpoint: r.get("serviceEndpoint")?.clone(),
            confirm,
        });
    }
    let r = entry.get("settingReadOnlyItemRenderer")?;
    let title = text(r.get("title"));
    let summary = [text(r.get("subtitle")), text(r.get("summary"))]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let url = find_key(r, "urlEndpoint", 6)
        .and_then(|endpoint| endpoint.get("url"))
        .and_then(Value::as_str)
        .filter(|url| url.starts_with("https://"))
        .map(str::to_string);
    (!title.is_empty() || !summary.is_empty()).then_some(SettingItem::Info { title, summary, url })
}

pub fn parse_settings(root: &Value) -> Vec<SettingCategory> {
    items_of(root.get("items"))
        .iter()
        .filter_map(|entry| {
            let category = entry.get("settingCategoryCollectionRenderer")?;
            Some(SettingCategory {
                title: text(category.get("title")),
                items: items_of(category.get("items")).iter().filter_map(setting_item).collect(),
            })
        })
        .filter(|category| !category.items.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn matches_current_locale() {
        let language = json!({"setClientSettingEndpoint": {"settingDatas": [
            {"clientSettingEnum": {"item": "I18N_LANGUAGE"}, "stringValue": "ru"}]}});
        let region = json!({"setClientSettingEndpoint": {"settingDatas": [
            {"clientSettingEnum": {"item": "I18N_REGION"}, "stringValue": "DE"}]}});
        assert!(is_current_locale(&language, "ru", "DE"));
        assert!(!is_current_locale(&language, "en", "DE"));
        assert!(is_current_locale(&region, "ru", "DE"));
        assert!(!is_current_locale(&json!({}), "ru", "DE"));
    }

    #[test]
    fn parses_settings() {
        let root = json!({"items": [{"settingCategoryCollectionRenderer": {
            "title": {"runs": [{"text": "General"}]},
            "items": [
                {"settingBooleanRenderer": {
                    "title": {"runs": [{"text": "Restricted mode"}]},
                    "enableServiceEndpoint": {"setClientSettingEndpoint": {"settingDatas": [
                        {"clientSettingEnum": {"item": "SAFETY_MODE"}, "boolValue": true}]}},
                    "disableServiceEndpoint": {"setClientSettingEndpoint": {"settingDatas": [
                        {"clientSettingEnum": {"item": "SAFETY_MODE"}, "boolValue": false}]}}
                }},
                {"settingSingleOptionMenuRenderer": {
                    "title": {"runs": [{"text": "Quality"}]},
                    "items": [{"settingMenuItemRenderer": {"name": "High", "selected": true,
                        "updateServiceEndpoint": {"setSettingEndpoint": {"settingItemId": "1", "intValue": "2"}}}}]
                }}
            ]
        }}]});
        let categories = parse_settings(&root);
        assert_eq!(categories[0].items.len(), 2);
        let SettingItem::Toggle { enable, enabled, .. } = &categories[0].items[0] else { panic!() };
        assert!(!enabled);
        assert!(is_safety_toggle(enable));
        assert!(reloads_player(enable));
        let SettingItem::Choice { options, .. } = &categories[0].items[1] else { panic!() };
        assert!(options[0].selected);
        assert!(!reloads_player(&options[0].endpoint));
    }

    #[test]
    fn parses_a_track_row() {
        let row = json!({"musicResponsiveListItemRenderer": {
            "flexColumns": [
                {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{
                    "text": "Song", "navigationEndpoint": {"watchEndpoint": {"videoId": "abc",
                    "watchEndpointMusicSupportedConfigs": {"watchEndpointMusicConfig": {"musicVideoType": "MUSIC_VIDEO_TYPE_ATV"}}}}
                }]}}},
                {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [
                    {"text": "Artist", "navigationEndpoint": {"browseEndpoint": {"browseId": "UC1"}}}
                ]}}}
            ],
            "fixedColumns": [{"musicResponsiveListItemFixedColumnRenderer": {"text": {"runs": [{"text": "3:21"}]}}}],
            "playlistItemData": {"videoId": "abc"}
        }});
        let item = item_of(&row).unwrap();
        assert_eq!(item.title, "Song");
        assert_eq!(item.duration, "3:21");
        assert_eq!(item.kind, Kind::Song);
        assert_eq!(item.video_id.as_deref(), Some("abc"));
        assert!(matches!(item.subtitle[0].route, Some(Route::Browse(_))));
    }

    #[test]
    fn clock_detection() {
        assert!(is_clock("3:21"));
        assert!(is_clock("1:02:03"));
        assert!(!is_clock("2024"));
        assert!(!is_clock("12 songs"));
    }
}
