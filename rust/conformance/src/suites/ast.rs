//! The AST, its normalization, and query hashes: `primitives/ast.test.ts`,
//! `primitives/ast-features.test.ts`, `primitives/hash.test.ts` (the query
//! hashes), and `primitives/sweep-wire.test.ts` (the name-and-args hashes).

use crate::case::{Case, ensure, quote, same};
use crate::{Outcome, Source, Suite};
use std::sync::OnceLock;
use zero_client::ast::{Ast, System, strict};
use zero_client::decode::JsonCodable;
use zero_client::format::Format;
use zero_client::hash::query;
use zero_client::json::{self, Value};
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
/// `ast-features` generator maps names with, as JS wrote it into
/// `ivm.json`'s shared fixtures.
fn test_schema() -> Result<&'static Schema, String> {
    static SCHEMA: OnceLock<Result<Schema, String>> = OnceLock::new();
    SCHEMA
        .get_or_init(|| {
            let path = crate::vectors_dir().join("ivm.json");
            let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
            let file = json::parse(&text).map_err(|e| e.to_string())?;
            let schema = file
                .get("shared")
                .and_then(|s| s.get("schema"))
                .ok_or("ivm.json has no shared schema")?;
            Schema::from_json(schema).map_err(|e| format!("ivm.json's schema: {e}"))
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
