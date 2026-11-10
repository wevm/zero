//! `GroupBy`, `GroupProjection` and their helpers
//! (`packages/zql/src/ivm/group-by.ts`).

use super::*;
use crate::ast::{Aggregate as AggregateSpec, Ast, Direction, OrderPart};
use crate::json::{self, Object, Value};
use crate::keys::aggregate_columns as cols;
use crate::map::OrderedMap;
use crate::schema::{Column, ValueType};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

/// The columns, primary key and sort of a grouped operator's rows.
pub type GroupSchema = (OrderedMap<Column>, Vec<String>, Vec<OrderPart>);

/// `groupSourceSchema`: the columns, primary key and sort of the rows
/// `GroupBy` emits: the group columns, the group's key (`_0_key`) and the
/// aggregate's payload, with a sum or average's row count (`_0_rows`).
///
/// The key is the group's values in one string, since a synced row key
/// can't hold a `null`, which is a value like any other here.
pub fn group_source_schema(
    input_columns: &OrderedMap<Column>,
    ordering: &[OrderPart],
    aggregate: &AggregateSpec,
) -> Result<GroupSchema, PipelineError> {
    let group_key: Vec<String> = ordering.iter().map(|p| p.field.clone()).collect();
    let (mut columns, _) = aggregate_source_schema(input_columns, &group_key, aggregate)?;
    let string = Column {
        type_: ValueType::String,
        optional: false,
        server_name: None,
    };
    columns.insert(cols::KEY, string);
    if carries_components(aggregate.fn_) {
        columns.insert(
            cols::ROWS,
            Column {
                type_: ValueType::Number,
                optional: false,
                server_name: None,
            },
        );
    }
    // The key is unique and follows from the columns: it only makes the
    // order total.
    let mut sort = ordering.to_vec();
    sort.push(OrderPart {
        field: cols::KEY.to_owned(),
        direction: Direction::Asc,
    });
    Ok((columns, vec![cols::KEY.to_owned()], sort))
}

/// `groupKey`: a group's `_0_key`, which also keys its state: `valueToKey`
/// of the value for one column, the values JSON encoded for several.
pub fn group_key(values: &[Value]) -> String {
    if values.len() == 1 {
        value_to_key(&values[0])
    } else {
        json::stringify(&Value::Array(values.to_vec()))
    }
}

/// `groupOrdering`: the order of a grouped query's rows: its `orderBy`
/// restricted to the group columns (its `groupBy`, or the columns a count
/// of distinct combinations groups by), then the rest of them ascending.
pub fn group_ordering(ast: &Ast) -> Vec<OrderPart> {
    let columns: &[String] = ast
        .group_by
        .as_deref()
        .or_else(|| ast.aggregate.as_ref().and_then(|a| a.distinct.as_deref()))
        .unwrap_or_default();
    let mut ordering: Vec<OrderPart> = Vec::new();
    for part in ast.order_by.as_deref().unwrap_or_default() {
        if columns.contains(&part.field) && !ordering.iter().any(|p| p.field == part.field) {
            ordering.push(part.clone());
        }
    }
    for column in columns {
        if !ordering.iter().any(|p| &p.field == column) {
            ordering.push(OrderPart {
                field: column.clone(),
                direction: Direction::Asc,
            });
        }
    }
    ordering
}

/// Groups its input by columns and reduces each group, as `GROUP BY` does:
/// one row per combination of the columns' values,
/// `{...columns, _0_key, _0_value}`, with a sum or average's components. A
/// distinct is a GroupBy with a count, whose value a view doesn't show.
///
/// The input is sorted by the group columns first, so a fetch folds each
/// run of a group in one pass. Every group's state is kept: a push adds a
/// group with its first row, removes it with its last, and otherwise edits
/// it when its value changes. A min or max whose extreme leaves is read
/// again in `reconcile`, as in `Aggregate`.
pub struct GroupBy {
    me: Weak<GroupBy>,
    input: Rc<dyn Input>,
    columns: Vec<String>,
    reducer: Reducer,
    /// A sum or average row carries its group's row count.
    carries_rows: bool,
    schema: Rc<SourceSchema>,
    storage: RefCell<HashMap<String, AggState>>,
    /// Set by the first fetch: until then nothing downstream has the rows.
    hydrated: Cell<bool>,
    /// min and max groups whose extreme a push removed, with a row of the
    /// group, in insertion order. Their storage keeps the value last sent,
    /// with the current count.
    dirty_groups: RefCell<Vec<(String, Row)>>,
    output: RefCell<Weak<dyn Output>>,
}

