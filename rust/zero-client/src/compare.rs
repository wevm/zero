//! Comparing row values (`packages/zql/src/ivm/data.ts`).

use crate::json::{Value, stringify};
use crate::strings;
use std::cmp::Ordering;
use std::fmt;

/// Two values JS's `compareValues` throws on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IncomparableError(pub String);

impl fmt::Display for IncomparableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for IncomparableError {}

/// JS's `typeof`, which calls an array an object.
fn type_of(v: &Value) -> &'static str {
    match v {
        Value::Array(_) => "object",
        v => v.type_name(),
    }
}

/// `compareValues(a, b)`: strings by UTF-8 bytes, numbers numerically,
/// `false < true`, and `null` before everything. Values of different
/// non-null types, and arrays or objects, can't be compared: JS throws.
pub fn compare_checked(a: &Value, b: &Value) -> Result<Ordering, IncomparableError> {
    match scalar_compare(a, b) {
        Some(o) => Ok(o),
        None if type_of(a) != type_of(b) => Err(IncomparableError(format!(
            "Cannot compare values of different types: {} and {}",
            type_of(a),
            type_of(b)
        ))),
        None => Err(IncomparableError(format!(
            "Unsupported type: {}",
            type_of(a)
        ))),
    }
}

/// [`compare_checked`], with a total order for the inputs JS throws on: by
/// type, then by JSON encoding. Pipelines use this so a bad `orderBy` (on a
/// json column) degrades instead of failing.
pub fn compare(a: &Value, b: &Value) -> Ordering {
    scalar_compare(a, b).unwrap_or_else(|| {
        let rank = |v: &Value| match v {
            Value::Null => 0,
            Value::Bool(_) => 1,
            Value::Number(_) => 2,
            Value::String(_) => 3,
            Value::Array(_) => 4,
            Value::Object(_) => 5,
        };
        rank(a)
            .cmp(&rank(b))
            .then_with(|| strings::utf8(&stringify(a), &stringify(b)))
    })
}

fn scalar_compare(a: &Value, b: &Value) -> Option<Ordering> {
    Some(match (a, b) {
        (Value::String(x), Value::String(y)) => strings::utf8(x, y),
        // NaN can't come from JSON; JS's `<`/`>` would call it equal.
        (Value::Number(x), Value::Number(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, _) => Ordering::Less,
        (_, Value::Null) => Ordering::Greater,
        _ => return None,
    })
}

/// `valuesEqual(a, b)`: like `===`, except `null` equals nothing (as in SQL
/// joins). Arrays and objects are never equal: JS compares them by
/// reference.
pub fn equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Number(x), Value::Number(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        _ => false,
    }
}
