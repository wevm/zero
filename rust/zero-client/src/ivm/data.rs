//! Value comparison for rows (`packages/zql/src/ivm/data.ts`); the value
//! orders themselves are in [`crate::compare`].

use super::Row;
use crate::ast::{Direction, OrderPart};
use crate::compare;
use crate::json::Value;
use std::cell::Cell;
use std::cmp::Ordering;
use std::rc::Rc;

/// Orders rows (`Comparator` in `data.ts`).
pub type Comparator = Rc<dyn Fn(&Row, &Row) -> Ordering>;

static NULL: Value = Value::Null;

/// A row's value for `column`; a missing column reads as `null`.
pub fn value<'a>(row: &'a Row, column: &str) -> &'a Value {
    row.get(column).unwrap_or(&NULL)
}

/// `compareValues` on possibly missing values: `undefined` reads as `null`.
pub fn compare_values(a: Option<&Value>, b: Option<&Value>) -> Ordering {
    compare::compare(a.unwrap_or(&NULL), b.unwrap_or(&NULL))
}

/// `valuesEqual` on possibly missing values: `undefined` and `null` equal
/// nothing.
pub fn values_equal(a: Option<&Value>, b: Option<&Value>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => compare::equal(a, b),
        _ => false,
    }
}

/// JS `===` on possibly missing values. Arrays and objects are never
/// identical, since rows are decoded fresh.
pub fn identical(a: Option<&Value>, b: Option<&Value>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(Value::Null), Some(Value::Null)) => true,
        (Some(Value::String(x)), Some(Value::String(y))) => x == y,
        (Some(Value::Number(x)), Some(Value::Number(y))) => x == y,
        (Some(Value::Bool(x)), Some(Value::Bool(y))) => x == y,
        _ => false,
    }
}

/// `makeComparator(order, reverse)`.
pub fn make_comparator(order: &[OrderPart], reverse: bool) -> Comparator {
    let parts: Vec<(String, bool, Cell<usize>)> = order
        .iter()
        .map(|p| (p.field.clone(), p.direction == Direction::Asc, Cell::new(0)))
        .collect();
    Rc::new(move |a: &Row, b: &Row| {
        for (field, asc, hint) in &parts {
            let c = compare_values(a.get_hinted(field, hint), b.get_hinted(field, hint));
            if c != Ordering::Equal {
                let c = if *asc { c } else { c.reverse() };
                return if reverse { c.reverse() } else { c };
            }
        }
        Ordering::Equal
    })
}
