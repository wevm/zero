//! String orders, hashes, and the t-digest: `primitives/compare.test.ts`,
//! `primitives/hash.test.ts`, and `primitives/tdigest.test.ts`.

use crate::case::{Case, quote, same};
use crate::{Outcome, Source, Suite};
use std::cmp::Ordering;
use zero_client::hash::xxhash32;
use zero_client::json::Value;
use zero_client::strings;
use zero_client::tdigest::TDigest;

pub fn suites() -> Vec<Suite> {
    vec![
        Suite {
            variant: None,
            file: "string-compare",
            source: Source::Checked,
            check: string_compare,
        },
        Suite {
            variant: None,
            file: "hash",
            source: Source::Checked,
            check: hash,
        },
        Suite {
            variant: None,
            file: "tdigest",
            source: Source::Checked,
            check: tdigest,
        },
    ]
}

pub fn sign(o: Ordering) -> f64 {
    match o {
        Ordering::Less => -1.0,
        Ordering::Equal => 0.0,
        Ordering::Greater => 1.0,
    }
}

fn string_compare(c: &Value, _: &Value) -> Result<Outcome, String> {
    let (a, b) = (c.str("a")?, c.str("b")?);
    let what = format!("{} vs {}", quote(a), quote(b));
    same(
        format!("utf8 {what}"),
        sign(strings::utf8(a, b)),
        c.num("utf8")?,
    )?;
    same(
        format!("utf16 {what}"),
        sign(strings::utf16(a, b)),
        c.num("utf16")?,
    )?;
    Ok(Outcome::Pass)
}

fn hash(c: &Value, _: &Value) -> Result<Outcome, String> {
    let input = c.str("input")?;
    let q = quote(input);
    same(
        format!("h32({q})"),
        f64::from(xxhash32::h32(input)),
        c.num("h32")?,
    )?;
    same(
        format!("h64({q})"),
        xxhash32::h64(input).to_string().as_str(),
        c.str("h64")?,
    )?;
    same(
        format!("h128({q})"),
        xxhash32::h128(input).to_string().as_str(),
        c.str("h128")?,
    )?;
    Ok(Outcome::Pass)
}

/// A vector number: NaN and the infinities are strings.
fn num(v: &Value) -> Result<f64, String> {
    match v {
        Value::Number(n) => Ok(*n),
        Value::String(s) if s == "NaN" => Ok(f64::NAN),
        Value::String(s) if s == "Infinity" => Ok(f64::INFINITY),
        Value::String(s) if s == "-Infinity" => Ok(f64::NEG_INFINITY),
        _ => Err(format!("not a vector number: {v}")),
    }
}

fn enc(n: f64) -> Value {
    if n.is_nan() {
        Value::from("NaN")
    } else if n.is_infinite() {
        Value::from(if n > 0.0 { "Infinity" } else { "-Infinity" })
    } else {
        Value::Number(n)
    }
}

/// Numbers compare by bits but for the sign of zero, which JSON drops.
fn same_numbers(got: &Value, want: &Value) -> bool {
    match (got, want) {
        (Value::Number(a), Value::Number(b)) => a == b,
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_numbers(a, b))
        }
        (a, b) => a == b,
    }
}

fn tdigest(c: &Value, _: &Value) -> Result<Outcome, String> {
    let mut digests: Vec<TDigest> = c
        .arr("compressions")?
        .iter()
        .map(|n| TDigest::new(n.as_f64().unwrap_or(100.0)))
        .collect();
    let expect = c.arr("expect")?;
    for (i, op) in c.arr("ops")?.iter().enumerate() {
        let d = op.num("d")? as usize;
        let got = match op.str("op")? {
            "add" => {
                for pair in op.arr("values")? {
                    let pair = pair.as_array().ok_or("a value pair")?;
                    digests[d].add(num(&pair[0])?, num(&pair[1])?);
                }
                Value::Null
            }
            "merge" => {
                let from = op.num("from")? as usize;
                // JS merges a digest into itself by processing it, then
                // adding its centroids: reading its count processes it.
                digests[from].count();
                let from = digests[from].clone();
                digests[d].merge(&from);
                Value::Null
            }
            "quantile" => Value::Array(
                op.arr("q")?
                    .iter()
                    .map(|q| num(q).map(|q| enc(digests[d].quantile(q))))
                    .collect::<Result<_, _>>()?,
            ),
            "cdf" => Value::Array(
                op.arr("x")?
                    .iter()
                    .map(|x| num(x).map(|x| enc(digests[d].cdf(x))))
                    .collect::<Result<_, _>>()?,
            ),
            "count" => enc(digests[d].count()),
            "toJSON" => Value::Array(digests[d].to_vec().into_iter().map(enc).collect()),
            "fromJSON" => match TDigest::from_json_value(op.field("json")?) {
                Ok(t) => {
                    digests[d] = t;
                    Value::from("ok")
                }
                Err(e) => {
                    let mut o = zero_client::json::Object::new();
                    o.insert("threw", Value::String(e));
                    Value::Object(o)
                }
            },
            "reset" => {
                digests[d].reset();
                Value::Null
            }
            other => return Err(format!("unknown op {other}")),
        };
        let want = expect.get(i).unwrap_or(&Value::Null);
        if !same_numbers(&got, want) {
            return Err(format!(
                "{} op {i} ({}): Rust {got}, JS {want}",
                c.str("name")?,
                op.str("op")?
            ));
        }
    }
    Ok(Outcome::Pass)
}
