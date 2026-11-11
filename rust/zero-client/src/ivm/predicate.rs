//! Row predicates for conditions without subqueries (`createPredicate` in
//! `packages/zql/src/builder/filter.ts`), and `transformFilters`.

use super::{PipelineError, Row, identical};
use crate::ast::{Condition, SimpleOperator, ValuePosition, json_value_at};
use crate::compare;
use crate::json::{Value, number};
use crate::like::LikePattern;
use crate::query::simplify_condition;
use std::cmp::Ordering;
use std::rc::Rc;

/// Whether a row passes a filter.
pub type RowPredicate = Rc<dyn Fn(&Row) -> bool>;

type ValuePredicate = Rc<dyn Fn(&Value) -> bool>;

/// `createPredicate(condition)` for a condition without subqueries.
pub fn create_predicate(condition: &Condition) -> Result<RowPredicate, PipelineError> {
    match condition {
        Condition::And(conditions) => {
            let predicates = conditions
                .iter()
                .map(create_predicate)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Rc::new(move |row| predicates.iter().all(|p| p(row))))
        }
        Condition::Or(conditions) => {
            let predicates = conditions
                .iter()
                .map(create_predicate)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Rc::new(move |row| predicates.iter().any(|p| p(row))))
        }
        Condition::CorrelatedSubquery { .. } => Err(PipelineError::new(
            "createPredicate: unexpected correlated subquery",
        )),
        Condition::Simple { op, left, right } => simple_predicate(*op, left, right),
    }
}

fn unresolved_static() -> PipelineError {
    PipelineError::new("static values should be resolved before creating predicates")
}

fn simple_predicate(
    op: SimpleOperator,
    left: &ValuePosition,
    right: &ValuePosition,
) -> Result<RowPredicate, PipelineError> {
    let rhs = match right {
        ValuePosition::Literal(v) => v.clone(),
        ValuePosition::Static { .. } => return Err(unresolved_static()),
        _ => {
            return Err(PipelineError::new(
                "createPredicate: right side must be a literal",
            ));
        }
    };
    if let ValuePosition::Static { .. } = left {
        return Err(unresolved_static());
    }
    if matches!(op, SimpleOperator::Is | SimpleOperator::IsNot) {
        let is_not = op == SimpleOperator::IsNot;
        let rhs = Some(rhs);
        return Ok(match left {
            ValuePosition::Literal(l) => {
                let result = identical(Some(l), rhs.as_ref()) != is_not;
                Rc::new(move |_| result)
            }
            ValuePosition::Json { column, path } => {
                let (column, path) = (column.clone(), path.clone());
                Rc::new(move |row| {
                    let v = json_value_at(&path, row.get(&column));
                    identical(Some(v), rhs.as_ref()) != is_not
                })
            }
            ValuePosition::Column(name) => {
                let name = name.clone();
                Rc::new(move |row| identical(row.get(&name), rhs.as_ref()) != is_not)
            }
            ValuePosition::Static { .. } => unreachable!("checked above"),
        });
    }
    if rhs.is_null() {
        return Ok(Rc::new(|_| false));
    }
    let imp = predicate_impl(&rhs, op)?;
    Ok(match left {
        ValuePosition::Literal(l) => {
            let result = !l.is_null() && imp(l);
            Rc::new(move |_| result)
        }
        ValuePosition::Json { column, path } => {
            // A JSON leaf is dynamically typed, so the comparison is
            // type-strict: a leaf of another type than the literal's is a
            // non-match, or a match for a negated operator, and never an
            // error. LIKE compares text.
            use SimpleOperator::*;
            let required = match op {
                Like | NotLike | Ilike | NotIlike => Some("string"),
                _ => match rhs.as_array().and_then(|a| a.first()).unwrap_or(&rhs) {
                    Value::String(_) => Some("string"),
                    Value::Number(_) => Some("number"),
                    Value::Bool(_) => Some("boolean"),
                    _ => None,
                },
            };
            let negated = matches!(op, Ne | NotLike | NotIlike | NotIn);
            let (column, path) = (column.clone(), path.clone());
            Rc::new(move |row| {
                let lhs = json_value_at(&path, row.get(&column));
                if lhs.is_null() {
                    return false;
                }
                match required {
                    Some(t) if lhs.type_name() == t => imp(lhs),
                    _ => negated,
                }
            })
        }
        ValuePosition::Column(name) => {
            let name = name.clone();
            Rc::new(move |row| match row.get(&name) {
                None | Some(Value::Null) => false,
                Some(lhs) => imp(lhs),
            })
        }
        ValuePosition::Static { .. } => unreachable!("checked above"),
    })
}

