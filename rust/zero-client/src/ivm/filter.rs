//! The `where` sub-graph: `Filter`, `FilterStart`, `FilterEnd`, `Exists`,
//! `FanOut`, and `FanIn` (`packages/zql/src/ivm/filter.ts`,
//! `filter-operators.ts`, `exists.ts`, `fan-out.ts`, and `fan-in.ts`).
//!
//! Filter operators have no `fetch`: a fetch through the sub-graph enters at
//! `FilterStart`, which asks the chain whether each node passes.

use super::*;
use crate::ast::{Condition, CorrelatedSubqueryOperator};
use crate::json::{self, Value};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

fn upgrade<T: ?Sized>(w: &RefCell<Weak<T>>) -> Option<Rc<T>> {
    w.borrow().upgrade()
}

/// Filters through a pure predicate (`filter.ts`).
pub struct Filter {
    input: Rc<dyn FilterInput>,
    predicate: RowPredicate,
    output: RefCell<Weak<dyn FilterOutput>>,
}

impl Filter {
    pub fn new(input: Rc<dyn FilterInput>, predicate: RowPredicate) -> Rc<Self> {
        let op = Rc::new(Filter {
            input: input.clone(),
            predicate,
            output: RefCell::new(no_filter_output()),
        });
        input.set_filter_output(Rc::downgrade(&op) as Weak<dyn FilterOutput>);
        op
    }
}

impl InputBase for Filter {
    fn get_schema(&self) -> Rc<SourceSchema> {
        self.input.get_schema()
    }
    fn destroy(&self) {
        self.input.destroy()
    }
}

impl FilterInput for Filter {
    fn set_filter_output(&self, output: Weak<dyn FilterOutput>) {
        *self.output.borrow_mut() = output;
    }
}

impl Output for Filter {
    fn push(&self, change: Change, _pusher: &dyn InputBase) {
        if let Some(out) = upgrade(&self.output) {
            filter_push(change, out.as_ref(), self, Some(&self.predicate));
        }
    }
    fn reconcile(&self, _pusher: &dyn InputBase) {
        if let Some(out) = upgrade(&self.output) {
            out.reconcile(self);
        }
    }
}

impl FilterOutput for Filter {
    fn begin_filter(&self) {
        if let Some(out) = upgrade(&self.output) {
            out.begin_filter();
        }
    }
    fn end_filter(&self) {
        if let Some(out) = upgrade(&self.output) {
            out.end_filter();
        }
    }
    fn filter(&self, node: &Node) -> bool {
        (self.predicate)(&node.row) && upgrade(&self.output).is_some_and(|o| o.filter(node))
    }
}

/// Adapts an `Input` to the `where` sub-graph (`filter-operators.ts`).
pub struct FilterStart {
    me: Weak<FilterStart>,
    input: Rc<dyn Input>,
    condition: Option<Condition>,
    output: RefCell<Weak<dyn FilterOutput>>,
}

impl FilterStart {
    pub fn new(input: Rc<dyn Input>, condition: Option<Condition>) -> Rc<Self> {
        let op = Rc::new_cyclic(|me| FilterStart {
            me: me.clone(),
            input: input.clone(),
            condition,
            output: RefCell::new(no_filter_output()),
        });
        input.set_output(Rc::downgrade(&op) as Weak<dyn Output>);
        op
    }

    pub fn fetch(&self, req: &FetchRequest) -> NodeStream {
        let me = self.me.clone();
        let req = req.clone();
        lazy(move || -> NodeStream {
            let Some(me) = me.upgrade() else {
                return empty();
            };
            let mut child = req.clone();
            child.filter = merge_filters(req.filter.as_ref(), me.condition.as_ref());
            let output = me.output.borrow().clone();
            Box::new(FilterIterator::new(me.input.fetch(&child), output))
        })
    }
}

impl InputBase for FilterStart {
    fn get_schema(&self) -> Rc<SourceSchema> {
        self.input.get_schema()
    }
    fn destroy(&self) {
        self.input.destroy()
    }
}

impl FilterInput for FilterStart {
    fn set_filter_output(&self, output: Weak<dyn FilterOutput>) {
        *self.output.borrow_mut() = output;
    }
}

impl Output for FilterStart {
    fn push(&self, change: Change, _pusher: &dyn InputBase) {
        if let Some(out) = upgrade(&self.output) {
            out.push(change, self);
        }
    }
    fn reconcile(&self, _pusher: &dyn InputBase) {
        if let Some(out) = upgrade(&self.output) {
            out.reconcile(self);
        }
    }
}

/// `FilterStart.fetch`'s generator: `beginFilter`, then `endFilter` once the
/// stream ends or is dropped (the JS `finally`).
struct FilterIterator {
    input: Option<NodeStream>,
    output: Weak<dyn FilterOutput>,
}

