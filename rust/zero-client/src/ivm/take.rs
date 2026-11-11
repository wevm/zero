//! `Take`, `Skip`, and `Cap` (`packages/zql/src/ivm/take.ts`, `skip.ts`,
//! and `cap.ts`).

use super::*;
use crate::ast::Bound;
use crate::json::{self, Value};
use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};

fn upgrade<T: ?Sized>(w: &RefCell<Weak<T>>) -> Option<Rc<T>> {
    w.borrow().upgrade()
}

/// `{key: [tag, ...partition values]}` as JS's `JSON.stringify`.
fn state_key<'a>(tag: &str, values: impl Iterator<Item = Option<&'a Value>>) -> String {
    let mut array = vec![Value::String(tag.to_owned())];
    array.extend(values.map(|v| v.cloned().unwrap_or(Value::Null)));
    json::stringify(&Value::Array(array))
}

fn fetch_first(input: &dyn Input, req: FetchRequest) -> Option<Node> {
    input.fetch(&req).next()
}

fn start(row: &Row, basis: Basis) -> Option<Start> {
    Some(Start {
        row: row.clone(),
        basis,
    })
}

/// The first fetch of a `Take` or `Cap` partition: passes the first
/// `limit` nodes of its input and hands the rows it counted to `store` once
/// it is done, as the JS generator's `finally` does.
///
/// A node counts once the next one is asked for, as in the generator, which
/// resumes after the `yield`. A consumer that stops early, even right after
/// the last node, leaves the generator suspended: JS stores what it counted
/// and asserts, so the query fails.
/// Hands a hydrated partition's rows to its operator.
type StoreRows = Box<dyn Fn(&[Row])>;

struct Hydration {
    input: Option<NodeStream>,
    limit: usize,
    rows: Vec<Row>,
    pending: Option<Row>,
    store: StoreRows,
}

impl Hydration {
    fn new(input: NodeStream, limit: usize, store: StoreRows) -> Self {
        Hydration {
            input: Some(input),
            limit,
            rows: Vec::new(),
            pending: None,
            store,
        }
    }

    fn finish(&mut self) {
        self.input = None;
        (self.store)(&self.rows);
    }
}

impl Iterator for Hydration {
    type Item = Node;

    fn next(&mut self) -> Option<Node> {
        self.input.as_ref()?;
        if let Some(row) = self.pending.take() {
            self.rows.push(row);
            if self.rows.len() == self.limit {
                self.finish();
                return None;
            }
        }
        match self.input.as_mut()?.next() {
            Some(node) => {
                self.pending = Some(node.row.clone());
                Some(node)
            }
            None => {
                self.finish();
                None
            }
        }
    }
}

impl Drop for Hydration {
    fn drop(&mut self) {
        if self.input.is_some() {
            self.finish();
            report("Unexpected early return prevented full hydration");
        }
    }
}

#[derive(Clone, Debug)]
struct TakeState {
    size: usize,
    bound: Option<Row>,
}

#[derive(Clone)]
struct DirtyPartition {
    constraint: Option<Constraint>,
    /// The bound reported to an upstream `TakeGate` until reconcile, so a
    /// removal does not unbound the gate during the push.
    gate_bound: Option<Row>,
}

/// Implements `limit`: passes the first `limit` nodes of its input and
/// keeps a bound, the last accepted row, to decide on pushes (`take.ts`).
/// Counts globally or per value of a partition key.
pub struct Take {
    me: Weak<Take>,
    input: Rc<dyn Input>,
    limit: usize,
    partition_key: Option<Vec<String>>,
    storage: RefCell<HashMap<String, TakeState>>,
    /// A row hidden from fetches while a split push is in flight.
    row_hidden_from_fetch: RefCell<Option<Row>>,
    take_gate: RefCell<Weak<TakeGate>>,
    /// Partitions with removals in phase one that reconcile must refill, in
    /// insertion order.
    dirty_partitions: RefCell<Vec<(String, DirtyPartition)>>,
    output: RefCell<Weak<dyn Output>>,
}

