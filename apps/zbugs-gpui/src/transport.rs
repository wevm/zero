//! zero-client's clock, timers, and sync socket on GPUI.
//!
//! The client is single-threaded, and GPUI owns the main thread, so the
//! client runs there: its timers and checkpoints are tasks on GPUI's
//! foreground executor. The WebSockets run on a tokio runtime on a thread
//! of their own; each socket's events come back to the main thread through
//! a channel a foreground task reads.

use futures::{SinkExt, StreamExt};
use gpui_kit::{BackgroundExecutor, ForegroundExecutor};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::{self, Message};
use zero_client::client::{
    Scheduler, SocketConnection, SocketEvents, SocketFactory, SocketMessage, Timer, timer_delay,
};

/// The clock and timers on GPUI's executors.
pub struct GpuiScheduler {
    foreground: ForegroundExecutor,
    background: BackgroundExecutor,
    start: Instant,
}

impl GpuiScheduler {
    pub fn new(foreground: ForegroundExecutor, background: BackgroundExecutor) -> Self {
        GpuiScheduler {
            foreground,
            background,
            start: Instant::now(),
        }
    }
}

/// A timer that may still fire unless cancelled: dropping it doesn't stop
/// it, as with `setTimeout`.
struct GpuiTimer(Rc<Cell<bool>>);

impl Timer for GpuiTimer {
    fn cancel(&self) {
        self.0.set(true);
    }
}

fn delay(ms: f64) -> Duration {
    Duration::from_secs_f64(timer_delay(ms) / 1000.0)
}

impl Scheduler for GpuiScheduler {
    fn now(&self) -> f64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0.0, |d| d.as_millis() as f64)
    }

    fn monotonic_now(&self) -> f64 {
        self.start.elapsed().as_secs_f64() * 1000.0
    }

    fn schedule(&self, ms: f64, action: Box<dyn FnOnce()>) -> Box<dyn Timer> {
        let cancelled = Rc::new(Cell::new(false));
        let timer = self.background.timer(delay(ms));
        let c = cancelled.clone();
        self.foreground
            .spawn(async move {
                timer.await;
                if !c.get() {
                    action();
                }
            })
            .detach();
        Box::new(GpuiTimer(cancelled))
    }

    fn schedule_repeating(&self, ms: f64, mut action: Box<dyn FnMut()>) -> Box<dyn Timer> {
        let cancelled = Rc::new(Cell::new(false));
        let period = delay(ms).max(Duration::from_millis(1));
        let background = self.background.clone();
        let c = cancelled.clone();
        self.foreground
            .spawn(async move {
                loop {
                    background.timer(period).await;
                    if c.get() {
                        return;
                    }
                    action();
                }
            })
            .detach();
        Box::new(GpuiTimer(cancelled))
    }

    /// After the caller's synchronous code: the next foreground task.
    fn checkpoint(&self, action: Box<dyn FnOnce()>) {
        self.foreground.spawn(async move { action() }).detach();
    }
}

/// The tokio runtime the sockets run on, on a thread of its own.
fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        // `wss://`: rustls on ring.
        let _ = rustls::crypto::ring::default_provider().install_default();
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("zbugs-sockets")
            .enable_all()
            .build()
            .expect("the sockets' tokio runtime")
    })
}

/// Opens the client's sync socket with tokio-tungstenite.
pub struct WebSocketFactory {
    foreground: ForegroundExecutor,
}

impl WebSocketFactory {
    pub fn new(foreground: ForegroundExecutor) -> Self {
        WebSocketFactory { foreground }
    }
}

enum Command {
    Send(String),
    Close(Option<u16>),
}

enum Event {
    Open,
    Message(SocketMessage),
    Close(u16, String, bool),
}

struct Socket {
    commands: mpsc::UnboundedSender<Command>,
    /// Closed by the client: no more events go to it.
    closed: Rc<Cell<bool>>,
}

