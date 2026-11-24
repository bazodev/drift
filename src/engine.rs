//! The hidden engine: a WKWebView running the real YouTube Music page, plus an async
//! client for calling it from Rust.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    future::Future,
    rc::Rc,
    time::Duration,
};

use futures::{
    channel::{
        mpsc::{UnboundedReceiver, UnboundedSender, unbounded},
        oneshot,
    },
    future::{Either, select},
};
use gpui::BackgroundExecutor;
use objc2_foundation::NSString;
use objc2_web_kit::WKInactiveSchedulingPolicy;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wry::{
    NewWindowResponse, PageLoadEvent, Rect, WebView, WebViewBuilder, WebViewExtMacOS,
    dpi::{LogicalPosition, LogicalSize},
    http::Uri,
    raw_window_handle::{HasWindowHandle, RawWindowHandle},
};

pub const HOME: &str = "https://music.youtube.com/";
const MUSIC_HOST: &str = "music.youtube.com";
const SIGN_IN_HOST: &str = "accounts.google.com";
const CONSENT_HOST: &str = "consent.youtube.com";
pub const SIGN_IN_URL: &str =
    "https://accounts.google.com/ServiceLogin?service=youtube&hl=en&continue=https%3A%2F%2Fmusic.youtube.com%2F";
pub const SIGN_OUT_URL: &str =
    "https://accounts.google.com/Logout?service=youtube&continue=https%3A%2F%2Fmusic.youtube.com%2F";
const BRIDGE: &str = include_str!("bridge.js");
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
const DESKTOP_LEVEL_KEY: i32 = 2;
/// canJoinAllSpaces | stationary | ignoresCycle: on every Space, and out of Mission Control and window cycling.
const UNSEEN_BEHAVIOR: usize = 1 | (1 << 4) | (1 << 6);

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGWindowLevelForKey(key: i32) -> i32;
}

/// YouTube Music rejects the bare WKWebView user agent, so its pages get the
/// Safari form of the same WebKit engine. Google's sign-in never sees this one:
/// it gets the unmodified WebView identity and makes its own security decision.
const MUSIC_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
    AppleWebKit/605.1.15 (KHTML, like Gecko) Version/27.0 Safari/605.1.15";

