//! A typed Zero query kept current in a GPUI view: a `LiveView`, whose
//! updates notify the GPUI view.
//!
//! The client calls a view's listeners while it applies a change, which may
//! be inside a GPUI update of the same view, so a listener only sends a
//! wakeup; a foreground task notifies the view afterwards.

use futures::StreamExt;
use futures::channel::mpsc;
use gpui_kit::{Context, Task};
use std::rc::Rc;
use zbugs_model::Context as Auth;
use zero_client::client::{Error, LiveView, Zero};
use zero_client::define::QueryRequest;
use zero_client::ivm::ResultType;
use zero_client::ttl::Ttl;
use zero_client::typed::QueryValue;

pub struct Live<T> {
    /// None when the query's rows didn't read at first.
    live: Option<LiveView<T>>,
    _notify: Task<()>,
}

impl<T: QueryValue + Default + 'static> Live<T> {
    /// Materializes `query` and notifies the view `cx` is for whenever its
    /// rows change, until dropped.
    pub fn new<R: 'static, V: 'static>(
        zero: &Zero<Auth>,
        query: QueryRequest<Auth, R, T>,
        ttl: &Ttl,
        cx: &mut Context<V>,
    ) -> Live<T> {
        let (wake, mut woken) = mpsc::unbounded::<()>();
        let live = match zero.live(&query, ttl) {
            Ok(live) => {
                // Removed when the live view drops.
                let _ = live.add_listener(move |_, _| {
                    let _ = wake.unbounded_send(());
                });
                Some(live)
            }
            // Rows the generated types can't read are a bug of the port:
            // say so, and show none until they read.
            Err(Error::Message(e)) => {
                eprintln!("zbugs-gpui: {}: {e}", query.name());
                None
            }
            Err(_) => panic!("a query of the app's schema materializes"),
        };
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
            live,
            _notify: notify,
        }
    }

    /// The rows last read; none if they never could be.
    pub fn value(&self) -> Rc<T> {
        self.live.as_ref().map_or_else(Rc::default, LiveView::value)
    }

    /// The server has confirmed the rows, on this connection or an earlier
    /// one.
    pub fn is_synced(&self) -> bool {
        self.live
            .as_ref()
            .is_some_and(|l| matches!(l.result_type(), ResultType::Complete | ResultType::Cached))
    }
}
