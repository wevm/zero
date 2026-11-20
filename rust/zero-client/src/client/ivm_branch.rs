//! The IVM sources of one state of the replica
//! (`packages/zero-client/src/client/ivm-branch.ts`).

use super::replica_store::{KvDiff, KvDiffOp, KvMap};
use crate::builder::OptimisticAggregate;
use crate::ivm::{MemorySource, Row, Source, SourceChange, comparator, has_failed};
use crate::json::{Object, Value};
use crate::keys::{
    AGGREGATE_TABLE_PREFIX, AGGREGATES_PREFIX, ENTITIES_PREFIX, ROW_VERSION_COLUMN,
    aggregate_columns as cols, aggregate_row_key_columns, is_aggregate_table_name,
    is_top_level_aggregate_table_name, source_name,
};
use crate::map::OrderedMap;
use crate::schema::{Column, Schema, Table, ValueType};
use crate::strings;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

/// The registry of optimistic aggregates, by child table and then by
/// aggregate table, shared by a branch and its forks.
type Optimism = Rc<RefCell<HashMap<String, HashMap<String, OptimisticAggregate>>>>;

/// The layout of each table's rows: a row of each, whose keys the next rows
/// with the same columns in the same order share (`Object::share_keys`), so
/// a table's rows keep one list of column names. A row with other columns,
/// or in another order, becomes the layout.
#[derive(Default)]
pub(crate) struct Layouts(RefCell<HashMap<String, Object>>);

impl Layouts {
    /// Puts `row`, a row of `table`, in the table's layout.
    pub(crate) fn share(&self, table: &str, row: &mut Object) {
        let mut layouts = self.0.borrow_mut();
        match layouts.get(table) {
            Some(layout) if row.share_keys(layout) => {}
            _ => {
                layouts.insert(table.to_owned(), row.clone());
            }
        }
    }
}

/// The rows of a replica's tables as [`IvmBranch::load`] loads them, each
/// table's in the order of its primary key: sorted on `Zero::prepare`'s
/// thread, so that the client's thread only makes their sources' trees.
#[derive(Clone, Debug, Default)]
pub(crate) struct SortedRows(Vec<(String, Run)>);

/// A run of one source's keys in a [`SortedRows`].
#[derive(Clone, Debug)]
enum Run {
    /// A table's rows, sorted.
    Sorted(Vec<Object>),
    /// The keys of a source that isn't a table of the schema, for `load`.
    Keys(Vec<String>),
}

impl SortedRows {
    /// The rows of the tables of `schema` among `entries`, a replica's in
    /// key order, under `e/`, by table; equal rows of a table keep the last,
    /// as `load` does. A source that isn't a table of `schema` is left to
    /// `load`, by its keys.
    pub(crate) fn new<'a>(
        schema: &Schema,
        entries: impl Iterator<Item = (&'a String, &'a Value)>,
    ) -> Self {
        let mut tables = Vec::new();
        let object = |v: &Value| v.as_object().cloned().unwrap_or_default();
        let mut entries = entries
            .skip_while(|(k, _)| !k.starts_with(ENTITIES_PREFIX))
            .take_while(|(k, _)| k.starts_with(ENTITIES_PREFIX))
            .peekable();
        while let Some((key, v)) = entries.next() {
            // A run of one source's keys, as `apply` pushes them together.
            let name = source_name(key);
            let prefix = format!("{ENTITIES_PREFIX}{name}/");
            let run = std::iter::once((key, v)).chain(std::iter::from_fn(|| {
                entries.next_if(|(k, _)| k.starts_with(&prefix))
            }));
            let Some(table) = schema.tables.get(&name) else {
                tables.push((name, Run::Keys(run.map(|(k, _)| k.clone()).collect())));
                continue;
            };
            let mut rows: Vec<Object> = run.map(|(_, v)| object(v)).collect();
            let compare = comparator::<Object>(&MemorySource::primary_key_order(table), false);
            rows.sort_by(&compare);
            let mut deduped: Vec<Object> = Vec::with_capacity(rows.len());
            for row in rows {
                match deduped.last_mut() {
                    Some(last) if compare(last, &row) == std::cmp::Ordering::Equal => *last = row,
                    _ => deduped.push(row),
                }
            }
            tables.push((name, Run::Sorted(deduped)));
        }
        SortedRows(tables)
    }
}

