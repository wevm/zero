//! zbugs on the Zero Rust client, for the Rust zbugs apps
//! (`apps/zbugs-gpui`, `apps/zbugs-rune`): the schema `zero-rust codegen`
//! generates from `apps/zbugs/shared/schema.ts`, and ports of its queries
//! (`queries.ts`), mutators (`mutators.ts`), and auth checks (`auth.ts`).
//!
//! The drift test (`tests/drift.rs`) checks the ports against what the TS
//! definitions do, with the vectors zbugs-swift's drift test reads too.
//! It has no UI and no transport: each app brings its own.

pub mod args;
pub mod auth;
pub mod mutators;
pub mod queries;
#[rustfmt::skip]
pub mod schema;

pub use auth::{AuthData, make_id};
pub use mutators::{Context, MutateRequest, client_mutators};
pub use queries::ZERO_PROJECT_NAME;

/// The zero-cache address as the client takes it. zbugs' backend prints it
/// as `ws://`, and the client only takes `http://` and `https://`, so `ws://`
/// becomes `http://` and `wss://` becomes `https://`. Any other scheme, or
/// no host, is an error to show.
pub fn cache_http_url(url: &str) -> Result<String, String> {
    let url = url.trim();
    let (scheme, rest) = url.split_once("://").unwrap_or(("", ""));
    let scheme = match scheme.to_ascii_lowercase().as_str() {
        "http" | "ws" => "http",
        "https" | "wss" => "https",
        _ => "",
    };
    if scheme.is_empty() || rest.is_empty() || rest.starts_with(['/', '?', '#']) {
        return Err(format!(
            "{url:?} isn't a zero-cache address. Use an http://, https://, ws://, or wss:// URL, such as http://127.0.0.1:4848."
        ));
    }
    Ok(format!("{scheme}://{rest}"))
}

#[cfg(test)]
mod tests {
    use super::cache_http_url;

    #[test]
    fn cache_http_url_takes_websocket_schemes() {
        let ok = |u: &str| cache_http_url(u).unwrap();
        assert_eq!(ok("ws://127.0.0.1:4848"), "http://127.0.0.1:4848");
        assert_eq!(ok(" WSS://zero.example/x "), "https://zero.example/x");
        assert_eq!(ok("http://h:1/p"), "http://h:1/p");
        assert_eq!(ok("https://h"), "https://h");
        for bad in [
            "",
            "127.0.0.1:4848",
            "ftp://h",
            "ws://",
            "http:///p",
            "wsx://h",
        ] {
            assert!(cache_http_url(bad).is_err(), "{bad}");
        }
    }
}