impl FilterIterator {
    fn new(input: NodeStream, output: Weak<dyn FilterOutput>) -> Self {
        if let Some(o) = output.upgrade() {
            o.begin_filter();
        }
        FilterIterator {
            input: Some(input),
            output,
        }
    }

    fn finish(&mut self) {
        if self.input.take().is_some()
            && let Some(o) = self.output.upgrade()
        {
            o.end_filter();
        }
    }
}

impl Iterator for FilterIterator {
    type Item = Node;

    fn next(&mut self) -> Option<Node> {
        while let Some(node) = self.input.as_mut()?.next() {
            if self.output.upgrade().is_some_and(|o| o.filter(&node)) {
                return Some(node);
            }
        }
        self.finish();
        None
    }
}

impl Drop for FilterIterator {
    fn drop(&mut self) {
        self.finish();
    }
}

/// The request's filter and the operator's own, ANDed.
pub fn merge_filters(req: Option<&Condition>, own: Option<&Condition>) -> Option<Condition> {
    match (req, own) {
        (r, None) => r.cloned(),
        (None, Some(o)) => Some(o.clone()),
        (Some(r), Some(o)) => Some(Condition::And(vec![r.clone(), o.clone()])),
    }
}

/// Adapts the end of the `where` sub-graph back to an `Input`.
pub struct FilterEnd {
    start: Rc<FilterStart>,
    input: Rc<dyn FilterInput>,
    output: RefCell<Weak<dyn Output>>,
}

impl FilterEnd {
    pub fn new(start: Rc<FilterStart>, input: Rc<dyn FilterInput>) -> Rc<Self> {
        let op = Rc::new(FilterEnd {
            start,
            input: input.clone(),
            output: RefCell::new(no_output()),
        });
        input.set_filter_output(Rc::downgrade(&op) as Weak<dyn FilterOutput>);
        op
    }
}

impl InputBase for FilterEnd {
    fn get_schema(&self) -> Rc<SourceSchema> {
        self.input.get_schema()
    }
    fn destroy(&self) {
        self.input.destroy()
    }
}

impl Input for FilterEnd {
    fn set_output(&self, output: Weak<dyn Output>) {
        *self.output.borrow_mut() = output;
    }
    fn fetch(&self, req: &FetchRequest) -> NodeStream {
        self.start.fetch(req)
    }
}

impl Output for FilterEnd {
    fn push(&self, change: Change, _pusher: &dyn InputBase) {
        if let Some(out) = upgrade(&self.output) {
            out.push(change, self);
        }
    }
    fn reconcile(&self, _pusher: &dyn InputBase) {
        if let Some(out) = upgrade(&self.output) {
            out.reconcile(self);
        }
    }
}

impl FilterOutput for FilterEnd {
    fn begin_filter(&self) {}
    fn end_filter(&self) {}
    fn filter(&self, _node: &Node) -> bool {
        true
    }
}

/// Filters on whether a relationship is non-empty (`exists.ts`).
pub struct Exists {
    input: Rc<dyn FilterInput>,
    relationship_name: String,
    not: bool,
    parent_join_key: Vec<String>,
    no_size_reuse: bool,
    cache: RefCell<HashMap<String, bool>>,
    output: RefCell<Weak<dyn FilterOutput>>,
    /// Set while processing a push, when relationships can be inconsistent,
    /// to stop reusing cached sizes across rows with the same join key.
    in_push: Cell<bool>,
}

impl Exists {
    pub fn new(
        input: Rc<dyn FilterInput>,
        relationship_name: impl Into<String>,
        parent_join_key: Vec<String>,
        op: CorrelatedSubqueryOperator,
    ) -> Rc<Self> {
        let relationship_name = relationship_name.into();
        let schema = input.get_schema();
        assert!(
            schema.relationship(&relationship_name).is_some(),
            "Input schema missing {relationship_name}"
        );
        let op = Rc::new(Exists {
            input: input.clone(),
            no_size_reuse: parent_join_key == schema.primary_key,
            relationship_name,
            not: op == CorrelatedSubqueryOperator::NotExists,
            parent_join_key,
            cache: RefCell::default(),
            output: RefCell::new(no_filter_output()),
            in_push: Cell::new(false),
        });
        input.set_filter_output(Rc::downgrade(&op) as Weak<dyn FilterOutput>);
        op
    }

    fn passes(&self, node: &Node, exists: Option<bool>) -> bool {
        let e = exists.unwrap_or_else(|| self.fetch_size(node) > 0);
        e != self.not
    }

    fn cache_key(&self, node: &Node) -> String {
        let values: Vec<Value> = self
            .parent_join_key
            .iter()
            .map(|k| node.row.get(k).cloned().unwrap_or(Value::Null))
            .collect();
        json::stringify(&Value::Array(values))
    }

