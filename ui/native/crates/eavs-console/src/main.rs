//! eavs-console: a native gpui desktop app built on the byteowlz design
//! system. `gpui_kit::application()` opens the platform, `gpui_kit::init()`
//! initializes the enabled layers, and `Root` is the first view in the window
//! (it owns Sheet/Dialog/Notification management).

mod app;
mod config;
mod theme;
mod ui;

use gpui_kit::component::Root;
use gpui_kit::{AppContext as _, WindowBounds, WindowOptions, px, size};

use crate::app::App;
use crate::config::Config;

fn main() {
    env_logger::try_init().ok();

    // Config is loaded (and a default written) before the window opens.
    let config = match config::load_or_create() {
        Ok(c) => c,
        Err(e) => {
            log::error!("load config: {e:#}");
            Config::default()
        }
    };

    gpui_kit::application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            theme::init(cx, &config);

            let width = config.window.width;
            let height = config.window.height;
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(width), px(height)), cx)),
                window_min_size: Some(size(px(640.), px(480.))),
                app_id: Some(env!("CARGO_PKG_NAME").into()),
                ..Default::default()
            };

            cx.spawn(async move |cx| {
                let opened = cx.open_window(options, |window, cx| {
                    let view = cx.new(|cx| App::new(config, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                });
                if let Err(e) = opened {
                    log::error!("open window: {e:#}");
                    cx.update(|cx| cx.quit());
                }
            })
            .detach();
        });
}
