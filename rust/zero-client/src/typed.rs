//! The typed query builder that `zero-rust codegen` generates code for: row
//! structs read from views, columns and relationships named by typed
//! constants, and [`Query`], a typed wrapper of [`AnyQuery`] (`Query` in
//! `packages/zql/src/query/query.ts`).
//!
//! The generated code holds a row struct per table with a [`Column`]
//! constant per column and a [`Relationship`] constant per relationship,
//! and a function per table that starts a query:
//!
//! ```ignore
//! zql::issue()
//!     .where_eq(Issue::STATUS, IssueStatus::Open)
//!     .where_expr(|e| e.or(vec![Issue::PRIORITY.ge(2.0), Issue::ASSIGNEE_ID.eq(None)]))
//!     .where_exists(Issue::LABELS, ExistsOptions::default(), |q| q.where_eq(Label::NAME, "bug".into()))
//!     .related(Issue::COMMENTS, |q| q.order_by(Comment::CREATED, Direction::Desc).limit(10))
//!     .order_by(Issue::MODIFIED, Direction::Desc)
//!     .limit(50)
//! ```
//!
//! A typed query builds the same AST as the [`AnyQuery`] calls with the
//! same names; [`Query::read`] reads a view of it as the query's result
//! type. In a mutator, the generated `mutate` functions return a
//! [`TypedTableMutator`] per table, whose updates and deletes take the
//! generated `<Row>Update` and `<Row>Key` structs. The items marked "for generated code" are what the generated
//! `from_entry`s and value conversions call.

pub use crate::ast::{Condition, Direction, JsonPathSegment, SimpleOperator};
pub use crate::client::Transaction;
pub use crate::ivm::{ViewEntry, ViewValue};
pub use crate::json::{Object, Value};
pub use crate::query::{AnyGroupedQuery, AnyQuery, ExistsOptions, StaticParameter};
pub use crate::schema::Schema;

use crate::ast::{System, ValuePosition};
use crate::client::Error;
use crate::query::{ExpressionBuilder, JsonPathReference};
use std::fmt;
use std::marker::PhantomData;
use std::sync::Arc;

/// A value that doesn't fit the Rust type a row declares for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZeroValueError(pub String);

impl ZeroValueError {
    /// A value that isn't the `expected` one.
    pub fn mismatch(expected: &str, value: &Value) -> Self {
        ZeroValueError(format!(
            "expected {expected}, got {} {}",
            value.type_name(),
            crate::json::stringify(value)
        ))
    }
}

impl fmt::Display for ZeroValueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ZeroValueError {}

/// A Rust type a column holds. Generated row structs use these for their
/// columns, and the query builder encodes compared values with them.
pub trait ZeroValue: Sized {
    /// Reads a value from a row; a missing column reads as `null`.
    fn from_zero_value(value: &Value) -> Result<Self, ZeroValueError>;
    /// Writes the value as the row and the query builder hold it.
    fn to_zero_value(&self) -> Value;
    /// What a row's `null` reads as when the schema says the column can't
    /// be null; `None` to fail. Databases drift from schemas, and JS shows
    /// such a row with the null rather than failing, so strings, numbers,
    /// and booleans read as `""`, `0`, and `false`. Declare the column
    /// `optional()` to read the null.
    fn value_for_null() -> Option<Self> {
        None
    }
}

impl ZeroValue for String {
    fn from_zero_value(value: &Value) -> Result<Self, ZeroValueError> {
        value
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| ZeroValueError::mismatch("a string", value))
    }
    fn to_zero_value(&self) -> Value {
        Value::String(self.clone())
    }
    fn value_for_null() -> Option<Self> {
        Some(String::new())
    }
}

impl ZeroValue for f64 {
    fn from_zero_value(value: &Value) -> Result<Self, ZeroValueError> {
        value
            .as_f64()
            .ok_or_else(|| ZeroValueError::mismatch("a number", value))
    }
    fn to_zero_value(&self) -> Value {
        Value::Number(*self)
    }
    fn value_for_null() -> Option<Self> {
        Some(0.0)
    }
}

/// For a `number()` column the codegen config declares an integer: the
/// value must be exact, which every integer up to 2^53 is.
impl ZeroValue for i64 {
    fn from_zero_value(value: &Value) -> Result<Self, ZeroValueError> {
        const MAX_SAFE: f64 = 9_007_199_254_740_992.0;
        match value.as_f64() {
            Some(n) if n.fract() == 0.0 && n.abs() <= MAX_SAFE => Ok(n as i64),
            _ => Err(ZeroValueError::mismatch("an integer", value)),
        }
    }
    fn to_zero_value(&self) -> Value {
        Value::Number(*self as f64)
    }
    fn value_for_null() -> Option<Self> {
        Some(0)
    }
}

impl ZeroValue for bool {
    fn from_zero_value(value: &Value) -> Result<Self, ZeroValueError> {
        value
            .as_bool()
            .ok_or_else(|| ZeroValueError::mismatch("a boolean", value))
    }
    fn to_zero_value(&self) -> Value {
        Value::Bool(*self)
    }
    fn value_for_null() -> Option<Self> {
        Some(false)
    }
}

impl ZeroValue for Value {
    fn from_zero_value(value: &Value) -> Result<Self, ZeroValueError> {
        Ok(value.clone())
    }
    fn to_zero_value(&self) -> Value {
        self.clone()
    }
}

/// An optional column: `null` reads as `None`.
impl<T: ZeroValue> ZeroValue for Option<T> {
    fn from_zero_value(value: &Value) -> Result<Self, ZeroValueError> {
        if value.is_null() {
            Ok(None)
        } else {
            T::from_zero_value(value).map(Some)
        }
    }
    fn to_zero_value(&self) -> Value {
        self.as_ref().map_or(Value::Null, T::to_zero_value)
    }
}