    fn push_with_filter(&self, change: Change, exists: Option<bool>) {
        if self.passes(change.node(), exists)
            && let Some(out) = upgrade(&self.output)
        {
            out.push(change, self);
        }
    }

    fn fetch_size(&self, node: &Node) -> usize {
        let Some(relationship) = node.relationships.get(&self.relationship_name) else {
            panic!(
                "Exists: relationship \"{}\" not found on node",
                self.relationship_name
            );
        };
        relationship().count()
    }

    fn push_to_output(&self, change: Change) {
        if let Some(out) = upgrade(&self.output) {
            out.push(change, self);
        }
    }
}

impl InputBase for Exists {
    fn get_schema(&self) -> Rc<SourceSchema> {
        self.input.get_schema()
    }
    fn destroy(&self) {
        self.input.destroy()
    }
}

impl FilterInput for Exists {
    fn set_filter_output(&self, output: Weak<dyn FilterOutput>) {
        *self.output.borrow_mut() = output;
    }
}

impl Output for Exists {
    fn push(&self, change: Change, _pusher: &dyn InputBase) {
        assert!(!self.in_push.get(), "Unexpected re-entrancy");
        self.in_push.set(true);
        struct Reset<'a>(&'a Cell<bool>);
        impl Drop for Reset<'_> {
            fn drop(&mut self) {
                self.0.set(false);
            }
        }
        let _reset = Reset(&self.in_push);
        let Change::Child(node, child) = &change else {
            return self.push_with_filter(change, None);
        };
        if child.relationship_name != self.relationship_name
            || matches!(child.change, Change::Edit { .. } | Change::Child(..))
        {
            return self.push_with_filter(change, None);
        }
        let name = self.relationship_name.as_str();
        match &child.change {
            Change::Add(_) => {
                let size = self.fetch_size(node);
                if size == 1 {
                    if self.not {
                        let gone = node.clone().with(name, Rc::new(empty));
                        self.push_to_output(Change::Remove(gone));
                    } else {
                        self.push_to_output(Change::Add(node.clone()));
                    }
                } else {
                    self.push_with_filter(change.clone(), Some(size > 0));
                }
            }
            Change::Remove(removed) => {
                let size = self.fetch_size(node);
                if size == 0 {
                    if self.not {
                        self.push_to_output(Change::Add(node.clone()));
                    } else {
                        let removed = removed.clone();
                        let gone = node
                            .clone()
                            .with(name, Rc::new(move || from_vec(vec![removed.clone()])));
                        self.push_to_output(Change::Remove(gone));
                    }
                } else {
                    self.push_with_filter(change.clone(), Some(size > 0));
                }
            }
            _ => {}
        }
    }

    fn reconcile(&self, _pusher: &dyn InputBase) {
        if let Some(out) = upgrade(&self.output) {
            out.reconcile(self);
        }
    }
}

impl FilterOutput for Exists {
    fn begin_filter(&self) {
        if let Some(out) = upgrade(&self.output) {
            out.begin_filter();
        }
    }

    fn end_filter(&self) {
        self.cache.borrow_mut().clear();
        if let Some(out) = upgrade(&self.output) {
            out.end_filter();
        }
    }

    fn filter(&self, node: &Node) -> bool {
        let mut exists = None;
        if !self.no_size_reuse && !self.in_push.get() {
            let key = self.cache_key(node);
            let cached = self.cache.borrow().get(&key).copied();
            exists = Some(cached.unwrap_or_else(|| {
                let e = self.fetch_size(node) > 0;
                self.cache.borrow_mut().insert(key, e);
                e
            }));
        }
        self.passes(node, exists) && upgrade(&self.output).is_some_and(|o| o.filter(node))
    }
}

/// Forks the `where` sub-graph for an OR (`fan-out.ts`).
pub struct FanOut {
    input: Rc<dyn FilterInput>,
    outputs: RefCell<Vec<Weak<dyn FilterOutput>>>,
    fan_in: RefCell<Weak<FanIn>>,
    destroy_count: Cell<usize>,
}

impl FanOut {
    pub fn new(input: Rc<dyn FilterInput>) -> Rc<Self> {
        let op = Rc::new(FanOut {
            input: input.clone(),
            outputs: RefCell::default(),
            fan_in: RefCell::new(Weak::new()),
            destroy_count: Cell::new(0),
        });
        input.set_filter_output(Rc::downgrade(&op) as Weak<dyn FilterOutput>);
        op
    }

    pub fn set_fan_in(&self, fan_in: &Rc<FanIn>) {
        *self.fan_in.borrow_mut() = Rc::downgrade(fan_in);
    }

