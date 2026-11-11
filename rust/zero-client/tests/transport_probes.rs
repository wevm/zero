//! What a browser `WebSocket` does, checked against the tokio transport with
//! a server that writes raw bytes, as zero-swift's `TransportProbeTests`:
//! frames that come with the 101 response, fragments, large frames, close
//! frames of any code, cuts partway through a frame, pings, the
//! `Sec-WebSocket-Protocol` header, declined upgrades, redirects, and closes
//! while the upgrade is under way. The engine reads close codes (1009,
//! 4xxx) and relies on every socket it closes being closed.
//!
//! Swift's probes that send binary frames up have no counterpart: the
//! client only sends text, as JS does. And where Swift reads the socket
//! after closing it, here no event may arrive after `close`
//! ([`SocketConnection::close`]).

use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::LocalSet;
use zero_client::client::{
    ConnectionStatus, SocketConnection, SocketEvents, SocketFactory, SocketMessage, Zero,
    ZeroOptions, ZeroStorage,
};
use zero_client::json;
use zero_client::protocol::{InitConnectionBody, sec_protocols};
use zero_client::schema::Schema;
use zero_client::tokio_transport::{TokioScheduler, TokioSocketFactory};

// The server

/// A client frame, unmasked.
#[derive(Clone, Debug, PartialEq)]
struct Frame {
    fin: bool,
    opcode: u8,
    payload: Vec<u8>,
}

impl Frame {
    /// The close frame's code.
    fn close_code(&self) -> Option<u16> {
        (self.opcode == 8 && self.payload.len() >= 2)
            .then(|| u16::from(self.payload[0]) << 8 | u16::from(self.payload[1]))
    }
}

/// A server frame.
fn frame(opcode: u8, payload: &[u8], fin: bool) -> Vec<u8> {
    let mut out = vec![if fin { 0x80 } else { 0 } | opcode];
    let n = payload.len();
    if n < 126 {
        out.push(n as u8);
    } else if n < 1 << 16 {
        out.extend([126, (n >> 8) as u8, n as u8]);
    } else {
        out.push(127);
        out.extend((n as u64).to_be_bytes());
    }
    out.extend_from_slice(payload);
    out
}

fn text(s: &str) -> Vec<u8> {
    frame(1, s.as_bytes(), true)
}

/// A close frame with `code` and `reason`; an empty one without a code.
fn close(code: Option<u16>, reason: &str) -> Vec<u8> {
    match code {
        None => frame(8, &[], true),
        Some(c) => {
            let mut payload = c.to_be_bytes().to_vec();
            payload.extend_from_slice(reason.as_bytes());
            frame(8, &payload, true)
        }
    }
}

#[derive(Default)]
struct Counts {
    accepted: AtomicUsize,
    open: AtomicUsize,
}

/// One client connection of a [`ScriptedServer`].
struct Conn {
    /// The request line and headers.
    request: Vec<String>,
    writer: tokio::sync::Mutex<Option<OwnedWriteHalf>>,
    /// The client's frames not read yet, and whether the client closed.
    received: Mutex<(VecDeque<Frame>, bool)>,
    reader: Mutex<Option<tokio::task::AbortHandle>>,
    closed: AtomicBool,
    counts: Arc<Counts>,
}

impl Conn {
    fn header(&self, name: &str) -> Option<String> {
        self.request.iter().skip(1).find_map(|line| {
            let (k, v) = line.split_once(':')?;
            k.eq_ignore_ascii_case(name).then(|| v.trim().to_owned())
        })
    }