impl Take {
    pub fn new(
        input: Rc<dyn Input>,
        limit: usize,
        partition_key: Option<Vec<String>>,
    ) -> Result<Rc<Self>, PipelineError> {
        let schema = input.get_schema();
        let Some(sort) = &schema.sort else {
            return Err(PipelineError::new("Take requires sorted input"));
        };
        assert_ordering_includes_pk(sort, &schema.primary_key)?;
        let op = Rc::new_cyclic(|me| Take {
            me: me.clone(),
            input: input.clone(),
            limit,
            partition_key,
            storage: RefCell::default(),
            row_hidden_from_fetch: RefCell::new(None),
            take_gate: RefCell::new(Weak::new()),
            dirty_partitions: RefCell::default(),
            output: RefCell::new(no_output()),
        });
        input.set_output(Rc::downgrade(&op) as Weak<dyn Output>);
        Ok(op)
    }

    pub fn set_take_gate(&self, gate: &Rc<TakeGate>) {
        *self.take_gate.borrow_mut() = Rc::downgrade(gate);
    }

    fn emit(&self, change: Change) {
        if let Some(out) = upgrade(&self.output) {
            out.push(change, self);
        }
    }

    fn gate_open(&self) {
        if let Some(g) = upgrade(&self.take_gate) {
            g.open();
        }
    }

    fn gate_close(&self) {
        if let Some(g) = upgrade(&self.take_gate) {
            g.close();
        }
    }

    fn compare_rows(&self) -> Comparator {
        self.input.get_schema().compare_rows.clone()
    }