#[derive(Clone, Copy, PartialEq)]
enum Identity {
    Music,
    WebView,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct PlayerState {
    pub signed_in: bool,
    pub has_track: bool,
    pub playing: bool,
    pub position: f64,
    pub duration: f64,
    pub volume: f64,
    pub muted: bool,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub artwork: String,
    pub is_ad: bool,
    pub visibility: String,
    pub url: String,
    pub video_id: String,
    pub like: String,
    pub repeat: String,
    pub shuffle: bool,
    pub safety_mode: bool,
    pub language: String,
    pub region: String,
    /// The song has a music video, so YouTube Music offers its Song / Video switch.
    pub clip_available: bool,
    /// The page plays the music video instead of the song.
    pub clip_mode: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct QueueItem {
    pub video_id: String,
    pub title: String,
    pub artist: String,
    pub duration: String,
    pub thumbnail: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct QueueState {
    pub index: usize,
    pub items: Vec<QueueItem>,
}

#[derive(Deserialize)]
struct ApiReply {
    id: u64,
    ok: bool,
    #[serde(default)]
    data: Value,
    #[serde(default)]
    error: String,
}

/// One picture of the music video (384×216), sent by the page as a base64 JPEG while Drift asks for the stream.
#[derive(Deserialize)]
struct ClipFrame {
    jpeg: String,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Incoming {
    State(Box<PlayerState>),
    Queue(QueueState),
    Api(ApiReply),
    Frame(ClipFrame),
}

pub enum EngineEvent {
    State(Box<PlayerState>),
    Queue(QueueState),
    Reply(u64, Result<Value, String>),
    PageStarted(String),
    Navigate(String),
    OpenExternal(String),
    #[cfg(debug_assertions)]
    Dev(String),
}

#[derive(Clone, Copy, Debug)]
pub enum Command {
    Toggle,
    Next,
    Previous,
    Seek(f64),
    Volume(f64),
    Mute,
    Select(usize),
    Remove(usize),
    Move(usize, usize),
    Shuffle,
    Repeat(&'static str),
    /// Plays the song's music video (true) or the song itself (false).
    Clip(bool),
    /// Starts or stops the page sending the music video's frames.
    Stream(bool),
}

pub struct Engine {
    webview: WebView,
    bounds: (f32, f32, f32, f32),
    visible: bool,
    /// A clip is being turned into stars. WebKit only keeps a page's video moving while the page
    /// is on screen, so the page stays in the window, fully transparent and under the interface,
    /// and counts as on screen even while other windows cover Drift.
    watching: bool,
    /// GPUI's own view: the page sits above it for sign-in and below it while watching.
    host: *mut objc2::runtime::AnyObject,
    /// The clip's stars also show on the desktop, so the clip must keep moving while the
    /// Drift window is down in the Dock.
    seen_elsewhere: bool,
    /// While the Drift window is minimized, the page waits unseen in a window of its own.
    parked: Option<Parked>,
    identity: Identity,
}

/// WebKit only keeps a page's video moving while the page's window is on screen. A minimized
/// window is not, so the page moves to a window that is on screen but never seen: clear,
/// click-through and under everything, where the Drift window was.
struct Parked {
    window: objc2::rc::Retained<objc2::runtime::AnyObject>,
    /// The view the page came from, and its place there.
    home: *mut objc2::runtime::AnyObject,
    frame: objc2_foundation::NSRect,
    /// The interface asked for a new place meanwhile, to be taken on the way back.
    moved: bool,
}

fn parse_https(url: &str) -> Option<Uri> {
    let uri = url.parse::<Uri>().ok()?;
    (uri.scheme_str() == Some("https")).then_some(uri)
}

fn host_is(host: &str, domain: &str) -> bool {
    host == domain || host.strip_suffix(domain).is_some_and(|rest| rest.ends_with('.'))
}

fn page_host_is(url: &str, expected: &str) -> bool {
    parse_https(url).as_ref().and_then(Uri::host) == Some(expected)
}

/// Google's cookie choice: only the user can answer it, so it must be shown.
pub fn is_consent_page(url: &str) -> bool {
    page_host_is(url, CONSENT_HOST)
}

pub fn is_music_page(url: &str) -> bool {
    page_host_is(url, MUSIC_HOST)
}

/// Google and YouTube popups (sign-in, consent) stay inside the engine.
fn stays_in_engine(host: &str) -> bool {
    ["google.com", "youtube.com"].iter().any(|domain| host_is(host, domain))
}

impl Engine {
    pub fn new(
        window: &impl HasWindowHandle,
    ) -> wry::Result<(Self, UnboundedSender<EngineEvent>, UnboundedReceiver<EngineEvent>)> {
        let (tx, rx) = unbounded();
        let host = match window.window_handle().map(|handle| handle.as_raw()) {
            Ok(RawWindowHandle::AppKit(handle)) => handle.ns_view.as_ptr().cast(),
            _ => std::ptr::null_mut(),
        };
        let ipc_tx = tx.clone();
        let popup_tx = tx.clone();
        let page_tx = tx.clone();

        let webview = WebViewBuilder::new()
            .with_url(HOME)
            .with_user_agent(MUSIC_USER_AGENT)
            .with_on_page_load_handler(move |event, url| {
                if matches!(event, PageLoadEvent::Started) {
                    let _ = page_tx.unbounded_send(EngineEvent::PageStarted(url));
                }
            })
            .with_autoplay(true)
            .with_visible(false)
            .with_clipboard(true)
            .with_accept_first_mouse(true)
            .with_devtools(cfg!(debug_assertions))
            .with_initialization_script_for_main_only(BRIDGE, true)
            .with_ipc_handler(move |request| {
                if request.uri().host() != Some(MUSIC_HOST) {
                    return;
                }
                let event = match serde_json::from_str::<Incoming>(request.body()) {
                    // Frames go straight to the stars, without a trip through the interface.
                    Ok(Incoming::Frame(frame)) => {
                        if let Some(jpeg) = decode_base64(&frame.jpeg) {
                            crate::live::clip_frame(jpeg);
                        }
                        return;
                    }
                    Ok(Incoming::State(state)) => EngineEvent::State(state),
                    Ok(Incoming::Queue(queue)) => EngineEvent::Queue(queue),
                    Ok(Incoming::Api(reply)) => {
                        EngineEvent::Reply(reply.id, if reply.ok { Ok(reply.data) } else { Err(reply.error) })
                    }
                    Err(_) => return,
                };
                let _ = ipc_tx.unbounded_send(event);
            })
            .with_new_window_req_handler(move |url, _features| {
                if let Some(host) = parse_https(&url).as_ref().and_then(Uri::host) {
                    let event = if stays_in_engine(host) {
                        EngineEvent::Navigate(url.clone())
                    } else {
                        EngineEvent::OpenExternal(url.clone())
                    };
                    let _ = popup_tx.unbounded_send(event);
                }
                NewWindowResponse::Deny
            })
            .build_as_child(window)?;

        // The engine is hidden most of the time, so WebKit must not suspend or throttle it.
        unsafe {
            webview
                .webview()
                .configuration()
                .preferences()
                .setInactiveSchedulingPolicy(WKInactiveSchedulingPolicy::None);
        }

        let engine = Self {
            webview,
            bounds: (0.0, 0.0, 0.0, 0.0),
            visible: false,
            watching: false,
            host,
            seen_elsewhere: false,
            parked: None,
            identity: Identity::Music,
        };
        Ok((engine, tx, rx))
    }

    /// Switches identity when the top-level page crosses between music and sign-in.
    pub fn page_started(&mut self, url: &str) {
        let Some(uri) = parse_https(url) else { return };
        let wanted = match uri.host() {
            Some(MUSIC_HOST) => Identity::Music,
            Some(SIGN_IN_HOST) => Identity::WebView,
            _ => return,
        };
        if wanted == self.identity {
            return;
        }
        self.identity = wanted;
        let agent = (wanted == Identity::Music).then(|| NSString::from_str(MUSIC_USER_AGENT));
        #[allow(unused_unsafe)]
        unsafe {
            self.webview.webview().setCustomUserAgent(agent.as_deref());
        }
        let _ = self.webview.load_url(url);
    }

    fn run(&self, script: &str) {
        let _ = self.webview.evaluate_script(script);
    }

    #[cfg(debug_assertions)]
    pub fn eval_logged(&self, script: &str) {
        let _ = self.webview.evaluate_script_with_callback(script, |result| {
            eprintln!("eval: {result}");
        });
    }

    /// Keeps the page rendering, unseen, while its music video is turned into stars.
    /// `seen_elsewhere`: the stars also show on the desktop, so the clip keeps moving even
    /// while the Drift window is minimized.
    pub fn set_watching(&mut self, watching: bool, seen_elsewhere: bool) {
        self.seen_elsewhere = seen_elsewhere;
        if watching != self.watching {
            self.unpark();
            self.watching = watching;
            self.present();
        }
        self.follow_window();
    }

    /// The page takes the keyboard only while it is shown for sign-in. Unseen it must never keep it:
    /// YouTube Music reads single keys as shortcuts, so typing into Drift's search would mute or skip.
    pub fn keep_keys(&self) {
        use objc2::{class, msg_send, runtime::AnyObject};
        if self.visible || self.host.is_null() || self.parked.is_some() {
            return;
        }
        let view = self.webview.webview();
        let page: *const AnyObject = objc2::rc::Retained::as_ptr(&view).cast();
        unsafe {
            let window: *mut AnyObject = msg_send![self.host, window];
            if window.is_null() {
                return;
            }
            let responder: *mut AnyObject = msg_send![window, firstResponder];
            let lost = if responder.is_null() || responder == window {
                true
            } else {
                let is_view: bool = msg_send![responder, isKindOfClass: class!(NSView)];
                is_view && {
                    let inside: bool = msg_send![responder, isDescendantOf: page];
                    inside
                }
            };
            if lost {
                #[cfg(debug_assertions)]
                if std::env::var_os("DRIFT_DEV_FIFO").is_some() {
                    eprintln!("engine: the keyboard was away from Drift, taking it back");
                }
                let _: bool = msg_send![window, makeFirstResponder: self.host];
            }
        }
    }

    /// Parks the page while the Drift window is away and the clip still shows on the desktop,
    /// and brings it home as soon as the window is back.
    fn follow_window(&mut self) {
        let away = self.watching && self.seen_elsewhere && !self.visible && self.window_away();
        if away && self.parked.is_none() {
            self.park();
        } else if !away && self.parked.is_some() {
            self.unpark();
            self.present();
        }
        self.keep_keys();
    }

    /// The Drift window is minimized or closed, while the app itself is not hidden:
    /// a hidden app hides the desktop galaxy too, and then nothing shows the clip.
    fn window_away(&self) -> bool {
        use objc2::{class, msg_send, runtime::AnyObject};
        if self.host.is_null() {
            return false;
        }
        unsafe {
            let window: *mut AnyObject = msg_send![self.host, window];
            if window.is_null() {
                return false;
            }
            let visible: bool = msg_send![window, isVisible];
            let minimized: bool = msg_send![window, isMiniaturized];
            let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let hidden: bool = msg_send![app, isHidden];
            (minimized || !visible) && !hidden
        }
    }

    fn park(&mut self) {
        use objc2::{
            class, msg_send,
            rc::Retained,
            runtime::{AnyObject, Bool},
        };
        use objc2_foundation::{NSPoint, NSRect};
        let view = self.webview.webview();
        let page: *const AnyObject = Retained::as_ptr(&view).cast();
        let nothing: *mut AnyObject = std::ptr::null_mut();
        unsafe {
            let home: *mut AnyObject = msg_send![page, superview];
            let main: *mut AnyObject = msg_send![self.host, window];
            if home.is_null() || main.is_null() {
                return;
            }
            let frame: NSRect = msg_send![page, frame];
            // Where the Drift window was: the same screen and scale keep the page and its video as they were.
            let main_frame: NSRect = msg_send![main, frame];
            let window: *mut AnyObject = msg_send![class!(NSWindow), alloc];
            let window: *mut AnyObject = msg_send![
                window,
                initWithContentRect: NSRect::new(main_frame.origin, frame.size),
                styleMask: 0usize,
                backing: 2usize,
                defer: Bool::NO
            ];
            let Some(window) = Retained::from_raw(window) else { return };
            let clear: *mut AnyObject = msg_send![class!(NSColor), clearColor];
            let _: () = msg_send![&*window, setReleasedWhenClosed: Bool::NO];
            let _: () = msg_send![&*window, setBackgroundColor: clear];
            let _: () = msg_send![&*window, setOpaque: Bool::NO];
            let _: () = msg_send![&*window, setHasShadow: Bool::NO];
            let _: () = msg_send![&*window, setIgnoresMouseEvents: Bool::YES];
            let _: () = msg_send![&*window, setExcludedFromWindowsMenu: Bool::YES];
            let _: () = msg_send![&*window, setLevel: CGWindowLevelForKey(DESKTOP_LEVEL_KEY) as isize];
            let _: () = msg_send![&*window, setCollectionBehavior: UNSEEN_BEHAVIOR];
            let content: *mut AnyObject = msg_send![&*window, contentView];
            let _: () = msg_send![page, setFrame: NSRect::new(NSPoint::new(0., 0.), frame.size)];
            let _: () = msg_send![content, addSubview: page];
            let _: () = msg_send![&*window, orderBack: nothing];
            self.parked = Some(Parked { window, home, frame, moved: false });
        }
    }

    fn unpark(&mut self) {
        use objc2::{msg_send, runtime::AnyObject};
        let Some(parked) = self.parked.take() else { return };
        let view = self.webview.webview();
        let page: *const AnyObject = objc2::rc::Retained::as_ptr(&view).cast();
        let nothing: *mut AnyObject = std::ptr::null_mut();
        unsafe {
            let _: () = msg_send![parked.home, addSubview: page];
            let _: () = msg_send![page, setFrame: parked.frame];
            let _: () = msg_send![&*parked.window, orderOut: nothing];
            let _: () = msg_send![&*parked.window, close];
        }
        if parked.moved {
            self.apply_bounds();
        }
    }

    /// Shows the page for sign-in, keeps it rendering unseen under the interface while a clip
    /// plays, or hides it.
    fn present(&mut self) {
        use objc2::{msg_send, runtime::AnyObject, sel};
        let view = self.webview.webview();
        let page: *const AnyObject = objc2::rc::Retained::as_ptr(&view).cast();
        // WebKit treats the page of a covered window as hidden, and its video then gives one frame
        // a second. While a clip feeds the galaxy, covering Drift must not stop it. This is set
        // before the page is shown, so WebKit already counts with it when the page appears.
        unsafe {
            let available: bool = msg_send![page, respondsToSelector: sel!(_setWindowOcclusionDetectionEnabled:)];
            if available {
                let _: () = msg_send![page, _setWindowOcclusionDetectionEnabled: !self.watching];
            }
        }
        let _ = self.webview.set_visible(self.visible || self.watching);
        unsafe {
            let _: () = msg_send![page, setAlphaValue: if self.visible { 1.0f64 } else { 0.0f64 }];
            // wry puts the page inside GPUI's own view, where even a fully transparent page catches
            // every click, scroll and hover meant for the interface. As GPUI's sibling it sits above
            // the interface for sign-in, and under it, out of the pointer's way, while watching.
            let content: *mut AnyObject =
                if self.host.is_null() { std::ptr::null_mut() } else { msg_send![self.host, superview] };
            if !content.is_null() {
                let order: isize = if self.visible { 1 } else { -1 };
                let _: () = msg_send![content, addSubview: page, positioned: order, relativeTo: self.host];
            }
        }
        // The page's place is counted from its parent, which may have just changed.
        if self.bounds.2 > 0. && self.bounds.3 > 0. {
            self.apply_bounds();
        }
        let _ = if self.visible { self.webview.focus() } else { self.webview.focus_parent() };
    }

    /// Debug: which views clicks across the window would reach, on a 4×4 grid, and who has the keyboard.
    #[cfg(debug_assertions)]
    pub fn report_hit(&self) {
        use objc2::{msg_send, runtime::AnyObject};
        use objc2_foundation::{NSPoint, NSRect};
        if self.host.is_null() {
            return;
        }
        unsafe {
            let content: *mut AnyObject = msg_send![self.host, superview];
            let window: *mut AnyObject = msg_send![self.host, window];
            if content.is_null() || window.is_null() {
                return;
            }
            let name = |object: *mut AnyObject| {
                if object.is_null() {
                    "nothing".to_string()
                } else {
                    (*object).class().name().to_string_lossy().into_owned()
                }
            };
            let frame: NSRect = msg_send![content, frame];
            let mut seen = std::collections::BTreeMap::<String, u32>::new();
            for row in 0..4 {
                for column in 0..4 {
                    let at = NSPoint::new(
                        frame.origin.x + frame.size.width * (column as f64 + 0.5) / 4.,
                        frame.origin.y + frame.size.height * (row as f64 + 0.5) / 4.,
                    );
                    let hit: *mut AnyObject = msg_send![content, hitTest: at];
                    *seen.entry(name(hit)).or_default() += 1;
                }
            }
            let responder: *mut AnyObject = msg_send![window, firstResponder];
            eprintln!("engine: clicks on a 4x4 grid go to {seen:?}; the keyboard is with {}", name(responder));
        }
    }

    pub fn navigate(&self, url: &str) {
        if parse_https(url).as_ref().and_then(Uri::host).is_some_and(stays_in_engine) {
            let _ = self.webview.load_url(url);
        }
    }

    /// The WebKit process that actually plays this page's audio.
    pub fn audio_process(&self) -> Option<i32> {
        let view = self.webview.webview();
        let selector = objc2::sel!(_gpuProcessIdentifier);
        let available: bool = unsafe { objc2::msg_send![&*view, respondsToSelector: selector] };
        if !available {
            return None;
        }
        let pid: i32 = unsafe { objc2::msg_send![&*view, _gpuProcessIdentifier] };
        (pid > 0).then_some(pid)
    }

    /// GPUI has no window dragging on macOS, so custom title bars ask AppKit directly.
    pub fn drag_window(&self) {
        let Some(mtm) = objc2::MainThreadMarker::new() else { return };
        let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
        if let (Some(event), Some(window)) = (app.currentEvent(), self.webview.webview().window()) {
            window.performWindowDragWithEvent(&event);
        }
    }

    pub fn place(&mut self, x: f32, y: f32, width: f32, height: f32) {
        let bounds = (x, y, width, height);
        if bounds == self.bounds {
            return;
        }
        self.bounds = bounds;
        // A parked page keeps its size and takes the new place once it is home.
        if let Some(parked) = &mut self.parked {
            parked.moved = true;
            return;
        }
        self.apply_bounds();
    }

    fn apply_bounds(&self) {
        let (x, y, width, height) = self.bounds;
        let _ = self.webview.set_bounds(Rect {
            position: LogicalPosition::new(x, y).into(),
            size: LogicalSize::new(width, height).into(),
        });
    }

    pub fn set_visible(&mut self, visible: bool) {
        if visible == self.visible {
            return;
        }
        self.visible = visible;
        self.unpark();
        self.present();
    }
}

/// Standard base64, as the page's btoa() writes it.
fn decode_base64(text: &str) -> Option<Vec<u8>> {
    fn value(byte: u8) -> Option<u32> {
        Some(u32::from(match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        }))
    }
    let bytes = text.trim_end_matches('=').as_bytes();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        if chunk.len() < 2 {
            return None;
        }
        let mut bits = 0u32;
        for (index, &byte) in chunk.iter().enumerate() {
            bits |= value(byte)? << (18 - 6 * index);
        }
        for index in 0..chunk.len() - 1 {
            out.push((bits >> (16 - 8 * index)) as u8);
        }
    }
    Some(out)
}

fn command_script(command: Command) -> String {
    let (name, arg) = match command {
        Command::Toggle => ("toggle", json!(null)),
        Command::Next => ("next", json!(null)),
        Command::Previous => ("previous", json!(null)),
        Command::Seek(seconds) => ("seek", json!(seconds)),
        Command::Volume(level) => ("volume", json!(level.round())),
        Command::Mute => ("mute", json!(null)),
        Command::Select(index) => ("select", json!(index)),
        Command::Remove(index) => ("remove", json!(index)),
        Command::Move(from, to) => ("move", json!({ "from": from, "to": to })),
        Command::Shuffle => ("shuffle", json!(null)),
        Command::Repeat(mode) => ("repeat", json!(mode)),
        Command::Clip(on) => ("clip", json!(on)),
        Command::Stream(on) => ("stream", json!(on)),
    };
    format!("window.__drift?.exec({},{arg})", json!(name))
}

struct ClientInner {
    engine: RefCell<Engine>,
    executor: BackgroundExecutor,
    next_id: Cell<u64>,
    pending: RefCell<HashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    ready: Cell<bool>,
    waiting: RefCell<Vec<String>>,
}

/// Async access to the engine. Calls made while the page loads are held until it is ready.
#[derive(Clone)]
pub struct Client(Rc<ClientInner>);

impl Client {
    pub fn new(engine: Engine, executor: BackgroundExecutor) -> Self {
        Self(Rc::new(ClientInner {
            engine: RefCell::new(engine),
            executor,
            next_id: Cell::new(1),
            pending: RefCell::default(),
            ready: Cell::new(false),
            waiting: RefCell::default(),
        }))
    }

    fn submit(&self, script: String) {
        if self.0.ready.get() {
            self.0.engine.borrow().run(&script);
        } else {
            self.0.waiting.borrow_mut().push(script);
        }
    }

    fn request(&self, build: impl FnOnce(u64) -> String) -> impl Future<Output = Result<Value, String>> + 'static {
        let id = self.0.next_id.get();
        self.0.next_id.set(id + 1);
        let (tx, rx) = oneshot::channel();
        self.0.pending.borrow_mut().insert(id, tx);
        self.submit(build(id));
        let timeout = self.0.executor.timer(CALL_TIMEOUT);
        let inner = Rc::downgrade(&self.0);
        async move {
            match select(rx, timeout).await {
                Either::Left((Ok(result), _)) => result,
                Either::Left((Err(_), _)) => Err("the request was cancelled".into()),
                Either::Right(_) => {
                    if let Some(inner) = inner.upgrade() {
                        inner.pending.borrow_mut().remove(&id);
                    }
                    Err("YouTube Music did not respond".into())
                }
            }
        }
    }

    pub fn api(&self, endpoint: &str, body: Value) -> impl Future<Output = Result<Value, String>> + 'static {
        let endpoint = json!(endpoint);
        self.request(move |id| format!("window.__drift.api({id},{endpoint},{body})"))
    }

    pub fn service(&self, endpoint: &Value) -> impl Future<Output = Result<Value, String>> + 'static {
        let endpoint = endpoint.clone();
        self.request(move |id| format!("window.__drift.service({id},{endpoint})"))
    }