    fn outputs(&self) -> Vec<Rc<dyn FilterOutput>> {
        self.outputs
            .borrow()
            .iter()
            .filter_map(Weak::upgrade)
            .collect()
    }
}

impl InputBase for FanOut {
    fn get_schema(&self) -> Rc<SourceSchema> {
        self.input.get_schema()
    }
    fn destroy(&self) {
        let n = self.outputs.borrow().len();
        assert!(
            self.destroy_count.get() < n,
            "FanOut already destroyed once for each output"
        );
        self.destroy_count.set(self.destroy_count.get() + 1);
        if self.destroy_count.get() == n {
            self.input.destroy();
        }
    }
}

impl FilterInput for FanOut {
    fn set_filter_output(&self, output: Weak<dyn FilterOutput>) {
        self.outputs.borrow_mut().push(output);
    }
}

impl Output for FanOut {
    fn push(&self, change: Change, _pusher: &dyn InputBase) {
        let change_type = change.change_type();
        for o in self.outputs() {
            o.push(change.clone(), self);
        }
        let fan_in = upgrade(&self.fan_in).expect("fan-out must have a corresponding fan-in set!");
        fan_in.fan_out_done_pushing_to_all_branches(change_type);
    }

    fn reconcile(&self, _pusher: &dyn InputBase) {
        for o in self.outputs() {
            o.reconcile(self);
        }
        if let Some(fan_in) = upgrade(&self.fan_in) {
            fan_in.fan_out_done_reconciling();
        }
    }
}

impl FilterOutput for FanOut {
    fn begin_filter(&self) {
        for o in self.outputs() {
            o.begin_filter();
        }
    }
    fn end_filter(&self) {
        for o in self.outputs() {
            o.end_filter();
        }
    }
    fn filter(&self, node: &Node) -> bool {
        self.outputs().iter().any(|o| o.filter(node))
    }
}

/// Merges the branches of an OR, removing duplicates (`fan-in.ts`).
pub struct FanIn {
    inputs: Vec<Rc<dyn FilterInput>>,
    schema: Rc<SourceSchema>,
    output: RefCell<Weak<dyn FilterOutput>>,
    accumulated: RefCell<Vec<Change>>,
}

impl FanIn {
    pub fn new(fan_out: &Rc<FanOut>, inputs: Vec<Rc<dyn FilterInput>>) -> Rc<Self> {
        let schema = fan_out.get_schema();
        let op = Rc::new(FanIn {
            inputs: inputs.clone(),
            schema: schema.clone(),
            output: RefCell::new(no_filter_output()),
            accumulated: RefCell::default(),
        });
        for input in &inputs {
            input.set_filter_output(Rc::downgrade(&op) as Weak<dyn FilterOutput>);
            assert!(
                Rc::ptr_eq(&schema, &input.get_schema()),
                "Schema mismatch in fan-in"
            );
        }
        op
    }

    pub fn fan_out_done_pushing_to_all_branches(&self, fan_out_change_type: ChangeType) {
        if self.inputs.is_empty() {
            assert!(
                self.accumulated.borrow().is_empty(),
                "If there are no inputs then fan-in should not receive any pushes."
            );
            return;
        }
        let changes = std::mem::take(&mut *self.accumulated.borrow_mut());
        let Some(out) = upgrade(&self.output) else {
            return;
        };
        push_accumulated_changes(
            changes,
            out.as_ref(),
            self,
            fan_out_change_type,
            &|a, _| a,
            &|c| c,
            None,
        );
    }

    pub fn fan_out_done_reconciling(&self) {
        if self.inputs.is_empty() {
            return;
        }
        if let Some(out) = upgrade(&self.output) {
            out.reconcile(self);
        }
    }
}

impl InputBase for FanIn {
    fn get_schema(&self) -> Rc<SourceSchema> {
        self.schema.clone()
    }
    fn destroy(&self) {
        for i in &self.inputs {
            i.destroy();
        }
    }
}

impl FilterInput for FanIn {
    fn set_filter_output(&self, output: Weak<dyn FilterOutput>) {
        *self.output.borrow_mut() = output;
    }
}

impl Output for FanIn {
    fn push(&self, change: Change, _pusher: &dyn InputBase) {
        self.accumulated.borrow_mut().push(change);
    }
    fn reconcile(&self, _pusher: &dyn InputBase) {}
}

impl FilterOutput for FanIn {
    fn begin_filter(&self) {
        if let Some(out) = upgrade(&self.output) {
            out.begin_filter();
        }
    }
    fn end_filter(&self) {
        if let Some(out) = upgrade(&self.output) {
            out.end_filter();
        }
    }
    fn filter(&self, node: &Node) -> bool {
        upgrade(&self.output).is_some_and(|o| o.filter(node))
    }
}