    fn dirty(&self, key: &str) -> Option<DirtyPartition> {
        self.dirty_partitions
            .borrow()
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, d)| d.clone())
    }

    fn state(&self, key: &str) -> Option<TakeState> {
        self.storage.borrow().get(key).cloned()
    }

    fn set_state(&self, key: &str, size: usize, bound: Option<Row>) {
        self.storage
            .borrow_mut()
            .insert(key.to_owned(), TakeState { size, bound });
    }

    fn row_key(&self, row: &Row) -> String {
        let keys = self.partition_key.as_deref().unwrap_or_default();
        state_key("take", keys.iter().map(|k| row.get(k)))
    }

    fn constraint_key(&self, constraint: Option<&Constraint>) -> String {
        match (&self.partition_key, constraint) {
            (Some(keys), Some(c)) => state_key("take", keys.iter().map(|k| c.get(k))),
            _ => state_key("take", std::iter::empty()),
        }
    }

    fn state_and_constraint(&self, row: &Row) -> (Option<TakeState>, String, Option<Constraint>) {
        let key = self.row_key(row);
        let state = self.state(&key);
        let constraint = match (&state, &self.partition_key) {
            (Some(_), Some(pk)) => Some(Constraint::from_row(row, pk)),
            _ => None,
        };
        (state, key, constraint)
    }

    fn fetch_now(self: &Rc<Self>, req: FetchRequest) -> NodeStream {
        // A flipped join fetches its child without a constraint, so a limited
        // flipped EXISTS gets here. JS throws, and `materialize` with it.
        if let Some(pk) = &self.partition_key
            && !constraint_contains_partition_key(req.constraint.as_ref(), Some(pk))
        {
            report("Partitioned take does not allow unpartitioned fetches");
            return empty();
        }
        let key = self.constraint_key(req.constraint.as_ref());
        let Some(state) = self.state(&key) else {
            if constraint_matches_partition_key(
                req.constraint.as_ref(),
                self.partition_key.as_deref(),
            ) {
                return self.initial_fetch(req, key);
            }
            return empty();
        };
        let Some(bound) = state.bound else {
            return empty();
        };
        let me = self.clone();
        let compare = self.compare_rows();
        let mut it = Some(self.input.fetch(&req));
        let mut count = 0;
        let mut yielded = false;
        Box::new(std::iter::from_fn(move || {
            // Count the node yielded last on resume, as the JS generator does.
            if yielded {
                yielded = false;
                count += 1;
                if count >= me.limit {
                    it = None;
                }
            }
            while let Some(node) = it.as_mut()?.next() {
                if compare(&bound, &node.row) == Ordering::Less {
                    break;
                }
                let hidden = me.row_hidden_from_fetch.borrow().clone();
                if hidden.is_some_and(|h| compare(&h, &node.row) == Ordering::Equal) {
                    continue;
                }
                yielded = true;
                return Some(node);
            }
            it = None;
            None
        }))
    }

    /// The first fetch of a partition: hydrates its state. The state is
    /// written once the stream ends, as in the JS `finally`.
    fn initial_fetch(self: &Rc<Self>, req: FetchRequest, key: String) -> NodeStream {
        assert!(req.start.is_none(), "Start should be undefined");
        assert!(!req.reverse, "Reverse should be false");
        if self.limit == 0 {
            return empty();
        }
        let me = self.clone();
        let limit = self.limit;
        Box::new(Hydration::new(
            self.input.fetch(&req),
            limit,
            Box::new(move |rows: &[Row]| me.set_state(&key, rows.len(), rows.last().cloned())),
        ))
    }

    fn push_add_or_remove(&self, change: Change) {
        let (state, key, constraint) = self.state_and_constraint(&change.node().row);
        let Some(state) = state else {
            return;
        };
        let compare = self.compare_rows();
        match &change {
            Change::Add(node) => {
                if state.size < self.limit {
                    if self.dirty(&key).is_some() {
                        // With an unresolved deficit, only admit rows before
                        // the bound, so reconcile can backfill earlier rows in
                        // order.
                        match &state.bound {
                            Some(b) if compare(&node.row, b) == Ordering::Less => {}
                            _ => return,
                        }
                    }
                    let next_size = state.size + 1;
                    if next_size == self.limit {
                        self.dirty_partitions
                            .borrow_mut()
                            .retain(|(k, _)| *k != key);
                    }
                    let bound = match &state.bound {
                        Some(b) if compare(b, &node.row) != Ordering::Less => b.clone(),
                        _ => node.row.clone(),
                    };
                    self.set_state(&key, next_size, Some(bound));
                    self.emit(change);
                    return;
                }
                // size == limit
                let Some(bound) = &state.bound else {
                    return;
                };
                if compare(&node.row, bound) != Ordering::Less {
                    return;
                }
                let (bound_node, before_bound_node) = if self.limit == 1 {
                    let first = fetch_first(
                        self.input.as_ref(),
                        FetchRequest {
                            constraint: constraint.clone(),
                            start: start(bound, Basis::At),
                            ..FetchRequest::default()
                        },
                    );
                    (first, None)
                } else {
                    let mut it = self.input.fetch(&FetchRequest {
                        constraint: constraint.clone(),
                        start: start(bound, Basis::At),
                        reverse: true,
                        ..FetchRequest::default()
                    });
                    let first = it.next();
                    (first, it.next())
                };
                let bound_node = bound_node.expect("Take: boundNode must be found during fetch");
                // Remove before add to keep output size <= limit.
                let new_bound = match &before_bound_node {
                    Some(b) if compare(&node.row, &b.row) != Ordering::Greater => b.row.clone(),
                    _ => node.row.clone(),
                };
                self.set_state(&key, state.size, Some(new_bound));
                self.push_with_row_hidden_from_fetch(&node.row, Change::Remove(bound_node));
                self.emit(change);
            }
            Change::Remove(node) => {
                let Some(bound) = &state.bound else {
                    return; // after the bound
                };
                let comp_to_bound = compare(&node.row, bound);
                if comp_to_bound == Ordering::Greater
                    || (comp_to_bound == Ordering::Less && self.limit == 1)
                {
                    return;
                }
                let before_bound_node = fetch_first(
                    self.input.as_ref(),
                    FetchRequest {
                        constraint: constraint.clone(),
                        start: start(bound, Basis::After),
                        reverse: true,
                        ..FetchRequest::default()
                    },
                );
                let final_bound = if state.size - 1 == 0 {
                    None
                } else if comp_to_bound == Ordering::Less {
                    Some(bound.clone())
                } else {
                    before_bound_node.map(|n| n.row)
                };
                if self.dirty(&key).is_none() {
                    // Snapshot the bound before the first removal for
                    // upstream gates.
                    self.dirty_partitions.borrow_mut().push((
                        key.clone(),
                        DirtyPartition {
                            constraint,
                            gate_bound: Some(bound.clone()),
                        },
                    ));
                }
                self.set_state(&key, state.size - 1, final_bound);
                self.emit(change);
            }
            Change::Child(node, _) => {
                if let Some(bound) = &state.bound
                    && compare(&node.row, bound) != Ordering::Greater
                {
                    self.emit(change);
                }
            }
            Change::Edit { .. } => {}
        }
    }

    fn push_edit_change(&self, change: Change) {
        let Change::Edit { node, old_node } = &change else {
            unreachable!()
        };
        if let Some(pk) = &self.partition_key {
            assert!(
                row_equals_for_compound_key(&old_node.row, &node.row, pk),
                "Unexpected change of partition key"
            );
        }
        let (state, key, constraint) = self.state_and_constraint(&old_node.row);
        let Some(state) = state else {
            return;
        };
        let bound = state.bound.clone().expect("Bound should be set");
        let compare = self.compare_rows();
        let old_cmp = compare(&old_node.row, &bound);
        let new_cmp = compare(&node.row, &bound);
        let fetch_one = |s: Option<Start>, reverse: bool| {
            fetch_first(
                self.input.as_ref(),
                FetchRequest {
                    constraint: constraint.clone(),
                    start: s,
                    reverse,
                    ..FetchRequest::default()
                },
            )
        };
        let replace_bound_and_forward = |change: Change| {
            self.set_state(&key, state.size, Some(node.row.clone()));
            self.emit(change);
        };

        match old_cmp {
            Ordering::Equal => {
                // The bound row changed.
                match new_cmp {
                    Ordering::Equal => self.emit(change.clone()),
                    Ordering::Less => {
                        if self.limit == 1 {
                            return replace_bound_and_forward(change.clone());
                        }
                        // The new row stays in, but might no longer be the
                        // bound.
                        let before = fetch_one(start(&bound, Basis::After), true)
                            .expect("Take: beforeBoundNode must be found during fetch");
                        self.set_state(&key, state.size, Some(before.row));
                        self.emit(change.clone());
                    }
                    Ordering::Greater => {
                        // The first row at the old bound is the new bound.
                        self.gate_open();
                        let new_bound = fetch_one(start(&bound, Basis::At), false);
                        self.gate_close();
                        let new_bound =
                            new_bound.expect("Take: newBoundNode must be found during fetch");
                        if compare(&new_bound.row, &node.row) == Ordering::Equal {
                            return replace_bound_and_forward(change.clone());
                        }
                        // The new row is out of bounds: remove the old row,
                        // add the new bound.
                        self.set_state(&key, state.size, Some(new_bound.row.clone()));
                        self.push_with_row_hidden_from_fetch(
                            &new_bound.row,
                            Change::Remove(old_node.clone()),
                        );
                        self.emit(Change::Add(new_bound));
                    }
                }
            }
            Ordering::Greater => {
                assert!(
                    new_cmp != Ordering::Equal,
                    "Invalid state. Row has duplicate primary key"
                );
                if new_cmp == Ordering::Greater {
                    return; // both outside
                }
                // Old was outside, new is inside: push out the old bound.
                let mut it = self.input.fetch(&FetchRequest {
                    constraint: constraint.clone(),
                    start: start(&bound, Basis::At),
                    reverse: true,
                    ..FetchRequest::default()
                });
                let old_bound = it
                    .next()
                    .expect("Take: oldBoundNode must be found during fetch");
                let new_bound = it
                    .next()
                    .expect("Take: newBoundNode must be found during fetch");
                drop(it);
                self.set_state(&key, state.size, Some(new_bound.row));
                self.push_with_row_hidden_from_fetch(&node.row, Change::Remove(old_bound));
                self.emit(Change::Add(node.clone()));
            }
            Ordering::Less => {
                assert!(
                    new_cmp != Ordering::Equal,
                    "Invalid state. Row has duplicate primary key"
                );
                if new_cmp == Ordering::Less {
                    return self.emit(change.clone()); // both inside
                }
                // Old was inside, new is past the bound: the row after the
                // bound or the new row is the new bound.
                self.gate_open();
                let after = fetch_one(start(&bound, Basis::After), false);
                self.gate_close();
                let after = after.expect("Take: afterBoundNode must be found during fetch");
                if compare(&after.row, &node.row) == Ordering::Equal {
                    return replace_bound_and_forward(change.clone());
                }
                self.emit(Change::Remove(old_node.clone()));
                self.set_state(&key, state.size, Some(after.row.clone()));
                self.emit(Change::Add(after));
            }
        }
    }

    fn push_with_row_hidden_from_fetch(&self, row: &Row, change: Change) {
        *self.row_hidden_from_fetch.borrow_mut() = Some(row.clone());
        self.emit(change);
        *self.row_hidden_from_fetch.borrow_mut() = None;
    }
}

