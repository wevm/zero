//! The transaction a mutator runs in on the client (`TransactionImpl` in
//! `packages/zero-client/src/client/custom.ts`, with the CRUD operations of
//! `crud-impl.ts`). Writes go to the local store and to the IVM branch the
//! mutator reads through `run`.

use super::ivm_branch::IvmBranch;
use super::query_context::QueryContext;
use super::replica_store::KvWrite;
use super::{ApplicationError, Error};
use crate::ast::AggregateFunction;
use crate::builder::OptimisticAggregate;
use crate::hash::query::{hash_of_ast, hash_of_name_and_args};
use crate::ivm::{
    MemorySource, Row, Source, SourceChange, ViewValue, aggregate_value, capture, group_key,
};
use crate::json::{Object, Value};
use crate::keys::{
    aggregate_columns as cols, aggregate_row_key, got_queries_key, primary_key_string,
};
use crate::query::AnyQuery;
use crate::schema::{Schema, Table};
use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

/// Why a mutator runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// The first run, when the app calls `mutate`.
    Optimistic,
    /// A replay over newer server data while the mutation is pending.
    Rebase,
}

impl Reason {
    /// The reason as JS names it.
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::Optimistic => "optimistic",
            Reason::Rebase => "rebase",
        }
    }
}

/// The transaction a mutator runs in.
pub struct Transaction {
    client_id: String,
    mutation_id: f64,
    reason: Reason,
    token: Option<String>,
    pub(crate) write: RefCell<KvWrite>,
    branch: Rc<IvmBranch>,
    schema: Arc<Schema>,
    /// The rows written, by key, as the branch has them, so the main branch
    /// can take the same rows rather than a copy of each.
    pub(crate) rows: RefCell<HashMap<String, Row>>,
    /// A mutator's query computes its aggregates from the branch's rows,
    /// unless the server has answered it: then it reads the synced rows,
    /// with this client's pending changes applied, as a view of it does
    /// (`MutatorContext` in `custom.ts`).
    context: OnceCell<Rc<QueryContext>>,
    synced_context: OnceCell<Rc<QueryContext>>,
}

impl Transaction {
    pub(crate) fn new(
        client_id: String,
        mutation_id: f64,
        reason: Reason,
        token: Option<String>,
        write: KvWrite,
        branch: Rc<IvmBranch>,
        schema: Arc<Schema>,
    ) -> Self {
        Transaction {
            client_id,
            mutation_id,
            reason,
            token,
            write: RefCell::new(write),
            branch,
            schema,
            rows: RefCell::default(),
            context: OnceCell::new(),
            synced_context: OnceCell::new(),
        }
    }

    /// The write and the rows it wrote, once the mutator is done.
    pub(crate) fn finish(self) -> (KvWrite, HashMap<String, Row>) {
        (self.write.into_inner(), self.rows.into_inner())
    }

