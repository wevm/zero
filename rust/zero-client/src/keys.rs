//! Storage keys (`packages/zero-client/src/client/keys.ts`) and the names of
//! synthetic aggregate tables.

use crate::hash::xxhash32;
use crate::json::{Object, Value, number, stringify};
use std::fmt;

pub const DESIRED_QUERIES_PREFIX: &str = "d/";
pub const GOT_QUERIES_PREFIX: &str = "g/";
pub const ENTITIES_PREFIX: &str = "e/";
/// Synced aggregate rows live outside `e/`, which a client from before them
/// loads into its schema's tables: both can share a database.
pub const AGGREGATES_PREFIX: &str = "a/";
pub const MUTATIONS_PREFIX: &str = "m/";
/// The row-version column the server stamps on synced rows; not part of an
/// aggregate row's key.
pub const ROW_VERSION_COLUMN: &str = "_0_version";

/// The prefix of every synthetic aggregate table's name.
pub const AGGREGATE_TABLE_PREFIX: &str = "aggregate:";

/// The columns of an aggregate's rows.
pub mod aggregate_columns {
    /// The aggregate's result. A view shows it as the bare value.
    pub const VALUE: &str = "_0_value";
    /// The constant key of a top-level aggregate's single row.
    pub const KEY: &str = "_0_key";
    /// A sum or average's running sum and count of non-null values.
    pub const SUM: &str = "_0_sum";
    pub const COUNT: &str = "_0_count";
    /// A grouped sum or average's number of rows, nulls included.
    pub const ROWS: &str = "_0_rows";
    /// The payload columns, which aren't part of a row's key.
    pub const PAYLOAD: [&str; 4] = [VALUE, SUM, COUNT, ROWS];
}

/// A primary key column whose value isn't a string, number, or boolean.
#[derive(Clone, Debug, PartialEq)]
pub struct InvalidPrimaryKeyValue {
    pub column: String,
    pub value: Option<Value>,
}

impl fmt::Display for InvalidPrimaryKeyValue {
    /// valita's message for `primaryKeyValueSchema`, which JS throws as a
    /// `TypeError`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Expected string, number or boolean. Got {}",
            valita_display(self.value.as_ref())
        )
    }
}

impl std::error::Error for InvalidPrimaryKeyValue {}

/// `toDisplay` in valita.ts: how its messages show a value.
pub fn valita_display(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::Array(_)) => "array".into(),
        Some(Value::Object(_)) => "object".into(),
        Some(v) => stringify(v),
    }
}

/// `toPrimaryKeyString`: `e/<table>/<id>`. A single-column key is the value
/// as JS `String()` prints it; a compound key is the decimal `h128` of the
/// JSON array of its values.
pub fn primary_key_string(
    table: &str,
    primary_key: &[impl AsRef<str>],
    row: &Object,
) -> Result<String, InvalidPrimaryKeyValue> {
    key_string(ENTITIES_PREFIX, table, primary_key, row)
}

/// `aggregateRowKey`: `a/`, the table name without its `aggregate:`, and the
/// row's key, from [`aggregate_row_key_columns`]. A `put`'s full row, a
/// `del`'s key, and an optimistic delta all give the same key.
pub fn aggregate_row_key(table: &str, row: &Object) -> Result<String, InvalidPrimaryKeyValue> {
    let columns = aggregate_row_key_columns(table, row.keys());
    let name = table.strip_prefix(AGGREGATE_TABLE_PREFIX).unwrap_or(table);
    key_string(AGGREGATES_PREFIX, name, &columns, row)
}

/// `aggregateRowKeyColumns`: `_0_key` for a top-level aggregate's or a
/// grouped query's row, and otherwise every column but the payload and the
/// row version, sorted by UTF-16 code unit. The table name decides, not the
/// row.
pub fn aggregate_row_key_columns<'a>(
    table: &str,
    columns: impl IntoIterator<Item = &'a String>,
) -> Vec<String> {
    if is_keyed_aggregate_table_name(table) {
        return vec![aggregate_columns::KEY.to_owned()];
    }
    let mut out: Vec<String> = columns
        .into_iter()
        .filter(|c| !aggregate_columns::PAYLOAD.contains(&c.as_str()) && *c != ROW_VERSION_COLUMN)
        .cloned()
        .collect();
    out.sort_by(|a, b| crate::strings::utf16(a, b));
    out
}

fn key_string(
    prefix: &str,
    table: &str,
    primary_key: &[impl AsRef<str>],
    row: &Object,
) -> Result<String, InvalidPrimaryKeyValue> {
    let scalar = |column: &str| match row.get(column) {
        Some(v @ (Value::String(_) | Value::Number(_) | Value::Bool(_))) => Ok(v.clone()),
        v => Err(InvalidPrimaryKeyValue {
            column: column.to_owned(),
            value: v.cloned(),
        }),
    };
    if let [column] = primary_key {
        let v = scalar(column.as_ref())?;
        return Ok(format!("{prefix}{table}/{}", js_string(&v)));
    }
    let values = primary_key
        .iter()
        .map(|c| scalar(c.as_ref()))
        .collect::<Result<Vec<_>, _>>()?;
    let hash = xxhash32::h128(&stringify(&Value::Array(values)));
    Ok(format!("{prefix}{table}/{hash}"))
}

/// JS `String(v)` for a scalar.
pub fn js_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => number::format(*n),
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".into(),
        Value::Array(_) | Value::Object(_) => stringify(v),
    }
}

/// `isAggregateTableName`: a synthetic aggregate table, of any kind.
pub fn is_aggregate_table_name(name: &str) -> bool {
    name.starts_with(AGGREGATE_TABLE_PREFIX)
}

/// `isTopLevelAggregateTableName`: `aggregate:<queryID>`, with no further
/// `:`, unlike a relationship aggregate's or a grouped query's.
pub fn is_top_level_aggregate_table_name(name: &str) -> bool {
    name.strip_prefix(AGGREGATE_TABLE_PREFIX)
        .is_some_and(|rest| !rest.contains(':'))
}

/// `isKeyedAggregateTableName`: a table whose rows `_0_key` alone keys: a
/// top-level aggregate's or a grouped query's.
pub fn is_keyed_aggregate_table_name(name: &str) -> bool {
    is_top_level_aggregate_table_name(name)
        || (is_aggregate_table_name(name) && name.ends_with(":#group"))
}

pub fn desired_queries_key(client_id: &str, hash: &str) -> String {
    format!("{DESIRED_QUERIES_PREFIX}{client_id}/{hash}")
}

pub fn got_queries_key(hash: &str) -> String {
    format!("{GOT_QUERIES_PREFIX}{hash}")
}

pub fn mutation_response_key(client_id: &str, id: f64) -> String {
    format!("{MUTATIONS_PREFIX}{client_id}/{}", number::format(id))
}
