//! Helpers for testing an app on Zero without a server or a real clock
//! (`ZeroTesting` in the Swift client): a clock that only moves when told
//! to, in-memory sockets a test plays the server on, and a log sink that
//! keeps what was logged.

use crate::client::{
    LogLevel, LogSink, Scheduler, SocketConnection, SocketEvents, SocketFactory, SocketMessage,
    Timer, timer_delay,
};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

enum Action {
    Once(Option<Box<dyn FnOnce()>>),
    Repeat(Option<Box<dyn FnMut()>>),
}

struct ManualTimer {
    sequence: u64,
    due: Cell<f64>,
    interval: Option<f64>,
    action: RefCell<Action>,
    cancelled: Cell<bool>,
}

struct TimerHandle(Rc<ManualTimer>);

impl Timer for TimerHandle {
    fn cancel(&self) {
        self.0.cancelled.set(true);
    }
}

/// A clock that only moves when told to, like fake timers in JS tests.
///
/// Timers run in order of due time, then of creation; a repeating timer
/// keeps its place among timers due at the same time (`setInterval` keeps
/// its timer ID).
pub struct ManualScheduler {
    now: Cell<f64>,
    monotonic_now: Cell<f64>,
    timers: RefCell<Vec<Rc<ManualTimer>>>,
    checkpoints: RefCell<Vec<Box<dyn FnOnce()>>>,
    next_sequence: Cell<u64>,
}

impl ManualScheduler {
    /// A clock at `now`, in milliseconds since 1970.
    pub fn new(now: f64) -> Rc<Self> {
        Rc::new(ManualScheduler {
            now: Cell::new(now),
            monotonic_now: Cell::new(0.0),
            timers: RefCell::default(),
            checkpoints: RefCell::default(),
            next_sequence: Cell::new(0),
        })
    }

    fn add(&self, due: f64, interval: Option<f64>, action: Action) -> Box<dyn Timer> {
        let sequence = self.next_sequence.get() + 1;
        self.next_sequence.set(sequence);
        let timer = Rc::new(ManualTimer {
            sequence,
            due: Cell::new(due),
            interval,
            action: RefCell::new(action),
            cancelled: Cell::new(false),
        });
        self.timers.borrow_mut().push(timer.clone());
        Box::new(TimerHandle(timer))
    }

    /// Runs the pending checkpoints, as JS runs microtasks before a timer.
    pub fn run_checkpoints(&self) {
        loop {
            let next = {
                let mut c = self.checkpoints.borrow_mut();
                if c.is_empty() {
                    break;
                }
                c.remove(0)
            };
            next();
        }
    }

    /// The number of timers that have not fired or been cancelled.
    pub fn pending_timers(&self) -> usize {
        self.timers
            .borrow()
            .iter()
            .filter(|t| !t.cancelled.get())
            .count()
    }

    fn next_due(&self, limit: f64) -> Option<Rc<ManualTimer>> {
        self.run_checkpoints();
        let mut timers = self.timers.borrow_mut();
        timers.retain(|t| !t.cancelled.get());
        let mut best: Option<&Rc<ManualTimer>> = None;
        for t in timers.iter() {
            if t.due.get() > limit {
                continue;
            }
            if let Some(b) = best
                && (b.due.get(), b.sequence) <= (t.due.get(), t.sequence)
            {
                continue;
            }
            best = Some(t);
        }
        best.cloned()
    }

    fn fire(&self, timer: &ManualTimer) {
        let elapsed = timer.due.get() - self.monotonic_now.get();
        if elapsed > 0.0 {
            self.monotonic_now.set(timer.due.get());
            self.now.set(self.now.get() + elapsed);
        }
        match timer.interval {
            Some(interval) => timer.due.set(timer.due.get() + interval),
            None => timer.cancelled.set(true),
        }
        let taken = match &mut *timer.action.borrow_mut() {
            Action::Once(f) => f.take().map(|f| Action::Once(Some(f))),
            Action::Repeat(f) => f.take().map(|f| Action::Repeat(Some(f))),
        };
        match taken {
            Some(Action::Once(Some(f))) => f(),
            Some(Action::Repeat(Some(mut f))) => {
                f();
                *timer.action.borrow_mut() = Action::Repeat(Some(f));
            }
            _ => {}
        }
    }

    /// Moves the clock forward by `ms`, firing every timer that comes due
    /// on the way, including ones those timers schedule.
    pub fn advance(&self, ms: f64) {
        let target = self.monotonic_now.get() + ms;
        while let Some(timer) = self.next_due(target) {
            self.fire(&timer);
        }
        self.now
            .set(self.now.get() + target - self.monotonic_now.get());
        self.monotonic_now.set(target);
    }

    /// Fires the timers due now, and the zero-delay timers they schedule,
    /// until none is left.
    pub fn run_due_timers(&self) {
        let mut count = 0;
        while let Some(timer) = self.next_due(self.monotonic_now.get()) {
            self.fire(&timer);
            count += 1;
            assert!(count < 100_000, "Timers keep scheduling timers");
        }
    }
}

