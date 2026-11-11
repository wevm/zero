//! Row values and keys: `primitives/compare.test.ts`, `primitives/keys.test.ts`,
//! `primitives/like.test.ts` and `primitives/ttl.test.ts`.

use super::primitives::sign;
use crate::case::{Case, ensure, quote, same};
use crate::{Outcome, Source, Suite};
use zero_client::json::Value;
use zero_client::like::LikePattern;
use zero_client::ttl::Ttl;
use zero_client::{compare, keys};

pub fn suites() -> Vec<Suite> {
    vec![
        Suite {
            variant: None,
            file: "compare-values",
            source: Source::Checked,
            check: compare_values,
        },
        Suite {
            variant: None,
            file: "primary-key-string",
            source: Source::Checked,
            check: primary_key_string,
        },
        Suite {
            variant: None,
            file: "aggregate-row-key",
            source: Source::Checked,
            check: aggregate_row_key,
        },
        Suite {
            variant: None,
            file: "like",
            source: Source::Checked,
            check: like,
        },
        Suite {
            variant: None,
            file: "ttl",
            source: Source::Checked,
            check: ttl,
        },
    ]
}

/// The strict `compareValues`, not the pipelines' fallback order.
fn compare_values(c: &Value, _: &Value) -> Result<Outcome, String> {
    let (a, b) = (c.field("a")?, c.field("b")?);
    let got = compare::compare_checked(a, b);
    match (got, c.field("compare")?) {
        (Ok(o), Value::Number(want)) => same(format!("compare({a}, {b})"), sign(o), *want)?,
        (Err(_), Value::String(s)) if s == "throws" => {}
        (got, want) => return Err(format!("compare({a}, {b}): Rust {got:?}, JS {want}")),
    }
    same(
        format!("equal({a}, {b})"),
        compare::equal(a, b),
        c.bool("equal")?,
    )?;
    Ok(Outcome::Pass)
}

fn strings(v: &[Value]) -> Result<Vec<String>, String> {
    v.iter()
        .map(|s| {
            s.as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{s} is not a string"))
        })
        .collect()
}

fn primary_key_string(c: &Value, _: &Value) -> Result<Outcome, String> {
    let table = c.str("table")?;
    let pk = strings(c.arr("primaryKey")?)?;
    let row = c.obj("row")?;
    let got = keys::primary_key_string(table, &pk, row).map_err(|e| e.to_string())?;
    same(
        format!("{table} {pk:?} {}", c.field("row")?),
        got.as_str(),
        c.str("key")?,
    )?;
    Ok(Outcome::Pass)
}

fn aggregate_row_key(c: &Value, _: &Value) -> Result<Outcome, String> {
    let table = c.str("table")?;
    let row = c.obj("row")?;
    let what = format!("{table} {}", c.field("row")?);
    let columns = keys::aggregate_row_key_columns(table, row.keys());
    same(
        format!("columns of {what}"),
        columns,
        strings(c.arr("columns")?)?,
    )?;
    let key = keys::aggregate_row_key(table, row).ok();
    let throws = c.get("throws").and_then(Value::as_bool).unwrap_or(false);
    same(format!("key of {what}"), key.is_none(), throws)?;
    if let Some(key) = key {
        same(format!("key of {what}"), key.as_str(), c.str("key")?)?;
    }
    Ok(Outcome::Pass)
}

fn like(c: &Value, _: &Value) -> Result<Outcome, String> {
    let pattern = c.str("pattern")?;
    let ci = c.str("flags")? == "i";
    let throws = c.bool("throws")?;
    let like = match LikePattern::new(pattern, ci) {
        Ok(like) => like,
        Err(e) => {
            ensure(throws, || {
                format!("pattern {} rejected: {e}", quote(pattern))
            })?;
            return Ok(Outcome::Pass);
        }
    };
    ensure(!throws, || format!("pattern {} accepted", quote(pattern)))?;
    for r in c.arr("results")? {
        let input = r.str("input")?;
        let op = if ci { "ILIKE" } else { "LIKE" };
        same(
            format!("{} {op} {}", quote(input), quote(pattern)),
            like.matches(input),
            r.bool("match")?,
        )?;
    }
    Ok(Outcome::Pass)
}

fn ttl(c: &Value, file: &Value) -> Result<Outcome, String> {
    let parse = |c: &Value| -> Result<Ttl, String> {
        let v = c.field("ttl")?;
        Ttl::from_json(v).ok_or_else(|| format!("bad TTL {v}"))
    };
    let ttl = parse(c)?;
    let what = c.field("ttl")?;
    same(format!("parseTTL({what})"), ttl.parsed(), c.num("parsed")?)?;
    same(
        format!("normalizeTTL({what})"),
        ttl.normalized().to_json(),
        c.field("normalized")?.clone(),
    )?;
    same(
        format!("clampTTL({what})"),
        ttl.clamped(),
        c.num("clamped")?,
    )?;
    let others = file.arr("cases")?;
    for (other, want) in others.iter().zip(c.arr("compare")?) {
        let got = Ttl::compare(&ttl, &parse(other)?);
        let got = if got > 0.0 {
            1.0
        } else if got < 0.0 {
            -1.0
        } else {
            0.0
        };
        same(
            format!("compareTTL({what}, {})", other.field("ttl")?),
            Value::Number(got),
            want.clone(),
        )?;
    }
    Ok(Outcome::Pass)
}
