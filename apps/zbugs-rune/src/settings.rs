//! What the app keeps across launches: the servers, the login, and where
//! the list was. A JSON file in the app's support directory, with the token
//! kept apart in the platform's credential store (`token.rs`).

use std::path::PathBuf;

use zero_client::json::{self, Object, Value};

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub cache_url: String,
    pub api_url: String,
    /// Loaded from and saved to `token.rs`, not the file.
    pub token: Option<String>,
    /// The selected issue's id.
    pub selection: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            cache_url: "http://127.0.0.1:4848".into(),
            api_url: "http://127.0.0.1:5173".into(),
            token: None,
            selection: None,
        }
    }
}

impl Settings {
    /// The saved settings, with `ZBUGS_CACHE_URL`, `ZBUGS_API_URL`, and
    /// `ZBUGS_TOKEN` from the environment over them, for tests: those last
    /// only for the launch, as `save` leaves them out.
    pub fn load() -> (Settings, Settings) {
        let json = std::fs::read_to_string(path())
            .ok()
            .and_then(|t| json::parse(&t).ok());
        let mut saved = json.as_ref().map(Settings::from_json).unwrap_or_default();
        // Earlier versions kept the token in the file: it moves to the store.
        let legacy = json
            .as_ref()
            .and_then(|v| v.get("token"))
            .and_then(Value::as_str);
        if let Some(t) = legacy {
            crate::token::save(t);
            saved.save();
        }
        saved.token = crate::token::load();
        let mut current = saved.clone();
        if let Ok(v) = std::env::var("ZBUGS_CACHE_URL") {
            current.cache_url = v;
        }
        if let Ok(v) = std::env::var("ZBUGS_API_URL") {
            current.api_url = v;
        }
        if let Ok(v) = std::env::var("ZBUGS_TOKEN") {
            current.token = Some(v);
        }
        (current, saved)
    }

    pub fn save(&self) {
        let path = path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let text = json::stringify(&self.to_json());
        let tmp = path.with_extension("json.tmp");
        if write_private(&tmp, &text).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    fn from_json(v: &Value) -> Settings {
        let d = Settings::default();
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
        Settings {
            cache_url: s("cacheURL").unwrap_or(d.cache_url),
            api_url: s("apiURL").unwrap_or(d.api_url),
            token: None,
            selection: s("selection"),
        }
    }

    fn to_json(&self) -> Value {
        let mut o = Object::new();
        o.insert("cacheURL", Value::from(self.cache_url.as_str()));
        o.insert("apiURL", Value::from(self.api_url.as_str()));
        if let Some(id) = &self.selection {
            o.insert("selection", Value::from(id.as_str()));
        }
        Value::Object(o)
    }
}

/// `ZBUGS_RUNE_DIR`, or the platform's place for app data.
pub fn dir() -> PathBuf {
    if let Some(d) = std::env::var_os("ZBUGS_RUNE_DIR") {
        return d.into();
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/zbugs-rune")
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"))
            .join("zbugs-rune")
    }
}

fn path() -> PathBuf {
    dir().join("settings.json")
}

#[cfg(unix)]
pub fn write_private(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(text.as_bytes())
}

#[cfg(not(unix))]
pub fn write_private(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    std::fs::write(path, text)
}