impl TakeBoundProvider for Take {
    fn get_bound(&self, constraint: Option<&Constraint>) -> Option<Row> {
        if let Some(pk) = &self.partition_key
            && !constraint_contains_partition_key(constraint, Some(pk))
        {
            return None;
        }
        let key = self.constraint_key(constraint);
        if let Some(dirty) = self.dirty(&key) {
            // Phase one: keep upstream gates at the bound before the removals.
            return dirty.gate_bound;
        }
        let state = self.state(&key)?;
        if state.size < self.limit {
            return None;
        }
        state.bound
    }
}

impl InputBase for Take {
    fn get_schema(&self) -> Rc<SourceSchema> {
        self.input.get_schema()
    }
    fn destroy(&self) {
        self.input.destroy()
    }
}

impl Input for Take {
    fn set_output(&self, output: Weak<dyn Output>) {
        *self.output.borrow_mut() = output;
    }

    fn fetch(&self, req: &FetchRequest) -> NodeStream {
        let me = self.me.clone();
        let req = req.clone();
        lazy(move || -> NodeStream {
            match me.upgrade() {
                Some(me) => me.fetch_now(req),
                None => empty(),
            }
        })
    }
}

impl Output for Take {
    fn push(&self, change: Change, _pusher: &dyn InputBase) {
        if matches!(change, Change::Edit { .. }) {
            self.push_edit_change(change);
        } else {
            self.push_add_or_remove(change);
        }
    }