impl Scheduler for ManualScheduler {
    fn now(&self) -> f64 {
        self.now.get()
    }

    fn monotonic_now(&self) -> f64 {
        self.monotonic_now.get()
    }

    fn schedule(&self, ms: f64, action: Box<dyn FnOnce()>) -> Box<dyn Timer> {
        self.add(
            self.monotonic_now.get() + timer_delay(ms),
            None,
            Action::Once(Some(action)),
        )
    }

    fn schedule_repeating(&self, ms: f64, action: Box<dyn FnMut()>) -> Box<dyn Timer> {
        let interval = timer_delay(ms).max(1.0);
        self.add(
            self.monotonic_now.get() + interval,
            Some(interval),
            Action::Repeat(Some(action)),
        )
    }

    /// Runs `action` before the next timer fires, or at the next
    /// [`run_checkpoints`](Self::run_checkpoints).
    fn checkpoint(&self, action: Box<dyn FnOnce()>) {
        self.checkpoints.borrow_mut().push(action);
    }
}

/// Opens in-memory sockets and keeps every one, in opening order, so a test
/// can play the server (`MockSocket` in
/// `packages/zero-client/src/client/test-utils.ts`).
#[derive(Default)]
pub struct MockSocketFactory {
    sockets: RefCell<Vec<Rc<MockSocket>>>,
}

impl MockSocketFactory {
    /// A factory with no sockets yet.
    pub fn new() -> Rc<Self> {
        Rc::default()
    }

    /// The sockets opened so far, oldest first.
    pub fn sockets(&self) -> Vec<Rc<MockSocket>> {
        self.sockets.borrow().clone()
    }
}

impl SocketFactory for MockSocketFactory {
    fn open(
        &self,
        url: &str,
        sec_protocol: Option<&str>,
        events: Rc<dyn SocketEvents>,
    ) -> Box<dyn SocketConnection> {
        let socket = Rc::new(MockSocket {
            url: url.to_owned(),
            sec_protocol: sec_protocol.map(str::to_owned),
            messages: RefCell::default(),
            closed: Cell::new(false),
            close_code: Cell::new(None),
            events: RefCell::new(Rc::downgrade(&events)),
        });
        self.sockets.borrow_mut().push(socket.clone());
        Box::new(MockConnection(socket))
    }
}

struct MockConnection(Rc<MockSocket>);

impl SocketConnection for MockConnection {
    fn send(&self, text: &str) {
        self.0.messages.borrow_mut().push(text.to_owned());
    }

    fn close(&self, code: Option<u16>) {
        self.0.closed.set(true);
        self.0.close_code.set(code);
    }
}

/// A socket the test drives: it records what the client sends and delivers
/// what the test receives on the server's behalf.
pub struct MockSocket {
    /// The URL the client opened.
    pub url: String,
    /// The `Sec-WebSocket-Protocol` header it sent.
    pub sec_protocol: Option<String>,
    messages: RefCell<Vec<String>>,
    closed: Cell<bool>,
    close_code: Cell<Option<u16>>,
    events: RefCell<Weak<dyn SocketEvents>>,
}

impl MockSocket {
    /// The text frames the client sent, oldest first.
    pub fn messages(&self) -> Vec<String> {
        self.messages.borrow().clone()
    }

    /// Whether the client closed the socket.
    pub fn closed(&self) -> bool {
        self.closed.get()
    }

    /// The code the client closed the socket with.
    pub fn close_code(&self) -> Option<u16> {
        self.close_code.get()
    }

    fn events(&self) -> Option<Rc<dyn SocketEvents>> {
        self.events.borrow().upgrade()
    }

    /// Delivers a frame from the server.
    pub fn receive(&self, message: SocketMessage) {
        if let Some(e) = self.events() {
            e.did_receive(message);
        }
    }

    /// Delivers a text frame from the server.
    pub fn receive_text(&self, text: &str) {
        self.receive(SocketMessage::Text(text.to_owned()));
    }

    /// The socket closes from the server or network side.
    pub fn server_close(&self, code: u16, reason: &str, was_clean: bool) {
        if let Some(e) = self.events() {
            e.did_close(code, reason, was_clean);
        }
    }

    /// The socket opens.
    pub fn open(&self) {
        if let Some(e) = self.events() {
            e.did_open();
        }
    }
}

/// Keeps every message logged, with its level.
#[derive(Default)]
pub struct RecordingLogSink {
    entries: RefCell<Vec<(LogLevel, String)>>,
}

impl RecordingLogSink {
    /// What was logged, oldest first.
    pub fn entries(&self) -> Vec<(LogLevel, String)> {
        self.entries.borrow().clone()
    }
}

impl LogSink for RecordingLogSink {
    fn log(&self, level: LogLevel, message: &str, _context: &[(String, String)]) {
        self.entries.borrow_mut().push((level, message.to_owned()));
    }
}