impl SocketConnection for Socket {
    fn send(&self, text: &str) {
        let _ = self.commands.send(Command::Send(text.to_owned()));
    }

    fn close(&self, code: Option<u16>) {
        self.closed.set(true);
        let _ = self.commands.send(Command::Close(code));
    }
}

impl SocketFactory for WebSocketFactory {
    fn open(
        &self,
        url: &str,
        sec_protocol: Option<&str>,
        events: Rc<dyn SocketEvents>,
    ) -> Box<dyn SocketConnection> {
        let (commands, command_rx) = mpsc::unbounded_channel();
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        runtime().spawn(run_socket(
            url.to_owned(),
            sec_protocol.map(str::to_owned),
            event_tx,
            command_rx,
        ));
        let closed = Rc::new(Cell::new(false));
        let c = closed.clone();
        self.foreground
            .spawn(async move {
                while let Some(event) = event_rx.recv().await {
                    if c.get() {
                        return;
                    }
                    match event {
                        Event::Open => events.did_open(),
                        Event::Message(m) => events.did_receive(m),
                        Event::Close(code, reason, clean) => {
                            events.did_close(code, &reason, clean);
                            return;
                        }
                    }
                }
            })
            .detach();
        Box::new(Socket { commands, closed })
    }
}

/// The browser's code for a socket that closed without a close frame.
const ABNORMAL_CLOSURE: u16 = 1006;

async fn run_socket(
    url: String,
    sec_protocol: Option<String>,
    events: mpsc::UnboundedSender<Event>,
    mut commands: mpsc::UnboundedReceiver<Command>,
) {
    let request = (|| -> Result<_, tungstenite::Error> {
        let mut request = url.as_str().into_client_request()?;
        if let Some(p) = &sec_protocol {
            let value = HeaderValue::from_str(p).map_err(tungstenite::http::Error::from)?;
            request
                .headers_mut()
                .insert("Sec-WebSocket-Protocol", value);
        }
        Ok(request)
    })();
    let connected = match request {
        Ok(request) => {
            tokio::select! {
                r = tokio_tungstenite::connect_async(request) => r.map(|(ws, _)| ws),
                // Closed before it opened.
                _ = commands.recv() => return,
            }
        }
        Err(e) => Err(e),
    };
    let mut ws = match connected {
        Ok(ws) => ws,
        Err(e) => {
            let _ = events.send(Event::Close(ABNORMAL_CLOSURE, e.to_string(), false));
            return;
        }
    };
    let _ = events.send(Event::Open);
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(Command::Send(text)) => {
                    let _ = ws.send(Message::text(text)).await;
                }
                Some(Command::Close(code)) => {
                    let frame = code.map(|c| CloseFrame { code: CloseCode::from(c), reason: "".into() });
                    let _ = ws.close(frame).await;
                    return;
                }
                None => {
                    let _ = ws.close(None).await;
                    return;
                }
            },
            message = ws.next() => {
                let event = match message {
                    Some(Ok(Message::Text(t))) => Event::Message(SocketMessage::Text(t.as_str().to_owned())),
                    Some(Ok(Message::Binary(b))) => Event::Message(SocketMessage::Binary(b.to_vec())),
                    Some(Ok(Message::Close(frame))) => {
                        let (code, reason) = frame.map_or((1005, String::new()), |f| {
                            (u16::from(f.code), f.reason.as_str().to_owned())
                        });
                        let _ = events.send(Event::Close(code, reason, true));
                        return;
                    }
                    Some(Ok(_)) => continue,
                    Some(Err(e)) => {
                        let _ = events.send(Event::Close(ABNORMAL_CLOSURE, e.to_string(), false));
                        return;
                    }
                    None => {
                        let _ = events.send(Event::Close(ABNORMAL_CLOSURE, String::new(), false));
                        return;
                    }
                };
                if events.send(event).is_err() {
                    return;
                }
            }
        }
    }
}
