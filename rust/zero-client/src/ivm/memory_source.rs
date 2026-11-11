//! A source of rows held in memory, kept in sorted order
//! (`packages/zql/src/ivm/memory-source.ts`).
//!
//! Each index is a sorted `Vec` behind an `Rc`. A fetch holds the `Rc` it
//! started with, and a write copies the `Vec` if a fetch still holds it, so
//! a fetch in progress keeps reading the rows it started with, as Swift's
//! copy-on-write B-tree does.

use super::*;
use crate::ast::{Condition, Direction, OrderPart};
use crate::json::{self, Value};
use crate::schema::Table;
use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};

#[derive(Clone)]
struct Overlay {
    epoch: u64,
    change: SourceChange,
}

#[derive(Clone, Default)]
struct Overlays {
    add: Option<Row>,
    remove: Option<Row>,
}

#[derive(Clone)]
struct Index {
    sort: Vec<OrderPart>,
    compare: Comparator,
    data: Rc<Vec<Row>>,
}

impl Index {
    fn new(sort: Vec<OrderPart>, data: Vec<Row>) -> Self {
        let compare = make_comparator(&sort, false);
        Index {
            sort,
            compare,
            data: Rc::new(data),
        }
    }

    fn search(&self, row: &Row) -> Result<usize, usize> {
        self.data.binary_search_by(|r| (self.compare)(r, row))
    }

    /// Adds `row`, replacing an equal one.
    fn add(&mut self, row: Row) {
        match self.search(&row) {
            Ok(i) => Rc::make_mut(&mut self.data)[i] = row,
            Err(i) => Rc::make_mut(&mut self.data).insert(i, row),
        }
    }

    /// Removes the row equal to `row`; whether there was one.
    fn delete(&mut self, row: &Row) -> bool {
        match self.search(row) {
            Ok(i) => {
                Rc::make_mut(&mut self.data).remove(i);
                true
            }
            Err(_) => false,
        }
    }

    /// `probe` for a scan starting at `bound`: the ordering of a row against
    /// the bound.
    fn probe(&self, bound: HashMap<String, Bound>) -> impl Fn(&Row) -> Ordering + use<> {
        let parts: Vec<(String, bool, Bound)> = self
            .sort
            .iter()
            .map(|p| {
                let b = bound.get(&p.field).cloned().unwrap_or(Bound::Value(None));
                (p.field.clone(), p.direction == Direction::Asc, b)
            })
            .collect();
        move |row: &Row| {
            for (field, asc, b) in &parts {
                let c = compare_bounds(&Bound::Value(row.get(field).cloned()), b);
                if c != Ordering::Equal {
                    return if *asc { c } else { c.reverse() };
                }
            }
            Ordering::Equal
        }
    }
}

/// A bound in an index scan: a value, or below or above every value.
#[derive(Clone, Debug)]
enum Bound {
    Value(Option<Value>),
    Min,
    Max,
}

fn compare_bounds(a: &Bound, b: &Bound) -> Ordering {
    use Bound::*;
    match (a, b) {
        (Min, Min) | (Max, Max) => Ordering::Equal,
        (Min, _) | (_, Max) => Ordering::Less,
        (Max, _) | (_, Min) => Ordering::Greater,
        (Value(x), Value(y)) => compare_values(x.as_ref(), y.as_ref()),
    }
}

struct Connection {
    input: RefCell<Weak<MemorySourceInput>>,
    output: RefCell<Weak<dyn Output>>,
    sort: Vec<OrderPart>,
    split_edit_keys: Option<HashSet<String>>,
    compare_rows: Comparator,
    filters: Option<(Condition, RowPredicate)>,
    last_pushed_epoch: Cell<u64>,
}

#[derive(Default)]
struct State {
    indexes: HashMap<String, Index>,
    /// Index keys in the order they were made: the order writes go in.
    index_order: Vec<String>,
    connections: Vec<Rc<Connection>>,
    overlay: Option<Overlay>,
    push_epoch: u64,
}

/// A table's rows in memory.
pub struct MemorySource {
    me: Weak<MemorySource>,
    table: Table,
    primary_index_sort: Vec<OrderPart>,
    primary_index_key: String,
    state: RefCell<State>,
}

