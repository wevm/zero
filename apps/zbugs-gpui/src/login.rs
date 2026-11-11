//! GitHub login through a loopback redirect (RFC 8252 §7.3), as zbugs' API
//! hands a desktop app its token: the app listens on `127.0.0.1` for the
//! one login it started, at a random path, and the API's GitHub callback
//! redirects the browser there with the token in `jwt`.

use futures::channel::oneshot;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

/// How long the login waits for the browser.
const TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Starts a login against the API at `api_url`: returns the URL to open in
/// the browser, and the token once the browser comes back with it. Dropping
/// the receiver stops listening.
pub fn github(api_url: &str) -> io::Result<(String, oneshot::Receiver<Result<String, String>>)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let mut state = [0u8; 16];
    getrandom::fill(&mut state).map_err(io::Error::other)?;
    let path: String = state.iter().map(|b| format!("{b:02x}")).collect();
    let path = format!("/auth/{path}");
    let redirect = format!("http://127.0.0.1:{}{path}", listener.local_addr()?.port());
    let url = format!(
        "{}/api/login/github?redirect={}",
        api_url.trim_end_matches('/'),
        encode(&redirect)
    );
    let (send, receive) = oneshot::channel();
    std::thread::spawn(move || {
        let result = listen(&listener, &path, || send.is_canceled());
        let _ = send.send(result);
    });
    Ok((url, receive))
}

/// Answers requests until one comes to `path` with a token, the login
/// times out, or `canceled`.
fn listen(
    listener: &TcpListener,
    path: &str,
    canceled: impl Fn() -> bool,
) -> Result<String, String> {
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline && !canceled() {
        match listener.accept() {
            Ok((stream, _)) => {
                if let Some(token) = answer(stream, path) {
                    return Ok(token);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(format!("The GitHub login stopped: {e}")),
        }
    }
    Err("The GitHub login timed out.".into())
}

/// Reads one request and answers it: the token when it's the login's
/// callback, and a page that says what happened either way.
fn answer(mut stream: TcpStream, path: &str) -> Option<String> {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut request = Vec::new();
    let mut buf = [0u8; 1024];
    while !request.windows(4).any(|w| w == b"\r\n\r\n") && request.len() < 16 * 1024 {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => request.extend_from_slice(&buf[..n]),
        }
    }
    let request = String::from_utf8_lossy(&request);
    let target = request
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("GET "))
        .and_then(|rest| rest.split(' ').next())
        .unwrap_or_default();
    let (at, query) = target.split_once('?').unwrap_or((target, ""));
    let token = (at == path)
        .then(|| {
            query
                .split('&')
                .find_map(|pair| pair.strip_prefix("jwt="))
                .map(decode)
        })
        .flatten()
        .filter(|t| !t.is_empty());
    let (status, message) = if token.is_some() {
        (
            "200 OK",
            "You're logged in to Zero Bugs. You can close this tab.",
        )
    } else {
        ("404 Not Found", "This isn't the Zero Bugs login.")
    };
    let body = format!(
        "<!doctype html><meta charset=utf-8><title>Zero Bugs</title>\
         <body style=\"font:16px system-ui;margin:4em;text-align:center\">{message}</body>"
    );
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    token
}

/// Percent-encodes everything but RFC 3986's unreserved characters.
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

fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(b)) => {
                out.push(b);
                i += 3;
            }
            (b'+', _) => {
                out.push(b' ');
                i += 1;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(redirect: &str, suffix: &str) -> String {
        let rest = redirect.strip_prefix("http://").unwrap();
        let (host, path) = rest.split_once('/').unwrap();
        let mut stream = TcpStream::connect(host).unwrap();
        write!(
            stream,
            "GET /{path}{suffix} HTTP/1.1\r\nHost: {host}\r\n\r\n"
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    /// Only the login's own path with a token logs in; the browser gets a
    /// page either way.
    #[test]
    fn takes_the_token_only_at_its_path() {
        let (url, mut token) = github("http://127.0.0.1:5173/").unwrap();
        let redirect = url
            .strip_prefix("http://127.0.0.1:5173/api/login/github?redirect=")
            .map(decode)
            .unwrap();
        assert!(redirect.starts_with("http://127.0.0.1:"));
        let elsewhere = redirect.rsplit_once('/').unwrap().0.to_owned() + "/other";
        assert!(get(&elsewhere, "?jwt=a.b.c").starts_with("HTTP/1.1 404"));
        assert!(get(&redirect, "").starts_with("HTTP/1.1 404"));
        assert_eq!(token.try_recv().unwrap(), None);
        assert!(get(&redirect, "?state=s&jwt=a.b%2Bc%2Fd%3D").starts_with("HTTP/1.1 200"));
        let token = futures::executor::block_on(token).unwrap();
        assert_eq!(token, Ok("a.b+c/d=".to_owned()));
    }
}
