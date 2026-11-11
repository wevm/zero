//! The tokio scheduler and WebSocket transport against a local
//! tungstenite server.

use futures_util::{SinkExt, StreamExt};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::task::LocalSet;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::{Message, http::HeaderValue};
use zero_client::client::{Scheduler, SocketEvents, SocketFactory, SocketMessage};
use zero_client::tokio_transport::{TokioScheduler, TokioSocketFactory};

#[derive(Default)]
struct Recorder(RefCell<Vec<String>>);

impl Recorder {
    fn events(&self) -> Vec<String> {
        self.0.borrow().clone()
    }
}

impl SocketEvents for Recorder {
    fn did_open(&self) {
        self.0.borrow_mut().push("open".to_owned());
    }
    fn did_receive(&self, message: SocketMessage) {
        let e = match message {
            SocketMessage::Text(t) => format!("text {t}"),
            SocketMessage::Binary(b) => format!("binary {b:?}"),
        };
        self.0.borrow_mut().push(e);
    }
    fn did_close(&self, code: u16, reason: &str, was_clean: bool) {
        self.0
            .borrow_mut()
            .push(format!("close {code} {reason:?} {was_clean}"));
    }
}

async fn until(what: &str, mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

/// Accepts one socket, echoing the requested subprotocol as a server must,
/// and returns it with the protocol the client asked for.
// tungstenite's handshake callback returns its own large error type.
#[allow(clippy::result_large_err)]
async fn accept(
    listener: &TcpListener,
) -> (
    tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    Option<String>,
) {
    let (stream, _) = listener.accept().await.unwrap();
    let protocol = Rc::new(RefCell::new(None));
    let p = protocol.clone();
    let ws =
        tokio_tungstenite::accept_hdr_async(stream, move |req: &Request, mut res: Response| {
            if let Some(v) = req.headers().get("Sec-WebSocket-Protocol") {
                *p.borrow_mut() = Some(v.to_str().unwrap().to_owned());
                res.headers_mut()
                    .insert("Sec-WebSocket-Protocol", HeaderValue::from(v));
            }
            Ok(res)
        })
        .await
        .unwrap();
    let protocol = protocol.borrow().clone();
    (ws, protocol)
}

#[tokio::test]
async fn frames_both_ways_and_the_servers_close() {
    LocalSet::new()
        .run_until(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("ws://{}/sync?a=1", listener.local_addr().unwrap());
            let events = Rc::new(Recorder::default());
            let socket = TokioSocketFactory::new().open(&url, Some("proto-1"), events.clone());

            let (mut ws, protocol) = accept(&listener).await;
            assert_eq!(protocol.as_deref(), Some("proto-1"));
            until("open", || events.events() == ["open"]).await;

            socket.send("[\"ping\",{}]");
            let got = ws.next().await.unwrap().unwrap();
            assert_eq!(got, Message::text("[\"ping\",{}]"));

            ws.send(Message::text("[\"pong\",{}]")).await.unwrap();
            ws.send(Message::binary(vec![1u8, 2, 3])).await.unwrap();
            ws.close(Some(CloseFrame {
                code: CloseCode::from(4003),
                reason: "bye".into(),
            }))
            .await
            .unwrap();
            until("the close", || events.events().len() == 4).await;
            assert_eq!(
                events.events(),
                [
                    "open",
                    "text [\"pong\",{}]",
                    "binary [1, 2, 3]",
                    "close 4003 \"bye\" true",
                ]
            );
        })
        .await;
}

#[tokio::test]
async fn closing_sends_the_code_and_reports_nothing() {
    LocalSet::new()
        .run_until(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("ws://{}/", listener.local_addr().unwrap());
            let events = Rc::new(Recorder::default());
            let socket = TokioSocketFactory::new().open(&url, None, events.clone());
            let (mut ws, protocol) = accept(&listener).await;
            assert_eq!(protocol, None);
            until("open", || events.events() == ["open"]).await;

            socket.close(Some(4000));
            match ws.next().await.unwrap().unwrap() {
                Message::Close(Some(frame)) => assert_eq!(u16::from(frame.code), 4000),
                m => panic!("expected a close frame, got {m:?}"),
            }
            ws.send(Message::text("late")).await.ok();
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert_eq!(events.events(), ["open"]);
        })
        .await;
}

#[tokio::test]
async fn a_socket_that_cannot_open_closes_abnormally() {
    LocalSet::new()
        .run_until(async {
            // Bound, then dropped: nothing listens there.
            let addr = TcpListener::bind("127.0.0.1:0")
                .await
                .unwrap()
                .local_addr()
                .unwrap();
            let events = Rc::new(Recorder::default());
            let _socket =
                TokioSocketFactory::new().open(&format!("ws://{addr}/"), None, events.clone());
            until("the close", || !events.events().is_empty()).await;
            let e = events.events();
            assert!(
                e.len() == 1 && e[0].starts_with("close 1006 ") && e[0].ends_with(" false"),
                "{e:?}"
            );
        })
        .await;
}

#[tokio::test]
async fn timers_fire_in_order_after_the_checkpoint_and_cancel() {
    LocalSet::new()
        .run_until(async {
            let s = TokioScheduler::new();
            let log = Rc::new(RefCell::new(Vec::new()));
            let push = |name: &'static str| {
                let log = log.clone();
                Box::new(move || log.borrow_mut().push(name)) as Box<dyn FnOnce()>
            };
            let start = s.monotonic_now();
            let _late = s.schedule(40.0, push("40 ms"));
            let _early = s.schedule(10.0, push("10 ms"));
            let cancelled = s.schedule(20.0, push("cancelled"));
            let ticks = Rc::new(RefCell::new(0));
            let t = ticks.clone();
            let repeating = s.schedule_repeating(
                5.0,
                Box::new(move || {
                    *t.borrow_mut() += 1;
                }),
            );
            s.checkpoint(push("checkpoint"));
            log.borrow_mut().push("sync");
            cancelled.cancel();
            until("the late timer", || log.borrow().len() == 4).await;
            assert_eq!(*log.borrow(), ["sync", "checkpoint", "10 ms", "40 ms"]);
            assert!(s.monotonic_now() - start >= 40.0);
            repeating.cancel();
            let n = *ticks.borrow();
            assert!(n >= 2, "{n} ticks");
            tokio::time::sleep(Duration::from_millis(30)).await;
            assert_eq!(*ticks.borrow(), n, "ticks after cancel");
            // A NaN delay fires at once, as setTimeout's does.
            let fired = Rc::new(RefCell::new(false));
            let f = fired.clone();
            s.schedule(f64::NAN, Box::new(move || *f.borrow_mut() = true));
            until("the NaN timer", || *fired.borrow()).await;
        })
        .await;
}
