//! Times the pushes through a query's source connections for the
//! `query-update-client` metric (`MeasurePushOperator` in
//! `packages/zql/src/query/measure-push-operator.ts`).

use super::query_manager::{ClientMetric, MetricsDelegate, UPDATE_SAMPLE_RATE};
use crate::ivm::{
    Change, FetchRequest, Input, InputBase, NodeStream, Output, SourceSchema, no_output,
};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

/// Passes a source connection's pushes and reconciles on, timing the first
/// of every `UPDATE_SAMPLE_RATE`; the others go straight to the output.
pub(crate) struct MeasurePush {
    me: Weak<MeasurePush>,
    input: Rc<dyn Input>,
    output: RefCell<Weak<dyn Output>>,
    query_id: Rc<str>,
    metrics: Rc<dyn MetricsDelegate>,
    /// Pushes and reconciles so far, sampled or not.
    count: Cell<u64>,
}

/// Records the time since `start` when dropped, so a push that fails is
/// still accounted to its query.
struct Timing<'a> {
    op: &'a MeasurePush,
    start: f64,
}

impl Drop for Timing<'_> {
    fn drop(&mut self) {
        let elapsed = self.op.metrics.monotonic_now() - self.start;
        self.op
            .metrics
            .add_metric(ClientMetric::UpdateClient, elapsed, &self.op.query_id, None);
    }
}

impl MeasurePush {
    pub(crate) fn new(
        input: Rc<dyn Input>,
        query_id: &str,
        metrics: Rc<dyn MetricsDelegate>,
    ) -> Rc<Self> {
        let op = Rc::new_cyclic(|me| MeasurePush {
            me: me.clone(),
            input,
            output: RefCell::new(no_output()),
            query_id: query_id.into(),
            metrics,
            count: Cell::new(0),
        });
        let me: Weak<dyn Output> = op.me.clone();
        op.input.set_output(me);
        op
    }

    fn sample(&self) -> bool {
        let n = self.count.get();
        self.count.set(n.wrapping_add(1));
        n.is_multiple_of(UPDATE_SAMPLE_RATE)
    }

    fn timing(&self) -> Timing<'_> {
        Timing {
            op: self,
            start: self.metrics.monotonic_now(),
        }
    }
}

impl InputBase for MeasurePush {
    fn get_schema(&self) -> Rc<SourceSchema> {
        self.input.get_schema()
    }

    fn destroy(&self) {
        self.input.destroy();
    }
}

impl Input for MeasurePush {
    fn set_output(&self, output: Weak<dyn Output>) {
        *self.output.borrow_mut() = output;
    }

    fn fetch(&self, req: &FetchRequest) -> NodeStream {
        self.input.fetch(req)
    }
}

impl Output for MeasurePush {
    fn push(&self, change: Change, _pusher: &dyn InputBase) {
        let Some(output) = self.output.borrow().upgrade() else {
            return;
        };
        if !self.sample() {
            return output.push(change, self);
        }
        let _timing = self.timing();
        output.push(change, self);
    }

    fn reconcile(&self, _pusher: &dyn InputBase) {
        let Some(output) = self.output.borrow().upgrade() else {
            return;
        };
        // An output without a reconcile phase isn't counted, as in JS.
        if !output.has_reconcile() {
            return;
        }
        if !self.sample() {
            return output.reconcile(self);
        }
        let _timing = self.timing();
        output.reconcile(self);
    }
}
