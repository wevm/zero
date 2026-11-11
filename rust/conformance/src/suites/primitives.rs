//! String orders and hashes: `primitives/compare.test.ts` and
//! `primitives/hash.test.ts`.

use crate::case::{Case, quote, same};
use crate::{Outcome, Source, Suite};
use std::cmp::Ordering;
use zero_client::hash::xxhash32;
use zero_client::json::Value;
use zero_client::strings;

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
