use super::value::RECURSION_DEPTH;
use super::{Value, number};
use crate::map;

/// Encodes a value as `JSON.stringify` does: the same string escaping and
/// number formatting, with non-finite numbers written as `null`.
///
/// Object keys are written in the object's order, which is JS property order,
/// so the output is what `JSON.stringify` writes for the same object.
pub fn stringify(value: &Value) -> String {
    let mut out = String::new();
    write(value, &mut out);
    out
}

/// Appends `value` as JSON text.
pub fn write(value: &Value, out: &mut String) {
    write_within(value, out, RECURSION_DEPTH);
}

/// [`write`], recursing `depth` levels deep.
fn write_within(value: &Value, out: &mut String, depth: usize) {
    match value {
        Value::Array(a) if depth > 0 => {
            out.push('[');
            for (i, e) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_within(e, out, depth - 1);
            }
            out.push(']');
        }
        Value::Object(o) if depth > 0 => {
            out.push('{');
            for (i, (k, v)) in o.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(k, out);
                out.push(':');
                write_within(v, out, depth - 1);
            }
            out.push('}');
        }
        Value::Array(_) | Value::Object(_) => write_iteratively(value, out),
        scalar => write_scalar(scalar, out),
    }
}

fn write_iteratively(value: &Value, out: &mut String) {
    enum Open<'a> {
        Array(std::slice::Iter<'a, Value>),
        Object(map::Iter<'a, Value>),
    }
    // The arrays and objects being written, each with whether it has
    // written an element yet.
    let mut stack: Vec<(Open, bool)> = Vec::new();
    let mut next = Some(value);
    loop {
        match next.take() {
            Some(Value::Array(a)) => {
                out.push('[');
                stack.push((Open::Array(a.iter()), false));
            }
            Some(Value::Object(o)) => {
                out.push('{');
                stack.push((Open::Object(o.iter()), false));
            }
            Some(scalar) => write_scalar(scalar, out),
            None => {}
        }
        let Some((open, started)) = stack.last_mut() else {
            return;
        };
        let element = match open {
            Open::Array(rest) => rest.next().map(|v| (None, v)),
            Open::Object(rest) => rest.next().map(|(k, v)| (Some(k), v)),
        };
        match element {
            Some((key, v)) => {
                if *started {
                    out.push(',');
                }
                *started = true;
                if let Some(key) = key {
                    write_string(key, out);
                    out.push(':');
                }
                next = Some(v);
            }
            None => {
                out.push(if matches!(open, Open::Array(_)) {
                    ']'
                } else {
                    '}'
                });
                stack.pop();
            }
        }
    }
}

fn write_scalar(value: &Value, out: &mut String) {
    match value {
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) if n.is_finite() => out.push_str(&number::format(*n)),
        Value::String(s) => write_string(s, out),
        // Null, the non-finite numbers, and (never passed here) containers.
        _ => out.push_str("null"),
    }
}

/// Appends `s` as a JSON string literal.
pub fn write_string(s: &str, out: &mut String) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.push('"');
    if !s.bytes().any(|b| b < 0x20 || b == b'"' || b == b'\\') {
        out.push_str(s);
        out.push('"');
        return;
    }
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\0'..='\u{1f}' => {
                out.push_str("\\u00");
                out.push(HEX[c as usize >> 4] as char);
                out.push(HEX[c as usize & 0xF] as char);
            }
            _ => out.push(c),
        }
    }
    out.push('"');
}
