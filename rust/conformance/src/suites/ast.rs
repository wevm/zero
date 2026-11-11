//! The AST, its normalization, and query hashes: `primitives/ast.test.ts`,
//! `primitives/ast-features.test.ts`, `primitives/hash.test.ts` (the query
//! hashes), and `primitives/sweep-wire.test.ts` (the name-and-args hashes
//! and the hashes of queries the query builder builds).

use crate::case::{Case, ensure, quote, same};
use crate::{Outcome, Source, Suite};
use std::cell::RefCell;
use std::sync::{Arc, OnceLock};
use zero_client::ast::{Ast, Condition, Direction, SimpleOperator, System, strict};
use zero_client::decode::JsonCodable;
use zero_client::format::Format;
use zero_client::hash::query;
use zero_client::json::{self, Value};
use zero_client::query::{AnyQuery, ExistsOptions, ExpressionBuilder};
use zero_client::schema::{NameMapper, Schema};

pub fn suites() -> Vec<Suite> {
    vec![
        Suite {
            file: "ast",
            source: Source::Checked,
            check: ast,
        },
        Suite {
            file: "ast-fuzz",
            source: Source::Generated,
            check: ast,
        },
        Suite {
            file: "ast-features",
            source: Source::Checked,
            check: features,
        },
        Suite {
            file: "query-internals-hash",
            source: Source::Checked,
            check: query_internals_hash,
        },
        Suite {
            file: "name-args-hash",
            source: Source::Checked,
            check: name_args_hash,
        },
        Suite {
            file: "sweep-name-args-hash",
            source: Source::Checked,
            check: sweep_name_args_hash,
        },
        Suite {
            file: "sweep-query-hash",
            source: Source::Checked,
            check: sweep_query_hash,
        },
    ]
}

/// Decoding then encoding is lossless, and the normalized form and the hash
/// are JS's.
fn ast(c: &Value, _: &Value) -> Result<Outcome, String> {
    let json = c.field("ast")?;
    let ast = Ast::from_json(json).map_err(|e| format!("rejected {json}: {e}"))?;
    same(format!("encode {json}"), &ast.to_json(), json)?;
    same(
        format!("normalize {json}"),
        &ast.normalized().to_json(),
        c.field("normalized")?,
    )?;
    same(
        format!("hash {json}"),
        query::hash_of_ast(&ast).as_str(),
        c.str("hash")?,
    )?;
    Ok(Outcome::Pass)
}