impl GroupBy {
    /// `ordering` names each group column once; the input must be sorted by
    /// it first.
    pub fn new(
        input: Rc<dyn Input>,
        ordering: &[OrderPart],
        table_name: impl Into<String>,
        aggregate: AggregateSpec,
    ) -> Result<Rc<Self>, PipelineError> {
        let reducer = Reducer::new(aggregate)?;
        if ordering.is_empty() {
            return Err(PipelineError::new("GroupBy: needs at least one column"));
        }
        let input_schema = input.get_schema();
        let input_sort = input_schema.sort.as_deref().unwrap_or_default();
        let sorted = ordering.iter().enumerate().all(|(i, part)| {
            input_sort
                .get(i)
                .is_some_and(|s| s.field == part.field && s.direction == part.direction)
        });
        if !sorted {
            return Err(PipelineError::new(
                "GroupBy: the input must be sorted by the group columns first",
            ));
        }
        let (columns, primary_key, sort) =
            group_source_schema(&input_schema.columns, ordering, &reducer.aggregate)?;
        let schema = Rc::new(SourceSchema {
            table_name: table_name.into(),
            columns,
            primary_key,
            relationships: Vec::new(),
            is_hidden: false,
            system: input_schema.system,
            compare_rows: make_comparator(&sort, false),
            sort: Some(sort),
        });
        let carries_rows = carries_components(reducer.fn_());
        let op = Rc::new_cyclic(|me| GroupBy {
            me: me.clone(),
            input: input.clone(),
            columns: ordering.iter().map(|p| p.field.clone()).collect(),
            reducer,
            carries_rows,
            schema,
            storage: RefCell::default(),
            hydrated: Cell::new(false),
            dirty_groups: RefCell::default(),
            output: RefCell::new(no_output()),
        });
        input.set_output(Rc::downgrade(&op) as Weak<dyn Output>);
        Ok(op)
    }

    fn emit(&self, change: Change) {
        let out = self.output.borrow().upgrade();
        if let Some(out) = out {
            out.push(change, self);
        }
    }

    fn group_values(&self, row: &Row) -> Vec<Value> {
        self.columns
            .iter()
            .map(|c| row.get(c).cloned().unwrap_or(Value::Null))
            .collect()
    }

    fn stored(&self, key: &str) -> Option<AggState> {
        self.storage.borrow().get(key).cloned()
    }

    /// Records `state` for the group and returns its row.
    fn store(&self, values: &[Value], state: AggState) -> Node {
        let node = self.make_node(values, &state);
        self.storage.borrow_mut().insert(group_key(values), state);
        node
    }

    fn make_node(&self, values: &[Value], state: &AggState) -> Node {
        let mut row = Object::new();
        for (c, v) in self.columns.iter().zip(values) {
            row.insert(c.clone(), v.clone());
        }
        row.insert(cols::KEY, Value::String(group_key(values)));
        self.reducer.write_payload(&mut row, state);
        if self.carries_rows {
            row.insert(cols::ROWS, Value::Number(state.count as f64));
        }
        Node::new(Rc::new(row))
    }

    fn is_dirty(&self, key: &str) -> bool {
        self.dirty_groups.borrow().iter().any(|(k, _)| k == key)
    }

    /// `Map.set`: a group already dirty keeps its place.
    fn mark_dirty(&self, key: &str, row: &Row) {
        let mut dirty = self.dirty_groups.borrow_mut();
        match dirty.iter_mut().find(|(k, _)| k == key) {
            Some(entry) => entry.1 = row.clone(),
            None => dirty.push((key.to_owned(), row.clone())),
        }
    }

