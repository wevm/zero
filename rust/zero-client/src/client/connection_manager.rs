//! The connection status state machine
//! (`packages/zero-client/src/client/connection-manager.ts`).

use super::event_loop::EventLoop;
use super::{ClientErrorKind, ConnectionError, Scheduler, Timer};
use crate::json::{Object, Value, number};
use crate::protocol::{ErrorBody, FailureCause};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

/// The engine's connection state.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum State {
    Initializing,
    Disconnected(ConnectionError),
    Connecting {
        attempt: u32,
        disconnect_at: f64,
        reason: Option<ConnectionError>,
    },
    Connected,
    NeedsAuth(ConnectionError),
    Error(ConnectionError),
    Closed(ConnectionError),
}

impl State {
    pub(crate) fn status(&self) -> ConnectionStatus {
        match self {
            State::Initializing => ConnectionStatus::Initializing,
            State::Disconnected(_) => ConnectionStatus::Disconnected,
            State::Connecting { .. } => ConnectionStatus::Connecting,
            State::Connected => ConnectionStatus::Connected,
            State::NeedsAuth(_) => ConnectionStatus::NeedsAuth,
            State::Error(_) => ConnectionStatus::Error,
            State::Closed(_) => ConnectionStatus::Closed,
        }
    }

    /// needs-auth and error: the run loop pauses until `connect()`.
    pub(crate) fn is_terminal(&self) -> bool {
        matches!(self, State::NeedsAuth(_) | State::Error(_))
    }
}

type Subscriber = Rc<dyn Fn(&State)>;
type Waiter = (Box<dyn Fn() -> bool>, Box<dyn FnOnce(State)>);

pub(crate) struct ConnectionManager {
    me: Weak<ConnectionManager>,
    state: RefCell<State>,
    disconnect_timeout: f64,
    timeout_check_interval: f64,
    scheduler: Rc<dyn Scheduler>,
    event_loop: Weak<EventLoop>,
    subscribers: RefCell<Vec<(u64, Subscriber)>>,
    next_subscriber_id: Cell<u64>,
    state_waiters: RefCell<Vec<Waiter>>,
    connect_requested: Cell<bool>,
    connect_request_waiters: RefCell<Vec<Box<dyn FnOnce()>>>,
    connecting_started_at: Cell<Option<f64>>,
    timeout_timer: RefCell<Option<Box<dyn Timer>>>,
    last_timeout_check_at: Cell<f64>,
}

impl ConnectionManager {
    pub(crate) fn new(
        disconnect_timeout: f64,
        scheduler: Rc<dyn Scheduler>,
        event_loop: Weak<EventLoop>,
    ) -> Rc<Self> {
        let now = scheduler.now();
        Rc::new_cyclic(|me| ConnectionManager {
            me: me.clone(),
            state: RefCell::new(State::Initializing),
            disconnect_timeout,
            timeout_check_interval: 1000.0,
            scheduler,
            event_loop,
            subscribers: RefCell::default(),
            next_subscriber_id: Cell::new(0),
            state_waiters: RefCell::default(),
            connect_requested: Cell::new(false),
            connect_request_waiters: RefCell::default(),
            connecting_started_at: Cell::new(None),
            timeout_timer: RefCell::new(None),
            last_timeout_check_at: Cell::new(now),
        })
    }

    pub(crate) fn state(&self) -> State {
        self.state.borrow().clone()
    }

    pub(crate) fn status(&self) -> ConnectionStatus {
        self.state.borrow().status()
    }

    pub(crate) fn is(&self, status: ConnectionStatus) -> bool {
        self.status() == status
    }

    pub(crate) fn is_in_terminal_state(&self) -> bool {
        self.state.borrow().is_terminal()
    }

    pub(crate) fn should_continue_run_loop(&self) -> bool {
        !self.is(ConnectionStatus::Closed)
    }

    fn microtask(&self, task: impl FnOnce() + 'static) {
        if let Some(l) = self.event_loop.upgrade() {
            l.microtask(task);
        }
    }