    /// The 101 response that accepts the upgrade, echoing the requested
    /// protocol as a server must.
    fn accept(&self) -> Vec<u8> {
        let key = self.header("Sec-WebSocket-Key").unwrap_or_default();
        let digest = sha1(format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes());
        let mut response = format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Accept: {}\r\n",
            base64(&digest)
        );
        if let Some(p) = self.header("Sec-WebSocket-Protocol") {
            response += &format!("Sec-WebSocket-Protocol: {p}\r\n");
        }
        (response + "\r\n").into_bytes()
    }

    /// Writes `bytes` in one write.
    async fn write(&self, bytes: &[u8]) {
        if let Some(w) = self.writer.lock().await.as_mut() {
            let _ = w.write_all(bytes).await;
        }
    }

    /// Closes the TCP connection, abruptly (a reset) or not.
    async fn close_tcp(&self, abrupt: bool) {
        let writer = self.writer.lock().await.take();
        if let Some(w) = writer
            && abrupt
        {
            let _ = w.as_ref().set_zero_linger();
            // No FIN first: the reset when the read half goes.
            w.forget();
        }
        if let Some(r) = self.reader.lock().unwrap().take() {
            r.abort();
        }
        self.mark_closed();
    }

    fn mark_closed(&self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            self.counts.open.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// The next frame the client sends, other than pongs unless `pongs`,
    /// within `timeout`.
    async fn next(&self, pongs: bool, timeout: Duration) -> Option<Frame> {
        let deadline = Instant::now() + timeout;
        loop {
            {
                let mut r = self.received.lock().unwrap();
                while let Some(f) = r.0.pop_front() {
                    if pongs || f.opcode != 10 {
                        return Some(f);
                    }
                }
                if r.1 {
                    return None;
                }
            }
            if Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    async fn next_frame(&self) -> Option<Frame> {
        self.next(false, Duration::from_secs(5)).await
    }

    /// Reads frames until the client closes the TCP connection.
    async fn drain(&self) {
        while self.next(true, Duration::from_secs(3600)).await.is_some() {}
    }
}

/// Reads the client's frames; when the client closes, closes the
/// connection, as NIO does in Swift's probes.
async fn read_frames(conn: Arc<Conn>, mut read: OwnedReadHalf, mut buffer: Vec<u8>) {
    let mut chunk = vec![0; 64 * 1024];
    loop {
        while let Some((f, used)) = parse(&buffer) {
            buffer.drain(..used);
            conn.received.lock().unwrap().0.push_back(f);
        }
        match read.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
        }
    }
    conn.received.lock().unwrap().1 = true;
    drop(conn.writer.lock().await.take());
    conn.mark_closed();
}

fn parse(b: &[u8]) -> Option<(Frame, usize)> {
    if b.len() < 2 {
        return None;
    }
    let mut i = 2;
    let mut len = usize::from(b[1] & 0x7F);
    if len == 126 {
        len = usize::from(u16::from_be_bytes(b.get(2..4)?.try_into().unwrap()));
        i = 4;
    } else if len == 127 {
        len = u64::from_be_bytes(b.get(2..10)?.try_into().unwrap()) as usize;
        i = 10;
    }
    let mut mask = [0; 4];
    if b[1] & 0x80 != 0 {
        mask.copy_from_slice(b.get(i..i + 4)?);
        i += 4;
    }
    let mut payload = b.get(i..i + len)?.to_vec();
    for (j, byte) in payload.iter_mut().enumerate() {
        *byte ^= mask[j % 4];
    }
    let f = Frame {
        fin: b[0] & 0x80 != 0,
        opcode: b[0] & 0x0F,
        payload,
    };
    Some((f, i + len))
}

type Script = Arc<dyn Fn(Arc<Conn>) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// A WebSocket server on the loopback interface that writes raw bytes, on
/// a thread and runtime of its own. Each connection is handed to the script
/// once its HTTP request has arrived; the script answers it, writes frames,
/// and reads what the client sends.
struct ScriptedServer {
    port: u16,
    counts: Arc<Counts>,
    stop: Option<oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ScriptedServer {
    fn new<F, Fut>(script: F) -> ScriptedServer
    where
        F: Fn(Arc<Conn>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let script: Script = Arc::new(move |c| Box::pin(script(c)));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let counts = Arc::new(Counts::default());
        let (stop, stopped) = oneshot::channel();
        let thread = {
            let counts = counts.clone();
            std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                runtime.block_on(async move {
                    let listener = TcpListener::from_std(listener).unwrap();
                    let accept = async {
                        loop {
                            let Ok((stream, _)) = listener.accept().await else {
                                continue;
                            };
                            counts.accepted.fetch_add(1, Ordering::SeqCst);
                            counts.open.fetch_add(1, Ordering::SeqCst);
                            tokio::spawn(serve(stream, script.clone(), counts.clone()));
                        }
                    };
                    tokio::select! {
                        () = accept => {}
                        _ = stopped => {}
                    }
                });
            })
        };
        ScriptedServer {
            port,
            counts,
            stop: Some(stop),
            thread: Some(thread),
        }
    }

    fn url(&self) -> String {
        format!("ws://127.0.0.1:{}/", self.port)
    }

    fn accepted(&self) -> usize {
        self.counts.accepted.load(Ordering::SeqCst)
    }

    fn open_connections(&self) -> usize {
        self.counts.open.load(Ordering::SeqCst)
    }

    /// Waits up to `timeout` for every connection to close; whether they
    /// did.
    async fn all_closed(&self, timeout: Duration) -> bool {
        within(timeout, || self.open_connections() == 0).await
    }
}