/// The JS test schema (`zql/src/query/test/test-schemas.ts`) the
/// `ast-features` and `sweep-query-hash` generators use, as JS wrote it
/// into `ivm.json`'s shared fixtures.
pub(crate) fn test_schema() -> Result<&'static Arc<Schema>, String> {
    static SCHEMA: OnceLock<Result<Arc<Schema>, String>> = OnceLock::new();
    SCHEMA
        .get_or_init(|| {
            let path = crate::vectors_dir().join("ivm.json");
            let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
            let file = json::parse(&text).map_err(|e| e.to_string())?;
            let schema = file
                .get("shared")
                .and_then(|s| s.get("schema"))
                .ok_or("ivm.json has no shared schema")?;
            Schema::from_json(schema)
                .map(Arc::new)
                .map_err(|e| format!("ivm.json's schema: {e}"))
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// Aggregates, groupBy, and JSON paths: the wire schema's verdict, and for
/// the ASTs it accepts, the normalized form, the hash, and the server names
/// or why mapping them fails. Rejections are compared by verdict, not by
/// valita's message.
fn features(c: &Value, file: &Value) -> Result<Outcome, String> {
    let json = c.field("ast")?;
    let decoded = strict::decode(json);
    if !c.bool("valid")? {
        ensure(decoded.is_err(), || format!("accepted {json}"))?;
        return Ok(Outcome::Pass);
    }
    let decoded = decoded.map_err(|e| format!("rejected {json}: {e}"))?;
    ast(c, file)?;
    let mapper = NameMapper::client_to_server(test_schema()?);
    match mapper.map(&decoded) {
        Ok(mapped) => same(
            format!("map {json}"),
            Some(mapped.to_json()),
            c.get("mapped").cloned(),
        )?,
        Err(e) => same(
            format!("map {json}"),
            Some(e.to_string().as_str()),
            c.get("mapError").and_then(Value::as_str),
        )?,
    }
    Ok(Outcome::Pass)
}

fn query_internals_hash(c: &Value, _: &Value) -> Result<Outcome, String> {
    let json = c.field("ast")?;
    let ast = Ast::from_json(json).map_err(|e| format!("rejected {json}: {e}"))?;
    let format = Format::from_json(c.field("format")?).map_err(|e| e.to_string())?;
    same("format", &format.to_json(), c.field("format")?)?;
    let hash = query::hash_of_query_internals(&ast, &format.to_json(), System::Client, None, None);
    same(
        format!("hash {}", c.field("format")?),
        hash.as_str(),
        c.str("hash")?,
    )?;
    Ok(Outcome::Pass)
}

fn name_args_hash(c: &Value, _: &Value) -> Result<Outcome, String> {
    let name = c.str("name")?;
    let args = c.arr("args")?;
    let got = query::hash_of_name_and_args(name, args);
    same(
        format!("{} {}", quote(name), c.field("args")?),
        got.as_str(),
        c.str("hash")?,
    )?;
    Ok(Outcome::Pass)
}

/// Arguments given as JSON text, with keys JS keeps apart.
fn sweep_name_args_hash(c: &Value, _: &Value) -> Result<Outcome, String> {
    let name = c.str("name")?;
    let text = c.str("args")?;
    let args = json::parse(text).map_err(|e| format!("args {}: {e}", quote(text)))?;
    let args = args
        .as_array()
        .ok_or_else(|| format!("args {text} is not an array"))?;
    let got = query::hash_of_name_and_args(name, args);
    same(
        format!("{} {text}", quote(name)),
        got.as_str(),
        c.str("hash")?,
    )?;
    Ok(Outcome::Pass)
}

/// `QueryImpl.hash()` of a query built by a program of builder calls: the
/// AST, the format (`one()` versus `limit(1)`), the system, the junction
/// of a two-hop relationship, and a custom query's name and arguments. JS
/// built every one without throwing, so none may be invalid.
fn sweep_query_hash(c: &Value, _: &Value) -> Result<Outcome, String> {
    let spec = c.field("query")?;
    let system = match spec.get("system").and_then(Value::as_str) {
        None => System::Client,
        Some(s) => System::parse(s).ok_or_else(|| format!("unknown system {s}"))?,
    };
    let root = AnyQuery::new(test_schema()?.clone(), spec.str("table")?, system);
    let error = RefCell::new(None);
    let q = run(root, spec.arr("ops")?, &error);
    if let Some(e) = error.into_inner() {
        return Err(format!("{spec}: {e}"));
    }
    if let Some(m) = q.invalid() {
        return Err(format!("{spec}: Rust made the query invalid: {m}"));
    }
    same(
        format!("AST of {spec}"),
        &q.ast().to_json(),
        c.field("ast")?,
    )?;
    same(format!("hash of {spec}"), q.hash().as_str(), c.str("hash")?)?;
    Ok(Outcome::Pass)
}

type OpError = RefCell<Option<String>>;

/// Records the first malformed op of a vector and leaves the query as is.
fn fail<T>(error: &OpError, message: String) -> Option<T> {
    error.borrow_mut().get_or_insert(message);
    None
}

/// `run` in `sweep-wire.test.ts`: applies the ops in order.
fn run(mut q: AnyQuery, ops: &[Value], error: &OpError) -> AnyQuery {
    for op in ops {
        q = match apply(q.clone(), op, error) {
            Some(next) => next,
            None => return q,
        };
    }
    q
}

fn apply(q: AnyQuery, op: &Value, error: &OpError) -> Option<AnyQuery> {
    let a = op.as_array().map(Vec::as_slice).unwrap_or_default();
    let arg = |i: usize| a.get(i).unwrap_or(&Value::Null);
    let string = |i: usize| match arg(i).as_str() {
        Some(s) => Some(s),
        None => fail(error, format!("op {op}: argument {i} is not a string")),
    };
    let ops = |i: usize| match arg(i).as_array() {
        Some(ops) => Some(ops.as_slice()),
        None => fail(error, format!("op {op}: argument {i} is not a list of ops")),
    };
    Some(match string(0)? {
        "where" => q.where_(string(1)?, operator(string(2)?, error)?, arg(3).clone()),
        "whereExpr" => q.where_expr(|eb| expression(eb, arg(1), error)),
        "whereExists" => {
            let sub = ops(3)?;
            q.where_exists(string(1)?, exists_options(arg(2)), |s| run(s, sub, error))
        }
        "related" => {
            let sub = ops(2)?;
            q.related(string(1)?, |s| run(s, sub, error))
        }
        "one" => q.one(),
        "limit" => match arg(1).as_f64() {
            Some(n) if n.fract() == 0.0 => q.limit(n as i64),
            _ => return fail(error, format!("op {op}: the limit is not an integer")),
        },
        "orderBy" => {
            let direction = match Direction::parse(string(2)?) {
                Some(d) => d,
                None => return fail(error, format!("op {op}: unknown direction")),
            };
            q.order_by(string(1)?, direction)
        }
        "start" => match (arg(1).as_object(), arg(2).as_bool()) {
            (Some(row), Some(inclusive)) => q.start(row.clone(), inclusive),
            _ => return fail(error, format!("op {op}: expected a row and a boolean")),
        },
        "nameAndArgs" => match arg(2).as_array() {
            Some(args) => q.name_and_args(string(1)?, args.clone()),
            None => return fail(error, format!("op {op}: the arguments are not a list")),
        },
        other => return fail(error, format!("unknown op {other}")),
    })
}

fn operator(s: &str, error: &OpError) -> Option<SimpleOperator> {
    SimpleOperator::parse(s).or_else(|| fail(error, format!("unknown operator {s}")))
}

/// `{flip, scalar}`, or null for neither.
fn exists_options(v: &Value) -> ExistsOptions {
    ExistsOptions {
        flip: v.get("flip").and_then(Value::as_bool),
        scalar: v.get("scalar").and_then(Value::as_bool),
    }
}

/// `expr` in `sweep-wire.test.ts`. A malformed expression is recorded and
/// stands in as an empty `and`.
fn expression(eb: &ExpressionBuilder, e: &Value, error: &OpError) -> Condition {
    let list = |key: &str| e.get(key).and_then(Value::as_array);
    let built = (|| {
        if let Some(cmp) = list("cmp") {
            let field = cmp.first()?.as_str()?;
            let op = operator(cmp.get(1)?.as_str()?, error)?;
            return Some(eb.cmp(field, op, cmp.get(2)?.clone()));
        }
        if let Some(lit) = list("cmpLit") {
            let op = operator(lit.get(1)?.as_str()?, error)?;
            return Some(eb.cmp_lit(lit.first()?.clone(), op, lit.get(2)?.clone()));
        }
        if let Some(and) = list("and") {
            return Some(eb.and(and.iter().map(|c| expression(eb, c, error)).collect()));
        }
        if let Some(or) = list("or") {
            return Some(eb.or(or.iter().map(|c| expression(eb, c, error)).collect()));
        }
        if let Some(not) = e.get("not") {
            return Some(eb.not(expression(eb, not, error)));
        }
        let exists = list("exists")?;
        let sub = exists.get(2)?.as_array()?;
        let options = exists_options(exists.get(1)?);
        Some(eb.exists(exists.first()?.as_str()?, options, |s| run(s, sub, error)))
    })();
    built.unwrap_or_else(|| {
        fail::<()>(error, format!("malformed expression {e}"));
        Condition::And(Vec::new())
    })
}
