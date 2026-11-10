//! The aggregate operators' helpers and the `Aggregate` operator
//! (`packages/zql/src/ivm/aggregate.ts`).

use super::*;
use crate::ast::{
    Aggregate as AggregateSpec, AggregateFunction, Direction, OrderPart, json_value_at,
};
use crate::compare;
use crate::json::{self, Object, Value};
use crate::keys::aggregate_columns as cols;
use crate::map::OrderedMap;
use crate::schema::{Column, ValueType};
use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

/// `isSyntheticColumn`: the columns the aggregate operators add to a row.
pub fn is_synthetic_column(column: &str) -> bool {
    column == cols::KEY || cols::PAYLOAD.contains(&column)
}

/// `carriesComponents`: whether `fn_`'s rows carry the sum and count.
pub fn carries_components(fn_: AggregateFunction) -> bool {
    matches!(fn_, AggregateFunction::Sum | AggregateFunction::Avg)
}

/// `isInvertible`: a change to one row moves the result by a fixed delta.
pub fn is_invertible(fn_: AggregateFunction) -> bool {
    matches!(
        fn_,
        AggregateFunction::Count | AggregateFunction::Sum | AggregateFunction::Avg
    )
}

/// `aggregateValue`: the value `row` contributes to `aggregate`: its field,
/// or the JSON leaf at the path when it is one the function takes (a number
/// for sum and avg, a number or string for min and max), else null.
pub fn aggregate_value(aggregate: &AggregateSpec, row: &Row) -> Value {
    let v = row.get(aggregate.field.as_deref().unwrap_or(""));
    let Some(path) = &aggregate.path else {
        return v.cloned().unwrap_or(Value::Null);
    };
    match json_value_at(path, v) {
        leaf @ Value::Number(_) => leaf.clone(),
        leaf @ Value::String(_)
            if matches!(
                aggregate.fn_,
                AggregateFunction::Min | AggregateFunction::Max
            ) =>
        {
            leaf.clone()
        }
        _ => Value::Null,
    }
}

fn column(type_: ValueType, optional: bool) -> Column {
    Column {
        type_,
        optional,
        server_name: None,
    }
}

/// `aggregateSourceSchema`: the columns and primary key of the rows an
/// aggregate emits, from its input's columns and its group key. A
/// top-level aggregate has no group key, so its one row is keyed by
/// `_0_key`.
pub fn aggregate_source_schema(
    input_columns: &OrderedMap<Column>,
    group_key: &[String],
    aggregate: &AggregateSpec,
) -> Result<(OrderedMap<Column>, Vec<String>), PipelineError> {
    let mut columns = OrderedMap::new();
    for c in group_key {
        assert_not_synthetic_column(c)?;
        let Some(schema) = input_columns.get(c) else {
            return Err(PipelineError::new(format!(
                "Aggregate: group key column \"{c}\" missing"
            )));
        };
        columns.insert(c.clone(), schema.clone());
    }
    if group_key.is_empty() {
        columns.insert(cols::KEY, column(ValueType::Number, false));
    }
    let value = match aggregate.fn_ {
        AggregateFunction::Count => column(ValueType::Number, false),
        AggregateFunction::Sum | AggregateFunction::Avg => column(ValueType::Number, true),
        AggregateFunction::Min | AggregateFunction::Max => {
            if aggregate.path.is_some() {
                column(ValueType::Json, true)
            } else {
                let field = aggregate.field.as_deref();
                let Some(schema) = field.and_then(|f| input_columns.get(f)) else {
                    return Err(PipelineError::new(format!(
                        "Aggregate: {} field \"{}\" missing",
                        aggregate.fn_.as_str(),
                        field.unwrap_or("")
                    )));
                };
                Column {
                    optional: true,
                    ..schema.clone()
                }
            }
        }
    };
    columns.insert(cols::VALUE, value);
    if carries_components(aggregate.fn_) {
        columns.insert(cols::SUM, column(ValueType::Number, false));
        columns.insert(cols::COUNT, column(ValueType::Number, false));
    }
    let primary_key = if group_key.is_empty() {
        vec![cols::KEY.to_owned()]
    } else {
        group_key.to_vec()
    };
    Ok((columns, primary_key))
}

/// `assertNotSyntheticColumn`.
pub fn assert_not_synthetic_column(column: &str) -> Result<(), PipelineError> {
    if is_synthetic_column(column) {
        return Err(PipelineError::new(format!(
            "Aggregate: group key column \"{column}\" collides with the synthetic aggregate column of the same name"
        )));
    }
    Ok(())
}

