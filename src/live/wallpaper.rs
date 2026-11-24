//! A living wallpaper under the desktop icons: every song becomes its own galaxy.
//! It only covers the real wallpaper while music plays and never changes system settings.
//! The same galaxy also fills the Drift window, under the interface, for as long as a song is loaded.

#[cfg(debug_assertions)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::{
    ffi::c_void,
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, Sender, channel},
    },
    thread,
    time::{Duration, Instant},
};

use objc2::{
    class, msg_send,
    rc::Retained,
    runtime::{AnyObject, Bool},
};
use objc2_foundation::{NSPoint, NSRect, NSString};

pub use super::galaxy::{COVER_SIZE, Cover};
#[cfg(debug_assertions)]
use super::pace::Pace;
use super::{
    Bus,
    galaxy::{ClipPicture, Galaxy, Screen, View},
    glyphs::GlyphWorker,
    story::{FADE, Input, Story},
};
use crate::lyrics::TimedLine;

const RENDER_SCALE: f64 = 1.5;
const FRAME_TIME: Duration = Duration::from_micros(16_666);
/// A paused song keeps its galaxy in the window, drawn at half the rate.
const PAUSED_FRAME_TIME: Duration = Duration::from_micros(33_333);
const IDLE_SLEEP: Duration = Duration::from_millis(150);
const DESKTOP_LEVEL_KEY: i32 = 2;
const BEHAVIOR: usize = 1 | (1 << 4) | (1 << 6);
const OCCLUSION_VISIBLE: usize = 1 << 1;
const SCREEN_CHECK: Duration = Duration::from_millis(500);
/// How far the scene turns toward the cursor, in radians.
const LOOK_RANGE: (f32, f32) = (0.1, 0.07);
const LOOK_SPEED: f32 = 2.4;
/// Inside the window the galaxy follows the cursor more gently, so it never pulls at the reading eye.
const WINDOW_LOOK: f32 = 0.45;
/// Highlights on an HDR screen stop here, so the core glows without blinding.
const PEAK_BRIGHTNESS: f32 = 2.6;
/// Below this a galaxy counts as gone, and its surface only needs one last clear.
const LIT: f32 = 0.003;
/// NSActivityOptions bits.
const ACTIVITY_USER_INITIATED: u64 = 0x00FF_FFFF;
const ACTIVITY_IDLE_SYSTEM_SLEEP_DISABLED: u64 = 1 << 20;
const ACTIVITY_LATENCY_CRITICAL: u64 = 0xFF_0000_0000;
/// While a galaxy is drawn, macOS must neither nap Drift nor bunch up its timers, or frames come
/// unevenly whenever another app is in front. The Mac itself may still fall asleep.
const DRAWING_ACTIVITY: u64 =
    (ACTIVITY_USER_INITIATED & !ACTIVITY_IDLE_SYSTEM_SLEEP_DISABLED) | ACTIVITY_LATENCY_CRITICAL;
/// The activity outlasts the last frame a little, so fade-outs and track changes stay smooth too.
const ACTIVITY_LINGER: Duration = Duration::from_secs(4);
/// QOS_CLASS_USER_INTERACTIVE: the render thread animates what is on screen, like the main thread does.
const QOS_CLASS_USER_INTERACTIVE: u32 = 0x21;

/// Debug control pipe: "activity off" lets frame pacing be compared with and without the activity.
#[cfg(debug_assertions)]
pub static DEV_NO_ACTIVITY: AtomicBool = AtomicBool::new(false);

/// Tells macOS that Drift is drawing in time with music. The token keeps it so until it is ended.
fn begin_drawing_activity() -> Option<Retained<AnyObject>> {
    unsafe {
        let info: *mut AnyObject = msg_send![class!(NSProcessInfo), processInfo];
        let reason = NSString::from_str("Drift draws the galaxy of the song that is playing");
        let token: *mut AnyObject = msg_send![info, beginActivityWithOptions: DRAWING_ACTIVITY, reason: &*reason];
        Retained::retain(token)
    }
}

fn end_drawing_activity(token: Retained<AnyObject>) {
    unsafe {
        let info: *mut AnyObject = msg_send![class!(NSProcessInfo), processInfo];
        let _: () = msg_send![info, endActivity: &*token];
    }
}