    pub fn play(&self, endpoint: &Value) -> impl Future<Output = Result<Value, String>> + 'static {
        let endpoint = endpoint.clone();
        self.request(move |id| format!("window.__drift.play({id},{endpoint})"))
    }

    /// The playing music video's own subtitles, in YouTube's json3 format, if it has any.
    pub fn captions(&self, video: &str) -> impl Future<Output = Result<Value, String>> + 'static {
        let video = json!(video);
        self.request(move |id| format!("window.__drift.captions({id},{video})"))
    }

    pub fn send(&self, command: Command) {
        self.submit(command_script(command));
    }

    pub fn resolve(&self, id: u64, result: Result<Value, String>) {
        if let Some(tx) = self.0.pending.borrow_mut().remove(&id) {
            let _ = tx.send(result);
        }
    }

    pub fn set_ready(&self, ready: bool) {
        self.0.ready.set(ready);
        if ready {
            let scripts = std::mem::take(&mut *self.0.waiting.borrow_mut());
            let engine = self.0.engine.borrow();
            for script in scripts {
                engine.run(&script);
            }
        }
    }

    pub fn engine(&self) -> std::cell::RefMut<'_, Engine> {
        self.0.engine.borrow_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::decode_base64;

    #[test]
    fn decodes_base64_like_btoa() {
        assert_eq!(decode_base64("TWFu").unwrap(), b"Man");
        assert_eq!(decode_base64("TWE=").unwrap(), b"Ma");
        assert_eq!(decode_base64("TQ==").unwrap(), b"M");
        assert_eq!(decode_base64("AP8A/w==").unwrap(), [0, 255, 0, 255]);
        assert!(decode_base64("T").is_none(), "a lone character is not a byte");
        assert!(decode_base64("TW!u").is_none());
    }
}