/// A group's running state (`AggState`).
#[derive(Clone, Debug, PartialEq, Default)]
pub(crate) struct AggState {
    /// count(*): every row.
    pub(crate) count: i64,
    /// The sum of the non-null values.
    pub(crate) sum: f64,
    /// How many values are non-null.
    pub(crate) non_null: i64,
    /// The min or max of the non-null values; null when there are none.
    pub(crate) extreme: Value,
}

/// How an aggregate function reduces a group's rows (`Reducer`).
pub(crate) struct Reducer {
    pub(crate) aggregate: AggregateSpec,
}

impl Reducer {
    pub(crate) fn new(aggregate: AggregateSpec) -> Result<Self, PipelineError> {
        let fn_ = aggregate.fn_;
        let is_count = fn_ == AggregateFunction::Count;
        if is_count != aggregate.field.is_none() {
            return Err(PipelineError::new(format!(
                "Aggregate: {} {} field",
                fn_.as_str(),
                if is_count { "takes no" } else { "requires a" }
            )));
        }
        if aggregate.path.is_some() && aggregate.field.is_none() {
            return Err(PipelineError::new(
                "Aggregate: a JSON path requires a field",
            ));
        }
        Ok(Reducer { aggregate })
    }

    pub(crate) fn fn_(&self) -> AggregateFunction {
        self.aggregate.fn_
    }

    /// The result for a state.
    pub(crate) fn value(&self, s: &AggState) -> Value {
        match self.fn_() {
            AggregateFunction::Count => Value::Number(s.count as f64),
            AggregateFunction::Sum if s.non_null == 0 => Value::Null,
            AggregateFunction::Sum => Value::Number(s.sum),
            AggregateFunction::Avg if s.non_null == 0 => Value::Null,
            AggregateFunction::Avg => Value::Number(s.sum / s.non_null as f64),
            AggregateFunction::Min | AggregateFunction::Max => s.extreme.clone(),
        }
    }

    pub(crate) fn beats(&self, v: &Value, current: &Value) -> bool {
        let c = compare_aggregate_values(v, current);
        if self.fn_() == AggregateFunction::Max {
            c == Ordering::Greater
        } else {
            c == Ordering::Less
        }
    }

    /// The value `row` contributes, null when none.
    pub(crate) fn read(&self, row: &Row) -> Value {
        aggregate_value(&self.aggregate, row)
    }

    /// Whether an edit from `old` to `new` leaves the value reduced as it
    /// was.
    pub(crate) fn same_value(&self, old: &Row, new: &Row) -> bool {
        self.aggregate.field.is_none()
            || compare_aggregate_values(&self.read(old), &self.read(new)) == Ordering::Equal
    }

    /// Adds a row to a state being accumulated.
    pub(crate) fn contribute(&self, s: &mut AggState, row: &Row) {
        s.count += 1;
        if self.aggregate.field.is_none() {
            return;
        }
        let v = self.read(row);
        if v.is_null() {
            return;
        }
        s.non_null += 1;
        match self.fn_() {
            AggregateFunction::Sum | AggregateFunction::Avg => match v {
                Value::Number(n) => s.sum += n,
                _ => report(format!("Aggregate: {} needs a number", self.fn_().as_str())),
            },
            AggregateFunction::Min | AggregateFunction::Max => {
                if s.extreme.is_null() || self.beats(&v, &s.extreme) {
                    s.extreme = v;
                }
            }
            AggregateFunction::Count => {}
        }
    }

    /// The invertible delta of count, sum and avg.
    pub(crate) fn apply_invertible(
        &self,
        s: &mut AggState,
        row: &Row,
        sign: i64,
        field_only: bool,
    ) {
        if !field_only {
            s.count += sign;
        }
        let Some(field) = &self.aggregate.field else {
            return;
        };
        match self.read(row) {
            Value::Null => {}
            Value::Number(n) => {
                s.sum += sign as f64 * n;
                s.non_null += sign;
            }
            _ => report(format!(
                "Aggregate: {}({field}) requires a number",
                self.fn_().as_str()
            )),
        }
    }

    /// Min and max maintenance. Returns false when the extreme was removed
    /// or edited away: the group must be read again.
    pub(crate) fn push_extreme(&self, s: &mut AggState, change: &Change) -> bool {
        match change {
            Change::Add(node) => {
                self.contribute(s, &node.row);
                true
            }
            Change::Remove(node) => {
                let v = self.read(&node.row);
                s.count -= 1;
                if v.is_null() {
                    return true;
                }
                if !s.extreme.is_null()
                    && compare_aggregate_values(&v, &s.extreme) == Ordering::Equal
                {
                    return false;
                }
                s.non_null -= 1;
                true
            }
            Change::Edit { node, old_node } => self.same_value(&old_node.row, &node.row),
            Change::Child(..) => true,
        }
    }