/// Colors of the clip are taken from every this many of its pictures.
const CLIP_COLOR_EVERY: u32 = 6;
/// A clip whose frames stopped this long ago gives the stage back to the galaxy. A paused one keeps it.
const CLIP_PATIENCE: Duration = Duration::from_millis(1500);
const VIEW_WIDTH_SIZABLE: usize = 2;
const VIEW_HEIGHT_SIZABLE: usize = 16;
const ORDER_BELOW: isize = -1;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGWindowLevelForKey(key: i32) -> i32;
}

unsafe extern "C" {
    fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
}

fn approach(current: f32, target: f32, speed: f32, dt: f32) -> f32 {
    current + (target - current) * (1. - (-speed * dt).exp())
}

/// What is playing right now, as the main thread sees it.
#[derive(Clone, Debug, Default)]
pub struct Playback {
    /// The desktop shows the galaxy: music plays and the wallpaper is switched on.
    pub active: bool,
    pub playing: bool,
    /// A song is loaded, even if paused. The window keeps its galaxy for it.
    pub loaded: bool,
    /// The galaxy under the interface is switched on.
    pub window: bool,
    pub track: String,
    pub position: f64,
    pub duration: f64,
    pub chorus: bool,
    /// The song's clip is playing and Drift turns it into stars.
    pub clip: bool,
}

#[derive(Clone, Copy)]
struct ScreenState {
    visible: bool,
    look: (f32, f32),
    headroom: f32,
}

#[derive(Clone, Copy)]
struct WindowState {
    visible: bool,
    look: (f32, f32),
}

#[derive(Default)]
struct Shared {
    playback: Playback,
    position_at: Option<Instant>,
    cover: Option<Cover>,
    lyrics: Option<(String, Vec<TimedLine>)>,
    screens: Vec<ScreenState>,
    window: Option<WindowState>,
    /// The newest picture of the clip, unpacked, and the colors of its scene when they were just taken.
    clip_picture: Option<ClipPicture>,
    clip_palette: Option<super::palette::Palette>,
}

/// Hands the clip's pictures, as JPEG, to the thread that unpacks them. Lives in the callback that receives
/// them from the page.
#[derive(Clone)]
pub struct ClipSink(Sender<Vec<u8>>);

impl ClipSink {
    pub fn push(&self, jpeg: Vec<u8>) {
        let _ = self.0.send(jpeg);
    }
}

/// Unpacks the clip's pictures and measures them away from the render thread, which then only uploads them.
/// When pictures pile up, only the newest is unpacked.
fn unpack_pictures(jpegs: &Receiver<Vec<u8>>, shared: &Mutex<Shared>) {
    let mut unpacked = 0u32;
    while let Ok(mut jpeg) = jpegs.recv() {
        while let Ok(newer) = jpegs.try_recv() {
            jpeg = newer;
        }
        let Some(picture) = ClipPicture::from_jpeg(&jpeg) else { continue };
        unpacked += 1;
        // Colors come from every few pictures: often enough to follow the scenes, rarely enough to stay calm.
        // A scene without vivid colors keeps the colors of the last one.
        let colors = unpacked
            .is_multiple_of(CLIP_COLOR_EVERY)
            .then(|| picture.palette())
            .filter(|found| *found != super::palette::Palette::default());
        if let Some(found) = colors {
            super::found_clip_colors(found);
        }
        let Ok(mut shared) = shared.lock() else { return };
        shared.clip_picture = Some(picture);
        if colors.is_some() {
            shared.clip_palette = colors;
        }
    }
}

struct Layer {
    pointer: *mut c_void,
    width: u32,
    height: u32,
}

// The layer is only drawn to from the render thread after AppKit finished creating it.
unsafe impl Send for Layer {}

/// The layer under the interface. The window can be resized, so its size is read every frame.
struct WindowLayer {
    pointer: *mut c_void,
    scale: f64,
}

unsafe impl Send for WindowLayer {}

impl WindowLayer {
    fn size(&self) -> (u32, u32) {
        // Core Animation allows reading a layer's geometry from any thread.
        let bounds: NSRect = unsafe { msg_send![self.pointer.cast::<AnyObject>(), bounds] };
        let side = |points: f64| ((points * self.scale).round() as u32).max(1);
        (side(bounds.size.width), side(bounds.size.height))
    }
}

struct Output {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    view: View,
    look: (f32, f32),
    headroom: f32,
}