fn index_key(sort: &[OrderPart]) -> String {
    let parts: Vec<Value> = sort
        .iter()
        .map(|p| {
            Value::Array(vec![
                Value::from(p.field.as_str()),
                Value::from(p.direction.as_str()),
            ])
        })
        .collect();
    json::stringify(&Value::Array(parts))
}

impl MemorySource {
    pub fn new(table: Table) -> Rc<Self> {
        let primary_index_sort: Vec<OrderPart> = table
            .primary_key
            .iter()
            .map(|k| OrderPart {
                field: k.clone(),
                direction: Direction::Asc,
            })
            .collect();
        let primary_index_key = index_key(&primary_index_sort);
        let mut state = State::default();
        state.indexes.insert(
            primary_index_key.clone(),
            Index::new(primary_index_sort.clone(), Vec::new()),
        );
        state.index_order.push(primary_index_key.clone());
        Rc::new_cyclic(|me| MemorySource {
            me: me.clone(),
            table,
            primary_index_sort,
            primary_index_key,
            state: RefCell::new(state),
        })
    }

    /// The rows, in primary key order.
    pub fn data(&self) -> Rc<Vec<Row>> {
        self.primary_index().data
    }

    /// A source with the same rows that shares them until either is written
    /// (`fork`). It has no connections and only the primary index.
    pub fn fork(&self) -> Rc<Self> {
        let fork = MemorySource::new(self.table.clone());
        let data = self.data();
        fork.state
            .borrow_mut()
            .indexes
            .get_mut(&fork.primary_index_key)
            .expect("primary index")
            .data = data;
        fork
    }

    /// Whether a pipeline reads the source.
    pub fn has_connections(&self) -> bool {
        !self.state.borrow().connections.is_empty()
    }

    /// Whether the source has a row with `row`'s primary key.
    pub fn contains(&self, row: &Row) -> bool {
        self.primary_index().search(row).is_ok()
    }

    /// Whether `a` and `b` have the same primary key.
    pub fn same_key(&self, a: &Row, b: &Row) -> bool {
        (self.primary_index().compare)(a, b) == Ordering::Equal
    }

    /// For tests: the keys of the indexes built so far.
    pub fn index_keys(&self) -> Vec<String> {
        self.state.borrow().index_order.clone()
    }

    fn primary_index(&self) -> Index {
        self.state.borrow().indexes[&self.primary_index_key].clone()
    }

    fn disconnect(&self, connection: &Rc<Connection>) {
        let mut state = self.state.borrow_mut();
        let i = state
            .connections
            .iter()
            .position(|c| Rc::ptr_eq(c, connection))
            .expect("Connection not found");
        // Indexes are kept for the life of the source (see the JS comment).
        state.connections.remove(i);
    }

    fn get_or_create_index(&self, sort: Vec<OrderPart>) -> Index {
        let key = index_key(&sort);
        let mut state = self.state.borrow_mut();
        if let Some(index) = state.indexes.get(&key) {
            return index.clone();
        }
        let mut index = Index::new(sort, Vec::new());
        let mut rows = state.indexes[&self.primary_index_key].data.to_vec();
        let compare = index.compare.clone();
        rows.sort_by(|a, b| compare(a, b));
        index.data = Rc::new(rows);
        state.indexes.insert(key.clone(), index.clone());
        state.index_order.push(key);
        index
    }

    fn fetch(&self, req: FetchRequest, conn: Rc<Connection>) -> NodeStream {
        let me = self.me.clone();
        lazy(move || match me.upgrade() {
            Some(source) => source.fetch_now(&req, &conn),
            None => empty(),
        })
    }

