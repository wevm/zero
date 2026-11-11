//! Who is logged in, where the servers are, and the client for them, as
//! zbugs-swift's `AppModel` keeps them.

use crate::transport::{GpuiScheduler, WebSocketFactory};
use gpui_kit::App;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};
use zbugs_model::args::ProjectArgs;
use zbugs_model::queries::{ALL_LABELS, ALL_PROJECTS, ALL_USERS, ISSUE_PRELOAD_V2};
use zbugs_model::{AuthData, ZERO_PROJECT_NAME, client_mutators, schema};
use zero_client::client::{Preload, Zero, ZeroOptions, ZeroStorage};
use zero_client::ttl::Ttl;

pub struct Session {
    pub zero: Rc<Zero<zbugs_model::Context>>,
    pub auth: Option<AuthData>,
    pub cache_url: String,
    pub api_url: String,
    pub token: Option<String>,
    _preloads: Vec<Preload>,
}

/// Milliseconds since 1970, as zbugs stamps `created` and `modified`.
pub fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_millis() as f64)
}

/// The app's own directory: its settings and its local store, apart from
/// zbugs-swift's. `ZBUGS_APP_DIR` moves it, as the end-to-end run does.
pub fn app_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("ZBUGS_APP_DIR") {
        let dir = PathBuf::from(dir);
        let _ = std::fs::create_dir_all(&dir);
        return dir;
    }
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from);
    let dir = if cfg!(target_os = "macos") {
        home.join("Library/Application Support/zbugs-gpui")
    } else {
        home.join(".local/share/zbugs-gpui")
    };
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// A saved setting, overridden by `env` for the launch (`ZBUGS_CACHE_URL`,
/// `ZBUGS_API_URL`, and `ZBUGS_TOKEN` for the token), as UI tests pass them.
fn setting(env: &str, file: &str) -> Option<String> {
    std::env::var(env)
        .ok()
        .or_else(|| std::fs::read_to_string(app_dir().join(file)).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// Keeps a setting for the next launch; `None` forgets it.
pub fn save_setting(file: &str, value: Option<&str>) {
    let path = app_dir().join(file);
    let _ = match value {
        Some(value) => std::fs::write(path, value),
        None => std::fs::remove_file(path),
    };
}

impl Session {
    /// The saved login and servers, or the environment's.
    pub fn load(cx: &App) -> Session {
        let cache_url = setting("ZBUGS_CACHE_URL", "cache-url")
            .unwrap_or_else(|| "http://127.0.0.1:4848".into());
        let api_url =
            setting("ZBUGS_API_URL", "api-url").unwrap_or_else(|| "http://127.0.0.1:5173".into());
        let token = match std::env::var("ZBUGS_TOKEN") {
            Ok(token) => Some(token.trim().to_owned()).filter(|t| !t.is_empty()),
            Err(_) => {
                let token = crate::token::load();
                // An expired one is gone for good, so it's deleted.
                if token
                    .as_deref()
                    .is_some_and(|t| AuthData::from_jwt(t, now_ms() / 1000.0).is_none())
                {
                    crate::token::save(None);
                    None
                } else {
                    token
                }
            }
        };
        Session::new(cache_url, api_url, token, cx)
    }

    /// A client for these servers and token. An expired token logs out.
    pub fn new(cache_url: String, api_url: String, token: Option<String>, cx: &App) -> Session {
        let auth = token
            .as_deref()
            .and_then(|t| AuthData::from_jwt(t, now_ms() / 1000.0));
        let token = auth.as_ref().and(token);
        let zero = Zero::new(
            schema::schema(),
            auth.clone(),
            client_mutators(),
            ZeroOptions {
                cache_url: Some(cache_url.clone()),
                user_id: auth.as_ref().map(|a| a.sub.clone()),
                auth: token.clone(),
                mutate_url: Some(format!("{api_url}/api/mutate")),
                query_url: Some(format!("{api_url}/api/query")),
                storage: ZeroStorage::Sqlite {
                    directory: Some(app_dir().join("store")),
                },
                ..ZeroOptions::default()
            },
            Rc::new(GpuiScheduler::new(
                cx.foreground_executor().clone(),
                cx.background_executor().clone(),
            )),
            Some(Rc::new(WebSocketFactory::new(
                cx.foreground_executor().clone(),
            ))),
        )
        .expect("zbugs' mutators are registered once each");

        // What zbugs preloads: the project's issues with their first
        // comments, and the users, labels, and projects.
        let ttl = Ttl::seconds(10.0);
        let project = ProjectArgs {
            project_name: ZERO_PROJECT_NAME.into(),
        };
        let a = auth.as_ref();
        let preloads = vec![
            zero.preload(ISSUE_PRELOAD_V2.call(&project, a).any(), &ttl),
            zero.preload(ALL_USERS.call(&(), a).any(), &ttl),
            zero.preload(ALL_LABELS.call(&(), a).any(), &ttl),
            zero.preload(ALL_PROJECTS.call(&(), a).any(), &ttl),
        ];
        Session {
            zero: Rc::new(zero),
            auth,
            cache_url,
            api_url,
            token,
            _preloads: preloads,
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.zero.flush_storage();
        self.zero.close();
    }
}
