//! The clock and timers the engine runs on, and values that arrive later.

use super::Error;
use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

/// The clock and timers the engine runs on: `Date.now()`,
/// `performance.now()` and `setTimeout` in JS. Tests use
/// [`ManualScheduler`](crate::testing::ManualScheduler) so a scenario runs
/// the same way every time; an app's transport provides one that runs on
/// its event loop.
///
/// The engine is single-threaded: every action runs on the thread that
/// made the client.
pub trait Scheduler {
    /// Milliseconds since 1970 (`Date.now()`).
    fn now(&self) -> f64;
    /// Milliseconds on a monotonic clock (`performance.now()`).
    fn monotonic_now(&self) -> f64;
    /// Calls `action` after `ms` milliseconds.
    fn schedule(&self, ms: f64, action: Box<dyn FnOnce()>) -> Box<dyn Timer>;
    /// Calls `action` every `ms` milliseconds until cancelled (`setInterval`).
    fn schedule_repeating(&self, ms: f64, action: Box<dyn FnMut()>) -> Box<dyn Timer>;
    /// Calls `action` once the caller's synchronous code is done, before any
    /// timer: where JS runs microtasks. The engine queues its follow-ups to
    /// API calls here.
    fn checkpoint(&self, action: Box<dyn FnOnce()>);
}

/// A timer from [`Scheduler::schedule`].
pub trait Timer {
    /// Stops the timer from firing. Does nothing once it has fired.
    fn cancel(&self);
}

/// The delay `setTimeout` and `setInterval` wait for `ms`: none for NaN, a
/// negative delay, or one past 2^31 - 1 ms, which browsers and Node fire at
/// once.
pub fn timer_delay(ms: f64) -> f64 {
    if (0.0..=2_147_483_647.0).contains(&ms) {
        ms
    } else {
        0.0
    }
}

type Observer<T> = Box<dyn FnOnce(&Result<T, Error>)>;

struct DeferredState<T> {
    result: Option<Result<T, Error>>,
    observers: Vec<Observer<T>>,
    wakers: Vec<Waker>,
}

/// A value that arrives later, like a JS promise: the first `resolve` or
/// `reject` wins. [`result`](Self::result) reads it without waiting; a
/// `Deferred` is also a future that waits for it.
pub struct Deferred<T>(Rc<RefCell<DeferredState<T>>>);

impl<T> Clone for Deferred<T> {
    fn clone(&self) -> Self {
        Deferred(self.0.clone())
    }
}

impl<T: Clone + 'static> Default for Deferred<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Clone + 'static> Deferred<T> {
    /// A deferred value that is not settled yet.
    pub fn new() -> Self {
        Deferred(Rc::new(RefCell::new(DeferredState {
            result: None,
            observers: Vec::new(),
            wakers: Vec::new(),
        })))
    }

    /// The result, once settled.
    pub fn result(&self) -> Option<Result<T, Error>> {
        self.0.borrow().result.clone()
    }

    /// Whether it has settled.
    pub fn is_settled(&self) -> bool {
        self.0.borrow().result.is_some()
    }

    pub(crate) fn resolve(&self, value: T) {
        self.settle(Ok(value));
    }

    pub(crate) fn reject(&self, error: impl Into<Error>) {
        self.settle(Err(error.into()));
    }

    pub(crate) fn settle(&self, result: Result<T, Error>) {
        let (observers, wakers) = {
            let mut s = self.0.borrow_mut();
            if s.result.is_some() {
                return;
            }
            s.result = Some(result.clone());
            (
                std::mem::take(&mut s.observers),
                std::mem::take(&mut s.wakers),
            )
        };
        for o in observers {
            o(&result);
        }
        for w in wakers {
            w.wake();
        }
    }

    /// Calls `observer` once settled (right away if it already is).
    pub fn on_settle(&self, observer: impl FnOnce(&Result<T, Error>) + 'static) {
        let result = self.0.borrow().result.clone();
        match result {
            Some(r) => observer(&r),
            None => self.0.borrow_mut().observers.push(Box::new(observer)),
        }
    }
}

impl<T: Clone + 'static> Future for Deferred<T> {
    type Output = Result<T, Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut s = self.0.borrow_mut();
        match &s.result {
            Some(r) => Poll::Ready(r.clone()),
            None => {
                s.wakers.push(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}