    fn fetch_now(&self, req: &FetchRequest, conn: &Rc<Connection>) -> NodeStream {
        if let Some(multis) = &req.multi_constraints
            && multis.iter().any(|m| !m.is_empty())
        {
            return self.fetch_multi(req, conn);
        }
        let primary_key = &self.table.primary_key;
        let compare_rows = conn.compare_rows.clone();
        let reverse = req.reverse;
        let connection_comparator: Comparator = if reverse {
            let c = compare_rows.clone();
            Rc::new(move |a, b| c(b, a))
        } else {
            compare_rows.clone()
        };

        let conn_filter = conn.filters.as_ref().map(|(c, _)| c);
        let merged_condition = match (conn_filter, &req.filter) {
            (Some(c), Some(r)) => Some(Condition::And(vec![c.clone(), r.clone()])),
            (c, r) => c.or(r.as_ref()).cloned(),
        };
        let merged_predicate =
            merge_predicates(conn.filters.as_ref().map(|(_, p)| p.clone()), &req.filter);

        let pk_constraint =
            primary_key_constraint_from_filters(merged_condition.as_ref(), primary_key);
        // The primary key constraint is more limiting than the constraint.
        let fetch_or_pk = pk_constraint.as_ref().or(req.constraint.as_ref());

        let mut index_sort: Vec<OrderPart> = fetch_or_pk
            .map(|c| {
                c.keys()
                    .into_iter()
                    .map(|k| OrderPart {
                        field: k.to_owned(),
                        direction: Direction::Asc,
                    })
                    .collect()
            })
            .unwrap_or_default();
        // Constraining by the whole primary key yields one row, so the
        // requested sort does not matter.
        if primary_key.len() > 1 || !fetch_or_pk.is_some_and(|c| c.matches_primary_key(primary_key))
        {
            index_sort.extend(conn.sort.iter().cloned());
        }

        let index = self.get_or_create_index(index_sort);
        let index_compare = index.compare.clone();
        let index_comparator: Comparator = if reverse {
            let c = index_compare.clone();
            Rc::new(move |a, b| c(b, a))
        } else {
            index_compare.clone()
        };
        let start_at = req.start.as_ref().map(|s| s.row.clone());

        // Start a constrained scan at the first row with the constraint's
        // values, whatever the direction of the following sort parts. A
        // column the sort names twice keeps its first bound: the comparator
        // decides on that part, and a later part for the column only sees
        // equal values. (JS takes the last, so a sort like
        // `text desc, text asc` starts past every match.)
        let data = index.data.clone();
        let position = if let Some(constraint) = fetch_or_pk {
            let mut scan_start: HashMap<String, Bound> = HashMap::new();
            for part in &index.sort {
                if scan_start.contains_key(&part.field) {
                    continue;
                }
                let asc = part.direction == Direction::Asc;
                let b = if constraint.has(&part.field) {
                    Bound::Value(constraint.get(&part.field).cloned())
                } else if asc != reverse {
                    Bound::Min
                } else {
                    Bound::Max
                };
                scan_start.insert(part.field.clone(), b);
            }
            let probe = index.probe(scan_start);
            start_position(&data, &probe, reverse)
        } else if let Some(start) = &start_at {
            let c = index_compare.clone();
            start_position(&data, &|r: &Row| c(r, start), reverse)
        } else if reverse {
            data.len() as isize - 1
        } else {
            0
        };
        let rows = TreeRows {
            data,
            next: position,
            reverse,
        };

        let (overlay, last_pushed) = {
            let state = self.state.borrow();
            (state.overlay.clone(), conn.last_pushed_epoch.get())
        };
        let overlay_active = overlay.as_ref().is_some_and(|o| last_pushed >= o.epoch);
        // Fast path: no overlay, no start, and no filters.
        if !overlay_active && req.start.is_none() && conn.filters.is_none() && req.filter.is_none()
        {
            let constraint = req.constraint.clone();
            return Box::new(
                rows.take_while(move |r| constraint.as_ref().is_none_or(|c| c.matches(r)))
                    .map(Node::new),
            );
        }

        // `req.constraint` and not the primary key constraint, which comes
        // from the filters and acts as one.
        let rows: Stream<Row> = if pk_constraint.is_some() {
            Box::new(rows.take(1))
        } else {
            Box::new(rows)
        };
        let overlay = overlay.filter(|o| last_pushed >= o.epoch);
        let overlays = compute_overlays(
            start_at.as_ref(),
            req.constraint.as_ref(),
            overlay.as_ref(),
            &connection_comparator,
            merged_predicate.as_ref(),
            None,
        );
        let with_overlay = generate_with_overlay(rows, overlays, index_comparator);
        let with_start =
            generate_with_start(with_overlay, req.start.clone(), connection_comparator);
        let with_constraint = generate_with_constraint(with_start, req.constraint.clone());
        match merged_predicate {
            Some(p) => Box::new(with_constraint.filter(move |n| p(&n.row))),
            None => with_constraint,
        }
    }

