//! `JSON.parse`, `JSON.stringify`, and JS number formatting and parsing:
//! `primitives/json.test.ts`, `primitives/harden-json.test.ts` and
//! `primitives/sweep-wire.test.ts`.

use crate::case::{Case, ensure, quote};
use crate::{Outcome, Source, Suite};
use zero_client::json::{self, Value, number};

/// The exception for JSON text with an escaped lone surrogate.
pub const LONE_SURROGATE: &str =
    "lone surrogate: a Rust String can't hold one, so `\\ud800` parses as U+FFFD";

/// The exception for JSON nested deeper than `json::MAX_DEPTH`.
pub const TOO_DEEP: &str = "nesting: JSON deeper than json::MAX_DEPTH is rejected";

pub fn suites() -> Vec<Suite> {
    vec![
        Suite {
            file: "number-format",
            source: Source::Checked,
            check: number_format,
        },
        Suite {
            file: "number-parse",
            source: Source::Checked,
            check: number_parse,
        },
        Suite {
            file: "json-parse",
            source: Source::Checked,
            check: parse_and_stringify,
        },
        Suite {
            file: "sweep-json",
            source: Source::Checked,
            check: parse_and_stringify,
        },
        Suite {
            file: "harden-json",
            source: Source::Checked,
            check: parse_and_stringify,
        },
        Suite {
            file: "harden-json-fuzz",
            source: Source::Generated,
            check: parse_and_stringify,
        },
        Suite {
            file: "json-stringify-strings",
            source: Source::Checked,
            check: stringify_strings,
        },
    ]
}

pub fn double_from_bits(hex: &str) -> Result<f64, String> {
    u64::from_str_radix(hex, 16)
        .map(f64::from_bits)
        .map_err(|e| format!("bad bits {hex}: {e}"))
}

fn number_format(c: &Value, _: &Value) -> Result<Outcome, String> {
    let bits = c.str("bits")?;
    let got = number::format(double_from_bits(bits)?);
    let want = c.str("string")?;
    ensure(got == want, || {
        format!("bits {bits}: Rust {got}, JS {want}")
    })?;
    Ok(Outcome::Pass)
}

fn number_parse(c: &Value, _: &Value) -> Result<Outcome, String> {
    let input = c.str("input")?;
    let got = number::parse(input);
    let want = double_from_bits(c.str("bits")?)?;
    ensure(
        got.to_bits() == want.to_bits() || (got.is_nan() && want.is_nan()),
        || format!("Number({}): Rust {got:?}, JS {want:?}", quote(input)),
    )?;
    // Any NaN is NaN in JS; the vectors don't pin one bit pattern.
    Ok(Outcome::Pass)
}

/// `JSON.parse` acceptance, and `JSON.stringify` of what it parsed, keys in
/// JS property order, compared as text.
fn parse_and_stringify(c: &Value, _: &Value) -> Result<Outcome, String> {
    let input = c.str("input")?;
    let ok = c.bool("ok")?;
    let value = match json::parse(input) {
        Ok(value) => value,
        Err(e) if ok && e.message == json::TOO_DEEP => return Ok(Outcome::Exception(TOO_DEEP)),
        Err(e) => {
            ensure(!ok, || format!("rejected {}: {e}", quote(input)))?;
            return Ok(Outcome::Pass);
        }
    };
    ensure(ok, || format!("accepted {}", quote(input)))?;
    let Some(want) = c.get("stringify").and_then(Value::as_str) else {
        return Ok(Outcome::Pass);
    };
    let got = json::stringify(&value);
    if got == want {
        return Ok(Outcome::Pass);
    }
    let replaced = replacing_lone_surrogate_escapes(want);
    ensure(replaced.as_deref() == Some(got.as_str()), || {
        format!("{}: Rust {}, JS {}", quote(input), quote(&got), quote(want))
    })?;
    Ok(Outcome::Exception(LONE_SURROGATE))
}

/// `JSON.stringify` output with each escaped lone surrogate (`\ud800`) as the
/// U+FFFD the parser reads it as, or `None` when it has none.
pub fn replacing_lone_surrogate_escapes(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut replaced = false;
    let mut i = 0;
    let mut copied = 0;
    while i < b.len() {
        if b[i] != b'\\' || i + 1 >= b.len() {
            i += 1;
            continue;
        }
        if b[i + 1] == b'u'
            && i + 6 <= b.len()
            && let Ok(unit) = u16::from_str_radix(&s[i + 2..i + 6], 16)
            && (0xD800..=0xDFFF).contains(&unit)
        {
            out.push_str(&s[copied..i]);
            out.push('\u{FFFD}');
            replaced = true;
            i += 6;
            copied = i;
        } else {
            i += 2;
        }
    }
    out.push_str(&s[copied..]);
    replaced.then_some(out)
}

fn stringify_strings(c: &Value, _: &Value) -> Result<Outcome, String> {
    let input = c.str("input")?;
    let got = json::stringify(&Value::String(input.to_owned()));
    let want = c.str("stringify")?;
    ensure(got == want, || {
        format!("{}: Rust {got}, JS {want}", quote(input))
    })?;
    Ok(Outcome::Pass)
}