/// The JSON arrays a `json()` column can hold.
impl<T: ZeroValue> ZeroValue for Vec<T> {
    fn from_zero_value(value: &Value) -> Result<Self, ZeroValueError> {
        value
            .as_array()
            .ok_or_else(|| ZeroValueError::mismatch("an array", value))?
            .iter()
            .map(T::from_zero_value)
            .collect()
    }
    fn to_zero_value(&self) -> Value {
        Value::Array(self.iter().map(T::to_zero_value).collect())
    }
}

/// A column `sum` and `avg` can reduce: a number (`NumericSelector`).
pub trait NumericZeroValue: ZeroValue {}
impl NumericZeroValue for f64 {}
impl NumericZeroValue for i64 {}
impl<T: NumericZeroValue> NumericZeroValue for Option<T> {}

/// A column `min` and `max` can reduce: a number or a string, enumerations
/// included (`OrderedSelector`). `Unwrapped` is the type of its non-null
/// values, the aggregate's type.
pub trait OrderedZeroValue: ZeroValue {
    /// The type of the column's non-null values.
    type Unwrapped: ZeroValue + OptionalQueryElement;
}
impl OrderedZeroValue for f64 {
    type Unwrapped = f64;
}
impl OrderedZeroValue for i64 {
    type Unwrapped = i64;
}
impl OrderedZeroValue for String {
    type Unwrapped = String;
}
impl<T: OrderedZeroValue> OrderedZeroValue for Option<T> {
    type Unwrapped = T::Unwrapped;
}

/// Reads `name`'s value of a row or object, for generated code. A `null`
/// in a column that can't be null reads as [`ZeroValue::value_for_null`].
pub fn decode<V: ZeroValue>(name: &str, value: Option<&Value>) -> Result<V, ZeroValueError> {
    let value = value.unwrap_or(&Value::Null);
    if value.is_null()
        && let Some(fallback) = V::value_for_null()
    {
        return Ok(fallback);
    }
    V::from_zero_value(value).map_err(|e| ZeroValueError(format!("{name}: {e}")))
}

/// A column of a row, for generated `from_entry`s: `index` is its position
/// in the table's schema, where the row has it unless it came from
/// elsewhere. Reading it there skips looking the name up.
pub fn column<V: ZeroValue>(
    entry: &ViewEntry,
    name: &str,
    index: usize,
) -> Result<V, ZeroValueError> {
    match entry.row.get_index(index) {
        Some((k, v)) if k == name => decode(name, Some(v)),
        _ => decode(name, entry.row.get(name)),
    }
}

/// A many relationship of a row, for generated `from_entry`s: `None` when
/// the query didn't ask for its rows, also when it asked for an aggregate
/// of them.
pub fn many<R: QueryElement>(
    entry: &ViewEntry,
    name: &str,
) -> Result<Option<Vec<R>>, ZeroValueError> {
    match entry.relationship(name) {
        None | Some(ViewValue::Value(_)) => Ok(None),
        Some(v) => v
            .entries()
            .into_iter()
            .map(R::from_entry)
            .collect::<Result<_, _>>()
            .map(Some),
    }
}

/// A one relationship of a row, for generated `from_entry`s: `None` when
/// the query didn't ask for it or no row matches.
pub fn one<R: QueryElement>(
    entry: &ViewEntry,
    name: &str,
) -> Result<Option<Box<R>>, ZeroValueError> {
    match entry
        .relationship(name)
        .and_then(|v| v.entries().first().copied())
    {
        Some(e) => R::from_entry(e).map(|r| Some(Box::new(r))),
        None => Ok(None),
    }
}

/// The object a generated `json<T>()` type reads its fields from.
pub fn object<'a>(value: &'a Value, type_name: &str) -> Result<&'a Object, ZeroValueError> {
    value
        .as_object()
        .ok_or_else(|| ZeroValueError::mismatch(&format!("a {type_name} object"), value))
}

/// A field of a `json<T>()` object; a missing one reads as `null`, so only
/// an `Option` takes it.
pub fn field<V: ZeroValue>(object: &Object, name: &str) -> Result<V, ZeroValueError> {
    V::from_zero_value(object.get(name).unwrap_or(&Value::Null))
        .map_err(|e| ZeroValueError(format!("{name}: {e}")))
}

/// The schema of generated code, from the JSON form [`Schema::from_json`]
/// reads. Panics when it isn't one, which generated code never is.
pub fn schema_from_json(json: &str) -> Arc<Schema> {
    let value = crate::json::parse(json).expect("the generated schema is JSON");
    Arc::new(Schema::from_json(&value).expect("the generated schema is a schema"))
}

/// A row of a query's result: a row struct, or a row of `group_by` or
/// `distinct`.
pub trait QueryElement: Sized {
    /// Reads the row from a view's entry.
    fn from_entry(entry: &ViewEntry) -> Result<Self, ZeroValueError>;
}

/// A row struct of a table, as `zero-rust codegen` generates it: the
/// table's columns as fields, and a field per relationship that a query
/// fills when it asks for it with `related`.
pub trait ZeroRow: QueryElement {
    /// The client name of the row's table.
    const TABLE: &'static str;
    /// What [`TypedTableMutator::update`] takes: the primary key, and the
    /// other columns to set.
    type Update: ColumnValues;
    /// What [`TypedTableMutator::delete`] takes: the primary key.
    type Key: ColumnValues;
    /// The row's columns, by client column name.
    fn column_values(&self) -> Object;
}

/// Some of a row's columns, by client column name: what an update or a
/// delete writes. For generated code.
pub trait ColumnValues {
    /// The columns, by client column name.
    fn column_values(&self) -> Object;
}

/// The typed CRUD operations of a table in a mutator (`tx.mutate.<table>`
/// in JS), which the generated `mutate` module returns:
///
/// ```ignore
/// mutate::issue(tx).insert(&issue)?;
/// mutate::issue(tx).update(&IssueUpdate { closed: Some(true), ..IssueUpdate::new(id) })?;
/// ```
pub struct TypedTableMutator<'tx, R> {
    tx: &'tx Transaction,
    row: PhantomData<fn() -> R>,
}

