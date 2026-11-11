//! A scheduler and a WebSocket transport on tokio, behind the
//! `tokio-transport` feature, for apps that don't bring their own.
//!
//! The client is single-threaded, so both run on a tokio
//! [`LocalSet`](tokio::task::LocalSet): make the client, and call it, from
//! inside one.
//!
//! ```no_run
//! # use std::{rc::Rc, sync::Arc};
//! # use zero_client::{client::{Zero, ZeroOptions}, schema::Schema};
//! # use zero_client::tokio_transport::{TokioScheduler, TokioSocketFactory};
//! # async fn run(schema: Arc<Schema>) -> Result<(), zero_client::client::Error> {
//! let local = tokio::task::LocalSet::new();
//! local
//!     .run_until(async {
//!         let zero = Zero::new(
//!             schema,
//!             (),
//!             Vec::new(),
//!             ZeroOptions {
//!                 cache_url: Some("https://zero.example.com".to_owned()),
//!                 ..ZeroOptions::default()
//!             },
//!             Rc::new(TokioScheduler::new()),
//!             Some(Rc::new(TokioSocketFactory::new())),
//!         )?;
//!         // Materialize views, mutate, and keep the LocalSet running.
//!         # drop(zero);
//!         Ok(())
//!     })
//!     .await
//! # }
//! ```
//!
//! `wss://` needs the `tls` feature too: rustls on the `ring` crypto
//! provider, with the Mozilla root certificates (`webpki-roots`). Without
//! it, a `wss://` connection fails as a close with code 1006, as a browser
//! reports a socket it couldn't open.
//!
//! A frame or message from the server may be up to 64 MB, as in zero-swift's
//! transports; a larger one closes the socket with 1006. A browser has no
//! limit, but tungstenite's default of 16 MB a frame would close the socket
//! on a large poke that zero-cache sends as one frame.

use crate::client::{
    Scheduler, SocketConnection, SocketEvents, SocketFactory, SocketMessage, Timer, timer_delay,
};
use futures_util::{SinkExt, StreamExt};
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio::task::{AbortHandle, spawn_local};
use tokio::time::{Instant, MissedTickBehavior};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, WebSocketConfig};
use tokio_tungstenite::tungstenite::{self, Message};

/// The clock and timers of the client on tokio: timers are local tasks, and
/// follow-ups to API calls run as a local task spawned when the call is
/// done, so they run after the caller's synchronous code, as JS microtasks
/// do.
///
/// Its methods must be called from inside a
/// [`LocalSet`](tokio::task::LocalSet); they panic otherwise.
pub struct TokioScheduler {
    start: Instant,
}

impl TokioScheduler {
    /// A scheduler on the current tokio `LocalSet`.
    pub fn new() -> Self {
        TokioScheduler {
            start: Instant::now(),
        }
    }
}

impl Default for TokioScheduler {
    fn default() -> Self {
        Self::new()
    }
}

fn delay(ms: f64) -> Duration {
    Duration::from_secs_f64(timer_delay(ms) / 1000.0)
}

struct TokioTimer(AbortHandle);

impl Timer for TokioTimer {
    fn cancel(&self) {
        self.0.abort();
    }
}

impl Scheduler for TokioScheduler {
    fn now(&self) -> f64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0.0, |d| d.as_millis() as f64)
    }

    /// Follows tokio's clock, so a test that pauses tokio's time moves it.
    fn monotonic_now(&self) -> f64 {
        self.start.elapsed().as_secs_f64() * 1000.0
    }

    fn schedule(&self, ms: f64, action: Box<dyn FnOnce()>) -> Box<dyn Timer> {
        let d = delay(ms);
        let task = spawn_local(async move {
            tokio::time::sleep(d).await;
            action();
        });
        Box::new(TokioTimer(task.abort_handle()))
    }

    fn schedule_repeating(&self, ms: f64, mut action: Box<dyn FnMut()>) -> Box<dyn Timer> {
        // tokio's interval needs a period; browsers clamp one too.
        let period = delay(ms).max(Duration::from_millis(1));
        let task = spawn_local(async move {
            let mut interval = tokio::time::interval_at(Instant::now() + period, period);
            interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                action();
            }
        });
        Box::new(TokioTimer(task.abort_handle()))
    }

    fn checkpoint(&self, action: Box<dyn FnOnce()>) {
        spawn_local(async move { action() });
    }
}