struct WindowOutput {
    output: Output,
    layer: WindowLayer,
    fade: f32,
    cleared: bool,
}

impl WindowOutput {
    fn follow_size(&mut self, galaxy: &Galaxy) {
        let (width, height) = self.layer.size();
        let config = &mut self.output.config;
        if (width, height) == (config.width, config.height) {
            return;
        }
        config.width = width;
        config.height = height;
        self.output.surface.configure(&galaxy.device, config);
        self.output.view = galaxy.view(width, height);
        self.cleared = false;
    }
}

pub struct Wallpaper {
    shared: Arc<Mutex<Shared>>,
    windows: Vec<(*mut AnyObject, NSRect)>,
    /// Held while a galaxy is being drawn, so frames keep an even pace when Drift is in the background.
    activity: Option<Retained<AnyObject>>,
    last_drawing: Instant,
    /// The Drift window, whose galaxy sits under the interface.
    main: Option<*mut AnyObject>,
    /// Where the clip's pictures go to be unpacked.
    clip_jpegs: Sender<Vec<u8>>,
    last_screen_check: Instant,
    #[cfg(debug_assertions)]
    last_headroom: f64,
}

unsafe fn create_window(frame: NSRect) -> (*mut AnyObject, Layer) {
    unsafe {
        let window: *mut AnyObject = msg_send![class!(NSWindow), alloc];
        let window: *mut AnyObject = msg_send![
            window,
            initWithContentRect: frame,
            styleMask: 0usize,
            backing: 2usize,
            defer: Bool::NO
        ];
        let clear: *mut AnyObject = msg_send![class!(NSColor), clearColor];
        let level = CGWindowLevelForKey(DESKTOP_LEVEL_KEY) as isize + 1;
        let _: () = msg_send![window, setReleasedWhenClosed: Bool::NO];
        let _: () = msg_send![window, setBackgroundColor: clear];
        let _: () = msg_send![window, setOpaque: Bool::NO];
        let _: () = msg_send![window, setHasShadow: Bool::NO];
        let _: () = msg_send![window, setIgnoresMouseEvents: Bool::YES];
        let _: () = msg_send![window, setLevel: level];
        let _: () = msg_send![window, setCollectionBehavior: BEHAVIOR];

        let scale: f64 = msg_send![window, backingScaleFactor];
        let render_scale = scale.min(RENDER_SCALE);
        let layer: *mut AnyObject = msg_send![class!(CAMetalLayer), new];
        let _: () = msg_send![layer, setOpaque: Bool::NO];
        let _: () = msg_send![layer, setContentsScale: render_scale];
        let view: *mut AnyObject = msg_send![window, contentView];
        let _: () = msg_send![view, setLayer: layer];
        let _: () = msg_send![view, setWantsLayer: Bool::YES];
        let _: () = msg_send![window, orderBack: std::ptr::null_mut::<AnyObject>()];
        let target = Layer {
            pointer: layer.cast(),
            width: (frame.size.width * render_scale) as u32,
            height: (frame.size.height * render_scale) as u32,
        };
        (window, target)
    }
}

/// Puts a galaxy layer at the very bottom of the window's content, under the GPUI view,
/// the same way GPUI itself places its blur. Where the interface is see-through, the galaxy shows.
unsafe fn create_window_layer(window: *mut AnyObject) -> Option<WindowLayer> {
    unsafe {
        let content: *mut AnyObject = msg_send![window, contentView];
        if content.is_null() {
            return None;
        }
        let bounds: NSRect = msg_send![content, bounds];
        let view: *mut AnyObject = msg_send![class!(NSView), alloc];
        let view: *mut AnyObject = msg_send![view, initWithFrame: bounds];
        let scale: f64 = msg_send![window, backingScaleFactor];
        let render_scale = scale.min(RENDER_SCALE);
        let layer: *mut AnyObject = msg_send![class!(CAMetalLayer), new];
        let _: () = msg_send![layer, setOpaque: Bool::NO];
        let _: () = msg_send![layer, setContentsScale: render_scale];
        let _: () = msg_send![view, setLayer: layer];
        let _: () = msg_send![view, setWantsLayer: Bool::YES];
        let _: () = msg_send![view, setAutoresizingMask: VIEW_WIDTH_SIZABLE | VIEW_HEIGHT_SIZABLE];
        let _: () = msg_send![content, addSubview: view, positioned: ORDER_BELOW, relativeTo: std::ptr::null_mut::<AnyObject>()];
        Some(WindowLayer { pointer: layer.cast(), scale: render_scale })
    }
}