    fn reconcile(&self, _pusher: &dyn InputBase) {
        let dirty = std::mem::take(&mut *self.dirty_partitions.borrow_mut());
        for (key, d) in dirty {
            let state = self
                .state(&key)
                .expect("Take: dirty partition must exist in storage");
            assert!(
                self.limit > state.size,
                "Take: dirty partition must have a deficit"
            );
            let deficit = self.limit - state.size;
            self.gate_open();
            let to_push: Vec<Node> = self
                .input
                .fetch(&FetchRequest {
                    constraint: d.constraint.clone(),
                    start: state.bound.as_ref().and_then(|b| start(b, Basis::After)),
                    ..FetchRequest::default()
                })
                .take(deficit)
                .collect();
            self.gate_close();
            let mut size = state.size;
            for node in to_push {
                size += 1;
                self.set_state(&key, size, Some(node.row.clone()));
                self.emit(Change::Add(node));
            }
        }
        if let Some(out) = upgrade(&self.output) {
            out.reconcile(self);
        }
    }
}

/// `constraintMatchesPartitionKey`: the constraint is on exactly the
/// partition key, or neither is set.
pub fn constraint_matches_partition_key(
    constraint: Option<&Constraint>,
    partition_key: Option<&[String]>,
) -> bool {
    match (constraint, partition_key) {
        (Some(c), Some(pk)) => pk.len() == c.len() && pk.iter().all(|k| c.has(k)),
        (c, pk) => c.is_none() && pk.is_none(),
    }
}

