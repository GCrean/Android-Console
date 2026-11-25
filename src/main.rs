// Release builds on Windows are GUI apps: no console window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod adb;
mod settings;
mod term;
mod ui;

use gpui::{
    px, size, App, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions,
    prelude::*,
};

fn main() {
    Application::new().run(|cx: &mut App| {
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let bounds = Bounds::centered(None, size(px(1180.), px(740.)), cx);
        let opened = cx.open_window(
            WindowOptions {
                app_id: Some("android-console".into()),
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Android Console".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| {
                let view = cx.new(|cx| ui::ConsoleView::new(window, cx));
                // Kill the adb child before the window goes away.
                let v = view.clone();
                window.on_window_should_close(cx, move |_, cx| {
                    v.update(cx, |view, _| view.shutdown());
                    true
                });
                view
            },
        );
        if let Err(e) = opened {
            eprintln!("failed to open window: {e:#}");
            cx.quit();
            return;
        }
        cx.activate(true);
    });
}
