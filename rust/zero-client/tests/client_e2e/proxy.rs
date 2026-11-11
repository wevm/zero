//! A TCP proxy in front of zero-cache that the chaos tests steer, as Swift's
//! `ChaosProxy`: it can refuse connections, accept them and never answer,
//! complete the WebSocket upgrade and then go silent, stall or throttle what
//! the server sends, hold what the client sends, and cut connections,
//! abruptly or partway through a frame.
//!
//! It runs on a thread and runtime of its own, so a test blocked on `psql`
//! doesn't stall it. A knob change applies to the open connections at once,
//! and once [`ChaosProxy::cut_all`] returns, nothing more crosses the
//! connections it cut.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, oneshot, watch};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Accept {
    /// Forward to zero-cache.
    Pass,
    /// Close the connection at once.
    Refuse,
    /// Accept, read, and never answer.
    Blackhole,
    /// Forward the HTTP upgrade both ways, then drop everything the server
    /// sends: an open WebSocket that never says `connected`.
    SilentAfterUpgrade,
}

#[derive(Clone, Debug)]
pub struct Knobs {
    pub accept: Accept,
    /// Bytes per second from the server to the client; `None` for no
    /// limit, `Some(0)` to stall.
    pub downstream_rate: Option<usize>,
    /// Holds what the client sends.
    pub stall_upstream: bool,
    /// Cuts each connection once the server has sent this many bytes on it:
    /// those bytes reach the client, then the connection closes abruptly.
    pub cut_after_downstream_bytes: Option<usize>,
}

impl Default for Knobs {
    fn default() -> Self {
        Knobs {
            accept: Accept::Pass,
            downstream_rate: None,
            stall_upstream: false,
            cut_after_downstream_bytes: None,
        }
    }
}

/// What the test sees of one connection.
struct Link {
    index: usize,
    opened: Instant,
    peer: u16,
    cut: AtomicBool,
    wake: Notify,
    up: AtomicUsize,
    down: AtomicUsize,
}

#[derive(Default)]
struct Links {
    accepted: usize,
    next_id: usize,
    open: HashMap<usize, Arc<Link>>,
}