impl<'tx, R: ZeroRow> TypedTableMutator<'tx, R> {
    /// For generated code.
    pub fn new(tx: &'tx Transaction) -> Self {
        TypedTableMutator {
            tx,
            row: PhantomData,
        }
    }

    /// Inserts a row unless one with its primary key exists.
    pub fn insert(&self, row: &R) -> Result<(), Error> {
        self.tx.table(R::TABLE).insert(&row.column_values())
    }

    /// Inserts a row, or replaces the columns of the existing one. Every
    /// column is written, so an optional one that is `None` sets null; JS
    /// keeps a column the value leaves out. Use `update` to set only some
    /// columns.
    pub fn upsert(&self, row: &R) -> Result<(), Error> {
        self.tx.table(R::TABLE).upsert(&row.column_values())
    }

    /// Sets the columns that `value` sets, of the row with its primary key,
    /// if the row exists.
    pub fn update(&self, value: &R::Update) -> Result<(), Error> {
        self.tx.table(R::TABLE).update(&value.column_values())
    }

    /// Deletes the row with the primary key `key`, if any.
    pub fn delete(&self, key: &R::Key) -> Result<(), Error> {
        self.tx.table(R::TABLE).delete(&key.column_values())
    }
}

/// A column in an update that can be null: left as it is (the default),
/// set to null, or set to a value. A column that can't be null is an
/// `Option` in an update instead, `None` leaving it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum ColumnUpdate<V> {
    #[default]
    /// Leaves the column as it is.
    Unchanged,
    /// Sets the column to null.
    Null,
    /// Sets the column to the value.
    Set(V),
}

impl<V> From<Option<V>> for ColumnUpdate<V> {
    /// Sets the column to the value, or to null for `None`.
    fn from(value: Option<V>) -> Self {
        value.map_or(ColumnUpdate::Null, ColumnUpdate::Set)
    }
}

impl<V: ZeroValue> ColumnUpdate<V> {
    /// What the update writes to the column; `None` when it leaves it. For
    /// generated code.
    pub fn to_zero_value(&self) -> Option<Value> {
        match self {
            ColumnUpdate::Unchanged => None,
            ColumnUpdate::Null => Some(Value::Null),
            ColumnUpdate::Set(v) => Some(v.to_zero_value()),
        }
    }
}

/// What a query materializes to: its rows (`Vec<Row>`, `Option<Row>` after
/// `one()`), or the value of an aggregate, such as the `i64` of `count()`.
pub trait QueryValue: Sized {
    /// Reads the query's result from its view.
    fn from_view(view: &ViewValue) -> Result<Self, ZeroValueError>;
}

impl<E: QueryElement> QueryValue for Vec<E> {
    fn from_view(view: &ViewValue) -> Result<Self, ZeroValueError> {
        view.entries().into_iter().map(E::from_entry).collect()
    }
}

/// What an `Option` query value holds: the row of `one()`, or the value of
/// `sum`, `avg`, `min` or `max`, which is `None` over no rows. Generated
/// rows implement it with [`optional_row`], generated enumerations with
/// [`optional_value`].
pub trait OptionalQueryElement: Sized {
    /// Reads the query's result from its view: `None` for no row, or for an
    /// aggregate over no rows.
    fn optional_from_view(view: &ViewValue) -> Result<Option<Self>, ZeroValueError>;
}

impl<T: OptionalQueryElement> QueryValue for Option<T> {
    fn from_view(view: &ViewValue) -> Result<Self, ZeroValueError> {
        T::optional_from_view(view)
    }
}

/// The value of `count()`.
impl QueryValue for i64 {
    fn from_view(view: &ViewValue) -> Result<Self, ZeroValueError> {
        i64::from_zero_value(aggregate_value(view)?)
    }
}

fn aggregate_value(view: &ViewValue) -> Result<&Value, ZeroValueError> {
    match view {
        ViewValue::Value(v) => Ok(v),
        _ => Err(ZeroValueError(
            "expected the value of an aggregate, got rows".into(),
        )),
    }
}

/// The row of a `one()` view, for generated rows' [`OptionalQueryElement`].
pub fn optional_row<R: QueryElement>(view: &ViewValue) -> Result<Option<R>, ZeroValueError> {
    view.entries().first().map(|e| R::from_entry(e)).transpose()
}

/// The value of an aggregate's view: `None` when it's null.
pub fn optional_value<V: ZeroValue>(view: &ViewValue) -> Result<Option<V>, ZeroValueError> {
    let value = aggregate_value(view)?;
    if value.is_null() {
        Ok(None)
    } else {
        V::from_zero_value(value).map(Some)
    }
}

macro_rules! optional_values {
    ($($t:ty),*) => {$(
        impl OptionalQueryElement for $t {
            fn optional_from_view(view: &ViewValue) -> Result<Option<Self>, ZeroValueError> {
                optional_value(view)
            }
        }
    )*};
}
optional_values!(f64, i64, String, bool, Value);

/// A column of the row type `R`, holding `V`s: generated as a constant of
/// the row struct, such as `Issue::TITLE`.
pub struct Column<R, V> {
    name: &'static str,
    types: PhantomData<fn() -> (R, V)>,
}

impl<R, V> Clone for Column<R, V> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<R, V> Copy for Column<R, V> {}

impl<R, V> fmt::Debug for Column<R, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Column").field(&self.name).finish()
    }
}

impl<R, V> Column<R, V> {
    /// The column named `name` (its client name). For generated code.
    pub const fn new(name: &'static str) -> Self {
        Column {
            name,
            types: PhantomData,
        }
    }