    fn fetch_multi(&self, req: &FetchRequest, conn: &Rc<Connection>) -> NodeStream {
        let multis: Vec<Vec<Constraint>> = req
            .multi_constraints
            .iter()
            .flatten()
            .filter(|m| !m.is_empty())
            .cloned()
            .collect();
        let (primary, rest) = multis.split_first().expect("a multi-constraint");
        let rest = rest.to_vec();
        let streams: Vec<NodeStream> = primary
            .iter()
            .map(|c| {
                let mut sub = req.clone();
                sub.constraint = Some(match &req.constraint {
                    Some(base) => base.merging(c),
                    None => c.clone(),
                });
                sub.multi_constraints = None;
                self.fetch(sub, conn.clone())
            })
            .collect();
        let compare_rows = conn.compare_rows.clone();
        let reverse = req.reverse;
        let merged = merge_sorted_streams(
            streams,
            Rc::new(move |a: &Node, b: &Node| {
                if reverse {
                    compare_rows(&b.row, &a.row)
                } else {
                    compare_rows(&a.row, &b.row)
                }
            }),
        );
        if rest.is_empty() {
            return merged;
        }
        Box::new(merged.filter(move |n| rest.iter().all(|mc| mc.iter().any(|c| c.matches(&n.row)))))
    }

    /// Pushes an add for each of `rows`. When nothing is connected and the
    /// source is empty, builds the primary index in one pass instead.
    pub fn push_adds(&self, rows: Vec<Row>) {
        let can_bulk_load = {
            let state = self.state.borrow();
            state.connections.is_empty()
                && state.indexes.len() == 1
                && state.indexes[&self.primary_index_key].data.is_empty()
        };
        if !can_bulk_load {
            for row in rows {
                if has_failed() {
                    return;
                }
                self.push(SourceChange::Add(row));
            }
            return;
        }
        let mut state = self.state.borrow_mut();
        let index = state
            .indexes
            .get_mut(&self.primary_index_key)
            .expect("primary");
        let compare = index.compare.clone();
        let mut rows = rows;
        rows.sort_by(|a, b| compare(a, b));
        // Equal rows: the last wins, as `add` replaces.
        let mut deduped: Vec<Row> = Vec::with_capacity(rows.len());
        for row in rows {
            match deduped.last_mut() {
                Some(last) if compare(last, &row) == Ordering::Equal => *last = row,
                _ => deduped.push(row),
            }
        }
        index.data = Rc::new(deduped);
    }

    fn gen_push_and_write_with_split_edit(&self, change: SourceChange) {
        if let SourceChange::Edit { row, old_row } = &change {
            let should_split = self.state.borrow().connections.iter().any(|conn| {
                conn.split_edit_keys.as_ref().is_some_and(|keys| {
                    keys.iter()
                        .any(|k| !values_equal(row.get(k), old_row.get(k)))
                })
            });
            if should_split {
                let epoch = self.next_epoch();
                self.gen_push_and_write(SourceChange::Remove(old_row.clone()), epoch);
                if has_failed() {
                    return;
                }
                let epoch = self.next_epoch();
                self.gen_push_and_write(SourceChange::Add(row.clone()), epoch);
                return;
            }
        }
        let epoch = self.next_epoch();
        self.gen_push_and_write(change, epoch);
    }

    fn next_epoch(&self) -> u64 {
        let mut state = self.state.borrow_mut();
        state.push_epoch += 1;
        state.push_epoch
    }

    fn gen_push_and_write(&self, change: SourceChange, epoch: u64) {
        self.gen_push(&change, epoch);
        // JS's throw skips the write.
        if has_failed() {
            return;
        }
        self.write_change(&change);
    }