/// A change of a diff, by its key, borrowed: a [`KvDiffOp`], or an entry
/// of a map read as its add.
#[derive(Clone, Copy)]
enum Op<'a> {
    Add(&'a str, &'a Value),
    Del(&'a str, &'a Value),
    /// The key, the old value, and the new.
    Change(&'a str, &'a Value, &'a Value),
}

impl<'a> Op<'a> {
    fn key(self) -> &'a str {
        match self {
            Op::Add(key, _) | Op::Del(key, _) | Op::Change(key, _, _) => key,
        }
    }
}

/// The IVM sources of one state of the replica. The main branch feeds the
/// app's views; mutators run against forks, so a rebase can replay them
/// over a newer server snapshot without touching the views.
pub(crate) struct IvmBranch {
    sources: RefCell<HashMap<String, Option<Rc<MemorySource>>>>,
    schema: Arc<Schema>,
    optimism: Optimism,
    /// Shared by a branch and its forks.
    pub(crate) layouts: Rc<Layouts>,
    /// Only the main branch registers optimistic aggregates and releases
    /// aggregate sources (see `aggregate_source`).
    is_fork: bool,
}

fn column(type_: ValueType, optional: bool) -> Column {
    Column {
        type_,
        optional,
        server_name: None,
    }
}

impl IvmBranch {
    pub(crate) fn new(schema: Arc<Schema>) -> Rc<Self> {
        Rc::new(IvmBranch {
            sources: RefCell::default(),
            schema,
            optimism: Rc::default(),
            layouts: Rc::default(),
            is_fork: false,
        })
    }

    pub(crate) fn source(&self, table: &str) -> Option<Rc<MemorySource>> {
        if let Some(source) = self.sources.borrow().get(table) {
            return source.clone();
        }
        let source = if let Some(t) = self.schema.tables.get(table) {
            Some(MemorySource::new(t.clone()))
        } else if is_top_level_aggregate_table_name(table) {
            // A synced top-level aggregate's source. Its shape is fixed, so
            // it can be made from the name alone, also before its query
            // materializes on a reload.
            let mut columns = OrderedMap::new();
            columns.insert(cols::KEY, column(ValueType::Number, false));
            for c in [cols::VALUE, cols::SUM, cols::COUNT, cols::ROWS] {
                columns.insert(c, column(ValueType::Number, true));
            }
            Some(MemorySource::new(Table {
                name: table.to_owned(),
                server_name: None,
                columns,
                primary_key: vec![cols::KEY.to_owned()],
            }))
        } else {
            None
        };
        self.sources
            .borrow_mut()
            .insert(table.to_owned(), source.clone());
        source
    }

    /// `getOrCreateAggregateSource`: the source of a relationship
    /// aggregate's or a grouped query's rows, whose shape the name doesn't
    /// give. The builder gives it when a query materializes; `apply` infers
    /// it from a row when stored rows are loaded first. The first caller's
    /// shape wins.
    pub(crate) fn aggregate_source(
        &self,
        name: &str,
        columns: OrderedMap<Column>,
        primary_key: Vec<String>,
        optimistic: Option<OptimisticAggregate>,
    ) -> Rc<MemorySource> {
        // A fork's queries are never synced, so only the main branch, which
        // releases them, registers.
        if let Some(optimistic) = optimistic
            && !self.is_fork
        {
            self.optimism
                .borrow_mut()
                .entry(optimistic.table.clone())
                .or_default()
                .insert(name.to_owned(), optimistic);
        }
        if let Some(Some(existing)) = self.sources.borrow().get(name) {
            return existing.clone();
        }
        let source = MemorySource::new(Table {
            name: name.to_owned(),
            server_name: None,
            columns,
            primary_key,
        });
        self.sources
            .borrow_mut()
            .insert(name.to_owned(), Some(source.clone()));
        source
    }

