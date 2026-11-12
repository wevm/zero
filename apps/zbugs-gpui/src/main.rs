//! zbugs on the Zero Rust client, in GPUI.

mod app;
mod emoji_picker;
mod http;
mod live;
mod login;
mod picker;
mod session;
mod token;
mod transport;

use gpui_kit::*;
use std::borrow::Cow;
use std::sync::Arc;

// The icons the app uses past GPUI Kit's default set.
gpui_kit::assets::icon_assets!(ExtraIcons, [Circle, SquarePen, Pencil, ListChecks, Trash]);

/// GPUI Kit's icons and `ExtraIcons`.
struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match ExtraIcons.load(path)? {
            Some(svg) => Ok(Some(svg)),
            None => gpui_kit::assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        Ok(paths)
    }
}

actions!(zbugs, [Quit, Hide, HideOthers, ShowAll]);

/// The macOS app menu, as zbugs-swift's and zbugs-rune's: Services, the
/// Hide items, and Quit with Cmd-Q. GPUI has no menu bar of its own, and
/// GPUI Kit leaves quitting to the app. Quitting drops the window, and with
/// it the session, which flushes the local store (`Session`'s `Drop`).
fn app_menu(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    // The menu shows these as the items' key equivalents.
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
    ]);
    // macOS titles the app menu with the bundle's name.
    cx.set_menus([Menu::new("ZBugs GPUI").items([
        MenuItem::os_submenu("Services", SystemMenuType::Services),
        MenuItem::separator(),
        MenuItem::action("Hide ZBugs GPUI", Hide),
        MenuItem::action("Hide Others", HideOthers),
        MenuItem::action("Show All", ShowAll),
        MenuItem::separator(),
        MenuItem::action("Quit ZBugs GPUI", Quit),
    ])]);
}

fn main() {
    gpui_kit::application()
        .with_assets(Assets)
        .with_http_client(Arc::new(http::UreqClient::new()))
        .run(|cx| {
            gpui_kit::init(cx);
            app::bind_keys(cx);
            app_menu(cx);
            let session = session::Session::load(cx);
            let bounds = Bounds::centered(None, size(px(1200.), px(800.)), cx);
            let (_window, _app) = gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    // The columns' toolbars take the title bar's place, as
                    // a macOS app's unified toolbar does, and move the
                    // window themselves (`ZbugsApp::toolbar`).
                    titlebar: Some(TitlebarOptions {
                        title: Some("Zero Bugs".into()),
                        appears_transparent: true,
                        traffic_light_position: Some(point(px(18.), px(18.))),
                    }),
                    app_owns_titlebar_drag: true,
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