    fn gen_push(&self, change: &SourceChange, epoch: u64) {
        let primary = self.primary_index();
        let check = match change {
            SourceChange::Add(row) => primary
                .search(row)
                .is_err()
                .then_some(())
                .ok_or("Row already exists"),
            SourceChange::Remove(row) | SourceChange::Edit { old_row: row, .. } => primary
                .search(row)
                .is_ok()
                .then_some(())
                .ok_or("Row not found"),
        };
        if let Err(message) = check {
            panic!("{message} {}", describe(change));
        }

        self.state.borrow_mut().overlay = Some(Overlay {
            epoch,
            change: change.clone(),
        });
        struct ClearOverlay<'a>(&'a RefCell<State>);
        impl Drop for ClearOverlay<'_> {
            fn drop(&mut self) {
                self.0.borrow_mut().overlay = None;
            }
        }
        let _clear = ClearOverlay(&self.state);

        // Index-based, like iterating the live JS array: a pipeline can
        // disconnect while a push is in flight.
        let mut i = 0;
        loop {
            let Some(conn) = self.state.borrow().connections.get(i).cloned() else {
                break;
            };
            i += 1;
            let output = conn.output.borrow().upgrade();
            let input = conn.input.borrow().upgrade();
            let (Some(output), Some(input)) = (output, input) else {
                continue;
            };
            conn.last_pushed_epoch.set(epoch);
            let output_change = match change {
                SourceChange::Edit { row, old_row } => Change::Edit {
                    node: Node::new(row.clone()),
                    old_node: Node::new(old_row.clone()),
                },
                SourceChange::Add(row) => Change::Add(Node::new(row.clone())),
                SourceChange::Remove(row) => Change::Remove(Node::new(row.clone())),
            };
            filter_push(
                output_change,
                output.as_ref(),
                input.as_ref(),
                conn.filters.as_ref().map(|(_, p)| p),
            );
            // JS's throw ends the push here, before the other connections.
            if has_failed() {
                return;
            }
        }
        let mut i = 0;
        loop {
            let Some(conn) = self.state.borrow().connections.get(i).cloned() else {
                break;
            };
            i += 1;
            let output = conn.output.borrow().upgrade();
            let input = conn.input.borrow().upgrade();
            if let (Some(output), Some(input)) = (output, input) {
                output.reconcile(input.as_ref());
            }
        }
    }

    fn write_change(&self, change: &SourceChange) {
        let mut state = self.state.borrow_mut();
        let state = &mut *state;
        for key in &state.index_order {
            let index = state.indexes.get_mut(key).expect("index");
            match change {
                SourceChange::Add(row) => index.add(row.clone()),
                SourceChange::Remove(row) => {
                    assert!(index.delete(row), "MemorySource: remove must succeed")
                }
                SourceChange::Edit { row, old_row } => {
                    assert!(
                        index.delete(old_row),
                        "MemorySource: edit remove must succeed"
                    );
                    index.add(row.clone());
                }
            }
        }
    }
}

fn describe(change: &SourceChange) -> String {
    let s = |r: &Row| json::stringify(&Value::Object((**r).clone()));
    match change {
        SourceChange::Add(r) => format!("[0,{},null]", s(r)),
        SourceChange::Remove(r) => format!("[1,{},null]", s(r)),
        SourceChange::Edit { row, old_row } => format!("[2,{},{}]", s(row), s(old_row)),
    }
}

impl Source for MemorySource {
    fn table_schema(&self) -> &Table {
        &self.table
    }

    fn connect(
        &self,
        sort: Option<Vec<OrderPart>>,
        filters: Option<&Condition>,
        split_edit_keys: Option<&HashSet<String>>,
    ) -> Result<Rc<dyn SourceInput>, PipelineError> {
        let (transformed, removed) = transform_filters(filters);
        let unordered = sort.is_none();
        let internal_sort = sort.unwrap_or_else(|| self.primary_index_sort.clone());
        let conn_filters = match transformed {
            Some(c) => {
                let p = create_predicate(&c)?;
                Some((c, p))
            }
            None => None,
        };
        if !unordered {
            assert_ordering_includes_pk(&internal_sort, &self.table.primary_key)?;
        }
        let connection = Rc::new(Connection {
            input: RefCell::new(Weak::new()),
            output: RefCell::new(no_output()),
            compare_rows: make_comparator(&internal_sort, false),
            sort: internal_sort.clone(),
            split_edit_keys: split_edit_keys.cloned(),
            filters: conn_filters,
            last_pushed_epoch: Cell::new(0),
        });
        let schema = Rc::new(SourceSchema {
            table_name: self.table.name.clone(),
            columns: self.table.columns.clone(),
            primary_key: self.table.primary_key.clone(),
            relationships: Vec::new(),
            is_hidden: false,
            system: crate::ast::System::Client,
            compare_rows: connection.compare_rows.clone(),
            sort: (!unordered).then_some(internal_sort),
        });
        let input = Rc::new(MemorySourceInput {
            source: self.me.clone(),
            connection: connection.clone(),
            schema,
            fully_applied_filters: !removed,
        });
        *connection.input.borrow_mut() = Rc::downgrade(&input);
        self.state.borrow_mut().connections.push(connection);
        Ok(input)
    }

