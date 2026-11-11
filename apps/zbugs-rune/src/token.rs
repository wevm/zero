//! The login token, kept apart from the settings: in the Keychain on macOS
//! (the Credential Manager on Windows, the Secret Service elsewhere), like
//! zbugs-swift. Where no store is available, such as a Linux box without a
//! Secret Service, or with `ZBUGS_RUNE_DIR` set for a test, it's a file
//! readable only by the user.

use std::path::PathBuf;

use keyring::Entry;

use crate::settings;

const SERVICE: &str = "dev.rocicorp.zbugs-rune";
const ACCOUNT: &str = "jwt";

/// The saved token, if any: the store's, else the file's, which a launch
/// without the store (a locked Keychain) may have left, and moves to the
/// store once there is one.
pub fn load() -> Option<String> {
    let entry = entry();
    if let Some(t) = entry.as_ref().and_then(|e| e.get_password().ok()) {
        return Some(t);
    }
    let t = std::fs::read_to_string(file()).ok()?.trim().to_owned();
    if entry.is_some() {
        save(&t);
    }
    Some(t)
}

pub fn save(token: &str) {
    if let Some(entry) = entry()
        && entry.set_password(token).is_ok()
    {
        let _ = std::fs::remove_file(file());
        return;
    }
    let path = file();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = settings::write_private(&path, token);
}

pub fn delete() {
    if let Some(entry) = entry() {
        let _ = entry.delete_credential();
    }
    let _ = std::fs::remove_file(file());
}

/// The store's entry, unless `ZBUGS_RUNE_DIR` keeps everything in a
/// directory of its own.
fn entry() -> Option<Entry> {
    if std::env::var_os("ZBUGS_RUNE_DIR").is_some() {
        return None;
    }
    Entry::new(SERVICE, ACCOUNT).ok()
}

fn file() -> PathBuf {
    settings::dir().join("token")
}