pub struct ChaosProxy {
    port: u16,
    knobs: watch::Sender<Knobs>,
    links: Arc<Mutex<Links>>,
    stop: Option<oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ChaosProxy {
    /// Proxies `127.0.0.1:<port>` to the zero-cache at `upstream`, a `ws://`
    /// or `http://` origin.
    pub fn new(upstream: &str) -> ChaosProxy {
        let authority = upstream
            .split("://")
            .last()
            .unwrap()
            .split('/')
            .next()
            .unwrap()
            .to_owned();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (knobs, knobs_rx) = watch::channel(Knobs::default());
        let links = Arc::new(Mutex::new(Links::default()));
        let (stop, stopped) = oneshot::channel();
        let thread = {
            let links = links.clone();
            std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                runtime.block_on(async move {
                    let listener = TcpListener::from_std(listener).unwrap();
                    tokio::select! {
                        () = accept_loop(listener, authority, knobs_rx, links) => {}
                        _ = stopped => {}
                    }
                });
            })
        };
        ChaosProxy {
            port,
            knobs,
            links,
            stop: Some(stop),
            thread: Some(thread),
        }
    }

    /// The origin a client connects to, `http://127.0.0.1:<port>`.
    pub fn cache_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Changes the knobs, and applies them to the open connections.
    pub fn set(&self, change: impl FnOnce(&mut Knobs)) {
        self.knobs.send_modify(change);
    }

    /// Closes every open connection abruptly, without a WebSocket close.
    pub fn cut_all(&self) {
        let cut: Vec<_> = self.lock().open.drain().map(|(_, l)| l).collect();
        for link in cut {
            link.cut.store(true, Ordering::SeqCst);
            link.wake.notify_one();
        }
    }

    /// The connections open now.
    pub fn open_connections(&self) -> usize {
        self.lock().open.len()
    }

    /// The connections accepted so far.
    pub fn accepted(&self) -> usize {
        self.lock().accepted
    }

    /// One line for each open connection: what has crossed it so far.
    pub fn open_connection_report(&self) -> Vec<String> {
        let links = self.lock();
        let mut open: Vec<_> = links.open.values().collect();
        open.sort_by_key(|l| l.index);
        open.iter()
            .map(|l| {
                format!(
                    "#{} peer={} age={}ms up={}B down={}B",
                    l.index,
                    l.peer,
                    l.opened.elapsed().as_millis(),
                    l.up.load(Ordering::SeqCst),
                    l.down.load(Ordering::SeqCst)
                )
            })
            .collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Links> {
        self.links.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Drop for ChaosProxy {
    fn drop(&mut self) {
        self.cut_all();
        // Lets the cut connections reset before the runtime goes.
        std::thread::sleep(Duration::from_millis(20));
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

async fn accept_loop(
    listener: TcpListener,
    upstream: String,
    knobs: watch::Receiver<Knobs>,
    links: Arc<Mutex<Links>>,
) {
    loop {
        let Ok((client, peer)) = listener.accept().await else {
            continue;
        };
        let accept = knobs.borrow().accept;
        let link = {
            let mut l = links.lock().unwrap_or_else(|e| e.into_inner());
            l.accepted += 1;
            if accept == Accept::Refuse {
                drop(client);
                continue;
            }
            let link = Arc::new(Link {
                index: l.accepted,
                opened: Instant::now(),
                peer: peer.port(),
                cut: AtomicBool::new(false),
                wake: Notify::new(),
                up: AtomicUsize::new(0),
                down: AtomicUsize::new(0),
            });
            let id = l.next_id;
            l.next_id += 1;
            l.open.insert(id, link.clone());
            (id, link)
        };
        let (upstream, knobs, links) = (upstream.clone(), knobs.clone(), links.clone());
        tokio::spawn(async move {
            let (id, link) = link;
            proxy_link(client, upstream, accept, knobs, &link).await;
            links
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .open
                .remove(&id);
        });
    }
}

/// How a link ends.
#[derive(PartialEq)]
enum End {
    /// A reset rather than an orderly shutdown.
    Abrupt,
    Orderly,
}

async fn proxy_link(
    mut client: TcpStream,
    upstream: String,
    accept: Accept,
    knobs: watch::Receiver<Knobs>,
    link: &Link,
) {
    if accept == Accept::Blackhole {
        let end = blackhole(&mut client, link).await;
        close(&client, &end);
        return;
    }
    let Ok(mut server) = TcpStream::connect(&upstream).await else {
        close(&client, &End::Abrupt);
        return;
    };
    let end = forward(
        &mut client,
        &mut server,
        accept == Accept::SilentAfterUpgrade,
        knobs,
        link,
    )
    .await;
    close(&client, &end);
    close(&server, &end);
}

fn close(stream: &TcpStream, end: &End) {
    if *end == End::Abrupt {
        let _ = stream.set_zero_linger();
    }
}

/// Reads what the client sends, and never answers.
async fn blackhole(client: &mut TcpStream, link: &Link) -> End {
    let mut buf = vec![0; 16 * 1024];
    loop {
        if link.cut.load(Ordering::SeqCst) {
            return End::Abrupt;
        }
        tokio::select! {
            r = client.read(&mut buf) => match r {
                Ok(0) | Err(_) => return End::Orderly,
                Ok(n) => { link.up.fetch_add(n, Ordering::SeqCst); }
            },
            () = link.wake.notified() => {}
        }
    }
}

async fn forward(
    client: &mut TcpStream,
    server: &mut TcpStream,
    silent_after_upgrade: bool,
    mut knobs: watch::Receiver<Knobs>,
    link: &Link,
) -> End {
    let (mut client_read, mut client_write) = client.split();
    let (mut server_read, mut server_write) = server.split();
    let mut up: Vec<u8> = Vec::new();
    let mut down: VecDeque<u8> = VecDeque::new();
    let mut down_sent = 0usize;
    let mut budget = 0usize;
    let mut header_tail: Vec<u8> = Vec::new();
    let mut headers_done = false;
    let mut server_gone = false;
    let mut client_buf = vec![0; 64 * 1024];
    let mut server_buf = vec![0; 64 * 1024];
    let mut ticker = tokio::time::interval(Duration::from_millis(10));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut tick = false;
    loop {
        if link.cut.load(Ordering::SeqCst) {
            return End::Abrupt;
        }
        let k = knobs.borrow_and_update().clone();
        if !k.stall_upstream && !up.is_empty() {
            if server_write.write_all(&up).await.is_err() {
                return End::Abrupt;
            }
            up.clear();
        }
        match k.downstream_rate {
            Some(rate) => {
                budget = budget.min(rate);
                if tick {
                    budget = (budget + rate / 100).min(rate.max(1));
                }
            }
            None => budget = usize::MAX,
        }
        tick = false;
        if !down.is_empty() && budget > 0 {
            let mut n = down.len().min(budget);
            if let Some(cut) = k.cut_after_downstream_bytes {
                n = n.min(cut.saturating_sub(down_sent));
            }
            if n > 0 {
                let chunk: Vec<u8> = down.drain(..n).collect();
                if client_write.write_all(&chunk).await.is_err() {
                    return End::Abrupt;
                }
                down_sent += n;
                if k.downstream_rate.is_some() {
                    budget -= n;
                }
            }
            if k.cut_after_downstream_bytes
                .is_some_and(|cut| down_sent >= cut)
            {
                let _ = client_write.flush().await;
                return End::Abrupt;
            }
        }
        if server_gone && down.is_empty() {
            return End::Orderly;
        }
        tokio::select! {
            r = client_read.read(&mut client_buf) => match r {
                Ok(0) | Err(_) => return End::Orderly,
                Ok(n) => {
                    link.up.fetch_add(n, Ordering::SeqCst);
                    up.extend_from_slice(&client_buf[..n]);
                }
            },
            r = server_read.read(&mut server_buf), if !server_gone => match r {
                // The server went away: deliver what it sent, then close.
                Ok(0) => server_gone = true,
                Err(_) => return End::Abrupt,
                Ok(n) => {
                    link.down.fetch_add(n, Ordering::SeqCst);
                    let mut bytes = &server_buf[..n];
                    if silent_after_upgrade {
                        if headers_done {
                            bytes = &[];
                        } else if let Some(end) = bytes.iter().position(|&b| {
                            // Forward up to the end of the upgrade response's headers.
                            header_tail.push(b);
                            if header_tail.len() > 4 {
                                header_tail.remove(0);
                            }
                            header_tail == b"\r\n\r\n"
                        }) {
                            headers_done = true;
                            bytes = &bytes[..=end];
                        }
                    }
                    down.extend(bytes);
                }
            },
            _ = ticker.tick() => tick = true,
            r = knobs.changed() => if r.is_err() { return End::Abrupt },
            () = link.wake.notified() => {}
        }
    }
}