    fn push(&self, change: SourceChange) {
        self.gen_push_and_write_with_split_edit(change)
    }
}

/// A pipeline's connection to a [`MemorySource`].
pub struct MemorySourceInput {
    /// Weak: a view can outlive the client and its sources.
    source: Weak<MemorySource>,
    connection: Rc<Connection>,
    schema: Rc<SourceSchema>,
    fully_applied_filters: bool,
}

impl InputBase for MemorySourceInput {
    fn get_schema(&self) -> Rc<SourceSchema> {
        self.schema.clone()
    }

    fn destroy(&self) {
        if let Some(source) = self.source.upgrade() {
            source.disconnect(&self.connection);
        }
    }
}

impl Input for MemorySourceInput {
    fn set_output(&self, output: Weak<dyn Output>) {
        *self.connection.output.borrow_mut() = output;
    }

    fn fetch(&self, req: &FetchRequest) -> NodeStream {
        match self.source.upgrade() {
            Some(source) => source.fetch(req.clone(), self.connection.clone()),
            None => empty(),
        }
    }
}

impl SourceInput for MemorySourceInput {
    fn fully_applied_filters(&self) -> bool {
        self.fully_applied_filters
    }
}

/// The first index of a scan from `probe`: forward, the first row not
/// before it; in reverse, the last row not after it (-1 for none).
fn start_position(data: &[Row], probe: &dyn Fn(&Row) -> Ordering, reverse: bool) -> isize {
    if reverse {
        data.partition_point(|r| probe(r) != Ordering::Greater) as isize - 1
    } else {
        data.partition_point(|r| probe(r) == Ordering::Less) as isize
    }
}

/// An index's rows from a position, ascending or descending.
struct TreeRows {
    data: Rc<Vec<Row>>,
    next: isize,
    reverse: bool,
}

impl Iterator for TreeRows {
    type Item = Row;

    fn next(&mut self) -> Option<Row> {
        if self.next < 0 {
            return None;
        }
        let row = self.data.get(self.next as usize)?.clone();
        self.next += if self.reverse { -1 } else { 1 };
        Some(row)
    }
}

/// `assertOrderingIncludesPK`.
pub fn assert_ordering_includes_pk(
    ordering: &[OrderPart],
    pk: &[String],
) -> Result<(), PipelineError> {
    let missing: Vec<&str> = pk
        .iter()
        .filter(|k| !ordering.iter().any(|p| &p.field == *k))
        .map(String::as_str)
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    Err(PipelineError::new(format!(
        "Ordering must include all primary key fields. Missing: {}.",
        missing.join(", ")
    )))
}

fn merge_predicates(
    conn: Option<RowPredicate>,
    req_filter: &Option<Condition>,
) -> Option<RowPredicate> {
    let Some(req_filter) = req_filter else {
        return conn;
    };
    // FilterStart's conditions were checked when the pipeline was built.
    let req: RowPredicate = create_predicate(req_filter).unwrap_or_else(|_| Rc::new(|_| false));
    Some(match conn {
        None => req,
        Some(conn) => Rc::new(move |r| conn(r) && req(r)),
    })
}

fn generate_with_constraint(nodes: NodeStream, constraint: Option<Constraint>) -> NodeStream {
    match constraint {
        None => nodes,
        Some(c) => Box::new(nodes.take_while(move |n| c.matches(&n.row))),
    }
}

/// `generateWithStart`: skips rows before `start`.
fn generate_with_start(nodes: NodeStream, start: Option<Start>, compare: Comparator) -> NodeStream {
    let Some(start) = start else {
        return nodes;
    };
    let mut started = false;
    Box::new(nodes.filter(move |n| {
        if !started {
            let c = compare(&n.row, &start.row);
            started = match start.basis {
                Basis::At => c != Ordering::Less,
                Basis::After => c == Ordering::Greater,
            };
        }
        started
    }))
}