    /// The column's client name.
    pub fn name(&self) -> &'static str {
        self.name
    }

    fn compare(self, op: SimpleOperator, right: ValuePosition) -> Condition {
        Condition::Simple {
            op,
            left: ValuePosition::Column(self.name.to_owned()),
            right,
        }
    }

    /// Compares the column with a value of the context, such as the auth
    /// data.
    pub fn cmp_param(self, op: SimpleOperator, param: &StaticParameter) -> Condition {
        self.compare(
            op,
            ValuePosition::Static {
                anchor: param.anchor,
                field: param.field.clone(),
            },
        )
    }
}

impl<R, V: ZeroValue> Column<R, V> {
    /// Compares the column with `value` by `op`.
    pub fn cmp(self, op: SimpleOperator, value: V) -> Condition {
        self.compare(op, ValuePosition::Literal(value.to_zero_value()))
    }

    /// Equal to `value`. Comparing with null uses `IS`, since `= NULL`
    /// matches nothing.
    pub fn eq(self, value: V) -> Condition {
        let value = value.to_zero_value();
        let op = if value.is_null() {
            SimpleOperator::Is
        } else {
            SimpleOperator::Eq
        };
        self.compare(op, ValuePosition::Literal(value))
    }

    /// Not equal to `value`; with null, `IS NOT`.
    pub fn ne(self, value: V) -> Condition {
        let value = value.to_zero_value();
        let op = if value.is_null() {
            SimpleOperator::IsNot
        } else {
            SimpleOperator::Ne
        };
        self.compare(op, ValuePosition::Literal(value))
    }

    /// The column is less than `value`.
    pub fn lt(self, value: V) -> Condition {
        self.cmp(SimpleOperator::Lt, value)
    }
    /// The column is at most `value`.
    pub fn le(self, value: V) -> Condition {
        self.cmp(SimpleOperator::Le, value)
    }
    /// The column is greater than `value`.
    pub fn gt(self, value: V) -> Condition {
        self.cmp(SimpleOperator::Gt, value)
    }
    /// The column is at least `value`.
    pub fn ge(self, value: V) -> Condition {
        self.cmp(SimpleOperator::Ge, value)
    }
    /// The column `IS` `value`: as `=`, but null matches null.
    pub fn is(self, value: V) -> Condition {
        self.cmp(SimpleOperator::Is, value)
    }
    /// The column `IS NOT` `value`: as `!=`, but null matches non-null values.
    pub fn is_not(self, value: V) -> Condition {
        self.cmp(SimpleOperator::IsNot, value)
    }

    /// `IN`.
    pub fn is_in(self, values: Vec<V>) -> Condition {
        self.compare(SimpleOperator::In, ValuePosition::Literal(array(values)))
    }

    /// `NOT IN`.
    pub fn not_in(self, values: Vec<V>) -> Condition {
        self.compare(SimpleOperator::NotIn, ValuePosition::Literal(array(values)))
    }
}

fn array<V: ZeroValue>(values: Vec<V>) -> Value {
    Value::Array(values.iter().map(V::to_zero_value).collect())
}

macro_rules! like {
    ($t:ty) => {
        impl<R> Column<R, $t> {
            /// The column matches the SQL `LIKE` pattern, with `%` and `_` as
            /// wildcards (`like::escape_like` escapes them).
            pub fn like(self, pattern: &str) -> Condition {
                self.pattern(SimpleOperator::Like, pattern)
            }
            /// The column doesn't match the `LIKE` pattern.
            pub fn not_like(self, pattern: &str) -> Condition {
                self.pattern(SimpleOperator::NotLike, pattern)
            }
            /// The column matches the pattern ignoring case (`ILIKE`).
            pub fn ilike(self, pattern: &str) -> Condition {
                self.pattern(SimpleOperator::Ilike, pattern)
            }
            /// The column doesn't match the pattern ignoring case (`NOT
            /// ILIKE`).
            pub fn not_ilike(self, pattern: &str) -> Condition {
                self.pattern(SimpleOperator::NotIlike, pattern)
            }
            fn pattern(self, op: SimpleOperator, pattern: &str) -> Condition {
                self.compare(
                    op,
                    ValuePosition::Literal(Value::String(pattern.to_owned())),
                )
            }
        }
    };
}
like!(String);
like!(Option<String>);

/// A column of `R` of any type, for the lists of `group_by`, `distinct`
/// and `start_values`: `Issue::OWNER_ID.into()`.
pub struct ColumnRef<R> {
    name: &'static str,
    row: PhantomData<fn() -> R>,
}

impl<R> Clone for ColumnRef<R> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<R> Copy for ColumnRef<R> {}

impl<R> fmt::Debug for ColumnRef<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ColumnRef").field(&self.name).finish()
    }
}

impl<R> ColumnRef<R> {
    /// The column's client name.
    pub fn name(&self) -> &'static str {
        self.name
    }
}

impl<R, V> From<Column<R, V>> for ColumnRef<R> {
    fn from(c: Column<R, V>) -> Self {
        ColumnRef {
            name: c.name,
            row: PhantomData,
        }
    }
}

/// A relationship from the row type `R` to the row type `D`: generated as a
/// constant of the row struct, such as `Issue::COMMENTS`.
pub struct Relationship<R, D> {
    name: &'static str,
    types: PhantomData<fn() -> (R, D)>,
}

impl<R, D> Clone for Relationship<R, D> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<R, D> Copy for Relationship<R, D> {}

impl<R, D> fmt::Debug for Relationship<R, D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Relationship").field(&self.name).finish()
    }
}

impl<R, D> Relationship<R, D> {
    /// The relationship named `name`. For generated code.
    pub const fn new(name: &'static str) -> Self {
        Relationship {
            name,
            types: PhantomData,
        }
    }

    /// The relationship's name.
    pub fn name(&self) -> &'static str {
        self.name
    }
}