    /// Hears every transition synchronously. Returns the unsubscribe.
    pub(crate) fn subscribe(&self, listener: impl Fn(&State) + 'static) -> Box<dyn Fn()> {
        let id = self.next_subscriber_id.get();
        self.next_subscriber_id.set(id + 1);
        self.subscribers.borrow_mut().push((id, Rc::new(listener)));
        let me = self.me.clone();
        Box::new(move || {
            if let Some(me) = me.upgrade() {
                me.subscribers.borrow_mut().retain(|(i, _)| *i != id);
            }
        })
    }

    /// Calls `waiter` in a microtask after the next transition. A waiter
    /// whose `is_current` turns false would do nothing then, and is dropped
    /// when the next one is added: while connected, the run loop waits again
    /// on every ping without a transition.
    pub(crate) fn on_next_state_change(
        &self,
        is_current: impl Fn() -> bool + 'static,
        waiter: impl FnOnce(State) + 'static,
    ) {
        let mut w = self.state_waiters.borrow_mut();
        w.retain(|(current, _)| current());
        w.push((Box::new(is_current), Box::new(waiter)));
    }

    pub(crate) fn request_connect(&self) {
        if self.connect_requested.replace(true) {
            return;
        }
        let waiters = std::mem::take(&mut *self.connect_request_waiters.borrow_mut());
        for w in waiters {
            self.microtask(w);
        }
    }

    /// Calls `waiter` in a microtask once `connect()` is requested (right
    /// away if it already was).
    pub(crate) fn on_connect_request(&self, waiter: impl FnOnce() + 'static) {
        if self.connect_requested.get() {
            self.microtask(waiter);
        } else {
            self.connect_request_waiters
                .borrow_mut()
                .push(Box::new(waiter));
        }
    }

    /// Consumes a pending connect request and resumes connecting.
    pub(crate) fn resume_from_connect_request(&self) -> bool {
        if !self.is_in_terminal_state() || !self.connect_requested.replace(false) {
            return false;
        }
        self.connecting(None);
        true
    }

    /// From initializing to connecting, once the local store is loaded.
    pub(crate) fn initialized(&self) {
        if !matches!(*self.state.borrow(), State::Initializing) {
            return;
        }
        let now = self.scheduler.now();
        self.connecting_started_at.set(Some(now));
        self.set(State::Connecting {
            attempt: 0,
            disconnect_at: now + self.disconnect_timeout,
            reason: None,
        });
        self.maybe_start_timeout_interval();
        self.publish();
    }

    pub(crate) fn connecting(&self, reason: Option<ConnectionError>) {
        let state = self.state();
        let mut hidden_disconnect = false;
        match &state {
            State::Closed(_) => return,
            State::Disconnected(r) => {
                if r.client_kind() != Some(ClientErrorKind::Hidden) {
                    return;
                }
                hidden_disconnect = true;
            }
            _ => {}
        }
        let now = self.scheduler.now();
        if hidden_disconnect {
            self.connecting_started_at.set(None);
        }
        if let State::Connecting {
            attempt,
            disconnect_at,
            ..
        } = state
        {
            self.set(State::Connecting {
                attempt: attempt + 1,
                disconnect_at,
                reason,
            });
            self.maybe_start_timeout_interval();
            self.publish();
            return;
        }
        let started = self.connecting_started_at.get().unwrap_or(now);
        self.connecting_started_at.set(Some(started));
        self.set(State::Connecting {
            attempt: 1,
            disconnect_at: started + self.disconnect_timeout,
            reason,
        });
        self.maybe_start_timeout_interval();
        self.publish();
    }

    pub(crate) fn connected(&self) {
        if matches!(*self.state.borrow(), State::Closed(_) | State::Connected) {
            return;
        }
        self.connecting_started_at.set(None);
        self.maybe_stop_timeout_interval();
        self.set(State::Connected);
        self.publish();
    }

    pub(crate) fn disconnected(&self, reason: ConnectionError) {
        match *self.state.borrow() {
            State::Closed(_) | State::Disconnected(_) => return,
            State::Connected => self.connecting_started_at.set(None),
            _ => {}
        }
        self.maybe_stop_timeout_interval();
        self.set(State::Disconnected(reason));
        self.publish();
    }

    pub(crate) fn needs_auth(&self, reason: ConnectionError) {
        if matches!(*self.state.borrow(), State::Closed(_) | State::NeedsAuth(_)) {
            return;
        }
        self.connecting_started_at.set(None);
        self.maybe_stop_timeout_interval();
        self.set(State::NeedsAuth(reason));
        self.publish();
    }