    /// Stores `state` and sends the move from `stored` downstream, if any.
    fn emit_if_changed(&self, values: &[Value], stored: &AggState, state: AggState) {
        let changed = self.reducer.changed(stored, &state)
            || (self.carries_rows && stored.count != state.count);
        let node = self.make_node(values, &state);
        self.storage.borrow_mut().insert(group_key(values), state);
        // A row whose field is null moves only a sum or average's row count.
        if changed {
            self.emit(Change::Edit {
                node,
                old_node: self.make_node(values, stored),
            });
        }
    }

    fn add(&self, row: &Row) {
        let values = self.group_values(row);
        let key = group_key(&values);
        let Some(stored) = self.stored(&key) else {
            let mut state = AggState::default();
            self.reducer.contribute(&mut state, row);
            let node = self.store(&values, state);
            return self.emit(Change::Add(node));
        };
        let mut state = stored.clone();
        if self.is_dirty(&key) {
            // Read again in reconcile; only the count is kept up to date.
            state.count += 1;
            self.storage.borrow_mut().insert(key, state);
            return;
        }
        self.reducer.contribute(&mut state, row);
        self.emit_if_changed(&values, &stored, state);
    }

    fn remove(&self, row: &Row) {
        let values = self.group_values(row);
        let key = group_key(&values);
        let Some(stored) = self.stored(&key) else {
            return report("GroupBy: removed a row of a group it lacks");
        };
        if stored.count == 1 {
            self.storage.borrow_mut().remove(&key);
            self.dirty_groups.borrow_mut().retain(|(k, _)| *k != key);
            return self.emit(Change::Remove(self.make_node(&values, &stored)));
        }
        let mut state = stored.clone();
        if is_invertible(self.reducer.fn_()) {
            self.reducer.apply_invertible(&mut state, row, -1, false);
        } else if self.is_dirty(&key)
            || !self
                .reducer
                .push_extreme(&mut state, &Change::Remove(Node::new(row.clone())))
        {
            // Keep what was last sent downstream, with the count down by one.
            self.mark_dirty(&key, row);
            let mut kept = stored;
            kept.count -= 1;
            self.storage.borrow_mut().insert(key, kept);
            return;
        }
        self.emit_if_changed(&values, &stored, state);
    }

    /// An edit within a group.
    fn update(&self, old_row: &Row, row: &Row) {
        let values = self.group_values(row);
        let key = group_key(&values);
        let Some(stored) = self.stored(&key) else {
            return;
        };
        if self.is_dirty(&key) || self.reducer.same_value(old_row, row) {
            return;
        }
        if !is_invertible(self.reducer.fn_()) {
            // The extreme may have moved either way.
            return self.mark_dirty(&key, row);
        }
        let mut state = stored.clone();
        self.reducer.apply_invertible(&mut state, old_row, -1, true);
        self.reducer.apply_invertible(&mut state, row, 1, true);
        self.emit_if_changed(&values, &stored, state);
    }

    /// Reads the rows of `row`'s group. It leaves out the null values, which
    /// a constraint never matches, so it can read rows of other groups too,
    /// which the caller skips.
    fn constraint(&self, row: &Row) -> Option<Constraint> {
        let mut constraint: Option<Constraint> = None;
        for c in &self.columns {
            match row.get(c) {
                None | Some(Value::Null) => {}
                Some(v) => constraint
                    .get_or_insert_with(Constraint::default)
                    .set(c, Some(v.clone())),
            }
        }
        constraint
    }