/// The aggregates a row's query asked for in `related`, by relationship:
/// `related(Issue::COMMENTS, |q| q.count())` gives
/// `issue.aggregates.count(Issue::COMMENTS)`. The relationship's own field
/// stays `None`.
pub struct RelationshipAggregates<R> {
    /// The values by relationship name.
    pub values: Vec<(String, Value)>,
    row: PhantomData<fn() -> R>,
}

impl<R> Clone for RelationshipAggregates<R> {
    fn clone(&self) -> Self {
        RelationshipAggregates {
            values: self.values.clone(),
            row: PhantomData,
        }
    }
}

impl<R> fmt::Debug for RelationshipAggregates<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map()
            .entries(self.values.iter().map(|(k, v)| (k, v)))
            .finish()
    }
}

impl<R> PartialEq for RelationshipAggregates<R> {
    fn eq(&self, other: &Self) -> bool {
        self.values == other.values
    }
}

impl<R> Default for RelationshipAggregates<R> {
    fn default() -> Self {
        RelationshipAggregates {
            values: Vec::new(),
            row: PhantomData,
        }
    }
}

impl<R> RelationshipAggregates<R> {
    /// The aggregates of `entry`'s relationships, for generated
    /// `from_entry`s.
    pub fn from_entry(entry: &ViewEntry) -> Self {
        RelationshipAggregates {
            values: entry
                .relationships
                .iter()
                .filter_map(|(name, v)| match v {
                    ViewValue::Value(v) => Some((name.clone(), v.clone())),
                    _ => None,
                })
                .collect(),
            row: PhantomData,
        }
    }

    /// Whether the query asked for no aggregates.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// The `count()` of a relationship; `None` when the query didn't ask
    /// for it.
    pub fn count<D>(&self, relationship: Relationship<R, D>) -> Option<i64> {
        self.value(relationship)
    }

    /// The `sum` of a relationship; `None` when the query didn't ask for it
    /// or it has no rows.
    pub fn sum<D>(&self, relationship: Relationship<R, D>) -> Option<f64> {
        self.value(relationship)
    }

    /// The `avg` of a relationship; `None` when the query didn't ask for it
    /// or it has no rows.
    pub fn avg<D>(&self, relationship: Relationship<R, D>) -> Option<f64> {
        self.value(relationship)
    }

    /// The aggregate of a relationship read as `V`, as for `min` and `max`;
    /// `None` when the query didn't ask for it, it's null, or it isn't a
    /// `V`.
    pub fn value<V: ZeroValue, D>(&self, relationship: Relationship<R, D>) -> Option<V> {
        let (_, v) = self.values.iter().find(|(k, _)| k == relationship.name)?;
        if v.is_null() {
            return None;
        }
        V::from_zero_value(v).ok()
    }
}

/// A query over the rows of `R`'s table that materializes to a `T`: the
/// rows (`Vec<R>`), the row of `one()` (`Option<R>`), or an aggregate.
pub struct Query<R, T = Vec<R>> {
    any: AnyQuery,
    types: PhantomData<fn() -> (R, T)>,
}

impl<R, T> Clone for Query<R, T> {
    fn clone(&self) -> Self {
        Query::from_any(self.any.clone())
    }
}

impl<R, T> fmt::Debug for Query<R, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Query").field(&self.any).finish()
    }
}

impl<R, T> Query<R, T> {
    /// The typed form of an untyped query of `R`'s table.
    pub fn from_any(any: AnyQuery) -> Self {
        Query {
            any,
            types: PhantomData,
        }
    }

    /// The untyped query this one builds.
    pub fn any(&self) -> &AnyQuery {
        &self.any
    }

    /// The untyped query this one wraps.
    pub fn into_any(self) -> AnyQuery {
        self.any
    }

    fn map(self, f: impl FnOnce(AnyQuery) -> AnyQuery) -> Self {
        Query::from_any(f(self.any))
    }

    fn to<U>(self, f: impl FnOnce(AnyQuery) -> AnyQuery) -> Query<R, U> {
        Query::from_any(f(self.any))
    }
}

impl<R: ZeroRow> Query<R> {
    /// All rows of `R`'s table, for the generated query functions.
    pub fn table(schema: Arc<Schema>) -> Self {
        Query::from_any(AnyQuery::new(schema, R::TABLE, System::Client))
    }
}

impl<R: ZeroRow, T> Query<R, T> {
    /// Rows whose `column` equals `value`; with null, `IS`.
    pub fn where_eq<V: ZeroValue>(self, column: Column<R, V>, value: V) -> Self {
        self.where_condition(column.eq(value))
    }

    /// Rows whose `column` compares to `value` by `op`.
    pub fn where_<V: ZeroValue>(self, column: Column<R, V>, op: SimpleOperator, value: V) -> Self {
        self.map(|q| q.where_(column.name, op, value.to_zero_value()))
    }

    /// Rows whose `column` is `IN` or `NOT IN` `values`.
    pub fn where_in<V: ZeroValue>(
        self,
        column: Column<R, V>,
        op: SimpleOperator,
        values: Vec<V>,
    ) -> Self {
        self.map(|q| q.where_(column.name, op, array(values)))
    }

    /// Rows whose `column` compares to a value of the context by `op`.
    pub fn where_param<V>(
        self,
        column: Column<R, V>,
        op: SimpleOperator,
        param: &StaticParameter,
    ) -> Self {
        self.map(|q| q.where_param(column.name, op, param))
    }

    /// Rows that match `condition`, and any earlier `where`.
    pub fn where_condition(self, condition: Condition) -> Self {
        self.map(|q| q.where_condition(condition))
    }

    /// Rows that match the condition `factory` builds from the row's
    /// expression: its JSON paths, `exists`, and `and`, `or` and `not`.
    pub fn where_expr(self, factory: impl FnOnce(&RowExpression<'_, '_, R>) -> Condition) -> Self {
        self.map(|q| {
            q.where_expr(|eb| {
                factory(&RowExpression {
                    builder: eb,
                    row: PhantomData,
                })
            })
        })
    }