    pub(crate) fn error(&self, reason: ConnectionError) {
        if matches!(*self.state.borrow(), State::Closed(_) | State::Error(_)) {
            return;
        }
        self.connecting_started_at.set(None);
        self.maybe_stop_timeout_interval();
        self.set(State::Error(reason));
        self.publish();
    }

    pub(crate) fn closed(&self) {
        if matches!(*self.state.borrow(), State::Closed(_)) {
            return;
        }
        self.connecting_started_at.set(None);
        self.maybe_stop_timeout_interval();
        self.set(State::Closed(ConnectionError::client(
            ClientErrorKind::ClientClosed,
            "Zero was explicitly closed by calling zero.close()",
        )));
        self.publish();
        self.subscribers.borrow_mut().clear();
        self.resolve_state_waiters();
    }

    fn set(&self, state: State) {
        *self.state.borrow_mut() = state;
    }

    fn publish(&self) {
        let state = self.state();
        let subscribers: Vec<Subscriber> = self
            .subscribers
            .borrow()
            .iter()
            .map(|(_, s)| s.clone())
            .collect();
        for s in subscribers {
            s(&state);
        }
        self.resolve_state_waiters();
    }

    fn resolve_state_waiters(&self) {
        let state = self.state();
        let waiters = std::mem::take(&mut *self.state_waiters.borrow_mut());
        for (_, w) in waiters {
            let state = state.clone();
            self.microtask(move || w(state));
        }
    }

    /// Pushes the disconnect deadline forward by however long timers were
    /// not running (a suspended app, a sleeping laptop).
    fn credit_frozen_time(&self) {
        let now = self.scheduler.now();
        let elapsed = now - self.last_timeout_check_at.get();
        self.last_timeout_check_at.set(now);
        if elapsed <= 2.0 * self.timeout_check_interval {
            return;
        }
        if let Some(started) = self.connecting_started_at.get() {
            self.connecting_started_at.set(Some(started + elapsed));
        }
        let mut state = self.state.borrow_mut();
        if let State::Connecting { disconnect_at, .. } = &mut *state {
            *disconnect_at += elapsed;
        }
    }

    fn check_timeout(&self) {
        let State::Connecting { disconnect_at, .. } = self.state() else {
            return;
        };
        if self.scheduler.now() >= disconnect_at {
            self.disconnected(ConnectionError::client(
                ClientErrorKind::Offline,
                format!(
                    "Zero was unable to connect for {} seconds and was disconnected",
                    number::format((self.disconnect_timeout / 1000.0).floor())
                ),
            ));
        }
    }

    fn maybe_start_timeout_interval(&self) {
        if self.timeout_timer.borrow().is_some() {
            return;
        }
        self.last_timeout_check_at.set(self.scheduler.now());
        let me = self.me.clone();
        let timer = self.scheduler.schedule_repeating(
            self.timeout_check_interval,
            Box::new(move || {
                if let Some(me) = me.upgrade() {
                    me.credit_frozen_time();
                    me.check_timeout();
                }
            }),
        );
        *self.timeout_timer.borrow_mut() = Some(timer);
    }

    fn maybe_stop_timeout_interval(&self) {
        if let Some(t) = self.timeout_timer.borrow_mut().take() {
            t.cancel();
        }
    }
}

crate::ast::string_enum!(
    /// The status names (`packages/zero-client/src/client/connection-status-enum.ts`).
    ConnectionStatus {
        Initializing = "initializing",
        Disconnected = "disconnected",
        Connecting = "connecting",
        Connected = "connected",
        NeedsAuth = "needs-auth",
        Error = "error",
        Closed = "closed",
    }
);

/// The connection state an app sees (`ConnectionState` in
/// `packages/zero-client/src/client/connection.ts`).
#[derive(Clone, Debug, PartialEq)]
pub enum ConnectionState {
    Initializing,
    Disconnected { reason: String },
    Connecting { reason: Option<String> },
    Connected,
    NeedsAuth(NeedsAuthReason),
    Error { reason: String },
    Closed { reason: String },
}

