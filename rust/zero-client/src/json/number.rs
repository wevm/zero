//! JS number semantics the client has to reproduce exactly:
//! `Number.prototype.toString` (also what `JSON.stringify` writes) and
//! `Number(string)`.

/// `String(n)` in JS, i.e. ECMAScript `Number::toString(n, 10)`.
///
/// Uses the shortest round-tripping digits (which Rust's `{:e}` also
/// produces) laid out with the JS rules: plain notation for decimal
/// exponents in `-6..21`, exponential otherwise.
pub fn format(value: f64) -> String {
    if value.is_nan() {
        return "NaN".into();
    }
    if value == 0.0 {
        return "0".into();
    }
    if value.is_infinite() {
        return if value < 0.0 { "-Infinity" } else { "Infinity" }.into();
    }
    if value < 0.0 {
        return format!("-{}", format(-value));
    }
    // Integers below 2^53 print exactly; skip the digit extraction.
    if value < 9_007_199_254_740_992.0 && value == value.trunc() {
        return (value as i64).to_string();
    }
    let (digits, n) = shortest_digits(value);
    let k = digits.len() as i32;
    if k <= n && n <= 21 {
        return digits + &"0".repeat((n - k) as usize);
    }
    if 0 < n && n <= 21 {
        let (int, frac) = digits.split_at(n as usize);
        return format!("{int}.{frac}");
    }
    if -6 < n && n <= 0 {
        return format!("0.{}{digits}", "0".repeat(-n as usize));
    }
    let e = n - 1;
    let sign = if e >= 0 { '+' } else { '-' };
    if k == 1 {
        format!("{digits}e{sign}{}", e.abs())
    } else {
        format!("{}.{}e{sign}{}", &digits[..1], &digits[1..], e.abs())
    }
}

/// The shortest digit string `s` and exponent `n` with
/// `value == 0.s × 10^n`, for a positive finite `value`.
fn shortest_digits(value: f64) -> (String, i32) {
    let text = format!("{value:e}"); // e.g. "1.2345e20", "1e-7"
    let (mantissa, exponent) = text.split_once('e').expect("exponent");
    let exponent: i32 = exponent.parse().expect("exponent");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let digits = digits.trim_end_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    (digits.to_owned(), exponent + 1)
}

/// `Number(string)` in JS (ECMAScript `StringToNumber`): trims JS
/// whitespace, maps the empty string to 0, and accepts decimal, `Infinity`,
/// and `0x`/`0o`/`0b` literals. Anything else is `NaN`.
pub fn parse(string: &str) -> f64 {
    let s = string.trim_matches(is_js_whitespace);
    if s.is_empty() {
        return 0.0;
    }
    match s {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    let prefix = s.get(..2).map(str::to_ascii_lowercase);
    let radix = match prefix.as_deref() {
        Some("0x") => 16,
        Some("0o") => 8,
        Some("0b") => 2,
        _ => 0,
    };
    if radix != 0 {
        let body = &s[2..];
        if body.is_empty() {
            return f64::NAN;
        }
        let mut result = 0.0;
        for c in body.chars() {
            match c.to_digit(radix) {
                Some(d) => result = result * f64::from(radix) + f64::from(d),
                None => return f64::NAN,
            }
        }
        return result;
    }
    if !is_decimal_literal(s) {
        return f64::NAN;
    }
    s.parse().unwrap_or(f64::NAN)
}

fn is_decimal_literal(s: &str) -> bool {
    let mut b = s.as_bytes();
    let digits = |b: &mut &[u8]| {
        let n = b.iter().take_while(|c| c.is_ascii_digit()).count();
        *b = &b[n..];
        n
    };
    if let [b'+' | b'-', rest @ ..] = b {
        b = rest;
    }
    let int = digits(&mut b);
    let mut frac = 0;
    if let [b'.', rest @ ..] = b {
        b = rest;
        frac = digits(&mut b);
    }
    if int == 0 && frac == 0 {
        return false;
    }
    if let [b'e' | b'E', rest @ ..] = b {
        b = rest;
        if let [b'+' | b'-', rest @ ..] = b {
            b = rest;
        }
        if digits(&mut b) == 0 {
            return false;
        }
    }
    b.is_empty()
}

/// ECMAScript `WhiteSpace` plus `LineTerminator`.
pub fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{9}'..='\u{d}'
            | ' '
            | '\u{a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
            | '\u{feff}'
    )
}

/// `(n | 0) === n` in JS: whether `n` is exactly an int32. `-0` counts,
/// because `-0 === 0`.
pub fn is_int32(n: f64) -> bool {
    (-2_147_483_648.0..=2_147_483_647.0).contains(&n) && n == n.trunc()
}

/// `Number.isSafeInteger(n)` in JS.
pub fn is_safe_integer(n: f64) -> bool {
    n.abs() <= 9_007_199_254_740_991.0 && n == n.trunc()
}

#[cfg(test)]
mod tests {
    #[test]
    fn formats_like_js() {
        for (n, s) in [
            (1e21, "1e+21"),
            (1e20, "100000000000000000000"),
            (0.000001, "0.000001"),
            (1e-7, "1e-7"),
            (1.5e-7, "1.5e-7"),
            (123.456, "123.456"),
            (-0.0, "0"),
            (2f64.powi(53), "9007199254740992"),
        ] {
            assert_eq!(super::format(n), s);
        }
    }
}