/// `constraintContainsPartitionKey`.
pub fn constraint_contains_partition_key(
    constraint: Option<&Constraint>,
    partition_key: Option<&[String]>,
) -> bool {
    match (constraint, partition_key) {
        (Some(c), Some(pk)) => pk.iter().all(|k| c.has(k)),
        _ => false,
    }
}

/// Sets where the pipeline starts: no row before the bound is output
/// (`skip.ts`).
pub struct Skip {
    me: Weak<Skip>,
    input: Rc<dyn Input>,
    exclusive: bool,
    /// The bound's row.
    bound_row: Row,
    comparator: Comparator,
    predicate: RowPredicate,
    output: RefCell<Weak<dyn Output>>,
}

impl Skip {
    pub fn new(input: Rc<dyn Input>, bound: &Bound) -> Rc<Self> {
        let schema = input.get_schema();
        assert!(schema.sort.is_some(), "Skip requires sorted input");
        let bound_row: Row = Rc::new(bound.row.clone());
        let comparator = schema.compare_rows.clone();
        let exclusive = bound.exclusive;
        let predicate: RowPredicate = {
            let (bound_row, comparator) = (bound_row.clone(), comparator.clone());
            Rc::new(move |row: &Row| match comparator(&bound_row, row) {
                Ordering::Less => true,
                Ordering::Equal => !exclusive,
                Ordering::Greater => false,
            })
        };
        let op = Rc::new_cyclic(|me| Skip {
            me: me.clone(),
            input: input.clone(),
            exclusive,
            bound_row,
            comparator,
            predicate,
            output: RefCell::new(no_output()),
        });
        input.set_output(Rc::downgrade(&op) as Weak<dyn Output>);
        op
    }

    /// The start of a fetch, or `None` if it yields nothing.
    fn get_start(&self, req: &FetchRequest) -> Option<Option<Start>> {
        let bound_start = Start {
            row: self.bound_row.clone(),
            basis: if self.exclusive {
                Basis::After
            } else {
                Basis::At
            },
        };
        let Some(req_start) = &req.start else {
            return Some((!req.reverse).then_some(bound_start));
        };
        let cmp = (self.comparator)(&self.bound_row, &req_start.row);
        if !req.reverse {
            // The skip bound is after the requested start.
            return Some(Some(match cmp {
                Ordering::Greater => bound_start,
                Ordering::Equal if self.exclusive || req_start.basis == Basis::After => Start {
                    row: self.bound_row.clone(),
                    basis: Basis::After,
                },
                Ordering::Equal => bound_start,
                Ordering::Less => req_start.clone(),
            }));
        }
        // Reverse: a bound after the start leaves nothing.
        match cmp {
            Ordering::Greater => None,
            Ordering::Equal if !self.exclusive && req_start.basis == Basis::At => {
                Some(Some(bound_start))
            }
            Ordering::Equal => None,
            Ordering::Less => Some(Some(req_start.clone())),
        }
    }
}

impl InputBase for Skip {
    fn get_schema(&self) -> Rc<SourceSchema> {
        self.input.get_schema()
    }
    fn destroy(&self) {
        self.input.destroy()
    }
}

impl Input for Skip {
    fn set_output(&self, output: Weak<dyn Output>) {
        *self.output.borrow_mut() = output;
    }

    fn fetch(&self, req: &FetchRequest) -> NodeStream {
        let me = self.me.clone();
        let req = req.clone();
        lazy(move || -> NodeStream {
            let Some(me) = me.upgrade() else {
                return empty();
            };
            let Some(start) = me.get_start(&req) else {
                return empty();
            };
            let stream = me.input.fetch(&FetchRequest {
                start,
                ..req.clone()
            });
            if !req.reverse {
                return stream;
            }
            let predicate = me.predicate.clone();
            Box::new(stream.take_while(move |n| predicate(&n.row)))
        })
    }
}

