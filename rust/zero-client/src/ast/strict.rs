//! `strictASTSchema` (`packages/zero-protocol/src/ast.ts`), for ASTs a client
//! sends: nested at most [`MAX_DEPTH`] deep, and parsed in valita's `strict`
//! mode even inside a lenient parse, so an AST with a key zero doesn't know
//! is rejected rather than run as a different query.

use super::Ast;
use crate::decode::{DecodeError, JsonCodable, Result};
use crate::json::{Object, Value};

/// `MAX_AST_DEPTH`.
pub const MAX_DEPTH: usize = 50;

pub fn decode(v: &Value) -> Result<Ast> {
    if exceeds_depth(v, MAX_DEPTH) {
        return Err(DecodeError::new(format!(
            "AST exceeds the maximum depth of {MAX_DEPTH}"
        )));
    }
    check_ast(v)?;
    Ast::from_json(v)
}

/// `isPlainObject` (`packages/shared/src/deep-merge.ts`) is true for arrays
/// too; they have none of the keys looked at.
fn plain_object(v: Option<&Value>) -> Option<Option<&Object>> {
    match v {
        Some(Value::Object(o)) => Some(Some(o)),
        Some(Value::Array(_)) => Some(None),
        _ => None,
    }
}

/// `exceedsASTDepth`: every step down counts, except the one from an AST
/// into its where clause.
pub fn exceeds_depth(ast: &Value, max_depth: usize) -> bool {
    ast_exceeds(Some(ast), 0, max_depth)
}

fn ast_exceeds(ast: Option<&Value>, depth: usize, max: usize) -> bool {
    if depth > max {
        return true;
    }
    let Some(o) = plain_object(ast) else {
        return false;
    };
    let Some(o) = o else { return false };
    if condition_exceeds(o.get("where"), depth, max) {
        return true;
    }
    let Some(Value::Array(related)) = o.get("related") else {
        return false;
    };
    related.iter().any(|r| match plain_object(Some(r)) {
        Some(Some(r)) => ast_exceeds(r.get("subquery"), depth + 1, max),
        _ => false,
    })
}

fn condition_exceeds(cond: Option<&Value>, depth: usize, max: usize) -> bool {
    if depth > max {
        return true;
    }
    let Some(Some(o)) = plain_object(cond) else {
        return false;
    };
    if let Some(Value::Array(conditions)) = o.get("conditions")
        && conditions
            .iter()
            .any(|c| condition_exceeds(Some(c), depth + 1, max))
    {
        return true;
    }
    match plain_object(o.get("related")) {
        Some(Some(r)) => ast_exceeds(r.get("subquery"), depth + 1, max),
        _ => false,
    }
}

// The keys each object of the AST may have. `Ast::from_json` checks the
// values; this only rejects the keys `strict` mode would.

fn only(o: &Object, keys: &[&str]) -> Result<()> {
    match o.keys().find(|k| !keys.contains(&k.as_str())) {
        Some(k) => Err(DecodeError::new("Unexpected key").at(k.clone())),
        None => Ok(()),
    }
}

fn check_ast(v: &Value) -> Result<()> {
    let Value::Object(o) = v else { return Ok(()) };
    only(
        o,
        &[
            "schema",
            "table",
            "alias",
            "where",
            "related",
            "limit",
            "orderBy",
            "start",
            "aggregate",
            "groupBy",
        ],
    )?;
    if let Some(a) = o.get("aggregate") {
        check_aggregate(a)?;
    }
    if let Some(w) = o.get("where") {
        check_condition(w)?;
    }
    if let Some(Value::Array(related)) = o.get("related") {
        for r in related {
            check_correlated_subquery(r)?;
        }
    }
    if let Some(Value::Object(start)) = o.get("start") {
        only(start, &["row", "exclusive"])?;
    }
    Ok(())
}

fn check_correlated_subquery(v: &Value) -> Result<()> {
    let Value::Object(o) = v else { return Ok(()) };
    only(
        o,
        &["correlation", "hidden", "system", "subquery", "aggregate"],
    )?;
    if let Some(a) = o.get("aggregate") {
        check_aggregate(a)?;
    }
    if let Some(Value::Object(c)) = o.get("correlation") {
        only(c, &["parentField", "childField"])?;
    }
    if let Some(s) = o.get("subquery") {
        check_ast(s)?;
    }
    Ok(())
}

fn check_aggregate(v: &Value) -> Result<()> {
    match v {
        Value::Object(o) => only(o, &["fn", "field", "path", "distinct"]),
        _ => Ok(()),
    }
}

fn check_condition(v: &Value) -> Result<()> {
    let Value::Object(o) = v else { return Ok(()) };
    match o.get("type").and_then(Value::as_str) {
        Some("simple") => {
            only(o, &["type", "op", "left", "right"])?;
            if let Some(l) = o.get("left") {
                check_value_position(l)?;
            }
            if let Some(r) = o.get("right") {
                check_value_position(r)?;
            }
        }
        Some("and" | "or") => {
            only(o, &["type", "conditions"])?;
            if let Some(Value::Array(conditions)) = o.get("conditions") {
                for c in conditions {
                    check_condition(c)?;
                }
            }
        }
        Some("correlatedSubquery") => {
            only(o, &["type", "related", "op", "flip", "scalar"])?;
            if let Some(r) = o.get("related") {
                check_correlated_subquery(r)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn check_value_position(v: &Value) -> Result<()> {
    let Value::Object(o) = v else { return Ok(()) };
    match o.get("type").and_then(Value::as_str) {
        Some("literal") => only(o, &["type", "value"]),
        Some("column") => only(o, &["type", "name"]),
        Some("json") => {
            only(o, &["type", "value", "path"])?;
            match o.get("value") {
                Some(c) => check_value_position(c),
                None => Ok(()),
            }
        }
        Some("static") => only(o, &["type", "anchor", "field"]),
        _ => Ok(()),
    }
}
