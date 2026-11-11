//! zbugs on the Zero Rust client, in GPUI.

mod app;
mod http;
mod live;
mod login;
mod session;
mod token;
mod transport;

use gpui_kit::*;
use std::sync::Arc;

fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .with_http_client(Arc::new(http::UreqClient::new()))
        .run(|cx| {
            gpui_kit::init(cx);
            app::bind_keys(cx);
            let session = session::Session::load(cx);
            let bounds = Bounds::centered(None, size(px(1200.), px(800.)), cx);
            let (_window, _app) = gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Zero Bugs".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                cx,
                |window, cx| cx.new(|cx| app::ZbugsApp::new(session, window, cx)),
            )
            .expect("the main window opens");
            cx.activate(true);
            #[cfg(feature = "e2e")]
            app::e2e::run(_window, _app, cx);
        });
}
