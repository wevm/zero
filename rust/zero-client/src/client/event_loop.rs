//! Runs the engine's work in the order the JS client's event loop does.

use super::{Scheduler, Timer};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::{Rc, Weak};

type Task = Box<dyn FnOnce()>;

/// Runs the engine's work in the order the JS client's event loop does.
///
/// The engine is synchronous. Where the JS client continues after a promise
/// settles, the engine queues a microtask. JS runs microtasks once the
/// current task's synchronous code is done and before the next task, so:
///
/// - Timers and socket events are tasks: they run in `dispatch`, which runs
///   the microtasks left by earlier code first and its own before it
///   returns.
/// - Calls from the app run in `call`, which leaves its microtasks for the
///   scheduler's next checkpoint, so that two calls in a row both happen
///   before either's follow-ups, as in JS.
///
/// Work nested in either runs in the outer one's drain.
pub(crate) struct EventLoop {
    me: Weak<EventLoop>,
    queue: RefCell<VecDeque<Task>>,
    after_task: RefCell<Vec<Task>>,
    depth: Cell<u32>,
    checkpoint_requested: Cell<bool>,
    stopped: Cell<bool>,
    /// Microtasks are running.
    draining: Cell<bool>,
    scheduler: Rc<dyn Scheduler>,
}

impl EventLoop {
    pub(crate) fn new(scheduler: Rc<dyn Scheduler>) -> Rc<Self> {
        Rc::new_cyclic(|me| EventLoop {
            me: me.clone(),
            queue: RefCell::default(),
            after_task: RefCell::default(),
            depth: Cell::new(0),
            checkpoint_requested: Cell::new(false),
            stopped: Cell::new(false),
            draining: Cell::new(false),
            scheduler,
        })
    }

    pub(crate) fn microtask(&self, task: impl FnOnce() + 'static) {
        if self.stopped.get() {
            return;
        }
        self.queue.borrow_mut().push_back(Box::new(task));
        if self.depth.get() == 0 {
            self.request_checkpoint();
        }
    }

    /// Runs `work` once the current task or call is done, before its
    /// microtasks. From a microtask, it is queued as a microtask, which runs
    /// once the current one is done: the task the microtask follows has
    /// already finished.
    pub(crate) fn after_current_task(&self, work: impl FnOnce() + 'static) {
        if self.stopped.get() {
            return;
        }
        if self.depth.get() == 0 || self.draining.get() {
            return self.microtask(work);
        }
        self.after_task.borrow_mut().push(Box::new(work));
    }

    fn run_after_task(&self) {
        loop {
            let next = {
                let mut a = self.after_task.borrow_mut();
                if a.is_empty() {
                    break;
                }
                a.remove(0)
            };
            next();
        }
    }

    /// Runs a task: the microtasks already queued, `body`, then the
    /// microtasks it queued.
    pub(crate) fn dispatch<T>(&self, body: impl FnOnce() -> T) -> T {
        if self.depth.get() > 0 {
            return body();
        }
        self.drain();
        self.depth.set(1);
        let result = body();
        self.run_after_task();
        self.drain();
        self.depth.set(0);
        result
    }

    /// Runs a call from the app. Its microtasks run at the next checkpoint
    /// or task, whichever comes first.
    pub(crate) fn call<T>(&self, body: impl FnOnce() -> T) -> T {
        if self.depth.get() > 0 {
            return body();
        }
        self.depth.set(1);
        let result = body();
        self.run_after_task();
        self.depth.set(0);
        if !self.queue.borrow().is_empty() {
            self.request_checkpoint();
        }
        result
    }

    /// Drops the queued microtasks and every later one: the engine they
    /// continue is gone.
    pub(crate) fn stop(&self) {
        self.stopped.set(true);
        self.queue.borrow_mut().clear();
        self.after_task.borrow_mut().clear();
    }

    fn request_checkpoint(&self) {
        if self.checkpoint_requested.replace(true) {
            return;
        }
        let me = self.me.clone();
        self.scheduler.checkpoint(Box::new(move || {
            if let Some(me) = me.upgrade() {
                me.checkpoint_requested.set(false);
                me.dispatch(|| {});
            }
        }));
    }

    fn drain(&self) {
        let was = self.draining.replace(true);
        loop {
            let next = self.queue.borrow_mut().pop_front();
            let Some(task) = next else { break };
            task();
        }
        self.draining.set(was);
    }
}

/// A scheduler whose timers fire inside the event loop's `dispatch`.
pub(crate) struct LoopScheduler {
    pub(crate) base: Rc<dyn Scheduler>,
    pub(crate) event_loop: Weak<EventLoop>,
}

impl Scheduler for LoopScheduler {
    fn now(&self) -> f64 {
        self.base.now()
    }

    fn monotonic_now(&self) -> f64 {
        self.base.monotonic_now()
    }

    fn schedule(&self, ms: f64, action: Box<dyn FnOnce()>) -> Box<dyn Timer> {
        let event_loop = self.event_loop.clone();
        self.base.schedule(
            ms,
            Box::new(move || {
                if let Some(l) = event_loop.upgrade() {
                    l.dispatch(action);
                }
            }),
        )
    }

    fn schedule_repeating(&self, ms: f64, mut action: Box<dyn FnMut()>) -> Box<dyn Timer> {
        let event_loop = self.event_loop.clone();
        self.base.schedule_repeating(
            ms,
            Box::new(move || {
                if let Some(l) = event_loop.upgrade() {
                    l.dispatch(&mut action);
                }
            }),
        )
    }

    fn checkpoint(&self, action: Box<dyn FnOnce()>) {
        self.base.checkpoint(action)
    }
}
