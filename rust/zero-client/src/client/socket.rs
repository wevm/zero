//! What the engine sees of a WebSocket: it opens one per connection
//! attempt, sends text frames, and is told about frames and the close. This
//! is the shape of the browser `WebSocket` the JS client uses.

use std::rc::Rc;

/// A frame from the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SocketMessage {
    /// A text frame.
    Text(String),
    /// A binary frame.
    Binary(Vec<u8>),
}

/// Opens the sync socket. Every call is on the engine's thread.
pub trait SocketFactory {
    /// Starts opening a WebSocket to `url`, sending `sec_protocol` (when
    /// set) as the `Sec-WebSocket-Protocol` header. Events go to `events`
    /// until the engine closes the socket.
    fn open(
        &self,
        url: &str,
        sec_protocol: Option<&str>,
        events: Rc<dyn SocketEvents>,
    ) -> Box<dyn SocketConnection>;
}

/// An open or opening WebSocket.
pub trait SocketConnection {
    /// Sends a text frame.
    fn send(&self, text: &str);
    /// Closes the socket. No more events are delivered after this.
    fn close(&self, code: Option<u16>);
}

/// Receives the events of one socket.
pub trait SocketEvents {
    /// The socket opened.
    fn did_open(&self);
    /// A frame arrived.
    fn did_receive(&self, message: SocketMessage);
    /// The socket closed, with its close code and reason; `was_clean` when the
    /// closing handshake finished.
    fn did_close(&self, code: u16, reason: &str, was_clean: bool);
}