fn surface_for(instance: &wgpu::Instance, pointer: *mut c_void) -> Result<wgpu::Surface<'static>, String> {
    unsafe { instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::CoreAnimationLayer(pointer)) }
        .map_err(|error| error.to_string())
}

fn render_loop(
    layers: Vec<Layer>,
    window_layer: Option<WindowLayer>,
    bus: Option<Bus>,
    shared: Arc<Mutex<Shared>>,
) -> Result<(), String> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let mut surfaces = Vec::new();
    for layer in &layers {
        surfaces.push(surface_for(&instance, layer.pointer)?);
    }
    let window_surface = match &window_layer {
        Some(layer) => Some(surface_for(&instance, layer.pointer)?),
        None => None,
    };
    let compatible = surfaces.first().or(window_surface.as_ref()).ok_or("no screens")?;
    let adapter = futures::executor::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        force_fallback_adapter: false,
        compatible_surface: Some(compatible),
        apply_limit_buckets: false,
    }))
    .map_err(|error| error.to_string())?;
    let (device, queue) = futures::executor::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
        .map_err(|error| error.to_string())?;
    let capabilities = compatible.get_capabilities(&adapter);
    // Floating point output in extended linear light is what lets the MacBook show HDR.
    let (format, color_space) = if capabilities.formats.contains(&wgpu::TextureFormat::Rgba16Float) {
        (wgpu::TextureFormat::Rgba16Float, wgpu::SurfaceColorSpace::ExtendedSrgbLinear)
    } else {
        let format =
            capabilities.formats.iter().copied().find(wgpu::TextureFormat::is_srgb).ok_or("no surface format")?;
        (format, wgpu::SurfaceColorSpace::Auto)
    };
    // Anything but an opaque layer, or the desktop would turn black when the music pauses.
    let alpha_mode = [wgpu::CompositeAlphaMode::PreMultiplied, wgpu::CompositeAlphaMode::PostMultiplied]
        .into_iter()
        .find(|mode| capabilities.alpha_modes.contains(mode))
        .ok_or("no transparent surface mode")?;
    let config_for = |width: u32, height: u32| wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        color_space,
        width,
        height,
        present_mode: wgpu::PresentMode::AutoNoVsync,
        desired_maximum_frame_latency: 2,
        alpha_mode,
        view_formats: vec![],
    };

    let mut galaxy = Galaxy::new(device, queue, format);
    let mut outputs = Vec::new();
    for (surface, layer) in surfaces.into_iter().zip(&layers) {
        let (width, height) = (layer.width.max(1), layer.height.max(1));
        let config = config_for(width, height);
        surface.configure(&galaxy.device, &config);
        let view = galaxy.view(width, height);
        outputs.push(Output { surface, config, view, look: (0., 0.), headroom: 1. });
    }
    let mut window_output = window_surface.zip(window_layer).map(|(surface, layer)| {
        let (width, height) = layer.size();
        let config = config_for(width, height);
        surface.configure(&galaxy.device, &config);
        let view = galaxy.view(width, height);
        WindowOutput {
            output: Output { surface, config, view, look: (0., 0.), headroom: 1. },
            layer,
            fade: 0.,
            cleared: false,
        }
    });

    let mut story = Story::new();
    let glyphs = GlyphWorker::start();
    let mut track = String::new();
    let mut pending_cover: Option<(Cover, Instant)> = None;
    let mut last = Instant::now();
    let mut next_frame = last;
    let mut desktop_fade = 0f32;
    let mut desktop_cleared = false;
    let mut last_clip_frame: Option<Instant> = None;
    #[cfg(debug_assertions)]
    let mut pace = Pace::new("galaxy");
    loop {
        let started = Instant::now();
        let dt = started.duration_since(last).as_secs_f32().min(0.1);
        last = started;

        let (playback, position_age, screens, window_state, new_cover, new_lyrics, clip_picture, clip_palette) = {
            let Ok(mut state) = shared.lock() else { return Ok(()) };
            let age = state.position_at.map_or(0., |at| at.elapsed().as_secs_f64());
            (
                state.playback.clone(),
                age,
                state.screens.clone(),
                state.window,
                state.cover.take(),
                state.lyrics.take(),
                state.clip_picture.take(),
                state.clip_palette.take(),
            )
        };
        if !playback.track.is_empty() && playback.track != track {
            track.clone_from(&playback.track);
            story.new_track(&track);
            last_clip_frame = None;
        }
        if playback.clip {
            if let Some(found) = clip_palette {
                galaxy.set_clip_palette(&found);
            }
            if let Some(picture) = clip_picture {
                galaxy.set_clip_picture(&picture);
                last_clip_frame = Some(started);
            }
        }
        if let Some((id, lines)) = new_lyrics
            && id == track
        {
            story.set_lines(lines);
        }
        if let Some(cover) = new_cover {
            pending_cover = Some((cover, started));
        }
        if pending_cover.as_ref().is_some_and(|(_, at)| story.can_swap_cover(at.elapsed().as_secs_f32()))
            && let Some((cover, _)) = pending_cover.take()
        {
            galaxy.set_cover(&cover);
            story.cover_swapped();
        }

        let desktop_on = playback.active;
        let window_visible = window_state.is_some_and(|state| state.visible);
        let window_on = window_output.is_some() && playback.window && playback.loaded && window_visible;
        let frame = bus.as_ref().and_then(Bus::fresh);
        let position = playback.position + if playback.playing { position_age } else { 0. };
        let clip_live =
            playback.clip && last_clip_frame.is_some_and(|at| !playback.playing || at.elapsed() < CLIP_PATIENCE);
        story.advance(
            dt,
            &Input {
                active: desktop_on || window_on,
                position,
                duration: playback.duration,
                level: frame.map_or(0., |frame| frame.level),
                chorus: playback.chorus,
                clip: clip_live,
            },
        );
        if !clip_live {
            galaxy.end_clip_colors();
        }
        if let Some(request) = story.take_request() {
            glyphs.request(request);
        }
        while let Some(reply) = glyphs.poll() {
            story.glyphs_ready(reply.generation, reply.words);
        }
        for (slot, points) in story.take_uploads() {
            galaxy.write_word(slot, &points);
        }
        galaxy.advance(dt, if playback.playing { frame.map_or(0., |frame| frame.voice) } else { 0. });

        desktop_fade = approach(desktop_fade, f32::from(u8::from(desktop_on)), FADE, dt);
        for (output, screen) in outputs.iter_mut().zip(&screens) {
            output.look.0 = approach(output.look.0, screen.look.0, LOOK_SPEED, dt);
            output.look.1 = approach(output.look.1, screen.look.1, LOOK_SPEED, dt);
            output.headroom = approach(output.headroom, screen.headroom.clamp(1., PEAK_BRIGHTNESS), 2., dt);
        }
        if let Some(window) = &mut window_output {
            window.fade = approach(window.fade, f32::from(u8::from(window_on)), FADE, dt);
            if let Some(state) = window_state {
                window.output.look.0 = approach(window.output.look.0, state.look.0, LOOK_SPEED, dt);
                window.output.look.1 = approach(window.output.look.1, state.look.1, LOOK_SPEED, dt);
            }
            window.follow_size(&galaxy);
        }

        let desktop_visible = screens.is_empty() || screens.iter().any(|screen| screen.visible);
        let desktop_lit = desktop_fade > LIT;
        let desktop_needed = !outputs.is_empty() && desktop_visible && (desktop_lit || !desktop_cleared);
        let window_lit = window_output.as_ref().is_some_and(|window| window.fade > LIT);
        let window_needed =
            window_visible && window_output.as_ref().is_some_and(|window| window_lit || !window.cleared);
        if !desktop_needed && !window_needed {
            #[cfg(debug_assertions)]
            pace.rest();
            thread::sleep(IDLE_SLEEP);
            next_frame = Instant::now();
            continue;
        }

        let pose = *story.pose();
        let mut encoder = galaxy.device.create_command_encoder(&Default::default());
        if (desktop_lit && desktop_visible) || window_lit {
            galaxy.build(&mut encoder, &pose);
        }
        let mut presenting = Vec::new();
        if desktop_needed {
            let mut lit = pose;
            lit.fade = desktop_fade;
            for (index, output) in outputs.iter().enumerate() {
                // A screen fully covered by windows costs nothing.
                if desktop_lit && screens.get(index).is_some_and(|screen| !screen.visible) {
                    continue;
                }
                let Some(texture) = next_texture(output, &galaxy) else { continue };
                let target = texture.texture.create_view(&Default::default());
                let screen = Screen { look: output.look, headroom: output.headroom };
                galaxy.draw(&mut encoder, &output.view, &target, &lit, screen);
                presenting.push(texture);
            }
            desktop_cleared = !desktop_lit;
        }
        if window_needed && let Some(window) = &mut window_output {
            let mut lit = pose;
            lit.fade = window.fade;
            if let Some(texture) = next_texture(&window.output, &galaxy) {
                let target = texture.texture.create_view(&Default::default());
                // No HDR boost here: the interface on top is plain white and must stay the brightest thing.
                let screen = Screen { look: window.output.look, headroom: 1. };
                galaxy.draw(&mut encoder, &window.output.view, &target, &lit, screen);
                presenting.push(texture);
                window.cleared = !window_lit;
            }
        }
        galaxy.queue.submit([encoder.finish()]);
        for texture in presenting {
            galaxy.queue.present(texture);
        }
        let spacing = if playback.playing || desktop_lit { FRAME_TIME } else { PAUSED_FRAME_TIME };
        #[cfg(debug_assertions)]
        pace.frame(spacing);

        // Steady frame spacing keeps slow motion free of micro-stutter.
        next_frame += spacing;
        let now = Instant::now();
        if next_frame > now {
            thread::sleep(next_frame - now);
        } else {
            next_frame = now;
        }
    }
}