    /// Rows with a related row in `relationship` that `sub` matches.
    pub fn where_exists<D: ZeroRow>(
        self,
        relationship: Relationship<R, D>,
        options: ExistsOptions,
        sub: impl FnOnce(Query<D>) -> Query<D>,
    ) -> Self {
        self.map(|q| q.where_exists(relationship.name, options, untyped(sub)))
    }

    /// Fills `relationship` of each row, or, when `sub` ends in an
    /// aggregate, sets the row's aggregate of it: `related(Issue::COMMENTS,
    /// |q| q.count())` gives `aggregates.count(Issue::COMMENTS)`.
    pub fn related<D: ZeroRow, S>(
        self,
        relationship: Relationship<R, D>,
        sub: impl FnOnce(Query<D>) -> Query<D, S>,
    ) -> Self {
        self.map(|q| q.related(relationship.name, |a| sub(Query::from_any(a)).any))
    }

    /// Orders the rows by `column`, after any earlier `order_by`; the primary
    /// key breaks ties.
    pub fn order_by<V>(self, column: Column<R, V>, direction: Direction) -> Self {
        self.map(|q| q.order_by(column.name, direction))
    }

    /// At most `limit` rows.
    pub fn limit(self, limit: i64) -> Self {
        self.map(|q| q.limit(limit))
    }

    /// Starts after `row`, or at it when `inclusive`.
    pub fn start(self, row: &R, inclusive: bool) -> Self {
        self.map(|q| q.start(row.column_values(), inclusive))
    }

    /// Starts after the row with these column values, or at it when
    /// `inclusive`: a cursor that holds only the columns the query orders
    /// by.
    pub fn start_values(self, values: Vec<(ColumnRef<R>, Value)>, inclusive: bool) -> Self {
        let row = values.into_iter().map(|(c, v)| (c.name, v)).collect();
        self.map(|q| q.start(row, inclusive))
    }

    /// At most one row.
    pub fn one(self) -> Query<R, Option<R>> {
        self.to(AnyQuery::one)
    }

    /// Reduces the query to a count of the rows left after `where`, `start`
    /// and `limit`.
    pub fn count(self) -> Query<R, i64> {
        self.to(AnyQuery::count)
    }

    /// The sum of a number column; `None` over no rows.
    pub fn sum<V: NumericZeroValue>(self, column: Column<R, V>) -> Query<R, Option<f64>> {
        self.to(|q| q.sum(column.name))
    }

    /// The sum of the numbers at `path` in a `json` column. A missing path,
    /// or a value that isn't a number, is skipped.
    pub fn sum_json<V>(
        self,
        column: Column<R, V>,
        path: Vec<JsonPathSegment>,
    ) -> Query<R, Option<f64>> {
        self.to(|q| q.sum_json(column.name, path))
    }

    /// The average of a number column; `None` over no rows.
    pub fn avg<V: NumericZeroValue>(self, column: Column<R, V>) -> Query<R, Option<f64>> {
        self.to(|q| q.avg(column.name))
    }

    /// Reduces the query to the average of the numbers at `path` in the JSON
    /// `column`.
    pub fn avg_json<V>(
        self,
        column: Column<R, V>,
        path: Vec<JsonPathSegment>,
    ) -> Query<R, Option<f64>> {
        self.to(|q| q.avg_json(column.name, path))
    }

    /// The least value of a number or string column; `None` over no rows.
    pub fn min<V: OrderedZeroValue>(self, column: Column<R, V>) -> Query<R, Option<V::Unwrapped>> {
        self.to(|q| q.min(column.name))
    }

    /// Reduces the query to the least value at `path` in the JSON `column`.
    pub fn min_json<V>(
        self,
        column: Column<R, V>,
        path: Vec<JsonPathSegment>,
    ) -> Query<R, Option<Value>> {
        self.to(|q| q.min_json(column.name, path))
    }

    /// The greatest value of a number or string column; `None` over no
    /// rows.
    pub fn max<V: OrderedZeroValue>(self, column: Column<R, V>) -> Query<R, Option<V::Unwrapped>> {
        self.to(|q| q.max(column.name))
    }

    /// Reduces the query to the greatest value at `path` in the JSON `column`.
    pub fn max_json<V>(
        self,
        column: Column<R, V>,
        path: Vec<JsonPathSegment>,
    ) -> Query<R, Option<Value>> {
        self.to(|q| q.max_json(column.name, path))
    }

    /// The distinct combinations of `columns` among the rows `where` keeps.
    /// `count()` after it counts the combinations.
    pub fn distinct(self, columns: Vec<ColumnRef<R>>) -> Query<R, Vec<DistinctRow<R>>> {
        self.to(|q| q.distinct(names(columns)))
    }

    /// Groups the rows by `columns`, for the aggregate that reduces each
    /// group.
    pub fn group_by(self, columns: Vec<ColumnRef<R>>) -> GroupedQuery<R> {
        GroupedQuery {
            any: self.any.group_by(names(columns)),
            row: PhantomData,
        }
    }
}

impl<C, R, T> crate::query::ZeroQuery<C> for Query<R, T> {
    fn any_query(&self, _: &C) -> std::borrow::Cow<'_, AnyQuery> {
        std::borrow::Cow::Borrowed(&self.any)
    }
}

impl<R, T: QueryValue> Query<R, T> {
    /// Reads a view of this query as its result.
    pub fn read(&self, view: &ViewValue) -> Result<T, ZeroValueError> {
        T::from_view(view)
    }
}

fn names<R>(columns: Vec<ColumnRef<R>>) -> Vec<String> {
    columns.into_iter().map(|c| c.name.to_owned()).collect()
}