fn is_nan(v: &Value) -> bool {
    matches!(v, Value::Number(n) if n.is_nan())
}

/// `createPredicateImpl`: `lhs` and `rhs` are not null.
fn predicate_impl(rhs: &Value, op: SimpleOperator) -> Result<ValuePredicate, PipelineError> {
    use SimpleOperator::*;
    let r = rhs.clone();
    let cmp = move |l: &Value| compare::compare(l, &r);
    Ok(match op {
        Eq => {
            let r = rhs.clone();
            Rc::new(move |l| identical(Some(l), Some(&r)))
        }
        Ne => {
            let r = rhs.clone();
            Rc::new(move |l| !identical(Some(l), Some(&r)))
        }
        // compareValues orders strings by UTF-8, matching ORDER BY and SQLite.
        Lt => Rc::new(move |l| cmp(l) == Ordering::Less),
        Gt => Rc::new(move |l| cmp(l) == Ordering::Greater),
        // `compare` orders NaN as equal to every number; JS's `a - b` is NaN,
        // and NaN <= 0 and NaN >= 0 are false.
        Le => {
            let nan = is_nan(rhs);
            Rc::new(move |l| !nan && !is_nan(l) && cmp(l) != Ordering::Greater)
        }
        Ge => {
            let nan = is_nan(rhs);
            Rc::new(move |l| !nan && !is_nan(l) && cmp(l) != Ordering::Less)
        }
        Like | Ilike | NotLike | NotIlike => {
            let p = like_predicate(rhs, matches!(op, Ilike | NotIlike))?;
            if matches!(op, NotLike | NotIlike) {
                Rc::new(move |l| !p(l))
            } else {
                p
            }
        }
        In | NotIn => {
            let Value::Array(values) = rhs else {
                return Err(PipelineError::new(format!(
                    "Expected rhs to be an array for {} operator",
                    op.as_str()
                )));
            };
            let values = values.clone();
            let is_in = op == In;
            Rc::new(move |l| same_value_zero_contains(&values, l) == is_in)
        }
        Is | IsNot => unreachable!("handled by create_predicate"),
    })
}

/// `new Set(values).has(lhs)`: SameValueZero, arrays and objects by
/// reference (so never).
fn same_value_zero_contains(values: &[Value], lhs: &Value) -> bool {
    match lhs {
        Value::Array(_) | Value::Object(_) => false,
        Value::Number(n) if n.is_nan() => values.iter().any(is_nan),
        _ => values.iter().any(|v| identical(Some(v), Some(lhs))),
    }
}

/// `getLikePredicate(pattern, flags)`. JS converts the pattern with
/// `String()` and throws on a non-string left side; Rust treats that as no
/// match.
fn like_predicate(
    pattern: &Value,
    case_insensitive: bool,
) -> Result<ValuePredicate, PipelineError> {
    let like = LikePattern::new(&js_string(pattern), case_insensitive)
        .map_err(|e| PipelineError::new(e.to_string()))?;
    Ok(Rc::new(move |l| match l {
        Value::String(s) => like.matches(s),
        _ => false,
    }))
}

/// JS `String(value)` for JSON values.
pub fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".to_owned(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => number::format(*n),
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .map(|e| {
                if e.is_null() {
                    String::new()
                } else {
                    js_string(e)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".to_owned(),
    }
}

/// `transformFilters`: a condition without subqueries that matches a
/// superset of the rows `filters` matches, and whether anything was
/// removed.
pub fn transform_filters(filters: Option<&Condition>) -> (Option<Condition>, bool) {
    let Some(filters) = filters else {
        return (None, false);
    };
    let (is_or, conditions) = match filters {
        Condition::Simple { .. } => return (Some(filters.clone()), false),
        Condition::CorrelatedSubquery { .. } => return (None, true),
        Condition::And(c) => (false, c),
        Condition::Or(c) => (true, c),
    };
    let mut transformed = Vec::new();
    let mut removed = false;
    for c in conditions {
        let (f, r) = transform_filters(Some(c));
        // If any branch of an OR ends up empty, the whole OR goes.
        if f.is_none() && is_or {
            return (None, true);
        }
        removed |= r;
        transformed.extend(f);
    }
    let c = if is_or {
        Condition::Or(transformed)
    } else {
        Condition::And(transformed)
    };
    (Some(simplify_condition(c)), removed)
}