impl Output for Skip {
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

#[derive(Clone)]
struct CapState {
    size: usize,
    /// Serialized primary keys, in the order they joined the set.
    pks: Vec<String>,
}

/// A count-based limit for EXISTS subqueries that needs no order: it tracks
/// membership by primary key (`cap.ts`).
pub struct Cap {
    me: Weak<Cap>,
    input: Rc<dyn Input>,
    limit: usize,
    partition_key: Option<Vec<String>>,
    primary_key: Vec<String>,
    storage: RefCell<HashMap<String, CapState>>,
    output: RefCell<Weak<dyn Output>>,
}

impl Cap {
    pub fn new(input: Rc<dyn Input>, limit: usize, partition_key: Option<Vec<String>>) -> Rc<Self> {
        let primary_key = input.get_schema().primary_key.clone();
        let op = Rc::new_cyclic(|me| Cap {
            me: me.clone(),
            input: input.clone(),
            limit,
            partition_key,
            primary_key,
            storage: RefCell::default(),
            output: RefCell::new(no_output()),
        });
        input.set_output(Rc::downgrade(&op) as Weak<dyn Output>);
        op
    }

    fn emit(&self, change: Change) {
        if let Some(out) = upgrade(&self.output) {
            out.push(change, self);
        }
    }

    fn state(&self, key: &str) -> Option<CapState> {
        self.storage.borrow().get(key).cloned()
    }

    fn set_state(&self, key: &str, size: usize, pks: Vec<String>) {
        self.storage
            .borrow_mut()
            .insert(key.to_owned(), CapState { size, pks });
    }

    fn row_key(&self, row: &Row) -> String {
        let keys = self.partition_key.as_deref().unwrap_or_default();
        state_key("cap", keys.iter().map(|k| row.get(k)))
    }

    fn constraint_key(&self, constraint: Option<&Constraint>) -> String {
        match (&self.partition_key, constraint) {
            (Some(keys), Some(c)) => state_key("cap", keys.iter().map(|k| c.get(k))),
            _ => state_key("cap", std::iter::empty()),
        }
    }

    fn serialize_pk(&self, row: &Row) -> String {
        let values = self
            .primary_key
            .iter()
            .map(|k| row.get(k).cloned().unwrap_or(Value::Null))
            .collect();
        json::stringify(&Value::Array(values))
    }

    fn deserialize_pk(&self, pk: &str) -> Constraint {
        let values = match json::parse(pk) {
            Ok(Value::Array(values)) => values,
            _ => Vec::new(),
        };
        let mut c = Constraint::default();
        for (i, k) in self.primary_key.iter().enumerate() {
            c.set(k, values.get(i).cloned());
        }
        c
    }

    fn fetch_now(self: &Rc<Self>, req: FetchRequest) -> NodeStream {
        assert!(req.start.is_none(), "Cap does not support start");
        assert!(!req.reverse, "Cap does not support reverse");
        assert!(
            self.partition_key.is_none()
                || (req.constraint.is_some()
                    && constraint_matches_partition_key(
                        req.constraint.as_ref(),
                        self.partition_key.as_deref()
                    )),
            "Cap fetch: constraint must match partition key when partitioned"
        );
        let key = self.constraint_key(req.constraint.as_ref());
        let Some(state) = self.state(&key) else {
            return self.initial_fetch(req, key);
        };
        if state.size == 0 {
            return empty();
        }
        // Point lookups by the tracked primary keys.
        let me = self.clone();
        Box::new(state.pks.into_iter().flat_map(move |pk| {
            let pk_constraint = me.deserialize_pk(&pk);
            // Keep the partition key so partitioned operators upstream keep
            // their scope.
            let constraint = match &req.constraint {
                Some(c) => c.merging(&pk_constraint),
                None => pk_constraint,
            };
            me.input.fetch(&FetchRequest {
                constraint: Some(constraint),
                ..FetchRequest::default()
            })
        }))
    }

