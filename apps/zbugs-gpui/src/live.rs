//! A typed Zero query kept current in a GPUI view: the view's rows are read
//! when the client's view changes, and the GPUI view is notified then.
//!
//! The client calls a view's listeners while it applies a change, which may
//! be inside a GPUI update of the same view, so a listener only stores the
//! rows and sends a wakeup; a foreground task notifies the view afterwards.

use futures::StreamExt;
use futures::channel::mpsc;
use gpui_kit::{Context, Task};
use std::cell::RefCell;
use std::rc::Rc;
use zbugs_model::Context as Auth;
use zero_client::client::{MaterializedView, Zero};
use zero_client::ivm::ResultType;
use zero_client::ttl::Ttl;
use zero_client::typed::{Query, QueryValue};

pub struct Live<T> {
    value: Rc<RefCell<Snapshot<T>>>,
    view: MaterializedView,
    remove: Box<dyn Fn()>,
    _notify: Task<()>,
}

pub struct Snapshot<T> {
    pub value: Option<T>,
    pub result_type: ResultType,
    pub error: Option<String>,
}

impl<T: QueryValue + 'static> Live<T> {
    /// Materializes `query` and notifies the view `cx` is for whenever its
    /// rows change, until dropped.
    pub fn new<R: 'static, V: 'static>(
        zero: &Zero<Auth>,
        query: Query<R, T>,
        ttl: &Ttl,
        cx: &mut Context<V>,
    ) -> Live<T> {
        let value = Rc::new(RefCell::new(Snapshot {
            value: None,
            result_type: ResultType::Unknown,
            error: None,
        }));
        let (wake, mut woken) = mpsc::unbounded::<()>();
        let view = zero
            .materialize(query.any(), ttl)
            .expect("a query of the app's schema materializes");
        let v = value.clone();
        let remove = view.add_listener(move |data, result_type, _| {
            let mut s = v.borrow_mut();
            match query.read(data) {
                Ok(t) => {
                    s.value = Some(t);
                    s.error = None;
                }
                Err(e) => s.error = Some(e.0),
            }
            s.result_type = result_type;
            let _ = wake.unbounded_send(());
        });
        let notify = cx.spawn(async move |this, cx| {
            while woken.next().await.is_some() {
                // Several changes at once notify once.
                while woken.try_recv().is_ok() {}
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
            }
        });
        Live {
            value,
            view,
            remove,
            _notify: notify,
        }
    }

    pub fn snapshot(&self) -> std::cell::Ref<'_, Snapshot<T>> {
        self.value.borrow()
    }

    /// The server has confirmed the rows, on this connection or an earlier
    /// one.
    pub fn is_synced(&self) -> bool {
        matches!(
            self.value.borrow().result_type,
            ResultType::Complete | ResultType::Cached
        )
    }
}

impl<T> Drop for Live<T> {
    fn drop(&mut self) {
        (self.remove)();
        self.view.destroy();
    }
}