    /// `serverColumnNames`: the server names of `columns` of `table`.
    pub(crate) fn server_column_names(&self, table: &str, columns: &[String]) -> Vec<String> {
        let table_columns = self.schema.tables.get(table).map(|t| &t.columns);
        columns
            .iter()
            .map(|c| {
                table_columns
                    .and_then(|cs| cs.get(c))
                    .and_then(|c| c.server_name.clone())
                    .unwrap_or_else(|| c.clone())
            })
            .collect()
    }

    /// `getOptimisticAggregates`: the synced aggregates whose child rows
    /// live in `table`, by name.
    pub(crate) fn optimistic_aggregates(&self, table: &str) -> Vec<OptimisticAggregate> {
        let optimism = self.optimism.borrow();
        let Some(by_name) = optimism.get(table) else {
            return Vec::new();
        };
        let mut entries: Vec<(&String, &OptimisticAggregate)> = by_name.iter().collect();
        entries.sort_by(|a, b| strings::utf16(a.0, b.0));
        entries.into_iter().map(|(_, a)| a.clone()).collect()
    }

    /// A copy whose sources share rows with these until written.
    pub(crate) fn fork(&self) -> Rc<IvmBranch> {
        let sources = self
            .sources
            .borrow()
            .iter()
            .map(|(k, v)| (k.clone(), v.as_ref().map(|s| s.fork())))
            .collect();
        Rc::new(IvmBranch {
            sources: RefCell::new(sources),
            schema: self.schema.clone(),
            optimism: self.optimism.clone(),
            layouts: self.layouts.clone(),
            is_fork: true,
        })
    }

    /// Applies the row changes of `diff` (`applyDiffs`): the aggregate rows
    /// under `a/`, then the table rows under `e/`, which is key order. A run
    /// of adds to one source is pushed together. A row in `written`, by key,
    /// is the one a transaction wrote, which is the value the diff has for
    /// it. The main branch then releases the aggregate sources nothing uses.
    pub(crate) fn apply(&self, diff: &KvDiff, written: &HashMap<String, Row>) {
        let ops = diff.iter().map(|op| match op {
            KvDiffOp::Add { key, new } => Op::Add(key, new),
            KvDiffOp::Del { key, old } => Op::Del(key, old),
            KvDiffOp::Change { key, old, new } => Op::Change(key, old, new),
        });
        self.apply_ops(ops, written);
    }

    /// Loads the rows of `map` into empty sources: `apply` of the diff
    /// that adds each of its entries, without making that diff.
    pub(crate) fn load(&self, map: &KvMap) {
        self.load_sorted(map, SortedRows::new(&self.schema, map.iter()));
    }

    /// [`load`](Self::load) of `map` with its tables' rows sorted already,
    /// as `Zero::prepare` sorts them on its own thread: `sorted` is
    /// `SortedRows::new` of `map`'s entries.
    pub(crate) fn load_sorted(&self, map: &KvMap, sorted: SortedRows) {
        // The synced aggregates' rows, which come first in key order.
        let aggregates = map.scan(AGGREGATES_PREFIX).map(|(k, v)| Op::Add(k, v));
        self.apply_ops(aggregates, &HashMap::new());
        for (name, rows) in sorted.0 {
            if has_failed() {
                return;
            }
            match rows {
                Run::Sorted(rows) => {
                    if let Some(source) = self.source(&name) {
                        source.push_sorted_adds(rows.into_iter().map(Rc::new).collect());
                    }
                }
                Run::Keys(keys) => {
                    let ops = keys.iter().filter_map(|k| Some(Op::Add(k, map.get(k)?)));
                    self.apply_ops(ops, &HashMap::new());
                }
            }
        }
    }