impl Drop for ScriptedServer {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

async fn serve(stream: TcpStream, script: Script, counts: Arc<Counts>) {
    let (mut read, write) = stream.into_split();
    let mut buffer = Vec::new();
    let mut chunk = vec![0; 16 * 1024];
    let end = loop {
        if let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break end;
        }
        match read.read(&mut chunk).await {
            Ok(0) | Err(_) => {
                counts.open.fetch_sub(1, Ordering::SeqCst);
                return;
            }
            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
        }
    };
    let request = String::from_utf8_lossy(&buffer[..end])
        .split("\r\n")
        .map(str::to_owned)
        .collect();
    let rest = buffer[end + 4..].to_vec();
    let conn = Arc::new(Conn {
        request,
        writer: tokio::sync::Mutex::new(Some(write)),
        received: Mutex::default(),
        reader: Mutex::new(None),
        closed: AtomicBool::new(false),
        counts,
    });
    let reader = tokio::spawn(read_frames(conn.clone(), read, rest));
    *conn.reader.lock().unwrap() = Some(reader.abort_handle());
    script(conn).await;
}

/// SHA-1, for `Sec-WebSocket-Accept`.
fn sha1(message: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    let mut m = message.to_vec();
    m.push(0x80);
    while m.len() % 64 != 56 {
        m.push(0);
    }
    m.extend((message.len() as u64 * 8).to_be_bytes());
    for chunk in m.chunks(64) {
        let mut w = [0u32; 80];
        for t in 0..16 {
            w[t] = u32::from_be_bytes(chunk[t * 4..t * 4 + 4].try_into().unwrap());
        }
        for t in 16..80 {
            w[t] = (w[t - 3] ^ w[t - 8] ^ w[t - 14] ^ w[t - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (t, &wt) in w.iter().enumerate() {
            let (f, k) = match t {
                0..20 => (b & c | !b & d, 0x5A82_7999),
                20..40 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..60 => (b & c | b & d | c & d, 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wt);
            (e, d, c, b, a) = (d, c, b.rotate_left(30), a, temp);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0; 20];
    for (i, v) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

fn base64(bytes: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let n = u32::from(c[0]) << 16
            | u32::from(*c.get(1).unwrap_or(&0)) << 8
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

// The client

#[derive(Clone, Debug, PartialEq)]
enum Event {
    Open,
    Message(SocketMessage),
    Close(u16, String, bool),
}

#[derive(Default)]
struct Recorder(RefCell<Vec<Event>>);

impl SocketEvents for Recorder {
    fn did_open(&self) {
        self.0.borrow_mut().push(Event::Open);
    }
    fn did_receive(&self, message: SocketMessage) {
        self.0.borrow_mut().push(Event::Message(message));
    }
    fn did_close(&self, code: u16, reason: &str, was_clean: bool) {
        self.0
            .borrow_mut()
            .push(Event::Close(code, reason.to_owned(), was_clean));
    }
}

impl Recorder {
    fn messages(&self) -> Vec<SocketMessage> {
        self.0
            .borrow()
            .iter()
            .filter_map(|e| match e {
                Event::Message(m) => Some(m.clone()),
                _ => None,
            })
            .collect()
    }

    fn closed(&self) -> Option<(u16, String, bool)> {
        self.0.borrow().iter().find_map(|e| match e {
            Event::Close(c, r, w) => Some((*c, r.clone(), *w)),
            _ => None,
        })
    }

    fn opened(&self) -> bool {
        self.0.borrow().contains(&Event::Open)
    }

    fn len(&self) -> usize {
        self.0.borrow().len()
    }
}

fn open(url: &str, protocol: Option<&str>) -> (Box<dyn SocketConnection>, Rc<Recorder>) {
    let events = Rc::new(Recorder::default());
    let socket = TokioSocketFactory::new().open(url, protocol, events.clone());
    (socket, events)
}

/// How the socket ended, within `timeout`.
async fn ending(events: &Recorder, timeout: Duration) -> Option<(u16, String, bool)> {
    within(timeout, || events.closed().is_some()).await;
    events.closed()
}

async fn within(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while !condition() {
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    true
}

async fn sleep_ms(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
}

fn secs(s: u64) -> Duration {
    Duration::from_secs(s)
}

/// Runs a probe on the transport's thread, within 2 minutes.
async fn run(body: impl Future<Output = ()>) {
    tokio::time::timeout(secs(120), LocalSet::new().run_until(body))
        .await
        .expect("the probe finished within 2 minutes");
}

fn text_message(s: &str) -> SocketMessage {
    SocketMessage::Text(s.to_owned())
}

// Frames

/// Frames written in the same TCP write as the 101 response are the
/// WebSocket's.
#[tokio::test]
async fn frames_that_come_with_the_101_are_delivered() {
    run(async {
        let server = ScriptedServer::new(|c| async move {
            let mut out = c.accept();
            out.extend(text("first"));
            out.extend(text("second"));
            c.write(&out).await;
            c.next_frame().await;
        });
        let (socket, events) = open(&server.url(), None);
        assert!(within(secs(5), || events.messages().len() == 2).await);
        assert_eq!(
            events.messages(),
            [text_message("first"), text_message("second")]
        );
        socket.close(Some(1000));
    })
    .await;
}

/// A message in fragments arrives as one message.
#[tokio::test]
async fn fragmented_message_arrives_whole() {
    run(async {
        let server = ScriptedServer::new(|c| async move {
            let mut out = c.accept();
            out.extend(frame(1, b"hel", false));
            out.extend(frame(0, "lo é".as_bytes(), true));
            c.write(&out).await;
            c.next_frame().await;
        });
        let (socket, events) = open(&server.url(), None);
        assert!(within(secs(5), || !events.messages().is_empty()).await);
        assert_eq!(events.messages(), [text_message("hello é")]);
        socket.close(Some(1000));
    })
    .await;
}

/// Large frames down, text and binary, and a large text frame up: 20 MB
/// and 12 MB from the server, which a browser takes, and 11 MB from the
/// client, the size of the oversized mutation in the harden tests.
#[tokio::test]
async fn large_frames_both_ways() {
    run(async {
        let big = "abcdefghij".repeat(2 << 20);
        let bytes: Vec<u8> = (0..12usize << 20).map(|i| i as u8).collect();
        let got = Arc::new(Mutex::new(None));
        let server = {
            let (big, bytes, got) = (big.clone(), bytes.clone(), got.clone());
            ScriptedServer::new(move |c| {
                let (big, bytes, got) = (big.clone(), bytes.clone(), got.clone());
                async move {
                    c.write(&c.accept()).await;
                    c.write(&text(&big)).await;
                    c.write(&frame(2, &bytes, true)).await;
                    let f = c.next(false, secs(60)).await;
                    *got.lock().unwrap() = f;
                    c.next_frame().await;
                }
            })
        };
        let (socket, events) = open(&server.url(), None);
        let delivered = within(secs(30), || {
            events.messages().len() == 2 || events.closed().is_some()
        })
        .await;
        assert!(delivered, "nothing within 30 s");
        let messages = events.messages();
        assert_eq!(
            messages.len(),
            2,
            "closed with {:?} after {} messages",
            events.closed(),
            messages.len()
        );
        assert!(
            messages[0] == SocketMessage::Text(big),
            "the 20 MB text frame"
        );
        assert!(
            messages[1] == SocketMessage::Binary(bytes),
            "the 12 MB binary frame"
        );
        let up = "x".repeat(11 << 20);
        socket.send(&up);
        assert!(within(secs(30), || got.lock().unwrap().is_some()).await);
        let f = got.lock().unwrap().clone().unwrap();
        assert_eq!((f.opcode, f.payload.len()), (1, up.len()));
        socket.close(Some(1000));
    })
    .await;
}

/// A frame over the transport's maximum message size. A browser takes it;
/// the engine must at least not mistake the transport's own limit for the
/// server's 1009, after which it drops the client group.
#[tokio::test]
async fn frame_over_the_maximum_message_size() {
    run(async {
        let server = ScriptedServer::new(|c| async move {
            c.write(&c.accept()).await;
            c.write(&frame(2, &vec![7; (64 << 20) + 1], true)).await;
            c.next(false, secs(20)).await;
        });
        let (socket, events) = open(&server.url(), None);
        let end = ending(&events, secs(30)).await;
        println!(
            "[probe] 64 MB + 1 frame: {} messages, then {end:?}",
            events.messages().len()
        );
        if events.messages().is_empty() {
            assert_ne!(
                end.map(|e| e.0),
                Some(1009),
                "the transport's own limit reads as the server's 1009"
            );
        }
        socket.close(Some(1000));
    })
    .await;
}

// Closes

/// The server's close frame: its code and reason, as a browser's
/// `CloseEvent` has them. The connection closes once the client is done.
#[tokio::test]
async fn server_close_codes_and_reasons() {
    run(async {
        for (code, reason) in [
            (1000, "bye"),
            (1001, "going away"),
            (1009, "too big"),
            (1011, "oops"),
            (4001, "x"),
            (4403, "unicode é😀"),
        ] {
            let server = ScriptedServer::new(move |c| async move {
                let mut out = c.accept();
                out.extend(text("hi"));
                out.extend(close(Some(code), reason));
                c.write(&out).await;
                // Answer the client's close, then hang up.
                c.next_frame().await;
                c.close_tcp(false).await;
            });
            let (socket, events) = open(&server.url(), None);
            let end = ending(&events, secs(5)).await;
            assert_eq!(events.messages(), [text_message("hi")], "{code}");
            assert_eq!(end, Some((code, reason.to_owned(), true)), "{code}");
            socket.close(Some(1000));
            assert!(
                server.all_closed(secs(5)).await,
                "{code}: the socket stayed open"
            );
        }
    })
    .await;
}

/// A close frame without a code: 1005 in a browser.
#[tokio::test]
async fn server_close_without_a_code() {
    run(async {
        let server = ScriptedServer::new(|c| async move {
            let mut out = c.accept();
            out.extend(close(None, ""));
            c.write(&out).await;
            c.next_frame().await;
            c.close_tcp(false).await;
        });
        let (_socket, events) = open(&server.url(), None);
        let end = ending(&events, secs(5)).await;
        assert_eq!(end.map(|e| e.0), Some(1005));
    })
    .await;
}

/// The server hangs up without a close frame, orderly or with a reset:
/// 1006, not clean.
#[tokio::test]
async fn server_hangs_up_without_a_close_frame() {
    run(async {
        for abrupt in [false, true] {
            let server = ScriptedServer::new(move |c| async move {
                let mut out = c.accept();
                out.extend(text("hi"));
                c.write(&out).await;
                sleep_ms(200).await;
                c.close_tcp(abrupt).await;
            });
            let (_socket, events) = open(&server.url(), None);
            let end = ending(&events, secs(5)).await;
            assert_eq!(events.messages(), [text_message("hi")], "abrupt {abrupt}");
            assert_eq!(
                end.map(|e| (e.0, e.2)),
                Some((1006, false)),
                "abrupt {abrupt}"
            );
        }
    })
    .await;
}

/// The server goes away partway through a frame: a browser doesn't
/// deliver the partial frame, and the close is abrupt.
#[tokio::test]
async fn server_goes_away_mid_frame() {
    run(async {
        let server = ScriptedServer::new(|c| async move {
            let whole = text(&"y".repeat(100_000));
            let mut out = c.accept();
            out.extend(text("hi"));
            out.extend(&whole[..5000]);
            c.write(&out).await;
            sleep_ms(200).await;
            c.close_tcp(false).await;
        });
        let (_socket, events) = open(&server.url(), None);
        let end = ending(&events, secs(5)).await;
        assert_eq!(events.messages(), [text_message("hi")]);
        assert_eq!(end.map(|e| (e.0, e.2)), Some((1006, false)));
    })
    .await;
}

/// The server goes away partway through a frame while a client is
/// connected: the client reconnects, as it does with a browser
/// `WebSocket`, and never goes to `error`.
#[tokio::test]
async fn client_reconnects_after_the_server_goes_away_mid_frame() {
    run(async {
        for binary in [false, true] {
            let connections = Arc::new(AtomicUsize::new(0));
            let server = {
                let connections = connections.clone();
                ScriptedServer::new(move |c| {
                    let n = connections.fetch_add(1, Ordering::SeqCst) + 1;
                    async move {
                        let mut out = c.accept();
                        out.extend(text(&format!(r#"["connected",{{"wsid":"w{n}"}}]"#)));
                        c.write(&out).await;
                        if n > 1 {
                            c.drain().await;
                            return;
                        }
                        sleep_ms(300).await;
                        let row = format!(
                            r#"{{"op":"put","tableName":"issues","value":{{"id":"x","title":"{}"}}}}"#,
                            "y".repeat(100_000)
                        );
                        let whole = if binary {
                            let mut payload = vec![0u8];
                            payload.extend(row.as_bytes());
                            frame(2, &payload, true)
                        } else {
                            text(&format!(
                                r#"["pokePart",{{"pokeID":"p1","rowsPatch":[{row}]}}]"#
                            ))
                        };
                        c.write(&whole[..5000]).await;
                        sleep_ms(50).await;
                        c.close_tcp(false).await;
                    }
                })
            };
            let schema = json::parse(
                r#"{"tables": {"issue": {"name": "issue", "serverName": "issues",
                  "columns": {"id": {"type": "string"}, "title": {"type": "string"}},
                  "primaryKey": ["id"]}}}"#,
            )
            .unwrap();
            let zero = Zero::new(
                Arc::new(Schema::from_json(&schema).unwrap()),
                (),
                Vec::new(),
                ZeroOptions {
                    cache_url: Some(format!("http://127.0.0.1:{}", server.port)),
                    storage: ZeroStorage::Memory,
                    ..ZeroOptions::default()
                },
                Rc::new(TokioScheduler::new()),
                Some(Rc::new(TokioSocketFactory::new())),
            )
            .unwrap();
            let states = Rc::new(RefCell::new(Vec::new()));
            let unsubscribe = {
                let states = states.clone();
                zero.connection()
                    .subscribe(move |s| states.borrow_mut().push(s.status()))
            };
            // The connect timeout, the first retry's longest wait, and 2 s.
            let reconnected = within(secs(13), || {
                connections.load(Ordering::SeqCst) >= 2
                    && zero.connection().state().status() == ConnectionStatus::Connected
            })
            .await;
            println!(
                "[probe] server gone mid-{} frame: reconnected {reconnected}, states {:?}",
                if binary { "binary" } else { "text" },
                states.borrow()
            );
            assert!(reconnected, "binary {binary}");
            assert!(
                !states.borrow().contains(&ConnectionStatus::Error),
                "binary {binary}"
            );
            unsubscribe();
            zero.close();
        }
    })
    .await;
}

/// The client's close: the server gets a close frame with the code, the
/// socket is closed, and nothing is reported after it.
#[tokio::test]
async fn client_close_sends_the_code() {
    run(async {
        let got = Arc::new(Mutex::new(None));
        let server = {
            let got = got.clone();
            ScriptedServer::new(move |c| {
                let got = got.clone();
                async move {
                    let mut out = c.accept();
                    out.extend(text("ready"));
                    c.write(&out).await;
                    let f = c.next_frame().await;
                    let code = f.as_ref().and_then(Frame::close_code).unwrap_or(1000);
                    *got.lock().unwrap() = f;
                    c.write(&close(Some(code), "")).await;
                    c.next_frame().await;
                    c.close_tcp(false).await;
                }
            })
        };
        let (socket, events) = open(&server.url(), None);
        assert!(within(secs(5), || !events.messages().is_empty()).await);
        socket.close(Some(4123));
        let seen = events.len();
        assert!(
            server.all_closed(secs(5)).await,
            "the socket stayed open after its close"
        );
        assert!(within(secs(5), || got.lock().unwrap().is_some()).await);
        let f = got.lock().unwrap().clone().unwrap();
        assert_eq!((f.opcode, f.close_code()), (8, Some(4123)));
        // A second close and a send after the close don't hang or panic.
        socket.close(Some(1000));
        socket.send("after");
        sleep_ms(100).await;
        assert_eq!(events.len(), seen, "an event after the close");
    })
    .await;
}

/// The client closes while the server says nothing: the socket is closed
/// at once, even when the server never answers the close.
#[tokio::test]
async fn client_close_against_a_silent_server_closes_the_socket() {
    run(async {
        let server = ScriptedServer::new(|c| async move {
            c.write(&c.accept()).await;
            // Never answer the close; keep the TCP connection open.
            sleep_ms(60_000).await;
        });
        let (socket, events) = open(&server.url(), None);
        assert!(within(secs(5), || events.opened()).await);
        sleep_ms(200).await;
        let start = Instant::now();
        socket.close(Some(1000));
        let closed = server.all_closed(secs(10)).await;
        println!(
            "[probe] close against a silent server: TCP closed {closed} after {:?}",
            start.elapsed()
        );
        assert!(closed, "the socket stayed open");
        assert!(start.elapsed() < secs(3));
    })
    .await;
}

/// A send after the server hung up does nothing; it doesn't hang or panic.
#[tokio::test]
async fn send_after_the_server_hung_up() {
    run(async {
        let server = ScriptedServer::new(|c| async move {
            c.write(&c.accept()).await;
            c.close_tcp(false).await;
        });
        let (socket, events) = open(&server.url(), None);
        assert!(ending(&events, secs(5)).await.is_some());
        for _ in 0..3 {
            socket.send("late");
        }
        socket.close(Some(1000));
        sleep_ms(50).await;
    })
    .await;
}

// Ping

/// The server's ping gets a pong with its payload, and isn't delivered as
/// a message.
#[tokio::test]
async fn ping_gets_a_pong() {
    run(async {
        let pong = Arc::new(Mutex::new(None));
        let server = {
            let pong = pong.clone();
            ScriptedServer::new(move |c| {
                let pong = pong.clone();
                async move {
                    let mut out = c.accept();
                    out.extend(frame(9, b"p1", true));
                    c.write(&out).await;
                    let f = c.next(true, secs(5)).await;
                    *pong.lock().unwrap() = f;
                    c.write(&text("after")).await;
                    c.next_frame().await;
                }
            })
        };
        let (socket, events) = open(&server.url(), None);
        assert!(within(secs(10), || !events.messages().is_empty()).await);
        assert_eq!(events.messages(), [text_message("after")]);
        assert_eq!(
            pong.lock().unwrap().clone(),
            Some(Frame {
                fin: true,
                opcode: 10,
                payload: b"p1".to_vec()
            })
        );
        socket.close(Some(1000));
    })
    .await;
}

// The handshake

/// The `Sec-WebSocket-Protocol` header carries the encoded
/// `initConnection` and auth token, up to `max_header_length` (8 KB): it
/// must arrive byte for byte.
#[tokio::test]
async fn sec_protocol_arrives_intact() {
    run(async {
        let token = "eyJhbGciOiJIUzI1NiJ9.ä+/=".repeat(200);
        let value = sec_protocols::encode(Some(&InitConnectionBody::default()), Some(&token));
        let long: String = value.repeat(8192 / value.len() + 1)[..8192].to_owned();
        for protocol in [value, long] {
            let seen = Arc::new(Mutex::new(None));
            let server = {
                let seen = seen.clone();
                ScriptedServer::new(move |c| {
                    let seen = seen.clone();
                    async move {
                        *seen.lock().unwrap() = c.header("Sec-WebSocket-Protocol");
                        c.write(&c.accept()).await;
                        c.next_frame().await;
                    }
                })
            };
            let (socket, events) = open(&server.url(), Some(&protocol));
            assert!(
                within(secs(5), || events.opened()).await,
                "{} bytes: {:?}",
                protocol.len(),
                events.closed()
            );
            assert_eq!(
                seen.lock().unwrap().as_deref(),
                Some(protocol.as_str()),
                "{} bytes",
                protocol.len()
            );
            socket.close(Some(1000));
        }
    })
    .await;
}

/// The server declines the upgrade with an HTTP error: the socket never
/// opens, and closes with 1006.
#[tokio::test]
async fn upgrade_declined_with_an_http_error() {
    run(async {
        for status in ["401 Unauthorized", "500 Internal Server Error", "200 OK"] {
            let server = ScriptedServer::new(move |c| async move {
                c.write(format!("HTTP/1.1 {status}\r\nContent-Length: 2\r\n\r\nno").as_bytes())
                    .await;
                c.close_tcp(false).await;
            });
            let (_socket, events) = open(&server.url(), Some("p"));
            let end = ending(&events, secs(5)).await;
            assert!(!events.opened(), "{status}");
            assert_eq!(end.map(|e| (e.0, e.2)), Some((1006, false)), "{status}");
        }
    })
    .await;
}

/// A redirect: browsers don't follow it for a WebSocket, so the auth token
/// in the protocol header never goes to another server.
#[tokio::test]
async fn redirect_is_not_followed() {
    run(async {
        let target = ScriptedServer::new(|c| async move {
            c.write(&c.accept()).await;
            c.next_frame().await;
        });
        let location = target.url();
        let server = ScriptedServer::new(move |c| {
            let location = location.clone();
            async move {
                c.write(
                    format!(
                        "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await;
                c.next(false, secs(2)).await;
                c.close_tcp(false).await;
            }
        });
        let (_socket, events) = open(&server.url(), Some("secret-token"));
        let end = ending(&events, secs(5)).await;
        println!("[probe] redirect: {end:?}");
        assert!(!events.opened());
        assert_eq!(
            target.accepted(),
            0,
            "followed the redirect, with the protocol header"
        );
    })
    .await;
}

/// A server that accepts the TCP connection and never answers the
/// upgrade: closing the connecting socket closes it at once, as closing a
/// connecting browser `WebSocket` does, and reports nothing. The engine
/// closes on its connect timeout and on close.
#[tokio::test]
async fn closing_a_hung_upgrade_closes_the_socket() {
    run(async {
        let server = ScriptedServer::new(|_| async move {
            sleep_ms(60_000).await;
        });
        for delay in [0, 1, 300] {
            let accepted = server.accepted();
            let (socket, events) = open(&server.url(), Some("p"));
            if delay > 0 {
                sleep_ms(delay).await;
            }
            if delay == 300 {
                assert!(within(secs(5), || server.accepted() > accepted).await);
            }
            socket.close(None);
            assert!(
                server.all_closed(secs(5)).await,
                "the socket closed after {delay} ms stayed open"
            );
            sleep_ms(50).await;
            assert_eq!(
                events.len(),
                0,
                "an event after {delay} ms: {:?}",
                events.closed()
            );
        }
    })
    .await;
}

/// Sockets closed just as the server writes the 101: every one is closed.
#[tokio::test]
async fn closing_as_the_101_arrives_closes_the_socket() {
    run(async {
        let (written, mut signals) = tokio::sync::mpsc::unbounded_channel();
        let server = ScriptedServer::new(move |c| {
            let written = written.clone();
            async move {
                c.write(&c.accept()).await;
                let _ = written.send(());
                c.drain().await;
                c.close_tcp(false).await;
            }
        });
        for n in 0..100 {
            let (socket, _events) = open(&server.url(), Some("p"));
            signals.recv().await.unwrap();
            // 0 to 3 turns of the client's task before the close.
            for _ in 0..n % 4 {
                tokio::task::yield_now().await;
            }
            socket.close(Some(1000));
        }
        assert!(
            server.all_closed(secs(5)).await,
            "{} sockets left open",
            server.open_connections()
        );
    })
    .await;
}

// Leaks

/// Sockets opened and closed, some mid-handshake: every one is closed, and
/// none is kept alive.
#[tokio::test]
async fn open_and_close_leaves_no_sockets_or_tasks() {
    run(async {
        let server = ScriptedServer::new(|c| async move {
            sleep_ms((c.request.len() as u64 * 7) % 20).await;
            let mut out = c.accept();
            out.extend(text("hi"));
            c.write(&out).await;
            c.next_frame().await;
            c.close_tcp(false).await;
        });
        let events = Rc::new(Recorder::default());
        let factory = TokioSocketFactory::new();
        for n in 0..40 {
            let socket = factory.open(&server.url(), Some("p"), events.clone());
            sleep_ms(n % 8 * 3).await;
            socket.close(None);
        }
        assert!(
            server.all_closed(secs(10)).await,
            "{} sockets left open",
            server.open_connections()
        );
        // Each socket's task holds the events until it ends.
        let released = within(secs(5), || Rc::strong_count(&events) == 1).await;
        println!(
            "[probe] open/close x40: {} accepted, {} tasks alive",
            server.accepted(),
            Rc::strong_count(&events) - 1
        );
        assert!(released, "socket tasks kept alive");
    })
    .await;
}
