mod assets;
#[cfg(debug_assertions)]
mod dev;
mod engine;
mod images;
mod live;
mod lyrics;
mod model;
mod prefs;
mod ui;
mod ytm;

use gpui::{App, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, point, px, size};

/// A dev build has no bundle, so the Dock gets the icon from here: a galaxy with play at its core.
/// An installed Drift.app carries the icon itself, and macOS draws it in the system's own shape.
#[cfg(target_os = "macos")]
fn show_icon() {
    use objc2::{AllocAnyThread, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::{NSBundle, NSData};

    const ICON: &[u8] = include_bytes!("../assets/icon.png");
    if NSBundle::mainBundle().bundleIdentifier().is_some() {
        return;
    }
    let Some(main_thread) = MainThreadMarker::new() else { return };
    let data = NSData::with_bytes(ICON);
    if let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) {
        unsafe { NSApplication::sharedApplication(main_thread).setApplicationIconImage(Some(&image)) };
    }
}

fn main() {
    Application::new().with_assets(assets::Assets).run(|cx: &mut App| {
        #[cfg(target_os = "macos")]
        show_icon();
        ui::init(cx);

        let bounds = Bounds::centered(None, size(px(1280.), px(840.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(960.), px(620.))),
            titlebar: Some(TitlebarOptions {
                title: Some("Drift".into()),
                appears_transparent: true,
                traffic_light_position: Some(point(px(16.), px(18.))),
            }),
            ..Default::default()
        };

        let window = cx.open_window(options, ui::Shell::build).expect("failed to open the window");
        let native = window.update(cx, |_, window, _| live::native_window(window)).ok().flatten();
        live::start(cx, native);
        cx.on_window_closed(|cx| cx.quit()).detach();
        // Dev runs driven through the control pipe should not steal focus.
        if std::env::var_os("DRIFT_DEV_FIFO").is_none() {
            cx.activate(true);
        }
    });
}
