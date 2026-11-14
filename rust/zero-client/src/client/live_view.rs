//! A view read as its query's Rust type and kept current (`LiveQuery` in
//! the Swift client, `useQuery` in zero-react).

use super::query_context::MaterializedView;
use super::{Error, QueryError};
use crate::ivm::{ResultType, ViewEntry, ViewValue};
use crate::protocol::ErroredQuery;
use crate::ttl::Ttl;
use crate::typed::{QueryValue, ZeroValueError};
use std::cell::{Cell, RefCell};
use std::fmt;
use std::rc::{Rc, Weak};

/// Why a [`LiveView`]'s value may not be what the server has.
#[derive(Clone, Debug, PartialEq)]
pub enum LiveError {
    /// The server could not run the query.
    Query(ErroredQuery),
    /// The rows could not be read as the query's result type. The value is
    /// the last one read.
    Decode(ZeroValueError),
}

impl fmt::Display for LiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LiveError::Query(e) => QueryError::new(Some(e.clone())).fmt(f),
            LiveError::Decode(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for LiveError {}

type Listener<T> = Rc<dyn Fn(&T, ResultType)>;

/// What a [`LiveView`] last read.
struct State<T> {
    value: Rc<T>,
    /// The entries `value` was read from.
    read: ViewValue,
    /// The view's rows when they were read, which it shares until they
    /// change. Unlike holding them, which would make the view copy them to
    /// change them, it lets them change in place, into another allocation.
    seen: Weak<Vec<ViewEntry>>,
    result_type: ResultType,
    error: Option<LiveError>,
}

fn rows_of(data: &ViewValue) -> Weak<Vec<ViewEntry>> {
    match data {
        ViewValue::Many(rows) => Rc::downgrade(rows),
        _ => Weak::new(),
    }
}

struct Inner<T> {
    view: MaterializedView,
    state: RefCell<State<T>>,
    listeners: RefCell<Vec<(u64, Listener<T>)>>,
    next_listener_id: Cell<u64>,
}

/// A query's rows, the row of a `one()` query, or the value of an
/// aggregate, read as its Rust type and kept current, from
/// [`Zero::live`](super::Zero::live) (`LiveQuery` in the Swift client,
/// `useQuery` in zero-react).
///
/// Each update of the view is read into the value, reading only the rows
/// whose entries changed. When the client replaces itself, the view moves
/// to the new client, and the live view with it. Dropping it removes its
/// listeners and destroys the view; the server keeps syncing the query for
/// its TTL.
pub struct LiveView<T> {
    inner: Rc<Inner<T>>,
    remove: Box<dyn Fn()>,
}

impl<T: QueryValue + 'static> LiveView<T> {
    /// Reads `view`, and follows it. Fails, destroying it, when its rows
    /// don't read as `T`.
    pub(crate) fn new(view: MaterializedView) -> Result<Self, Error> {
        let read = view.data();
        let value = match T::from_view(&read) {
            Ok(value) => value,
            Err(e) => {
                view.destroy();
                return Err(Error::Message(e.0));
            }
        };
        let inner = Rc::new(Inner {
            state: RefCell::new(State {
                value: Rc::new(value),
                seen: rows_of(&read),
                read,
                result_type: view.result_type(),
                error: view.error().map(LiveError::Query),
            }),
            view,
            listeners: RefCell::default(),
            next_listener_id: Cell::new(0),
        });
        let weak: Weak<Inner<T>> = Rc::downgrade(&inner);
        let remove = inner.view.add_listener(move |data, result_type, error| {
            if let Some(inner) = weak.upgrade() {
                inner.update(data, result_type, error);
            }
        });
        Ok(LiveView { inner, remove })
    }
}

impl<T: 'static> LiveView<T> {
    /// The value last read. Holding it past the next update makes that
    /// update read every row again rather than only those that changed.
    pub fn value(&self) -> Rc<T> {
        self.inner.state.borrow().value.clone()
    }

    /// Whether the server has confirmed the rows on this connection
    /// (`Complete`) or an earlier one (`Cached`), or not yet (`Unknown`), or
    /// the query failed (`Error`).
    pub fn result_type(&self) -> ResultType {
        self.inner.state.borrow().result_type
    }

    /// Why the server could not run the query, or why the rows could not be
    /// read as `T`, which keeps the value last read.
    pub fn error(&self) -> Option<LiveError> {
        self.inner.state.borrow().error.clone()
    }

    /// The view the value is read from.
    pub fn view(&self) -> &MaterializedView {
        &self.inner.view
    }

    /// Raises the query's TTL; a lower one is ignored.
    pub fn update_ttl(&self, ttl: &Ttl) {
        self.inner.view.update_ttl(ttl);
    }

    /// Calls `listener` with the value and result type now, and after every
    /// update of the view, once the value is read. After a read that
    /// failed, it gets the value kept, and [`error`](LiveView::error) says
    /// why. Returns the function that removes it.
    pub fn add_listener(&self, listener: impl Fn(&T, ResultType) + 'static) -> Box<dyn Fn()> {
        let id = self.inner.next_listener_id.get();
        self.inner.next_listener_id.set(id + 1);
        let listener: Listener<T> = Rc::new(listener);
        self.inner
            .listeners
            .borrow_mut()
            .push((id, listener.clone()));
        let (value, result_type) = {
            let s = self.inner.state.borrow();
            (s.value.clone(), s.result_type)
        };
        listener(&value, result_type);
        let weak = Rc::downgrade(&self.inner);
        Box::new(move || {
            if let Some(inner) = weak.upgrade() {
                inner.listeners.borrow_mut().retain(|(i, _)| *i != id);
            }
        })
    }
}

impl<T: QueryValue> Inner<T> {
    /// Reads the view's update, and tells the listeners.
    fn update(&self, data: &ViewValue, result_type: ResultType, error: Option<&ErroredQuery>) {
        let value = {
            let mut state = self.state.borrow_mut();
            let s = &mut *state;
            s.result_type = result_type;
            let unchanged = match data {
                ViewValue::Many(rows) => std::ptr::eq(s.seen.as_ptr(), Rc::as_ptr(rows)),
                data => s.read == *data,
            };
            let read = if unchanged {
                Ok(())
            } else {
                let read = match Rc::get_mut(&mut s.value) {
                    Some(value) => value.update_from_view(&mut s.read, data),
                    // A value the app still holds is left as it is.
                    None => T::from_view(data).map(|value| {
                        s.value = Rc::new(value);
                        s.read = data.clone();
                    }),
                };
                if read.is_ok() {
                    s.seen = rows_of(data);
                }
                read
            };
            s.error = match read {
                Ok(()) => error.cloned().map(LiveError::Query),
                Err(e) => Some(LiveError::Decode(e)),
            };
            s.value.clone()
        };
        // Each listener in turn, reading the list again after every call,
        // as `MaterializedView` does.
        let mut last: Option<u64> = None;
        loop {
            let next = self
                .listeners
                .borrow()
                .iter()
                .find(|(id, _)| last.is_none_or(|l| *id > l))
                .cloned();
            let Some((id, listener)) = next else { break };
            last = Some(id);
            listener(&value, result_type);
        }
    }
}

impl<T> Drop for LiveView<T> {
    fn drop(&mut self) {
        (self.remove)();
        self.inner.listeners.borrow_mut().clear();
        self.inner.view.destroy();
    }
}