    fn apply_ops<'a>(&self, ops: impl Iterator<Item = Op<'a>>, written: &HashMap<String, Row>) {
        let row_of = |key: &str, v: &Value| -> Row {
            written
                .get(key)
                .cloned()
                .unwrap_or_else(|| Rc::new(v.as_object().cloned().unwrap_or_default()))
        };
        let plain = |v: &Value| -> Row { Rc::new(v.as_object().cloned().unwrap_or_default()) };
        let mut ops = ops.peekable();
        // JS's throw ends `applyDiffs` at the change an operator failed on.
        while !has_failed() {
            let Some(op) = ops.next() else {
                break;
            };
            let key = op.key();
            let is_aggregate = key.starts_with(AGGREGATES_PREFIX);
            if !is_aggregate && !key.starts_with(ENTITIES_PREFIX) {
                continue;
            }
            let name = source_name(key);
            let mut source = self.source(&name);
            if source.is_none() && is_aggregate {
                // A relationship aggregate's or a grouped query's rows
                // replayed before its query materializes (a reload): the
                // source takes the row's shape, and the builder reuses it.
                let row = match op {
                    Op::Del(_, old) => old,
                    Op::Add(_, new) | Op::Change(_, _, new) => new,
                };
                let empty = Object::new();
                let (columns, primary_key) =
                    infer_aggregate_schema(&name, row.as_object().unwrap_or(&empty));
                source = Some(self.aggregate_source(&name, columns, primary_key, None));
            }
            let Some(source) = source else {
                continue;
            };
            match op {
                Op::Del(_, old) => source.push(SourceChange::Remove(plain(old))),
                Op::Change(key, old, new) => source.push(SourceChange::Edit {
                    row: row_of(key, new),
                    old_row: plain(old),
                }),
                Op::Add(key, new) => {
                    // The source's own prefix: the key up to the slash after
                    // its name.
                    let source_prefix = if is_aggregate {
                        format!(
                            "{AGGREGATES_PREFIX}{}/",
                            name.strip_prefix(AGGREGATE_TABLE_PREFIX).unwrap_or(&name)
                        )
                    } else {
                        format!("{ENTITIES_PREFIX}{name}/")
                    };
                    let mut rows = vec![row_of(key, new)];
                    while let Some(Op::Add(key, new)) = ops.next_if(
                        |op| matches!(op, Op::Add(key, _) if key.starts_with(&source_prefix)),
                    ) {
                        rows.push(row_of(key, new));
                    }
                    source.push_adds(rows);
                }
            }
        }
        if !self.is_fork && !has_failed() {
            self.release_aggregate_sources();
        }
    }

    /// `#releaseAggregateSources`: drops the aggregate sources that are
    /// empty and that no view reads, with their optimistic registrations.
    fn release_aggregate_sources(&self) {
        let released: Vec<String> = self
            .sources
            .borrow()
            .iter()
            .filter(|(name, s)| {
                is_aggregate_table_name(name)
                    && s.as_ref()
                        .is_some_and(|s| s.is_empty() && !s.has_connections())
            })
            .map(|(name, _)| name.clone())
            .collect();
        if released.is_empty() {
            return;
        }
        let mut sources = self.sources.borrow_mut();
        let mut optimism = self.optimism.borrow_mut();
        for name in released {
            sources.remove(&name);
            for by_name in optimism.values_mut() {
                by_name.remove(&name);
            }
        }
    }
}