    fn initial_fetch(self: &Rc<Self>, req: FetchRequest, key: String) -> NodeStream {
        if self.limit == 0 {
            return empty();
        }
        assert!(
            constraint_matches_partition_key(
                req.constraint.as_ref(),
                self.partition_key.as_deref()
            ),
            "Constraint should match partition key"
        );
        let me = self.clone();
        Box::new(Hydration::new(
            self.input.fetch(&req),
            self.limit,
            Box::new(move |rows: &[Row]| {
                let pks = rows.iter().map(|r| me.serialize_pk(r)).collect();
                me.set_state(&key, rows.len(), pks)
            }),
        ))
    }
}

impl InputBase for Cap {
    fn get_schema(&self) -> Rc<SourceSchema> {
        self.input.get_schema()
    }
    fn destroy(&self) {
        self.input.destroy()
    }
}

impl Input for Cap {
    fn set_output(&self, output: Weak<dyn Output>) {
        *self.output.borrow_mut() = output;
    }

    fn fetch(&self, req: &FetchRequest) -> NodeStream {
        let me = self.me.clone();
        let req = req.clone();
        lazy(move || -> NodeStream {
            match me.upgrade() {
                Some(me) => me.fetch_now(req),
                None => empty(),
            }
        })
    }
}

impl Output for Cap {
    fn push(&self, change: Change, _pusher: &dyn InputBase) {
        if let Change::Edit { node, old_node } = &change {
            if let Some(pk) = &self.partition_key {
                assert!(
                    row_equals_for_compound_key(&old_node.row, &node.row, pk),
                    "Unexpected change of partition key"
                );
            }
            let key = self.row_key(&old_node.row);
            let Some(state) = self.state(&key) else {
                return;
            };
            let old_pk = self.serialize_pk(&old_node.row);
            if state.pks.contains(&old_pk) {
                let new_pk = self.serialize_pk(&node.row);
                if old_pk != new_pk {
                    let pks = state
                        .pks
                        .iter()
                        .map(|p| {
                            if *p == old_pk {
                                new_pk.clone()
                            } else {
                                p.clone()
                            }
                        })
                        .collect();
                    self.set_state(&key, state.size, pks);
                }
                self.emit(change);
            }
            return;
        }
        let key = self.row_key(&change.node().row);
        let Some(state) = self.state(&key) else {
            return;
        };
        let pk = self.serialize_pk(&change.node().row);
        match &change {
            Change::Add(_) => {
                if state.size < self.limit {
                    let mut pks = state.pks;
                    pks.push(pk);
                    self.set_state(&key, state.size + 1, pks);
                    self.emit(change);
                }
            }
            Change::Remove(node) => {
                let Some(i) = state.pks.iter().position(|p| *p == pk) else {
                    return;
                };
                let mut pks = state.pks;
                pks.remove(i);
                let new_size = state.size - 1;
                // Refill with the first row not in the set.
                let pk_set: HashSet<&String> = pks.iter().collect();
                let constraint = self
                    .partition_key
                    .as_ref()
                    .map(|k| Constraint::from_row(&node.row, k));
                let replacement = self
                    .input
                    .fetch(&FetchRequest {
                        constraint,
                        ..FetchRequest::default()
                    })
                    .find(|c| !pk_set.contains(&self.serialize_pk(&c.row)));
                match replacement {
                    Some(replacement) => {
                        // Without the replacement while the remove is
                        // forwarded.
                        self.set_state(&key, new_size, pks.clone());
                        self.emit(change);
                        pks.push(self.serialize_pk(&replacement.row));
                        self.set_state(&key, new_size + 1, pks);
                        self.emit(Change::Add(replacement));
                    }
                    None => {
                        self.set_state(&key, new_size, pks);
                        self.emit(change);
                    }
                }
            }
            Change::Child(..) => {
                if state.pks.contains(&pk) {
                    self.emit(change);
                }
            }
            Change::Edit { .. } => {}
        }
    }

    fn reconcile(&self, _pusher: &dyn InputBase) {
        if let Some(out) = upgrade(&self.output) {
            out.reconcile(self);
        }
    }
}