    /// Where the mutator runs: always `client` here.
    pub fn location(&self) -> &'static str {
        "client"
    }

    /// The client running the mutation.
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// The mutation's ID.
    pub fn mutation_id(&self) -> f64 {
        self.mutation_id
    }

    /// Why the mutator is running: optimistically, or again in a rebase.
    pub fn reason(&self) -> Reason {
        self.reason
    }

    /// The auth token the client has.
    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    /// The CRUD operations on `table`: `tx.mutate.issue` in JS.
    pub fn table<'a>(&'a self, table: &'a str) -> TableMutator<'a> {
        TableMutator { tx: self, table }
    }

    /// The rows `query` returns over this transaction's data.
    pub fn run(&self, query: &AnyQuery) -> Result<ViewValue, Error> {
        let query_id = match query.custom_query_id() {
            Some(id) => hash_of_name_and_args(&id.name, &id.args),
            None => hash_of_ast(query.ast()),
        };
        let synced = self.write.borrow().has(&got_queries_key(&query_id));
        let context = if synced {
            self.synced_context
                .get_or_init(|| QueryContext::local(self.branch.clone(), true))
        } else {
            self.context
                .get_or_init(|| QueryContext::local(self.branch.clone(), false))
        };
        context.snapshot(query)
    }

    fn table_schema(&self, table: &str) -> Result<&Table, Error> {
        self.schema
            .tables
            .get(table)
            .ok_or_else(|| ApplicationError::new(format!("Unknown table: {table}")).into())
    }

    fn key(table: &Table, value: &Object) -> Result<String, Error> {
        Ok(primary_key_string(&table.name, &table.primary_key, value)?)
    }

    fn source(&self, table: &str) -> Rc<MemorySource> {
        self.branch
            .source(table)
            .unwrap_or_else(|| panic!("No source for table {table}"))
    }

    /// Pushes `change`, failing as JS's push throws when an operator does.
    fn push(source: &MemorySource, change: SourceChange) -> Result<(), Error> {
        Ok(capture(|| source.push(change))?)
    }

    /// `insert` in `crud-impl.ts`: a row that exists is left alone. Columns
    /// the value leaves out are null.
    fn insert(&self, table_name: &str, value: &Object) -> Result<(), Error> {
        let table = self.table_schema(table_name)?;
        let key = Self::key(table, value)?;
        if self.write.borrow().has(&key) {
            return Ok(());
        }
        let mut row = value.clone();
        for column in table.columns.keys() {
            if !row.contains_key(column) {
                row.insert(column.clone(), Value::Null);
            }
        }
        let row: Row = Rc::new(row);
        self.write
            .borrow_mut()
            .set(&key, Value::Object((*row).clone()));
        self.rows.borrow_mut().insert(key, row.clone());
        Self::push(&self.source(table_name), SourceChange::Add(row.clone()))?;
        self.apply_optimistic_aggregate_deltas(table_name, None, Some(&row))
    }

    fn upsert(&self, table_name: &str, value: &Object) -> Result<(), Error> {
        let table = self.table_schema(table_name)?;
        if self.write.borrow().has(&Self::key(table, value)?) {
            self.update(table_name, value)
        } else {
            self.insert(table_name, value)
        }
    }

    /// Merges the value's columns into the row; a missing row is left alone.
    fn update(&self, table_name: &str, value: &Object) -> Result<(), Error> {
        let table = self.table_schema(table_name)?;
        let key = Self::key(table, value)?;
        let Some(prev) = self
            .write
            .borrow()
            .get(&key)
            .and_then(|v| v.as_object().cloned())
        else {
            return Ok(());
        };
        let mut next = prev.clone();
        for (k, v) in value.iter() {
            next.insert(k.clone(), v.clone());
        }
        self.write
            .borrow_mut()
            .set(&key, Value::Object(next.clone()));
        let row: Row = Rc::new(next);
        let old_row: Row = Rc::new(prev);
        self.rows.borrow_mut().insert(key, row.clone());
        Self::push(
            &self.source(table_name),
            SourceChange::Edit {
                row: row.clone(),
                old_row: old_row.clone(),
            },
        )?;
        self.apply_optimistic_aggregate_deltas(table_name, Some(&old_row), Some(&row))
    }

    fn delete(&self, table_name: &str, id: &Object) -> Result<(), Error> {
        let table = self.table_schema(table_name)?;
        let key = Self::key(table, id)?;
        let Some(prev) = self
            .write
            .borrow()
            .get(&key)
            .and_then(|v| v.as_object().cloned())
        else {
            return Ok(());
        };
        self.write.borrow_mut().delete(&key);
        self.rows.borrow_mut().remove(&key);
        let old_row: Row = Rc::new(prev);
        Self::push(
            &self.source(table_name),
            SourceChange::Remove(old_row.clone()),
        )?;
        self.apply_optimistic_aggregate_deltas(table_name, Some(&old_row), None)
    }

    /// `applyOptimisticAggregateDeltas`: moves the synced aggregates whose
    /// child rows live in `table` by a child row's change, in this
    /// transaction, so views and `run` see it at once. The rebase replays
    /// it until the server confirms the mutation, whose value then replaces
    /// it. An aggregate row not on the client is left alone, except that a
    /// grouped query's group is added by its first row.
    fn apply_optimistic_aggregate_deltas(
        &self,
        table: &str,
        old: Option<&Row>,
        new: Option<&Row>,
    ) -> Result<(), Error> {
        for agg in self.branch.optimistic_aggregates(table) {
            let old_key = old.map(|r| aggregate_key(&agg, r)).transpose()?.flatten();
            let new_key = new.map(|r| aggregate_key(&agg, r)).transpose()?.flatten();
            if old_key == new_key {
                if let Some(k) = old_key {
                    self.apply_aggregate_delta(&agg, &k, old, new)?;
                }
            } else {
                // The child leaves one aggregate and joins another; either
                // may be none.
                if let Some(k) = old_key {
                    self.apply_aggregate_delta(&agg, &k, old, None)?;
                }
                if let Some(k) = new_key {
                    self.apply_aggregate_delta(&agg, &k, None, new)?;
                }
            }
        }
        Ok(())
    }

    /// `applyAggregateDelta`: writes the aggregate row at `key` as one child
    /// change moves it, if the row is on the client, and pushes it to the
    /// branch so a later `run` reads it. A grouped query's row is added by a
    /// group's first row and removed with its last.
    fn apply_aggregate_delta(
        &self,
        agg: &OptimisticAggregate,
        key: &str,
        old: Option<&Row>,
        new: Option<&Row>,
    ) -> Result<(), Error> {
        // The branch holds the row when a view of its query reads it.
        let source = self.branch.source(&agg.aggregate_table);
        let cur = self
            .write
            .borrow()
            .get(key)
            .and_then(|v| v.as_object().cloned());
        let Some(cur) = cur else {
            if agg.grouped
                && let Some(added) = new_group_row(agg, old, new)
            {
                self.write
                    .borrow_mut()
                    .set(key, Value::Object(added.clone()));
                if let Some(source) = &source {
                    Self::push(source, SourceChange::Add(Rc::new(added)))?;
                }
            }
            return Ok(());
        };
        let Some(updated) = next_aggregate_row(agg, &cur, old, new) else {
            return Ok(());
        };
        let cur_row: Row = Rc::new(cur);
        let in_branch = source.as_ref().is_some_and(|s| s.contains(&cur_row));
        if agg.grouped && group_row_count(agg, &updated) == Some(&Value::Number(0.0)) {
            self.write.borrow_mut().delete(key);
            if in_branch && let Some(source) = &source {
                Self::push(source, SourceChange::Remove(cur_row))?;
            }
            return Ok(());
        }
        self.write
            .borrow_mut()
            .set(key, Value::Object(updated.clone()));
        if in_branch && let Some(source) = &source {
            Self::push(
                source,
                SourceChange::Edit {
                    row: Rc::new(updated),
                    old_row: cur_row,
                },
            )?;
        }
        Ok(())
    }
}