    fn fetch_now(self: &Rc<Self>, req: &FetchRequest) -> NodeStream {
        // A top-level query without a bound: the view reads every row once.
        if req.constraint.is_some() || req.start.is_some() || req.reverse {
            report("GroupBy: fetch takes no constraint, start or reverse");
            return empty();
        }
        self.hydrated.set(true);
        let me = self.clone();
        // The first row of the current run, and its group's state.
        let mut first: Option<Row> = None;
        let mut state = AggState::default();
        let mut it = self.input.fetch(&FetchRequest::default());
        let mut done = false;
        Box::new(std::iter::from_fn(move || {
            while !done {
                let Some(node) = it.next() else {
                    done = true;
                    let first = first.take()?;
                    let values = me.group_values(&first);
                    return Some(me.store(&values, std::mem::take(&mut state)));
                };
                if let Some(current) = &first
                    && row_equals_for_compound_key(current, &node.row, &me.columns)
                {
                    me.reducer.contribute(&mut state, &node.row);
                    continue;
                }
                let finished = first.take().map(|f| {
                    let values = me.group_values(&f);
                    me.store(&values, std::mem::take(&mut state))
                });
                first = Some(node.row.clone());
                me.reducer.contribute(&mut state, &node.row);
                if finished.is_some() {
                    return finished;
                }
            }
            None
        }))
    }
}

impl InputBase for GroupBy {
    fn get_schema(&self) -> Rc<SourceSchema> {
        self.schema.clone()
    }
    fn destroy(&self) {
        self.input.destroy()
    }
}

impl Input for GroupBy {
    fn set_output(&self, output: Weak<dyn Output>) {
        *self.output.borrow_mut() = output;
    }

    fn fetch(&self, req: &FetchRequest) -> NodeStream {
        let me = self.me.clone();
        let req = req.clone();
        lazy(move || -> NodeStream {
            match me.upgrade() {
                Some(me) => me.fetch_now(&req),
                None => empty(),
            }
        })
    }
}

impl Output for GroupBy {
    fn push(&self, change: Change, _pusher: &dyn InputBase) {
        if !self.hydrated.get() {
            return;
        }
        match change {
            Change::Add(node) => self.add(&node.row),
            Change::Remove(node) => self.remove(&node.row),
            Change::Edit { node, old_node } => {
                if row_equals_for_compound_key(&old_node.row, &node.row, &self.columns) {
                    self.update(&old_node.row, &node.row);
                } else {
                    self.remove(&old_node.row);
                    self.add(&node.row);
                }
            }
            // The input has no relationships the group rows show.
            Change::Child(..) => {}
        }
    }

    fn reconcile(&self, _pusher: &dyn InputBase) {
        let dirty = std::mem::take(&mut *self.dirty_groups.borrow_mut());
        for (key, group_row) in dirty {
            // Its last row may have gone in the meantime.
            let Some(stored) = self.stored(&key) else {
                continue;
            };
            let mut state = AggState::default();
            for node in self.input.fetch(&FetchRequest {
                constraint: self.constraint(&group_row),
                ..FetchRequest::default()
            }) {
                if row_equals_for_compound_key(&node.row, &group_row, &self.columns) {
                    self.reducer.contribute(&mut state, &node.row);
                }
            }
            self.emit_if_changed(&self.group_values(&group_row), &stored, state);
        }
        let out = self.output.borrow().upgrade();
        if let Some(out) = out {
            out.reconcile(self);
        }
    }
}

/// A group column as a view shows it: its name in the view and in the
/// input.
#[derive(Clone, Debug)]
pub struct ProjectedColumn {
    pub out: String,
    pub from: String,
}

/// Turns the rows of a `GroupBy`, or of the source a synced client reads
/// them from, into the rows a view shows: the group columns under the
/// query's names and, for a `groupBy()` with an aggregate, its value under
/// `value_name` (`count`, `sum`, …). A `distinct()` shows no value, so an
/// edit, which only moves its count, doesn't reach the view.
pub struct GroupProjection {
    input: Rc<dyn Input>,
    projection: Rc<(Vec<ProjectedColumn>, Option<String>)>,
    schema: Rc<SourceSchema>,
    output: RefCell<Weak<dyn Output>>,
}