fn untyped<D>(sub: impl FnOnce(Query<D>) -> Query<D>) -> impl FnOnce(AnyQuery) -> AnyQuery {
    |a| sub(Query::from_any(a)).any
}

/// The row a `where_expr` condition is about: JSON paths into its columns,
/// `exists` over its relationships, and the connectives. Compare columns
/// with their constants' methods, such as `Issue::TITLE.like("%bug%")`.
pub struct RowExpression<'a, 'q, R> {
    builder: &'a ExpressionBuilder<'q>,
    row: PhantomData<fn() -> R>,
}

impl<R> RowExpression<'_, '_, R> {
    /// A value inside a `json` column: `e.json(Issue::METADATA,
    /// vec!["tags".into(), 0.into()]).eq("bug".into())`.
    pub fn json<V>(&self, column: Column<R, V>, path: Vec<JsonPathSegment>) -> JsonPath<'_, '_> {
        JsonPath {
            builder: self.builder,
            reference: self.builder.json(column.name, path),
        }
    }

    /// Whether a related row in `relationship` matches `sub`.
    pub fn exists<D: ZeroRow>(
        &self,
        relationship: Relationship<R, D>,
        options: ExistsOptions,
        sub: impl FnOnce(Query<D>) -> Query<D>,
    ) -> Condition {
        self.builder
            .exists(relationship.name, options, untyped(sub))
    }

    /// All of `conditions`; true when there are none.
    pub fn and(&self, conditions: Vec<Condition>) -> Condition {
        self.builder.and(conditions)
    }

    /// Any of `conditions`; false when there are none.
    pub fn or(&self, conditions: Vec<Condition>) -> Condition {
        self.builder.or(conditions)
    }

    /// Rows that `condition` doesn't match.
    pub fn not(&self, condition: Condition) -> Condition {
        self.builder.not(condition)
    }
}

/// A value inside a `json` column in a `where_expr`. Its JSON type isn't
/// known, so it compares with any [`Value`]: one of another type never
/// equals it. Comparing with null uses `IS`.
pub struct JsonPath<'a, 'q> {
    builder: &'a ExpressionBuilder<'q>,
    reference: JsonPathReference,
}

impl JsonPath<'_, '_> {
    /// The value at the path compares to `value` by `op`.
    pub fn cmp(&self, op: SimpleOperator, value: Value) -> Condition {
        self.builder.cmp_json(&self.reference, op, value)
    }

    /// The value at the path compares to a value of the context by `op`.
    pub fn cmp_param(&self, op: SimpleOperator, param: &StaticParameter) -> Condition {
        self.builder.cmp_json_param(&self.reference, op, param)
    }

    /// The value at the path equals `value` (`IS` for null).
    pub fn eq(&self, value: Value) -> Condition {
        let op = if value.is_null() {
            SimpleOperator::Is
        } else {
            SimpleOperator::Eq
        };
        self.cmp(op, value)
    }

    /// The value at the path differs from `value` (`IS NOT` for null).
    pub fn ne(&self, value: Value) -> Condition {
        let op = if value.is_null() {
            SimpleOperator::IsNot
        } else {
            SimpleOperator::Ne
        };
        self.cmp(op, value)
    }

    /// The value at the path is less than `value`.
    pub fn lt(&self, value: Value) -> Condition {
        self.cmp(SimpleOperator::Lt, value)
    }
    /// The value at the path is at most `value`.
    pub fn le(&self, value: Value) -> Condition {
        self.cmp(SimpleOperator::Le, value)
    }
    /// The value at the path is greater than `value`.
    pub fn gt(&self, value: Value) -> Condition {
        self.cmp(SimpleOperator::Gt, value)
    }
    /// The value at the path is at least `value`.
    pub fn ge(&self, value: Value) -> Condition {
        self.cmp(SimpleOperator::Ge, value)
    }
    /// The value at the path `IS` `value`.
    pub fn is(&self, value: Value) -> Condition {
        self.cmp(SimpleOperator::Is, value)
    }
    /// The value at the path `IS NOT` `value`.
    pub fn is_not(&self, value: Value) -> Condition {
        self.cmp(SimpleOperator::IsNot, value)
    }
    /// `values` must be of one JSON type.
    pub fn is_in(&self, values: Vec<Value>) -> Condition {
        self.cmp(SimpleOperator::In, Value::Array(values))
    }
    /// The value at the path is none of `values`.
    pub fn not_in(&self, values: Vec<Value>) -> Condition {
        self.cmp(SimpleOperator::NotIn, Value::Array(values))
    }
    /// The value at the path matches the `LIKE` pattern.
    pub fn like(&self, pattern: &str) -> Condition {
        self.cmp(SimpleOperator::Like, Value::String(pattern.to_owned()))
    }
    /// The value at the path doesn't match the `LIKE` pattern.
    pub fn not_like(&self, pattern: &str) -> Condition {
        self.cmp(SimpleOperator::NotLike, Value::String(pattern.to_owned()))
    }
    /// The value at the path matches the pattern ignoring case (`ILIKE`).
    pub fn ilike(&self, pattern: &str) -> Condition {
        self.cmp(SimpleOperator::Ilike, Value::String(pattern.to_owned()))
    }
    /// The value at the path doesn't match the pattern ignoring case.
    pub fn not_ilike(&self, pattern: &str) -> Condition {
        self.cmp(SimpleOperator::NotIlike, Value::String(pattern.to_owned()))
    }
}

/// A query of the [`GroupRow`]s of `group_by` and an aggregate.
pub type Grouped<R, V> = Query<R, Vec<GroupRow<R, V>>>;

/// A query grouped by columns, waiting for the aggregate that reduces each
/// group (`GroupedQuery` in `packages/zql/src/query/query.ts`): one
/// [`GroupRow`] per group.
pub struct GroupedQuery<R> {
    any: AnyGroupedQuery,
    row: PhantomData<fn() -> R>,
}