/// Opens the client's sync socket with `tokio-tungstenite`, on a local task
/// per socket. Like [`TokioScheduler`], it must be used from inside a
/// [`LocalSet`](tokio::task::LocalSet).
#[derive(Default)]
pub struct TokioSocketFactory {
    _private: (),
}

impl TokioSocketFactory {
    /// A factory of WebSockets on the current tokio `LocalSet`.
    pub fn new() -> Self {
        Self::default()
    }
}

enum Command {
    Send(String),
    Close(Option<u16>),
}

struct TokioSocket {
    commands: mpsc::UnboundedSender<Command>,
}

impl SocketConnection for TokioSocket {
    fn send(&self, text: &str) {
        // The task is gone once the socket closed, and the engine ignores
        // what it sends after that.
        let _ = self.commands.send(Command::Send(text.to_owned()));
    }

    fn close(&self, code: Option<u16>) {
        let _ = self.commands.send(Command::Close(code));
    }
}

impl SocketFactory for TokioSocketFactory {
    fn open(
        &self,
        url: &str,
        sec_protocol: Option<&str>,
        events: Rc<dyn SocketEvents>,
    ) -> Box<dyn SocketConnection> {
        let (commands, receiver) = mpsc::unbounded_channel();
        let url = url.to_owned();
        let sec_protocol = sec_protocol.map(str::to_owned);
        spawn_local(run_socket(url, sec_protocol, events, receiver));
        Box::new(TokioSocket { commands })
    }
}

/// The browser's code for a socket that closed without a close frame.
const ABNORMAL_CLOSURE: u16 = 1006;

/// The largest frame and message the socket takes, as in zero-swift.
const MAX_MESSAGE_SIZE: usize = 64 << 20;

async fn run_socket(
    url: String,
    sec_protocol: Option<String>,
    events: Rc<dyn SocketEvents>,
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
                r = tokio_tungstenite::connect_async_with_config(request, Some(config()), false) => {
                    r.map(|(ws, _)| ws)
                }
                // Closed (or dropped) before it opened: no events.
                _ = commands.recv() => return,
            }
        }
        Err(e) => Err(e),
    };
    let mut ws = match connected {
        Ok(ws) => ws,
        Err(e) => {
            events.did_close(ABNORMAL_CLOSURE, &e.to_string(), false);
            return;
        }
    };
    events.did_open();
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(Command::Send(text)) => {
                    // A failed write shows up as an error on the next read.
                    let _ = ws.send(Message::text(text)).await;
                }
                // The engine closed the socket, or dropped it.
                Some(Command::Close(code)) => {
                    let _ = ws.close(close_frame(code)).await;
                    return;
                }
                None => {
                    let _ = ws.close(None).await;
                    return;
                }
            },
            message = ws.next() => match message {
                Some(Ok(Message::Text(text))) => {
                    events.did_receive(SocketMessage::Text(text.as_str().to_owned()));
                }
                Some(Ok(Message::Binary(bytes))) => {
                    events.did_receive(SocketMessage::Binary(bytes.to_vec()));
                }
                Some(Ok(Message::Close(frame))) => {
                    let (code, reason) = frame.map_or((1005, String::new()), |f| {
                        (u16::from(f.code), f.reason.as_str().to_owned())
                    });
                    events.did_close(code, &reason, true);
                    return;
                }
                // tungstenite answers pings itself.
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    events.did_close(ABNORMAL_CLOSURE, &e.to_string(), false);
                    return;
                }
                None => {
                    events.did_close(ABNORMAL_CLOSURE, "", false);
                    return;
                }
            },
        }
    }
}

fn config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_frame_size(Some(MAX_MESSAGE_SIZE))
        .max_message_size(Some(MAX_MESSAGE_SIZE))
}

fn close_frame(code: Option<u16>) -> Option<CloseFrame> {
    code.map(|c| CloseFrame {
        code: CloseCode::from(c),
        reason: "".into(),
    })
}