    /// Whether a group's row changes from `stored` to `state`: its value,
    /// or a sum or average's components.
    pub(crate) fn changed(&self, stored: &AggState, state: &AggState) -> bool {
        !identical(Some(&self.value(stored)), Some(&self.value(state)))
            || (carries_components(self.fn_())
                && (stored.sum != state.sum || stored.non_null != state.non_null))
    }

    /// Writes the result, and a sum or average's components, into `row`.
    pub(crate) fn write_payload(&self, row: &mut Object, s: &AggState) {
        row.insert(cols::VALUE, self.value(s));
        if carries_components(self.fn_()) {
            row.insert(cols::SUM, Value::Number(s.sum));
            row.insert(cols::COUNT, Value::Number(s.non_null as f64));
        }
    }
}

/// `compareAggregateValues`: numbers before strings, as in JS and SQLite,
/// for the leaves of a JSON path; otherwise `compareValues`. Values JS can't
/// compare, such as the objects of a json column without a path, fail the
/// query as JS's throw does. Equal values compare equal first, as JS's
/// `a === b` does for one row's own value.
pub fn compare_aggregate_values(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Number(_), Value::String(_)) => Ordering::Less,
        (Value::String(_), Value::Number(_)) => Ordering::Greater,
        _ => match compare::compare_checked(a, b) {
            Ok(o) => o,
            Err(_) if a == b => Ordering::Equal,
            Err(e) => {
                report(e.to_string());
                compare::compare(a, b)
            }
        },
    }
}

/// `valueToKey`: a string without surrogates (in UTF-16) is `s` and the
/// string, any other value its JSON.
pub fn value_to_key(v: &Value) -> String {
    match v {
        Value::String(s) if s.chars().all(|c| (c as u32) <= 0xFFFF) => format!("s{s}"),
        v => json::stringify(v),
    }
}

/// Reduces the rows of a relationship per parent, or of a whole query, to
/// an aggregate: one row per group key, `{...groupKey, _0_value}`, so a
/// `Join` routes it as it would the child rows.
///
/// count, sum and avg are kept in O(1) per change. min and max are too,
/// unless the extreme leaves; then the group is read again in `reconcile`.
pub struct Aggregate {
    me: Weak<Aggregate>,
    input: Rc<dyn Input>,
    group_key: Vec<String>,
    reducer: Reducer,
    schema: Rc<SourceSchema>,
    storage: RefCell<HashMap<String, AggState>>,
    /// min and max groups whose extreme a push removed, in insertion order,
    /// to read again in `reconcile`. Their storage keeps the state last
    /// sent.
    dirty_groups: RefCell<Vec<(String, Vec<Value>)>>,
    output: RefCell<Weak<dyn Output>>,
}

impl Aggregate {
    pub fn new(
        input: Rc<dyn Input>,
        group_key: Vec<String>,
        aggregate: AggregateSpec,
        table_name: Option<String>,
    ) -> Result<Rc<Self>, PipelineError> {
        let reducer = Reducer::new(aggregate)?;
        let input_schema = input.get_schema();
        let (columns, primary_key) =
            aggregate_source_schema(&input_schema.columns, &group_key, &reducer.aggregate)?;
        let sort: Vec<OrderPart> = primary_key
            .iter()
            .map(|k| OrderPart {
                field: k.clone(),
                direction: Direction::Asc,
            })
            .collect();
        let schema = Rc::new(SourceSchema {
            table_name: table_name.unwrap_or_else(|| input_schema.table_name.clone()),
            columns,
            primary_key,
            relationships: Vec::new(),
            is_hidden: false,
            system: input_schema.system,
            compare_rows: make_comparator(&sort, false),
            sort: Some(sort),
        });
        let op = Rc::new_cyclic(|me| Aggregate {
            me: me.clone(),
            input: input.clone(),
            group_key,
            reducer,
            schema,
            storage: RefCell::default(),
            dirty_groups: RefCell::default(),
            output: RefCell::new(no_output()),
        });
        input.set_output(Rc::downgrade(&op) as Weak<dyn Output>);
        Ok(op)
    }

    fn compute(&self, constraint: Option<Constraint>) -> AggState {
        let mut s = AggState::default();
        for node in self.input.fetch(&FetchRequest {
            constraint,
            ..FetchRequest::default()
        }) {
            self.reducer.contribute(&mut s, &node.row);
        }
        s
    }

    fn emit_if_changed(&self, values: &[Value], stored: &AggState, state: &AggState) {
        if !self.reducer.changed(stored, state) {
            return;
        }
        let out = self.output.borrow().upgrade();
        if let Some(out) = out {
            out.push(
                Change::Edit {
                    node: self.make_node(values, state),
                    old_node: self.make_node(values, stored),
                },
                self,
            );
        }
    }