fn compute_overlays(
    start_at: Option<&Row>,
    constraint: Option<&Constraint>,
    overlay: Option<&Overlay>,
    start_at_compare: &Comparator,
    filter: Option<&RowPredicate>,
    multi_constraints: Option<&[Vec<Constraint>]>,
) -> Overlays {
    let mut o = match overlay.map(|o| &o.change) {
        Some(SourceChange::Add(r)) => Overlays {
            add: Some(r.clone()),
            remove: None,
        },
        Some(SourceChange::Remove(r)) => Overlays {
            add: None,
            remove: Some(r.clone()),
        },
        Some(SourceChange::Edit { row, old_row }) => Overlays {
            add: Some(row.clone()),
            remove: Some(old_row.clone()),
        },
        None => Overlays::default(),
    };
    let keep = |o: &mut Overlays, f: &dyn Fn(&Row) -> bool| {
        o.add = o.add.take().filter(|r| f(r));
        o.remove = o.remove.take().filter(|r| f(r));
    };
    if let Some(start_at) = start_at {
        keep(&mut o, &|r| start_at_compare(r, start_at) != Ordering::Less);
    }
    if let Some(c) = constraint {
        keep(&mut o, &|r| c.matches(r));
    }
    for mc in multi_constraints.into_iter().flatten() {
        if !mc.is_empty() {
            keep(&mut o, &|r| mc.iter().any(|c| c.matches(r)));
        }
    }
    if let Some(p) = filter {
        keep(&mut o, &|r| p(r));
    }
    o
}

/// `generateWithOverlayInner`: splices the in-flight push into `rows`.
fn generate_with_overlay(rows: Stream<Row>, overlays: Overlays, compare: Comparator) -> NodeStream {
    Box::new(OverlayIterator {
        rows,
        overlays,
        compare,
        add_yielded: false,
        remove_skipped: false,
        pending: None,
        finished: false,
    })
}

struct OverlayIterator {
    rows: Stream<Row>,
    overlays: Overlays,
    compare: Comparator,
    add_yielded: bool,
    remove_skipped: bool,
    pending: Option<Row>,
    finished: bool,
}

impl Iterator for OverlayIterator {
    type Item = Node;

    fn next(&mut self) -> Option<Node> {
        if let Some(p) = self.pending.take() {
            return Some(Node::new(p));
        }
        if self.finished {
            return None;
        }
        for row in self.rows.by_ref() {
            let mut emit_add = None;
            if !self.add_yielded
                && let Some(add) = &self.overlays.add
                && (self.compare)(add, &row) == Ordering::Less
            {
                self.add_yielded = true;
                emit_add = Some(add.clone());
            }
            if !self.remove_skipped
                && let Some(remove) = &self.overlays.remove
                && (self.compare)(remove, &row) == Ordering::Equal
            {
                self.remove_skipped = true;
                if let Some(add) = emit_add {
                    return Some(Node::new(add));
                }
                continue;
            }
            if let Some(add) = emit_add {
                self.pending = Some(row);
                return Some(Node::new(add));
            }
            return Some(Node::new(row));
        }
        self.finished = true;
        if !self.add_yielded
            && let Some(add) = &self.overlays.add
        {
            self.add_yielded = true;
            return Some(Node::new(add.clone()));
        }
        None
    }
}

/// Orders nodes.
pub type NodeComparator = Rc<dyn Fn(&Node, &Node) -> Ordering>;

