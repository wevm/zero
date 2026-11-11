//! AST normalization (`normalizeAST` in `packages/zero-protocol/src/ast.ts`):
//! `related` sorted by alias, `where` flattened and sorted. Two ASTs that
//! describe the same query hash the same once normalized.
//!
//! The comparators are ported verbatim, including their asymmetries, since
//! the sort order decides the hash; [`crate::sort`] reproduces V8's sort.

use super::*;
use crate::sort;
use crate::strings::utf8;
use std::cmp::Ordering;

impl Ast {
    pub fn normalized(&self) -> Ast {
        let mut ast = self.clone();
        ast.where_ = self
            .where_
            .as_ref()
            .map(|w| normalize_condition(&normalize_subqueries(w)));
        if let Some(related) = &self.related {
            let mut related: Vec<CorrelatedSubquery> = related
                .iter()
                .map(|r| {
                    let mut r = r.clone();
                    r.subquery = Box::new(r.subquery.normalized());
                    r
                })
                .collect();
            if related
                .windows(2)
                .any(|w| cmp_related(&w[0], &w[1]).is_gt())
            {
                sort::sort(&mut related, cmp_related);
            }
            ast.related = Some(related);
        }
        ast
    }
}

fn normalize_subqueries(cond: &Condition) -> Condition {
    match cond {
        Condition::Simple { .. } => cond.clone(),
        Condition::CorrelatedSubquery {
            related,
            op,
            flip,
            scalar,
        } => {
            let mut related = related.clone();
            related.subquery = Box::new(related.subquery.normalized());
            Condition::CorrelatedSubquery {
                related,
                op: *op,
                flip: *flip,
                scalar: *scalar,
            }
        }
        Condition::And(c) => Condition::And(c.iter().map(normalize_subqueries).collect()),
        Condition::Or(c) => Condition::Or(c.iter().map(normalize_subqueries).collect()),
    }
}

pub fn normalize_condition(cond: &Condition) -> Condition {
    let (is_and, children) = match cond {
        Condition::And(c) => (true, c),
        Condition::Or(c) => (false, c),
        _ => return cond.clone(),
    };
    let mut conditions = Vec::with_capacity(children.len());
    for c in children {
        match (is_and, normalize_condition(c)) {
            (true, Condition::And(nested)) | (false, Condition::Or(nested)) => {
                conditions.extend(nested)
            }
            (_, n) => conditions.push(n),
        }
    }
    // A singleton conjunction is the condition itself. Empty ones are
    // meaningful: an empty `and` is true and an empty `or` is false.
    if conditions.len() == 1 {
        return conditions.pop().expect("one");
    }
    sort::sort(&mut conditions, cmp_condition);
    if is_and {
        Condition::And(conditions)
    } else {
        Condition::Or(conditions)
    }
}

fn type_name(c: &Condition) -> &'static str {
    match c {
        Condition::Simple { .. } => "simple",
        Condition::And(_) => "and",
        Condition::Or(_) => "or",
        Condition::CorrelatedSubquery { .. } => "correlatedSubquery",
    }
}

fn cmp_condition(a: &Condition, b: &Condition) -> Ordering {
    if let Condition::Simple {
        op: aop,
        left: al,
        right: ar,
    } = a
    {
        let Condition::Simple {
            op: bop,
            left: bl,
            right: br,
        } = b
        else {
            return Ordering::Less;
        };
        return compare_value_position(al, bl)
            .then_with(|| utf8(aop.as_str(), bop.as_str()))
            .then_with(|| compare_value_position(ar, br));
    }
    if matches!(b, Condition::Simple { .. }) {
        return Ordering::Greater;
    }
    if let Condition::CorrelatedSubquery {
        related: ar,
        op: aop,
        flip: af,
        scalar: asc,
    } = a
    {
        let Condition::CorrelatedSubquery {
            related: br,
            op: bop,
            flip: bf,
            scalar: bsc,
        } = b
        else {
            return Ordering::Less;
        };
        return cmp_related(ar, br)
            .then_with(|| utf8(aop.as_str(), bop.as_str()))
            .then_with(|| cmp_optional_bool(*af, *bf))
            .then_with(|| cmp_optional_bool(*asc, *bsc));
    }
    // Asymmetric, as in JS: a subquery and a conjunction each sort before
    // the other, whichever is on the left.
    if matches!(b, Condition::CorrelatedSubquery { .. }) {
        return Ordering::Less;
    }
    let t = utf8(type_name(a), type_name(b));
    if t.is_ne() {
        return t;
    }
    let (ac, bc) = (
        a.conditions().expect("and/or"),
        b.conditions().expect("and/or"),
    );
    for (l, r) in ac.iter().zip(bc) {
        let v = cmp_condition(l, r);
        if v.is_ne() {
            return v;
        }
    }
    ac.len().cmp(&bc.len())
}

fn value_position_type(v: &ValuePosition) -> &'static str {
    match v {
        ValuePosition::Literal(_) => "literal",
        ValuePosition::Column(_) => "column",
        ValuePosition::Json { .. } => "json",
        ValuePosition::Static { .. } => "static",
    }
}

fn compare_value_position(a: &ValuePosition, b: &ValuePosition) -> Ordering {
    match (a, b) {
        (ValuePosition::Literal(x), ValuePosition::Literal(y)) => cmp_literal_value(x, y),
        (ValuePosition::Column(x), ValuePosition::Column(y)) => utf8(x, y),
        (
            ValuePosition::Json {
                column: ac,
                path: ap,
            },
            ValuePosition::Json {
                column: bc,
                path: bp,
            },
        ) => utf8(ac, bc)
            .then_with(|| cmp_literal_value(&coding::path_to_json(ap), &coding::path_to_json(bp))),
        (
            ValuePosition::Static {
                anchor: aa,
                field: af,
            },
            ValuePosition::Static {
                anchor: ba,
                field: bf,
            },
        ) => utf8(aa.as_str(), ba.as_str())
            .then_with(|| cmp_literal_value(&af.to_json(), &bf.to_json())),
        _ => utf8(value_position_type(a), value_position_type(b)),
    }
}

/// null < boolean < number < string < array.
fn literal_value_type(v: &Value) -> u8 {
    match v {
        Value::Null | Value::Object(_) => 0,
        Value::Bool(_) => 1,
        Value::Number(_) => 2,
        Value::String(_) => 3,
        Value::Array(_) => 4,
    }
}

fn cmp_literal_value(a: &Value, b: &Value) -> Ordering {
    let t = literal_value_type(a).cmp(&literal_value_type(b));
    if t.is_ne() {
        return t;
    }
    match (a, b) {
        (Value::Array(x), Value::Array(y)) => {
            for (l, r) in x.iter().zip(y) {
                let v = cmp_literal_value(l, r);
                if v.is_ne() {
                    return v;
                }
            }
            x.len().cmp(&y.len())
        }
        (Value::String(x), Value::String(y)) => utf8(x, y),
        (Value::Number(x), Value::Number(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        _ => Ordering::Equal,
    }
}

pub(crate) fn cmp_related(a: &CorrelatedSubquery, b: &CorrelatedSubquery) -> Ordering {
    utf8(
        a.subquery.alias.as_deref().unwrap_or(""),
        b.subquery.alias.as_deref().unwrap_or(""),
    )
}

fn cmp_optional_bool(a: Option<bool>, b: Option<bool>) -> Ordering {
    let n = |v: Option<bool>| match v {
        None => 0,
        Some(false) => 1,
        Some(true) => 2,
    };
    n(a).cmp(&n(b))
}