impl<R> GroupedQuery<R> {
    /// The untyped grouped query this one wraps.
    pub fn any(&self) -> &AnyGroupedQuery {
        &self.any
    }

    /// Each group's row count.
    pub fn count(self) -> Grouped<R, i64> {
        Query::from_any(self.any.count())
    }

    /// Each group's sum of `column`.
    pub fn sum<V: NumericZeroValue>(self, column: Column<R, V>) -> Grouped<R, Option<f64>> {
        Query::from_any(self.any.sum(column.name))
    }

    /// Each group's sum of the numbers at `path` in the JSON `column`.
    pub fn sum_json<V>(
        self,
        column: Column<R, V>,
        path: Vec<JsonPathSegment>,
    ) -> Grouped<R, Option<f64>> {
        Query::from_any(self.any.sum_json(column.name, path))
    }

    /// Each group's average of `column`.
    pub fn avg<V: NumericZeroValue>(self, column: Column<R, V>) -> Grouped<R, Option<f64>> {
        Query::from_any(self.any.avg(column.name))
    }

    /// Each group's average of the numbers at `path` in the JSON `column`.
    pub fn avg_json<V>(
        self,
        column: Column<R, V>,
        path: Vec<JsonPathSegment>,
    ) -> Grouped<R, Option<f64>> {
        Query::from_any(self.any.avg_json(column.name, path))
    }

    /// Each group's least value of `column`.
    pub fn min<V: OrderedZeroValue>(
        self,
        column: Column<R, V>,
    ) -> Grouped<R, Option<V::Unwrapped>> {
        Query::from_any(self.any.min(column.name))
    }

    /// Each group's least value at `path` in the JSON `column`.
    pub fn min_json<V>(
        self,
        column: Column<R, V>,
        path: Vec<JsonPathSegment>,
    ) -> Grouped<R, Option<Value>> {
        Query::from_any(self.any.min_json(column.name, path))
    }

    /// Each group's greatest value of `column`.
    pub fn max<V: OrderedZeroValue>(
        self,
        column: Column<R, V>,
    ) -> Grouped<R, Option<V::Unwrapped>> {
        Query::from_any(self.any.max(column.name))
    }

    /// Each group's greatest value at `path` in the JSON `column`.
    pub fn max_json<V>(
        self,
        column: Column<R, V>,
        path: Vec<JsonPathSegment>,
    ) -> Grouped<R, Option<Value>> {
        Query::from_any(self.any.max_json(column.name, path))
    }
}

/// A row of `group_by(...)` and an aggregate: the group's columns, read with
/// [`GroupRow::get`], and the aggregate's `value`.
pub struct GroupRow<R, V> {
    /// The aggregate of the group's rows.
    pub value: V,
    /// The group's columns by client name.
    pub columns: Object,
    row: PhantomData<fn() -> R>,
}

impl<R, V: Clone> Clone for GroupRow<R, V> {
    fn clone(&self) -> Self {
        GroupRow {
            value: self.value.clone(),
            columns: self.columns.clone(),
            row: PhantomData,
        }
    }
}

impl<R, V: fmt::Debug> fmt::Debug for GroupRow<R, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GroupRow")
            .field("value", &self.value)
            .field("columns", &self.columns)
            .finish()
    }
}

impl<R, V: PartialEq> PartialEq for GroupRow<R, V> {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value && self.columns == other.columns
    }
}

impl<R, V: ZeroValue> QueryElement for GroupRow<R, V> {
    fn from_entry(entry: &ViewEntry) -> Result<Self, ZeroValueError> {
        // `GroupProjection` puts the value after the group columns. Its name
        // is the function's, which can be another group column's.
        let mut columns: Object = (*entry.row).clone();
        let last = columns
            .get_index(columns.len().wrapping_sub(1))
            .map(|(k, v)| (k.clone(), v.clone()))
            .ok_or_else(|| ZeroValueError("a group row has no value".into()))?;
        columns.remove(&last.0);
        Ok(GroupRow {
            value: decode(&last.0, Some(&last.1))?,
            columns,
            row: PhantomData,
        })
    }
}

impl<R, V> GroupRow<R, V> {
    /// A column the query groups by, read as a row's column is.
    pub fn get<C: ZeroValue>(&self, column: Column<R, C>) -> Result<C, ZeroValueError> {
        group_column(&self.columns, column)
    }
}

/// A row of `distinct(...)`: the values of its columns, read with
/// [`DistinctRow::get`].
pub struct DistinctRow<R> {
    /// The columns by client name.
    pub columns: Object,
    row: PhantomData<fn() -> R>,
}

impl<R> Clone for DistinctRow<R> {
    fn clone(&self) -> Self {
        DistinctRow {
            columns: self.columns.clone(),
            row: PhantomData,
        }
    }
}

impl<R> fmt::Debug for DistinctRow<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("DistinctRow").field(&self.columns).finish()
    }
}

impl<R> PartialEq for DistinctRow<R> {
    fn eq(&self, other: &Self) -> bool {
        self.columns == other.columns
    }
}

impl<R> QueryElement for DistinctRow<R> {
    fn from_entry(entry: &ViewEntry) -> Result<Self, ZeroValueError> {
        Ok(DistinctRow {
            columns: (*entry.row).clone(),
            row: PhantomData,
        })
    }
}

impl<R> DistinctRow<R> {
    /// A column of the `distinct`, read as a row's column is.
    pub fn get<C: ZeroValue>(&self, column: Column<R, C>) -> Result<C, ZeroValueError> {
        group_column(&self.columns, column)
    }
}

fn group_column<R, C: ZeroValue>(
    columns: &Object,
    column: Column<R, C>,
) -> Result<C, ZeroValueError> {
    match columns.get(column.name) {
        Some(v) => decode(column.name, Some(v)),
        None => Err(ZeroValueError(format!(
            "{} is not a column of the group",
            column.name
        ))),
    }
}
