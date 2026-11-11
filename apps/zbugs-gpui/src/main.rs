//! zbugs on the Zero Rust client, in GPUI.

mod app;
mod live;
mod session;
mod transport;

use gpui_kit::*;

fn main() {
    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            let session = session::Session::load(cx);
            let bounds = Bounds::centered(None, size(px(1200.), px(800.)), cx);
            gpui_kit::open_window(
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
        });
}
