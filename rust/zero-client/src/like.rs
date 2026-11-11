//! `LIKE` and `ILIKE` (`packages/zql/src/builder/like.ts`).
//!
//! JS compiles a pattern without wildcards to a (lowercased) string
//! comparison, and anything else to a non-Unicode `RegExp` with the `s` flag.
//! That regexp works on UTF-16 code units: `_` matches one code unit (half of
//! a surrogate pair), and `i` folds case per code unit through simple
//! uppercasing. This reproduces both paths.

use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidPattern(pub &'static str);

impl fmt::Display for InvalidPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for InvalidPattern {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Token {
    /// `%`
    Any,
    /// `_`
    One,
    Unit(u16),
}

#[derive(Clone, Debug)]
enum Matcher {
    Exact(String),
    Lowercased(String),
    Wildcard {
        tokens: Vec<Token>,
        case_insensitive: bool,
    },
}

/// A compiled `LIKE` or `ILIKE` pattern.
#[derive(Clone, Debug)]
pub struct LikePattern(Matcher);

impl LikePattern {
    pub fn new(pattern: &str, case_insensitive: bool) -> Result<Self, InvalidPattern> {
        if !pattern.contains(['_', '%', '\\']) {
            return Ok(LikePattern(if case_insensitive {
                Matcher::Lowercased(pattern.to_lowercase())
            } else {
                Matcher::Exact(pattern.to_owned())
            }));
        }
        let fold = |u: u16| if case_insensitive { canonicalize(u) } else { u };
        let units: Vec<u16> = pattern.encode_utf16().collect();
        let mut tokens = Vec::with_capacity(units.len());
        let mut i = 0;
        while i < units.len() {
            tokens.push(match units[i] {
                0x25 => Token::Any,
                0x5F => Token::One,
                0x5C => {
                    i += 1;
                    let Some(&u) = units.get(i) else {
                        return Err(InvalidPattern(
                            "LIKE pattern must not end with escape character",
                        ));
                    };
                    Token::Unit(fold(u))
                }
                u => Token::Unit(fold(u)),
            });
            i += 1;
        }
        Ok(LikePattern(Matcher::Wildcard {
            tokens,
            case_insensitive,
        }))
    }

    pub fn matches(&self, input: &str) -> bool {
        match &self.0 {
            Matcher::Exact(p) => input == p,
            Matcher::Lowercased(p) => input.to_lowercase() == *p,
            Matcher::Wildcard {
                tokens,
                case_insensitive,
            } => {
                let units: Vec<u16> = if *case_insensitive {
                    input.encode_utf16().map(canonicalize).collect()
                } else {
                    input.encode_utf16().collect()
                };
                wildcard_match(tokens, &units)
            }
        }
    }
}

/// Full-string wildcard match with backtracking to the last `%`.
fn wildcard_match(p: &[Token], s: &[u16]) -> bool {
    let (mut pi, mut si) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while si < s.len() {
        match p.get(pi) {
            Some(Token::Any) => {
                star = Some((pi, si));
                pi += 1;
                continue;
            }
            Some(Token::One) => {
                pi += 1;
                si += 1;
                continue;
            }
            Some(Token::Unit(u)) if *u == s[si] => {
                pi += 1;
                si += 1;
                continue;
            }
            _ => {}
        }
        let Some((sp, ss)) = star else { return false };
        pi = sp + 1;
        si = ss + 1;
        star = Some((sp, ss + 1));
    }
    p[pi..].iter().all(|t| *t == Token::Any)
}

/// `Canonicalize` for a non-Unicode, ignore-case `RegExp`: uppercase the code
/// unit if that yields exactly one code unit, unless it would map a non-ASCII
/// unit to ASCII.
fn canonicalize(unit: u16) -> u16 {
    let Some(c) = char::from_u32(unit.into()) else {
        return unit; // a lone surrogate
    };
    let mut upper = c.to_uppercase();
    let (Some(u), None) = (upper.next(), upper.next()) else {
        return unit;
    };
    match u16::try_from(u32::from(u)) {
        Ok(cu) if !(unit >= 128 && cu < 128) => cu,
        _ => unit,
    }
}

/// Escapes the wildcards of `value` (`%` and `_`) for a `LIKE` pattern
/// (`packages/zql/src/query/escape-like.ts`).
pub fn escape_like(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if c == '%' || c == '_' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}