fn project(projection: &(Vec<ProjectedColumn>, Option<String>), node: &Node) -> Node {
    let (columns, value_name) = projection;
    let mut row = Object::new();
    for c in columns {
        if let Some(v) = node.row.get(&c.from) {
            row.insert(c.out.clone(), v.clone());
        }
    }
    if let Some(name) = value_name
        && let Some(v) = node.row.get(cols::VALUE)
    {
        row.insert(name.clone(), v.clone());
    }
    Node::new(Rc::new(row))
}

impl GroupProjection {
    pub fn new(
        input: Rc<dyn Input>,
        columns: Vec<ProjectedColumn>,
        value_name: Option<String>,
    ) -> Result<Rc<Self>, PipelineError> {
        // The value would overwrite the group column. The query builder
        // rejects that; a hand-built AST can still name one.
        if let Some(name) = &value_name
            && columns.iter().any(|c| &c.out == name)
        {
            return Err(PipelineError::new(format!(
                "groupBy(): the column \"{name}\" has the same name as the {name}() value"
            )));
        }
        let input_schema = input.get_schema();
        let view_name = |field: &str| columns.iter().find(|c| c.from == field).map(|c| &c.out);
        let sort: Vec<OrderPart> = input_schema
            .sort
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter_map(|p| {
                view_name(&p.field).map(|out| OrderPart {
                    field: out.clone(),
                    direction: p.direction,
                })
            })
            .collect();
        let mut view_columns = OrderedMap::new();
        for c in &columns {
            if let Some(col) = input_schema.columns.get(&c.from) {
                view_columns.insert(c.out.clone(), col.clone());
            }
        }
        if let Some(name) = &value_name
            && let Some(col) = input_schema.columns.get(cols::VALUE)
        {
            view_columns.insert(name.clone(), col.clone());
        }
        let schema = Rc::new(SourceSchema {
            table_name: input_schema.table_name.clone(),
            columns: view_columns,
            primary_key: columns.iter().map(|c| c.out.clone()).collect(),
            relationships: Vec::new(),
            is_hidden: false,
            system: input_schema.system,
            compare_rows: make_comparator(&sort, false),
            sort: Some(sort),
        });
        let op = Rc::new(GroupProjection {
            input: input.clone(),
            projection: Rc::new((columns, value_name)),
            schema,
            output: RefCell::new(no_output()),
        });
        input.set_output(Rc::downgrade(&op) as Weak<dyn Output>);
        Ok(op)
    }

    fn emit(&self, change: Change) {
        let out = self.output.borrow().upgrade();
        if let Some(out) = out {
            out.push(change, self);
        }
    }
}

impl InputBase for GroupProjection {
    fn get_schema(&self) -> Rc<SourceSchema> {
        self.schema.clone()
    }
    fn destroy(&self) {
        self.input.destroy()
    }
}

impl Input for GroupProjection {
    fn set_output(&self, output: Weak<dyn Output>) {
        *self.output.borrow_mut() = output;
    }

    fn fetch(&self, req: &FetchRequest) -> NodeStream {
        let projection = self.projection.clone();
        Box::new(self.input.fetch(req).map(move |n| project(&projection, &n)))
    }
}

impl Output for GroupProjection {
    fn push(&self, change: Change, _pusher: &dyn InputBase) {
        let p = &self.projection;
        match change {
            Change::Add(n) => self.emit(Change::Add(project(p, &n))),
            Change::Remove(n) => self.emit(Change::Remove(project(p, &n))),
            Change::Edit { node, old_node } => {
                // A row's key is its group's values, so an edit only moves
                // its value, which a distinct doesn't show.
                if p.1.is_none()
                    || identical(node.row.get(cols::VALUE), old_node.row.get(cols::VALUE))
                {
                    return;
                }
                self.emit(Change::Edit {
                    node: project(p, &node),
                    old_node: project(p, &old_node),
                });
            }
            Change::Child(..) => {}
        }
    }

    fn reconcile(&self, _pusher: &dyn InputBase) {
        let out = self.output.borrow().upgrade();
        if let Some(out) = out {
            out.reconcile(self);
        }
    }
}