/// `aggregateKeyFor`: the key of the aggregate row `child` counts in;
/// `None` when a correlation value is null, which gives the child no
/// parent. A grouped query's group can have null values.
fn aggregate_key(agg: &OptimisticAggregate, child: &Row) -> Result<Option<String>, Error> {
    if agg.grouped {
        let values: Vec<Value> = agg
            .child_field
            .iter()
            .map(|c| child.get(c).cloned().unwrap_or(Value::Null))
            .collect();
        let mut row = Object::new();
        row.insert(cols::KEY, Value::String(group_key(&values)));
        return Ok(Some(aggregate_row_key(&agg.aggregate_table, &row)?));
    }
    let mut row = Object::new();
    for (i, column) in agg.child_field.iter().enumerate() {
        match child.get(column) {
            None | Some(Value::Null) => return Ok(None),
            Some(v) => {
                row.insert(agg.key_field[i].clone(), v.clone());
            }
        }
    }
    Ok(Some(aggregate_row_key(&agg.aggregate_table, &row)?))
}

/// `groupRowCount`: the number of rows of a group, `None` for a sum or
/// average row stored before the rows carried it.
fn group_row_count<'a>(agg: &OptimisticAggregate, row: &'a Object) -> Option<&'a Value> {
    row.get(if agg.aggregate.fn_ == AggregateFunction::Count {
        cols::VALUE
    } else {
        cols::ROWS
    })
}

fn counted(agg: &OptimisticAggregate, row: Option<&Row>) -> bool {
    row.is_some_and(|r| agg.predicate.as_ref().is_none_or(|p| p(r)))
}

/// `newGroupRow`: the row of a group not on the client that `new` is the
/// first row of: it passes the `where`, and `old` wasn't counted already.
fn new_group_row(
    agg: &OptimisticAggregate,
    old: Option<&Row>,
    new: Option<&Row>,
) -> Option<Object> {
    if !counted(agg, new) || counted(agg, old) {
        return None;
    }
    let new = new?;
    let values: Vec<Value> = agg
        .child_field
        .iter()
        .map(|c| new.get(c).cloned().unwrap_or(Value::Null))
        .collect();
    // The group with no rows, which `new` then joins.
    let mut empty = Object::new();
    for (i, column) in agg.key_field.iter().enumerate() {
        empty.insert(column.clone(), values[i].clone());
    }
    empty.insert(cols::KEY, Value::String(group_key(&values)));
    if agg.aggregate.fn_ == AggregateFunction::Count {
        empty.insert(cols::VALUE, Value::Number(0.0));
    } else {
        empty.insert(cols::VALUE, Value::Null);
        empty.insert(cols::SUM, Value::Number(0.0));
        empty.insert(cols::COUNT, Value::Number(0.0));
        empty.insert(cols::ROWS, Value::Number(0.0));
    }
    next_aggregate_row(agg, &empty, None, Some(new))
}

