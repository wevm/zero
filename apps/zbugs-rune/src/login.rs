//! GitHub login through a loopback redirect (RFC 8252 §7.3).
//!
//! The app listens on 127.0.0.1, on a port the system picks, for the one
//! login it started: the API server's GitHub callback redirects the browser
//! to `http://127.0.0.1:<port>/auth/<random>?jwt=<token>`. A request to any
//! other path gets a 404 and is otherwise ignored. The listener stops once
//! it has the token, when the login is dropped, or after five minutes.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(300);

/// A login in progress. Dropping it stops listening.
pub struct LoopbackLogin {
    /// The page that starts the login, for the browser.
    pub url: String,
    cancelled: Arc<AtomicBool>,
}

impl LoopbackLogin {
    /// Listens for the callback, and calls `on_token` with the token, on
    /// the listener's thread, once it comes.
    pub fn start(
        api_url: &str,
        on_token: impl FnOnce(String) + Send + 'static,
    ) -> std::io::Result<LoopbackLogin> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let path = format!("/auth/{}", zbugs_model::make_id());
        let redirect = format!("http://127.0.0.1:{}{path}", listener.local_addr()?.port());
        let url = format!(
            "{}/api/login/github?redirect={}",
            api_url.trim_end_matches('/'),
            encode(&redirect)
        );
        let cancelled = Arc::new(AtomicBool::new(false));
        let stop = cancelled.clone();
        std::thread::Builder::new()
            .name("zbugs-login".into())
            .spawn(move || serve(&listener, &path, &stop, on_token))?;
        Ok(LoopbackLogin { url, cancelled })
    }
}

impl Drop for LoopbackLogin {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

fn serve(
    listener: &TcpListener,
    path: &str,
    cancelled: &AtomicBool,
    on_token: impl FnOnce(String),
) {
    let deadline = Instant::now() + WAIT;
    while !cancelled.load(Ordering::Relaxed) && Instant::now() < deadline {
        match listener.accept() {
            Ok((stream, _)) => {
                if let Some(token) = answer(stream, path) {
                    on_token(token);
                    return;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => return,
        }
    }
}

/// Reads one request and answers it: the token, when the request is the
/// callback.
fn answer(stream: TcpStream, path: &str) -> Option<String> {
    // An accepted socket can inherit the listener's non-blocking mode.
    stream.set_nonblocking(false).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let mut reader = BufReader::new(&stream);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).ok()?;
    // The headers, so the browser sees the whole request read.
    let mut header = String::new();
    while reader.read_line(&mut header).is_ok_and(|n| n > 0) && header.trim() != "" {
        header.clear();
    }
    let token = callback_token(&request_line, path);
    let (status, body) = match token {
        Some(_) => ("200 OK", "Logged in to Zero Bugs. You can close this tab."),
        None => ("404 Not Found", "Not found."),
    };
    let _ = write!(
        &stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    token
}

/// The `jwt` of a `GET` of `path`.
fn callback_token(request_line: &str, path: &str) -> Option<String> {
    let target = request_line.strip_prefix("GET ")?.split(' ').next()?;
    let (p, query) = target.split_once('?')?;
    if p != path {
        return None;
    }
    query
        .split('&')
        .find_map(|kv| kv.strip_prefix("jwt="))
        .and_then(decode)
        .filter(|t| !t.is_empty())
}

/// Percent-encodes all but RFC 3986's unreserved characters.
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Decodes a query value (`+` is a space).
fn decode(s: &str) -> Option<String> {
    let mut out = Vec::with_capacity(s.len());
    let mut bytes = s.bytes();
    while let Some(b) = bytes.next() {
        match b {
            b'%' => {
                let hex = [bytes.next()?, bytes.next()?];
                out.push(u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?);
            }
            b'+' => out.push(b' '),
            b => out.push(b),
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::sync::mpsc;

    fn get(redirect: &str, suffix: &str) -> String {
        let rest = redirect.strip_prefix("http://").unwrap();
        let (host, path) = rest.split_once('/').unwrap();
        let mut s = TcpStream::connect(host).unwrap();
        write!(s, "GET /{path}{suffix} HTTP/1.1\r\nHost: {host}\r\n\r\n").unwrap();
        let mut response = String::new();
        s.read_to_string(&mut response).unwrap();
        response
    }

    #[test]
    fn hands_over_the_token_of_its_own_callback_only() {
        let (tx, rx) = mpsc::channel();
        let login = LoopbackLogin::start("http://api.test/", move |t| tx.send(t).unwrap()).unwrap();
        let encoded = login
            .url
            .strip_prefix("http://api.test/api/login/github?redirect=")
            .unwrap();
        assert!(!encoded.contains(':') && !encoded.contains('/'));
        let redirect = decode(encoded).unwrap();
        assert!(redirect.starts_with("http://127.0.0.1:"));

        // Another path on the port, and the callback without a token.
        let other = redirect.rsplit_once('/').unwrap().0.to_owned() + "/guess";
        assert!(get(&other, "?jwt=x").starts_with("HTTP/1.1 404"));
        assert!(get(&redirect, "?state=1").starts_with("HTTP/1.1 404"));
        assert!(rx.try_recv().is_err());

        let response = get(&redirect, "?jwt=a.b%2Bc-d_e");
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            "a.b+c-d_e"
        );
    }
}