/// `mergeSortedStreams`: an N-way merge of streams sorted by `compare`. The
/// stream of the node emitted last is pulled again only when the next node
/// is asked for, as the JS generator does on resume. Ties go to the earlier
/// stream.
pub fn merge_sorted_streams(streams: Vec<NodeStream>, compare: NodeComparator) -> NodeStream {
    lazy(move || {
        let mut streams = streams;
        let mut heads: Vec<Option<Node>> = streams.iter_mut().map(|s| s.next()).collect();
        let mut refill: Option<usize> = None;
        std::iter::from_fn(move || {
            if let Some(i) = refill.take() {
                heads[i] = streams[i].next();
            }
            let mut best: Option<usize> = None;
            for (i, h) in heads.iter().enumerate() {
                let Some(h) = h else { continue };
                if best
                    .is_none_or(|b| compare(h, heads[b].as_ref().expect("head")) == Ordering::Less)
                {
                    best = Some(i);
                }
            }
            let i = best?;
            refill = Some(i);
            heads[i].take()
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::Object;
    use crate::map::OrderedMap;
    use crate::schema::{Column, ValueType};

    fn table() -> Table {
        let col = |t| Column {
            type_: t,
            optional: false,
            server_name: None,
        };
        let mut columns = OrderedMap::new();
        columns.insert("id".to_owned(), col(ValueType::String));
        columns.insert("n".to_owned(), col(ValueType::Number));
        Table {
            name: "t".to_owned(),
            server_name: None,
            columns,
            primary_key: vec!["id".to_owned()],
        }
    }

    fn row(id: &str, n: f64) -> Row {
        let mut o = Object::new();
        o.insert("id", Value::from(id));
        o.insert("n", Value::Number(n));
        Rc::new(o)
    }

    fn ids(s: NodeStream) -> Vec<String> {
        s.map(|n| n.row.get("id").unwrap().as_str().unwrap().to_owned())
            .collect()
    }

    fn sort(parts: &[(&str, Direction)]) -> Option<Vec<OrderPart>> {
        Some(
            parts
                .iter()
                .map(|(f, d)| OrderPart {
                    field: (*f).to_owned(),
                    direction: *d,
                })
                .collect(),
        )
    }

    #[test]
    fn fetches_in_order_with_constraints_and_starts() {
        let source = MemorySource::new(table());
        source.push_adds(vec![row("c", 1.0), row("a", 2.0), row("b", 1.0)]);
        let input = source
            .connect(
                sort(&[("n", Direction::Asc), ("id", Direction::Desc)]),
                None,
                None,
            )
            .unwrap();
        assert_eq!(ids(input.fetch(&FetchRequest::default())), ["c", "b", "a"]);
        let reverse = FetchRequest {
            reverse: true,
            ..FetchRequest::default()
        };
        assert_eq!(ids(input.fetch(&reverse)), ["a", "b", "c"]);
        let constrained = FetchRequest {
            constraint: Some(Constraint::new(vec![(
                "n".to_owned(),
                Some(Value::Number(1.0)),
            )])),
            ..FetchRequest::default()
        };
        assert_eq!(ids(input.fetch(&constrained)), ["c", "b"]);
        let after_c = FetchRequest {
            start: Some(Start {
                row: row("c", 1.0),
                basis: Basis::After,
            }),
            ..FetchRequest::default()
        };
        assert_eq!(ids(input.fetch(&after_c)), ["b", "a"]);
        assert!(
            source
                .connect(sort(&[("n", Direction::Asc)]), None, None)
                .is_err()
        );
    }

    /// An output that fetches back through the source while a push is in
    /// flight sees the pushed row, as it will be once written.
    #[test]
    fn a_push_is_visible_to_fetches_it_triggers() {
        let source = MemorySource::new(table());
        source.push_adds(vec![row("a", 1.0), row("c", 1.0)]);
        let input = source
            .connect(sort(&[("id", Direction::Asc)]), None, None)
            .unwrap();
        let seen: Rc<RefCell<Vec<Vec<String>>>> = Rc::default();
        let (i, s) = (Rc::downgrade(&input), seen.clone());
        let output = ClosureOutput::new(
            move |_| {
                let input = i.upgrade().unwrap();
                s.borrow_mut()
                    .push(ids(input.fetch(&FetchRequest::default())));
            },
            || {},
        );
        input.set_output(Rc::downgrade(&output) as Weak<dyn Output>);
        source.push(SourceChange::Add(row("b", 1.0)));
        source.push(SourceChange::Edit {
            row: row("a", 5.0),
            old_row: row("a", 1.0),
        });
        source.push(SourceChange::Remove(row("c", 1.0)));
        assert_eq!(
            *seen.borrow(),
            [vec!["a", "b", "c"], vec!["a", "b", "c"], vec!["a", "b"]]
        );
        assert_eq!(ids(input.fetch(&FetchRequest::default())), ["a", "b"]);
        assert_eq!(source.data()[0].get("n"), Some(&Value::Number(5.0)));
    }
}
