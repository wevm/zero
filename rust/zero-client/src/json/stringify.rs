use super::{Value, number};

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
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) if n.is_finite() => out.push_str(&number::format(*n)),
        Value::Number(_) => out.push_str("null"),
        Value::String(s) => write_string(s, out),
        Value::Array(a) => {
            out.push('[');
            for (i, e) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write(e, out);
            }
            out.push(']');
        }
        Value::Object(o) => {
            out.push('{');
            for (i, (k, v)) in o.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(k, out);
                out.push(':');
                write(v, out);
            }
            out.push('}');
        }
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