fn next_texture(output: &Output, galaxy: &Galaxy) -> Option<wgpu::SurfaceTexture> {
    match output.surface.get_current_texture() {
        wgpu::CurrentSurfaceTexture::Success(texture) | wgpu::CurrentSurfaceTexture::Suboptimal(texture) => {
            Some(texture)
        }
        wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
            output.surface.configure(&galaxy.device, &output.config);
            None
        }
        _ => None,
    }
}

impl Drop for Wallpaper {
    fn drop(&mut self) {
        if let Some(token) = self.activity.take() {
            end_drawing_activity(token);
        }
    }
}

impl Wallpaper {
    /// `main` is the Drift window. Its galaxy lives under the interface.
    pub fn start(bus: Option<Bus>, main: Option<*mut AnyObject>) -> Option<Self> {
        let mut windows = Vec::new();
        let mut layers = Vec::new();
        let window_layer = unsafe {
            let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
            let count: usize = msg_send![screens, count];
            for index in 0..count {
                let screen: *mut AnyObject = msg_send![screens, objectAtIndex: index];
                let frame: NSRect = msg_send![screen, frame];
                let (window, layer) = create_window(frame);
                windows.push((window, frame));
                layers.push(layer);
            }
            main.and_then(|window| create_window_layer(window))
        };
        if windows.is_empty() && window_layer.is_none() {
            return None;
        }
        let shared = Arc::new(Mutex::new(Shared {
            screens: vec![ScreenState { visible: true, look: (0., 0.), headroom: 1. }; windows.len()],
            window: window_layer.as_ref().map(|_| WindowState { visible: true, look: (0., 0.) }),
            ..Default::default()
        }));
        let main = main.filter(|_| window_layer.is_some());
        let render_shared = shared.clone();
        thread::Builder::new()
            .name("drift-wallpaper".into())
            .spawn(move || {
                // Without this a frame waits on timers that macOS may push back by several milliseconds.
                unsafe { pthread_set_qos_class_self_np(QOS_CLASS_USER_INTERACTIVE, 0) };
                if let Err(error) = render_loop(layers, window_layer, bus, render_shared) {
                    eprintln!("drift: wallpaper stopped: {error}");
                }
            })
            .ok()?;
        let (clip_jpegs, incoming) = channel();
        let unpack_shared = shared.clone();
        thread::Builder::new()
            .name("drift-clip".into())
            .spawn(move || unpack_pictures(&incoming, &unpack_shared))
            .ok()?;
        Some(Self {
            shared,
            windows,
            activity: None,
            last_drawing: Instant::now(),
            main,
            clip_jpegs,
            last_screen_check: Instant::now(),
            #[cfg(debug_assertions)]
            last_headroom: 0.,
        })
    }