impl ConnectionState {
    pub fn status(&self) -> ConnectionStatus {
        match self {
            ConnectionState::Initializing => ConnectionStatus::Initializing,
            ConnectionState::Disconnected { .. } => ConnectionStatus::Disconnected,
            ConnectionState::Connecting { .. } => ConnectionStatus::Connecting,
            ConnectionState::Connected => ConnectionStatus::Connected,
            ConnectionState::NeedsAuth(_) => ConnectionStatus::NeedsAuth,
            ConnectionState::Error { .. } => ConnectionStatus::Error,
            ConnectionState::Closed { .. } => ConnectionStatus::Closed,
        }
    }

    pub(crate) fn of(state: &State) -> ConnectionState {
        match state {
            State::Initializing => ConnectionState::Initializing,
            State::Closed(r) => ConnectionState::Closed {
                reason: r.message().to_owned(),
            },
            State::Connected => ConnectionState::Connected,
            State::Connecting { reason, .. } => ConnectionState::Connecting {
                reason: reason
                    .as_ref()
                    .map(|r| r.message().to_owned())
                    .filter(|m| !m.is_empty()),
            },
            State::Disconnected(r) => ConnectionState::Disconnected {
                reason: r.message().to_owned(),
            },
            State::Error(r) => ConnectionState::Error {
                reason: r.message().to_owned(),
            },
            State::NeedsAuth(r) => ConnectionState::NeedsAuth(NeedsAuthReason::of(r)),
        }
    }

    /// The JSON form of the JS `ConnectionState`.
    pub fn to_json(&self) -> Value {
        let mut o = Object::new();
        o.insert("name", Value::from(self.status().as_str()));
        match self {
            ConnectionState::Initializing | ConnectionState::Connected => {}
            ConnectionState::Connecting { reason } => {
                if let Some(r) = reason {
                    o.insert("reason", Value::from(r.as_str()));
                }
            }
            ConnectionState::Disconnected { reason }
            | ConnectionState::Error { reason }
            | ConnectionState::Closed { reason } => {
                o.insert("reason", Value::from(reason.as_str()));
            }
            ConnectionState::NeedsAuth(r) => {
                o.insert("reason", r.to_json());
            }
        }
        Value::Object(o)
    }
}

/// Why the client needs new credentials.
#[derive(Clone, Debug, PartialEq)]
pub enum NeedsAuthReason {
    /// The API server answered a push with 401 or 403.
    Mutate { status: f64, body: Option<String> },
    /// The API server answered a query transform with 401 or 403.
    Query { status: f64, body: Option<String> },
    /// zero-cache rejected the credentials.
    ZeroCache { reason: String },
}

impl NeedsAuthReason {
    fn of(error: &ConnectionError) -> NeedsAuthReason {
        if let ConnectionError::Server(body) = error {
            let http = |cause: &FailureCause| match cause {
                FailureCause::Http {
                    status,
                    body_preview,
                } => Some((*status, body_preview.clone().filter(|b| !b.is_empty()))),
                _ => None,
            };
            match &**body {
                ErrorBody::PushFailed { cause, .. } => {
                    if let Some((status, body)) = http(cause) {
                        return NeedsAuthReason::Mutate { status, body };
                    }
                }
                ErrorBody::TransformFailed { cause, .. } => {
                    if let Some((status, body)) = http(cause) {
                        return NeedsAuthReason::Query { status, body };
                    }
                }
                _ => {}
            }
        }
        NeedsAuthReason::ZeroCache {
            reason: error.message().to_owned(),
        }
    }

    pub fn to_json(&self) -> Value {
        let mut o = Object::new();
        match self {
            NeedsAuthReason::Mutate { status, body } | NeedsAuthReason::Query { status, body } => {
                let ty = if matches!(self, NeedsAuthReason::Mutate { .. }) {
                    "mutate"
                } else {
                    "query"
                };
                o.insert("type", Value::from(ty));
                o.insert("status", Value::from(*status));
                if let Some(b) = body {
                    o.insert("body", Value::from(b.as_str()));
                }
            }
            NeedsAuthReason::ZeroCache { reason } => {
                o.insert("type", Value::from("zero-cache"));
                o.insert("reason", Value::from(reason.as_str()));
            }
        }
        Value::Object(o)
    }
}