fn number(v: Option<&Value>) -> f64 {
    match v {
        Some(Value::Number(n)) => *n,
        _ => 0.0,
    }
}

/// `nextAggregateRow`: the aggregate row after a child changes from `old`
/// to `new`, `None` when it doesn't move. A row the `where` rejects counts
/// as absent, so a row entering or leaving the filtered set moves it too.
fn next_aggregate_row(
    agg: &OptimisticAggregate,
    cur: &Object,
    old: Option<&Row>,
    new: Option<&Row>,
) -> Option<Object> {
    let passes = |r: &&Row| agg.predicate.as_ref().is_none_or(|p| p(r));
    let old = old.filter(passes);
    let new = new.filter(passes);
    let contribution = |r: Option<&Row>| r.map(|r| aggregate_value(&agg.aggregate, r));
    let contributes = |r: Option<&Row>| matches!(contribution(r), Some(Value::Number(_)));
    let present = |r: Option<&Row>| if r.is_some() { 1.0 } else { 0.0 };
    let mut next = cur.clone();
    match agg.aggregate.fn_ {
        AggregateFunction::Count => {
            let delta = present(new) - present(old);
            if delta == 0.0 {
                return None;
            }
            next.insert(
                cols::VALUE,
                Value::Number(number(cur.get(cols::VALUE)) + delta),
            );
            Some(next)
        }
        AggregateFunction::Sum | AggregateFunction::Avg => {
            agg.aggregate.field.as_ref()?;
            // The row carries the sum of the non-null values and their
            // count, so both move and the value follows: SQL's sum and avg
            // skip nulls and are null over none. A group's row also carries
            // its number of rows, nulls included, unless it was stored
            // before it did.
            let sum_delta = number(contribution(new).as_ref()) - number(contribution(old).as_ref());
            let count_delta = (contributes(new) as u8 as f64) - (contributes(old) as u8 as f64);
            let has_rows = cur.contains_key(cols::ROWS);
            let rows_delta = if has_rows {
                present(new) - present(old)
            } else {
                0.0
            };
            if sum_delta == 0.0 && count_delta == 0.0 && rows_delta == 0.0 {
                return None;
            }
            let sum = number(cur.get(cols::SUM)) + sum_delta;
            let count = number(cur.get(cols::COUNT)) + count_delta;
            let result = if agg.aggregate.fn_ == AggregateFunction::Sum {
                sum
            } else {
                sum / count
            };
            next.insert(
                cols::VALUE,
                if count > 0.0 {
                    Value::Number(result)
                } else {
                    Value::Null
                },
            );
            next.insert(cols::SUM, Value::Number(sum));
            next.insert(cols::COUNT, Value::Number(count));
            if has_rows {
                next.insert(
                    cols::ROWS,
                    Value::Number(number(cur.get(cols::ROWS)) + rows_delta),
                );
            }
            Some(next)
        }
        // Not invertible: the server's next poke moves them.
        AggregateFunction::Min | AggregateFunction::Max => None,
    }
}

/// The CRUD operations on one table (`TableCRUD` in
/// `packages/zql/src/mutate/crud.ts`). Values are keyed by client column
/// name.
pub struct TableMutator<'a> {
    tx: &'a Transaction,
    table: &'a str,
}

impl TableMutator<'_> {
    /// Inserts a row unless one with its primary key exists. Columns it
    /// leaves out are null.
    pub fn insert(&self, value: &Object) -> Result<(), Error> {
        self.tx.insert(self.table, value)
    }

    /// Inserts a row, or merges the value into the existing one.
    pub fn upsert(&self, value: &Object) -> Result<(), Error> {
        self.tx.upsert(self.table, value)
    }

    /// Merges the value's columns into the row with its primary key, if any.
    pub fn update(&self, value: &Object) -> Result<(), Error> {
        self.tx.update(self.table, value)
    }

    /// Deletes the row with the primary key in `id`, if any.
    pub fn delete(&self, id: &Object) -> Result<(), Error> {
        self.tx.delete(self.table, id)
    }
}