    fn make_node(&self, values: &[Value], state: &AggState) -> Node {
        let mut row = Object::new();
        for (c, v) in self.group_key.iter().zip(values) {
            row.insert(c.clone(), v.clone());
        }
        if self.group_key.is_empty() {
            row.insert(cols::KEY, Value::Number(0.0));
        }
        self.reducer.write_payload(&mut row, state);
        Node::new(Rc::new(row))
    }

    fn constraint(&self, values: &[Value]) -> Constraint {
        Constraint::new(
            self.group_key
                .iter()
                .zip(values)
                .map(|(k, v)| (k.clone(), Some(v.clone())))
                .collect(),
        )
    }

    /// The group a fetch is for, or `None` when its constraint lacks the
    /// group key, which JS asserts.
    fn group_values(&self, constraint: Option<&Constraint>) -> Option<Vec<Value>> {
        if self.group_key.is_empty() {
            return Some(Vec::new());
        }
        let Some(constraint) = constraint else {
            report(
                "Aggregate requires a constraint on the group key (relationship aggregate only)",
            );
            return None;
        };
        let mut values = Vec::new();
        for c in &self.group_key {
            if !constraint.has(c) {
                report(format!(
                    "Aggregate: constraint missing group key column \"{c}\""
                ));
                return None;
            }
            values.push(constraint.get(c).cloned().unwrap_or(Value::Null));
        }
        Some(values)
    }

    /// `stateKey`.
    fn state_key(values: &[Value]) -> String {
        if values.len() == 1 {
            value_to_key(&values[0])
        } else {
            json::stringify(&Value::Array(values.to_vec()))
        }
    }

    fn fetch_now(&self, req: &FetchRequest) -> NodeStream {
        let Some(values) = self.group_values(req.constraint.as_ref()) else {
            return empty();
        };
        let key = Self::state_key(&values);
        let stored = self.storage.borrow().get(&key).cloned();
        let state = match stored {
            Some(s) => s,
            None => {
                let s = self.compute(req.constraint.clone());
                self.storage.borrow_mut().insert(key, s.clone());
                s
            }
        };
        from_vec(vec![self.make_node(&values, &state)])
    }

    fn is_dirty(&self, key: &str) -> bool {
        self.dirty_groups.borrow().iter().any(|(k, _)| k == key)
    }
}

impl InputBase for Aggregate {
    fn get_schema(&self) -> Rc<SourceSchema> {
        self.schema.clone()
    }
    fn destroy(&self) {
        self.input.destroy()
    }
}

impl Input for Aggregate {
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

impl Output for Aggregate {
    fn push(&self, change: Change, _pusher: &dyn InputBase) {
        let row = &change.node().row;
        let values: Vec<Value> = self
            .group_key
            .iter()
            .map(|k| row.get(k).cloned().unwrap_or(Value::Null))
            .collect();
        let key = Self::state_key(&values);
        // Nothing downstream has fetched the group: a later fetch computes
        // it.
        let Some(stored) = self.storage.borrow().get(&key).cloned() else {
            return;
        };
        let mut state = stored.clone();
        let r = &self.reducer;
        if is_invertible(r.fn_()) {
            match &change {
                Change::Add(n) => r.apply_invertible(&mut state, &n.row, 1, false),
                Change::Remove(n) => r.apply_invertible(&mut state, &n.row, -1, false),
                Change::Edit { node, old_node } => {
                    // Subtracting and adding again a value that didn't change
                    // would only add rounding error.
                    if r.same_value(&old_node.row, &node.row) {
                        return;
                    }
                    r.apply_invertible(&mut state, &old_node.row, -1, true);
                    r.apply_invertible(&mut state, &node.row, 1, true);
                }
                Change::Child(..) => return,
            }
        } else if self.is_dirty(&key) || !r.push_extreme(&mut state, &change) {
            if !self.is_dirty(&key) {
                self.dirty_groups.borrow_mut().push((key, values));
            }
            return;
        }
        if state.count < 0 {
            report("Aggregate: removed a row from an empty group");
            return;
        }
        self.storage.borrow_mut().insert(key, state.clone());
        self.emit_if_changed(&values, &stored, &state);
    }

    fn reconcile(&self, _pusher: &dyn InputBase) {
        let dirty = std::mem::take(&mut *self.dirty_groups.borrow_mut());
        for (key, values) in dirty {
            let Some(stored) = self.storage.borrow().get(&key).cloned() else {
                continue;
            };
            let state = self.compute(Some(self.constraint(&values)));
            self.storage.borrow_mut().insert(key, state.clone());
            self.emit_if_changed(&values, &stored, &state);
        }
        let out = self.output.borrow().upgrade();
        if let Some(out) = out {
            out.reconcile(self);
        }
    }
}
