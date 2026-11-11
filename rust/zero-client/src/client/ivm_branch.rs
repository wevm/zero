//! The IVM sources of one state of the replica
//! (`packages/zero-client/src/client/ivm-branch.ts`).

use super::replica_store::{KvDiff, KvDiffOp};
use crate::builder::OptimisticAggregate;
use crate::ivm::{MemorySource, Row, Source, SourceChange, has_failed};
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

/// The IVM sources of one state of the replica. The main branch feeds the
/// app's views; mutators run against forks, so a rebase can replay them
/// over a newer server snapshot without touching the views.
pub(crate) struct IvmBranch {
    sources: RefCell<HashMap<String, Option<Rc<MemorySource>>>>,
    schema: Arc<Schema>,
    optimism: Optimism,
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
            is_fork: true,
        })
    }

    /// Applies the row changes of `diff` (`applyDiffs`): the aggregate rows
    /// under `a/`, then the table rows under `e/`, which is key order. A run
    /// of adds to one source is pushed together. A row in `written`, by key,
    /// is the one a transaction wrote, which is the value the diff has for
    /// it. The main branch then releases the aggregate sources nothing uses.
    pub(crate) fn apply(&self, diff: &KvDiff, written: &HashMap<String, Row>) {
        let row_of = |key: &str, v: &Value| -> Row {
            written
                .get(key)
                .cloned()
                .unwrap_or_else(|| Rc::new(v.as_object().cloned().unwrap_or_default()))
        };
        let plain = |v: &Value| -> Row { Rc::new(v.as_object().cloned().unwrap_or_default()) };
        let mut i = 0;
        // JS's throw ends `applyDiffs` at the change an operator failed on.
        while i < diff.len() && !has_failed() {
            let op = &diff[i];
            let is_aggregate = op.key().starts_with(AGGREGATES_PREFIX);
            if !is_aggregate && !op.key().starts_with(ENTITIES_PREFIX) {
                i += 1;
                continue;
            }
            let name = source_name(op.key());
            let mut source = self.source(&name);
            if source.is_none() && is_aggregate {
                // A relationship aggregate's or a grouped query's rows
                // replayed before its query materializes (a reload): the
                // source takes the row's shape, and the builder reuses it.
                let row = match op {
                    KvDiffOp::Del { old, .. } => old,
                    KvDiffOp::Add { new, .. } | KvDiffOp::Change { new, .. } => new,
                };
                let empty = Object::new();
                let (columns, primary_key) =
                    infer_aggregate_schema(&name, row.as_object().unwrap_or(&empty));
                source = Some(self.aggregate_source(&name, columns, primary_key, None));
            }
            let Some(source) = source else {
                i += 1;
                continue;
            };
            match op {
                KvDiffOp::Del { old, .. } => {
                    source.push(SourceChange::Remove(plain(old)));
                    i += 1;
                }
                KvDiffOp::Change { key, old, new } => {
                    source.push(SourceChange::Edit {
                        row: row_of(key, new),
                        old_row: plain(old),
                    });
                    i += 1;
                }
                KvDiffOp::Add { .. } => {
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
                    let mut rows = Vec::new();
                    while let Some(KvDiffOp::Add { key, new }) = diff.get(i) {
                        if !key.starts_with(&source_prefix) {
                            break;
                        }
                        rows.push(row_of(key, new));
                        i += 1;
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
                        .is_some_and(|s| s.data().is_empty() && !s.has_connections())
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