/// `inferAggregateSchema`: an aggregate source's shape from one of its rows:
/// the payload columns are nullable numbers, the others typed by their
/// value; the key is `aggregateRowKeyColumns`.
fn infer_aggregate_schema(name: &str, row: &Object) -> (OrderedMap<Column>, Vec<String>) {
    let mut columns = OrderedMap::new();
    for (c, v) in row.iter() {
        if c == ROW_VERSION_COLUMN {
            continue;
        }
        let col = if cols::PAYLOAD.contains(&c.as_str()) {
            column(ValueType::Number, true)
        } else {
            column(value_type(v), false)
        };
        columns.insert(c.clone(), col);
    }
    if !columns.contains_key(cols::VALUE) {
        columns.insert(cols::VALUE, column(ValueType::Number, true));
    }
    (columns, aggregate_row_key_columns(name, row.keys()))
}

/// `valueTypeOf`: the column type of a value.
fn value_type(v: &Value) -> ValueType {
    match v {
        Value::String(_) => ValueType::String,
        Value::Number(_) => ValueType::Number,
        Value::Bool(_) => ValueType::Boolean,
        Value::Null => ValueType::Null,
        Value::Array(_) | Value::Object(_) => ValueType::Json,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::replica_store::adds;
    use crate::json;

    /// `load`, which sorts each table's rows first, as `Zero::prepare` does
    /// on its thread, loads the sources `apply` of the adds of every entry
    /// does: each table's rows in key order, a table the schema doesn't
    /// have left out, and the synced aggregates' rows.
    #[test]
    fn load_loads_what_applying_adds_loads() {
        let schema = json::parse(
            r#"{"tables": {
                "issue": {"name": "issue", "primaryKey": ["n"],
                    "columns": {"id": {"type": "string"}, "n": {"type": "number"}}},
                "issueLabel": {"name": "issueLabel", "primaryKey": ["labelID", "issueID"],
                    "columns": {"issueID": {"type": "string"}, "labelID": {"type": "string"}}}}}"#,
        )
        .unwrap();
        let schema = Arc::new(Schema::from_json(&schema).unwrap());
        let mut map = KvMap::default();
        let row = |text: &str| json::parse(text).unwrap();
        // Keys whose order isn't the primary key's: "10" before "9".
        for i in [3, 10, 9, 1] {
            map.set(
                format!("e/issue/{i}"),
                row(&format!(r#"{{"id": "i{i}", "n": {i}}}"#)),
            );
        }
        for (label, issue) in [("l2", "i1"), ("l1", "i3"), ("l1", "i1")] {
            map.set(
                format!("e/issueLabel/{label}{issue}"),
                row(&format!(
                    r#"{{"issueID": "{issue}", "labelID": "{label}"}}"#
                )),
            );
        }
        map.set("e/gone/x", row(r#"{"id": "x"}"#));
        map.set(
            "a/q1/k",
            row(r#"{"_0_key": "k", "_0_value": 2, "_0_count": 2}"#),
        );
        map.set("m/c/1", row("{}"));

        let applied = IvmBranch::new(schema.clone());
        applied.apply(&adds(map.iter()), &HashMap::new());
        let loaded = IvmBranch::new(schema.clone());
        loaded.load(&map);
        let names = |b: &IvmBranch| {
            let mut names: Vec<String> = b.sources.borrow().keys().cloned().collect();
            names.sort();
            names
        };
        assert_eq!(names(&loaded), names(&applied));
        assert_eq!(
            names(&loaded),
            ["aggregate:q1", "gone", "issue", "issueLabel"]
        );
        for name in names(&loaded) {
            let rows = |b: &IvmBranch| b.source(&name).map(|s| (*s.data()).clone());
            assert_eq!(rows(&loaded), rows(&applied), "{name}");
        }
        let ids: Vec<String> = loaded
            .source("issue")
            .unwrap()
            .data()
            .iter()
            .map(|r| r.get("id").unwrap().as_str().unwrap().to_owned())
            .collect();
        assert_eq!(ids, ["i1", "i3", "i9", "i10"]);
    }
}
