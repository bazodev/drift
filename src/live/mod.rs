//! "Live Mac": the parts of Drift that react to the music outside the main window.
//! Rhythm comes from the Drift Audio helper as small frames over a local socket.

mod chorus;
mod galaxy;
mod glyphs;
mod island;
mod motion;
#[cfg(debug_assertions)]
mod pace;
mod palette;
mod scatter;
mod story;
mod wallpaper;
mod words;

use std::{
    env, fs,
    os::unix::net::UnixDatagram,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU32, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use gpui::App;

#[cfg(debug_assertions)]
pub use island::dev_command;
use motion::Motion;

pub const BANDS: usize = 32;
const FRAME_MAGIC: u32 = 0x4446_4C31;
const FRAME_BYTES: usize = 8 + 5 * 4 + BANDS * 4;
const HELPER_RUN_SECONDS: &str = "604800";

#[derive(Clone, Copy, Debug)]
pub struct Frame {
    pub bands: [f32; BANDS],
    pub level: f32,
    pub low: f32,
    pub mid: f32,
    pub high: f32,
    pub voice: f32,
    pub at: Instant,
}

pub fn decode(bytes: &[u8], at: Instant) -> Option<Frame> {
    if bytes.len() != FRAME_BYTES || u32::from_le_bytes(bytes[0..4].try_into().ok()?) != FRAME_MAGIC {
        return None;
    }
    let value = |index: usize| {
        let start = 8 + index * 4;
        f32::from_le_bytes([bytes[start], bytes[start + 1], bytes[start + 2], bytes[start + 3]])
    };
    let mut bands = [0.; BANDS];
    for (index, band) in bands.iter_mut().enumerate() {
        *band = value(5 + index).clamp(0., 1.);
    }
    Some(Frame { bands, level: value(0), low: value(1), mid: value(2), high: value(3), voice: 0., at })
}

/// Receives rhythm frames and keeps the newest one.
#[derive(Clone)]
pub struct Bus {
    latest: Arc<Mutex<Option<Frame>>>,
    socket: PathBuf,
    report: PathBuf,
}

impl Bus {
    fn start() -> Option<Self> {
        let directory = PathBuf::from(env::var_os("HOME")?).join("Library/Caches/Drift");
        fs::create_dir_all(&directory).ok()?;
        let socket = directory.join("live.sock");
        let _ = fs::remove_file(&socket);
        let receiver = UnixDatagram::bind(&socket).ok()?;
        let latest = Arc::new(Mutex::new(None));
        let shared = latest.clone();
        thread::spawn(move || {
            let mut motion = Motion::default();
            let mut previous = Instant::now();
            let mut buffer = [0; FRAME_BYTES + 16];
            while let Ok(count) = receiver.recv(&mut buffer) {
                let at = Instant::now();
                if let Some(mut frame) = decode(&buffer[..count], at) {
                    motion.apply(&mut frame, at.duration_since(previous).as_secs_f32());
                    previous = at;
                    if let Ok(mut slot) = shared.lock() {
                        *slot = Some(frame);
                    }
                }
            }
        });
        Some(Self { latest, socket, report: directory.join("live-report.json") })
    }

    /// The newest frame, if the helper sent one recently.
    pub fn fresh(&self) -> Option<Frame> {
        let frame = (*self.latest.lock().ok()?)?;
        (frame.at.elapsed() < Duration::from_millis(400)).then_some(frame)
    }

    /// Starts the helper for this WebKit audio process, replacing any stale one.
    pub fn launch_helper(&self, audio_pid: i32) {
        let Some(helper) = helper_path() else { return };
        let (socket, report) = (self.socket.clone(), self.report.clone());
        thread::spawn(move || {
            // macOS lends a helper inside Drift.app the sound permission of Drift.app itself, and every update
            // signs Drift.app anew. A copy of its own, outside the bundle, keeps the permission it was given once.
            let helper = settled_helper(&helper).unwrap_or(helper);
            let _ = Command::new("pkill").args(["-x", "drift-audio"]).stderr(Stdio::null()).status();
            thread::sleep(Duration::from_millis(250));
            let _ = Command::new("open")
                .args(["-n", "-g"])
                .arg(&helper)
                .arg("--args")
                .arg(audio_pid.to_string())
                .arg(&report)
                .arg(HELPER_RUN_SECONDS)
                .arg(&socket)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        });
    }
}

/// The helper's own copy in Drift's folder, made again only when the helper itself has changed.
/// The copy keeps the helper's signature, so macOS knows it as the same helper.
fn settled_helper(bundled: &Path) -> Option<PathBuf> {
    let settled = PathBuf::from(env::var_os("HOME")?).join("Library/Application Support/Drift/Drift Audio.app");
    let program = |app: &Path| fs::read(app.join("Contents/MacOS/drift-audio")).ok();
    let wanted = program(bundled)?;
    if program(&settled).as_deref() != Some(wanted.as_slice()) {
        let _ = fs::remove_dir_all(&settled);
        fs::create_dir_all(settled.parent()?).ok()?;
        let copied = Command::new("ditto").arg(bundled).arg(&settled).stderr(Stdio::null()).status().ok()?;
        if !copied.success() {
            return None;
        }
    }
    Some(settled)
}

/// Inside Drift.app the helper lives in Contents/Helpers. A dev build keeps it next to target/debug.
fn helper_path() -> Option<PathBuf> {
    let executable = env::current_exe().ok()?;
    let near = executable.parent()?.parent()?;
    [near.join("Helpers/Drift Audio.app"), near.join("live/Drift Audio.app")].into_iter().find(|path| path.exists())
}

/// Where the page's clip frames land: the galaxy's render thread.
static CLIP_SINK: OnceLock<wallpaper::ClipSink> = OnceLock::new();
static CLIP_FRAMES: AtomicU32 = AtomicU32::new(0);
static CLIP_PICTURES: AtomicU32 = AtomicU32::new(0);
static CLIP_LAST: AtomicU64 = AtomicU64::new(0);
/// The colors of the clip's current scene, and when they were taken. The island glows in them too.
static CLIP_COLORS: Mutex<Option<(palette::Palette, Instant)>> = Mutex::new(None);
/// A picture of the music video, as a JPEG, straight from the page. The galaxy's render thread unpacks it.
/// Pictures are never kept.
pub fn clip_frame(jpeg: Vec<u8>) {
    let print: u64 =
        jpeg.iter().step_by(97).enumerate().map(|(index, &value)| (index as u64 + 1) * u64::from(value)).sum::<u64>()
            ^ jpeg.len() as u64;
    CLIP_FRAMES.fetch_add(1, Ordering::Relaxed);
    if CLIP_LAST.swap(print, Ordering::Relaxed) != print {
        CLIP_PICTURES.fetch_add(1, Ordering::Relaxed);
    }
    if let Some(sink) = CLIP_SINK.get() {
        sink.push(jpeg);
    }
}

/// The render thread found the colors of the clip's current scene. The island glows in them too.
fn found_clip_colors(found: palette::Palette) {
    if let Ok(mut slot) = CLIP_COLORS.lock() {
        *slot = Some((found, Instant::now()));
    }
}

/// The colors of the clip's current scene, while its frames keep coming.
pub fn clip_palette() -> Option<palette::Palette> {
    let slot = CLIP_COLORS.lock().ok()?;
    slot.as_ref().filter(|(_, at)| at.elapsed() < Duration::from_millis(1500)).map(|(found, _)| *found)
}

/// Frames that arrived since the last call, and how many of them showed a new picture.
#[cfg(debug_assertions)]
fn clip_counts() -> (u32, u32) {
    (CLIP_FRAMES.swap(0, Ordering::Relaxed), CLIP_PICTURES.swap(0, Ordering::Relaxed))
}

/// `main` is the Drift window: the song's galaxy is drawn under its interface too.
pub fn start(cx: &mut App, main: Option<*mut objc2::runtime::AnyObject>) {
    let bus = Bus::start();
    let wallpaper = wallpaper::Wallpaper::start(bus.clone(), main);
    if let Some(wallpaper) = &wallpaper {
        let _ = CLIP_SINK.set(wallpaper.clip_sink());
    }
    island::open(bus, wallpaper, cx);
}

/// The AppKit window behind a GPUI window.
pub fn native_window(window: &gpui::Window) -> Option<*mut objc2::runtime::AnyObject> {
    use objc2::{msg_send, runtime::AnyObject};
    use wry::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else { return None };
    let view = handle.ns_view.as_ptr().cast::<AnyObject>();
    let native: *mut AnyObject = unsafe { msg_send![view, window] };
    (!native.is_null()).then_some(native)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_a_frame() {
        let mut bytes = vec![0; FRAME_BYTES];
        bytes[0..4].copy_from_slice(&FRAME_MAGIC.to_le_bytes());
        bytes[4..8].copy_from_slice(&9u32.to_le_bytes());
        bytes[8..12].copy_from_slice(&0.5f32.to_le_bytes());
        bytes[28..32].copy_from_slice(&2f32.to_le_bytes());
        let frame = decode(&bytes, Instant::now()).unwrap();
        assert_eq!(frame.level, 0.5);
        assert_eq!(frame.bands[0], 1., "bands are clamped");
    }

    #[test]
    fn rejects_foreign_data() {
        assert!(decode(&[0; 12], Instant::now()).is_none());
        assert!(decode(&[0; FRAME_BYTES], Instant::now()).is_none());
    }
}