    pub fn update(&mut self, playback: Playback) {
        let mouse: NSPoint = unsafe { msg_send![class!(NSEvent), mouseLocation] };
        let check_screens = self.last_screen_check.elapsed() >= SCREEN_CHECK;
        if check_screens {
            self.last_screen_check = Instant::now();
        }
        let Ok(mut guard) = self.shared.lock() else { return };
        let shared = &mut *guard;
        let (desktop_drawn, window_wanted) = (playback.active, playback.window && playback.loaded);
        let moved = (playback.position - shared.playback.position).abs() > 0.0005;
        if moved || playback.track != shared.playback.track || playback.playing != shared.playback.playing {
            shared.position_at = Some(Instant::now());
        }
        shared.playback = playback;
        let toward = |frame: NSRect, range: (f32, f32)| {
            let across = ((mouse.x - frame.origin.x) / frame.size.width * 2. - 1.).clamp(-1., 1.) as f32;
            let up = ((mouse.y - frame.origin.y) / frame.size.height * 2. - 1.).clamp(-1., 1.) as f32;
            (across * range.0, -up * range.1)
        };
        if let (Some(window), Some(state)) = (self.main, shared.window.as_mut()) {
            unsafe {
                let frame: NSRect = msg_send![window, frame];
                state.look = toward(frame, (LOOK_RANGE.0 * WINDOW_LOOK, LOOK_RANGE.1 * WINDOW_LOOK));
                // Asked on every tick: an uncovered window gets its galaxy back with the very next frame.
                let occlusion: usize = msg_send![window, occlusionState];
                let minimized: Bool = msg_send![window, isMiniaturized];
                state.visible = occlusion & OCCLUSION_VISIBLE != 0 && !minimized.as_bool();
            }
        }
        for (state, (window, frame)) in shared.screens.iter_mut().zip(&self.windows) {
            state.look = toward(*frame, LOOK_RANGE);
            if check_screens {
                unsafe {
                    let occlusion: usize = msg_send![*window, occlusionState];
                    state.visible = occlusion & OCCLUSION_VISIBLE != 0;
                    let screen: *mut AnyObject = msg_send![*window, screen];
                    if !screen.is_null() {
                        // Asking for more than the current value is what makes macOS raise it.
                        let possible: f64 = msg_send![screen, maximumPotentialExtendedDynamicRangeColorComponentValue];
                        state.headroom = possible as f32;
                        #[cfg(debug_assertions)]
                        {
                            let current: f64 = msg_send![screen, maximumExtendedDynamicRangeColorComponentValue];
                            if std::env::var_os("DRIFT_DEV_FIFO").is_some()
                                && possible > 1.
                                && (current - self.last_headroom).abs() > 0.1
                            {
                                self.last_headroom = current;
                                eprintln!("wallpaper: HDR headroom {current:.2} of {possible:.1}");
                            }
                        }
                    }
                }
            }
        }
        let window_drawn = window_wanted && shared.window.is_some_and(|window| window.visible);
        drop(guard);
        self.keep_pace(desktop_drawn || window_drawn);
    }

    /// Holds the activity while a galaxy is on screen, and a little after.
    fn keep_pace(&mut self, drawing: bool) {
        #[cfg(debug_assertions)]
        if DEV_NO_ACTIVITY.load(Ordering::Relaxed) {
            if let Some(token) = self.activity.take() {
                end_drawing_activity(token);
            }
            return;
        }
        if drawing {
            self.last_drawing = Instant::now();
            if self.activity.is_none() {
                self.activity = begin_drawing_activity();
            }
        } else if self.last_drawing.elapsed() >= ACTIVITY_LINGER
            && let Some(token) = self.activity.take()
        {
            end_drawing_activity(token);
        }
    }

    pub fn set_cover(&self, cover: Cover) {
        if let Ok(mut shared) = self.shared.lock() {
            shared.cover = Some(cover);
        }
    }

    /// Where the page's clip frames go.
    pub fn clip_sink(&self) -> ClipSink {
        ClipSink(self.clip_jpegs.clone())
    }

    pub fn set_lyrics(&self, track: String, lines: Vec<TimedLine>) {
        if let Ok(mut shared) = self.shared.lock() {
            shared.lyrics = Some((track, lines));
        }
    }
}
