//! The login token, in the system's credential store, as zbugs-swift keeps
//! it in the Keychain: Keychain Services on macOS, Credential Manager on
//! Windows, and the Secret Service elsewhere. Where none is available, a
//! file only the user can read, in the app's directory.

use crate::session::app_dir;
use keyring::{Entry, Error};
use std::path::PathBuf;

const SERVICE: &str = "dev.rocicorp.zbugs-gpui";
const ACCOUNT: &str = "jwt";

/// The fallback file, and where the app kept the token before the
/// credential store.
fn file() -> PathBuf {
    app_dir().join("token")
}

fn entry() -> Option<Entry> {
    Entry::new(SERVICE, ACCOUNT).ok()
}

/// The saved token. One left in the file while the store works moves into
/// the store.
pub fn load() -> Option<String> {
    let from_file = || {
        std::fs::read_to_string(file())
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    };
    match entry().map(|e| e.get_password()) {
        Some(Ok(token)) => Some(token),
        Some(Err(Error::NoEntry)) => {
            let token = from_file()?;
            save(Some(&token));
            Some(token)
        }
        _ => from_file(),
    }
}

/// Saves the token for the next launch, or forgets it with `None`.
pub fn save(token: Option<&str>) {
    let entry = entry();
    match token {
        Some(token) => {
            if entry.is_some_and(|e| e.set_password(token).is_ok()) {
                let _ = std::fs::remove_file(file());
            } else {
                write_private(token);
            }
        }
        None => {
            if let Some(entry) = entry {
                let _ = entry.delete_credential();
            }
            let _ = std::fs::remove_file(file());
        }
    }
}

/// Writes the fallback file readable by the user alone.
fn write_private(token: &str) {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        options.mode(0o600);
        // An older file keeps its mode through `open`.
        let _ = std::fs::set_permissions(file(), std::fs::Permissions::from_mode(0o600));
    }
    if let Ok(mut f) = options.open(file()) {
        let _ = f.write_all(token.as_bytes());
    }
}
