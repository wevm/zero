use super::value::{ObjectBuilder, RecentKeys};
use super::{Object, Value};
use std::fmt;

/// Why [`parse`] rejected its input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub message: &'static str,
    /// The byte offset of the problem.
    pub offset: usize,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at offset {}", self.message, self.offset)
    }
}

impl std::error::Error for ParseError {}

/// Nesting deeper than this is rejected with [`TOO_DEEP`].
///
/// It is above the about 4,100 levels zero-cache can write, so the client
/// takes any value the server sends. JS has no limit short of its own
/// stack; see `rust/PARITY.md`. Parsing, cloning, comparing, and writing a
/// value don't recurse that deep, and dropping one fits in a 2 MB thread
/// stack (the default for spawned Rust threads) even in a debug build.
pub const MAX_DEPTH: usize = 5_000;

/// The message of the error for JSON nested deeper than [`MAX_DEPTH`].
pub const TOO_DEEP: &str = "JSON nested too deeply";

/// Parses JSON text as `JSON.parse` does.
///
/// Differences from JS are limited to what a Rust `String` can hold: an
/// escaped lone surrogate (`"\ud800"`) becomes U+FFFD, and nesting is
/// limited to [`MAX_DEPTH`].
pub fn parse(text: &str) -> Result<Value, ParseError> {
    let mut p = Parser {
        b: text.as_bytes(),
        i: 0,
        recent: RecentKeys::default(),
    };
    p.skip_whitespace();
    let value = p.parse_value()?;
    p.skip_whitespace();
    if p.i != p.b.len() {
        return Err(p.error("Unexpected non-whitespace character after JSON"));
    }
    Ok(value)
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
    recent: RecentKeys,
}

/// An array or object that the value being parsed is in.
enum Container {
    Array(Vec<Value>),
    /// An object and the key of the value being parsed: `None` for the key
    /// the layout it follows has next (see `Parser::next_key`).
    Object(ObjectBuilder, Option<String>),
}

impl Parser<'_> {
    fn error(&self, message: &'static str) -> ParseError {
        ParseError {
            message,
            offset: self.i,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn skip_whitespace(&mut self) {
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.peek() {
            self.i += 1;
        }
    }

    /// Parses a value without recursion: the arrays and objects it is in are
    /// on `stack`, so nesting takes heap, not stack.
    fn parse_value(&mut self) -> Result<Value, ParseError> {
        let mut stack: Vec<Container> = Vec::new();
        loop {
            let Some(c) = self.peek() else {
                return Err(self.error("Unexpected end of JSON input"));
            };
            let mut value = match c {
                b'{' | b'[' => {
                    if stack.len() >= MAX_DEPTH {
                        return Err(self.error(TOO_DEEP));
                    }
                    let is_object = c == b'{';
                    self.i += 1;
                    self.skip_whitespace();
                    if self.peek() == Some(if is_object { b'}' } else { b']' }) {
                        self.i += 1;
                        if is_object {
                            Value::Object(Object::new())
                        } else {
                            Value::Array(Vec::new())
                        }
                    } else {
                        if is_object {
                            let mut builder = ObjectBuilder::default();
                            let key = self.next_key(&mut builder)?;
                            stack.push(Container::Object(builder, key));
                        } else {
                            stack.push(Container::Array(Vec::new()));
                        }
                        continue;
                    }
                }
                b'"' => Value::String(self.parse_string()?),
                b't' => {
                    self.expect_literal(b"true")?;
                    Value::Bool(true)
                }
                b'f' => {
                    self.expect_literal(b"false")?;
                    Value::Bool(false)
                }
                b'n' => {
                    self.expect_literal(b"null")?;
                    Value::Null
                }
                b'-' | b'0'..=b'9' => Value::Number(self.parse_number()?),
                _ => return Err(self.error("Unexpected token")),
            };
            // Adds the value to its container, closing the containers that
            // end after it, until one has another value.
            loop {
                self.skip_whitespace();
                match stack.last_mut() {
                    None => return Ok(value),
                    Some(Container::Array(a)) => {
                        a.push(value);
                        match self.peek() {
                            None => return Err(self.error("Unexpected end of JSON input")),
                            Some(b',') => {
                                self.i += 1;
                                self.skip_whitespace();
                                break;
                            }
                            Some(b']') => self.i += 1,
                            Some(_) => {
                                return Err(self.error("Expected ',' or ']' after array element"));
                            }
                        }
                    }
                    Some(Container::Object(o, key)) => {
                        // A repeated key keeps its first place and its last value.
                        match key.take() {
                            Some(k) => {
                                o.insert(k, value);
                            }
                            None => o.push_expected(value),
                        }
                        match self.peek() {
                            None => return Err(self.error("Unexpected end of JSON input")),
                            Some(b',') => {
                                self.i += 1;
                                *key = self.next_key(o)?;
                                break;
                            }
                            Some(b'}') => self.i += 1,
                            Some(_) => {
                                return Err(self.error("Expected ',' or '}' after property value"));
                            }
                        }
                    }
                }
                value = match stack.pop() {
                    Some(Container::Array(a)) => Value::Array(a),
                    Some(Container::Object(o, _)) => {
                        let mut o = o.build();
                        self.recent.remember(&mut o);
                        Value::Object(o)
                    }
                    None => unreachable!(),
                };
            }
        }
    }

    /// A property name and its colon, and the whitespace around them.
    fn parse_key(&mut self) -> Result<String, ParseError> {
        self.skip_whitespace();
        match self.peek() {
            None => return Err(self.error("Unexpected end of JSON input")),
            Some(b'"') => {}
            Some(_) => return Err(self.error("Expected property name")),
        }
        let key = self.parse_string()?;
        self.skip_whitespace();
        if self.peek() != Some(b':') {
            return Err(self.error("Expected ':' after property name"));
        }
        self.i += 1;
        self.skip_whitespace();
        Ok(key)
    }

    /// Reads `o`'s next key. When it is the next key of the layout `o`
    /// follows, or of a recent layout with `o`'s keys before it, `o` follows
    /// that layout and the key, which is the layout's, isn't allocated:
    /// `None`. Otherwise the key as [`Parser::parse_key`] reads it.
    fn next_key(&mut self, o: &mut ObjectBuilder) -> Result<Option<String>, ParseError> {
        self.skip_whitespace();
        let found = o.prefix().and_then(|(prefix, next)| match next {
            Some(k) if self.is_raw_key(k) => Some((None, k.len())),
            _ => self
                .recent
                .find_next(prefix, |k| self.is_raw_key(k))
                .map(|layout| {
                    let len = layout.names[prefix.len()].len();
                    (Some(layout), len)
                }),
        });
        let Some((layout, len)) = found else {
            return Ok(Some(self.parse_key()?));
        };
        if let Some(layout) = layout {
            o.follow(layout);
        }
        self.skip_raw_key(len)?;
        Ok(None)
    }

    /// Whether the input is at `key` as a string without escapes, which then
    /// parses as `key`.
    fn is_raw_key(&self, key: &str) -> bool {
        let k = key.as_bytes();
        let start = self.i + 1;
        self.peek() == Some(b'"')
            && self.b.get(start..start + k.len()) == Some(k)
            && self.b.get(start + k.len()) == Some(&b'"')
            && k.iter().all(|&c| c >= 0x20 && c != b'"' && c != b'\\')
    }

    /// Skips a key of `len` bytes that [`Parser::is_raw_key`] accepted, and
    /// the colon after it, as [`Parser::parse_key`] does.
    fn skip_raw_key(&mut self, len: usize) -> Result<(), ParseError> {
        self.i += len + 2;
        self.skip_whitespace();
        if self.peek() != Some(b':') {
            return Err(self.error("Expected ':' after property name"));
        }
        self.i += 1;
        self.skip_whitespace();
        Ok(())
    }

    fn expect_literal(&mut self, literal: &[u8]) -> Result<(), ParseError> {
        if self.b.len() - self.i < literal.len() {
            return Err(self.error("Unexpected end of JSON input"));
        }
        if &self.b[self.i..self.i + literal.len()] != literal {
            return Err(self.error("Unexpected token"));
        }
        self.i += literal.len();
        Ok(())
    }

    fn parse_string(&mut self) -> Result<String, ParseError> {
        self.i += 1; // opening quote
        let start = self.i;
        // Fast path: no escapes.
        while let Some(c) = self.peek() {
            match c {
                b'"' => {
                    let s = self.text(start, self.i);
                    self.i += 1;
                    return Ok(s);
                }
                b'\\' => break,
                0..0x20 => return Err(self.error("Bad control character in string literal")),
                _ => self.i += 1,
            }
        }
        let mut out = self.text(start, self.i);
        while let Some(c) = self.peek() {
            match c {
                b'"' => {
                    self.i += 1;
                    return Ok(out);
                }
                0..0x20 => return Err(self.error("Bad control character in string literal")),
                b'\\' => {}
                _ => {
                    // Copies the run up to the next quote, backslash or control.
                    let run = self.i;
                    while let Some(c) = self.peek() {
                        if c == b'"' || c == b'\\' || c < 0x20 {
                            break;
                        }
                        self.i += 1;
                    }
                    out.push_str(&self.text(run, self.i));
                    continue;
                }
            }
            self.i += 1;
            let Some(e) = self.peek() else {
                return Err(self.error("Unterminated string"));
            };
            self.i += 1;
            match e {
                b'"' => out.push('"'),
                b'\\' => out.push('\\'),
                b'/' => out.push('/'),
                b'b' => out.push('\u{8}'),
                b'f' => out.push('\u{c}'),
                b'n' => out.push('\n'),
                b'r' => out.push('\r'),
                b't' => out.push('\t'),
                b'u' => {
                    let unit = self.parse_hex4()?;
                    let ch = if (0xD800..=0xDBFF).contains(&unit) {
                        // A high surrogate; combine with a following escaped
                        // low surrogate.
                        if self.b[self.i..].starts_with(b"\\u") && self.b.len() - self.i >= 6 {
                            let save = self.i;
                            self.i += 2;
                            let low = self.parse_hex4()?;
                            if (0xDC00..=0xDFFF).contains(&low) {
                                char::from_u32(0x10000 + ((unit - 0xD800) << 10) + (low - 0xDC00))
                                    .unwrap_or('\u{FFFD}')
                            } else {
                                self.i = save;
                                '\u{FFFD}'
                            }
                        } else {
                            '\u{FFFD}'
                        }
                    } else {
                        char::from_u32(unit).unwrap_or('\u{FFFD}')
                    };
                    out.push(ch);
                }
                _ => return Err(self.error("Bad escaped character in JSON")),
            }
        }
        Err(self.error("Unterminated string in JSON"))
    }

    /// The text between two byte offsets, which are on character boundaries
    /// because they are next to ASCII bytes.
    fn text(&self, start: usize, end: usize) -> String {
        String::from_utf8_lossy(&self.b[start..end]).into_owned()
    }

    fn parse_hex4(&mut self) -> Result<u32, ParseError> {
        if self.b.len() - self.i < 4 {
            return Err(self.error("Bad Unicode escape"));
        }
        let mut v = 0;
        for _ in 0..4 {
            let d = match self.b[self.i] {
                c @ b'0'..=b'9' => c - b'0',
                c @ b'a'..=b'f' => c - b'a' + 10,
                c @ b'A'..=b'F' => c - b'A' + 10,
                _ => return Err(self.error("Bad Unicode escape")),
            };
            v = v << 4 | u32::from(d);
            self.i += 1;
        }
        Ok(v)
    }

    fn parse_number(&mut self) -> Result<f64, ParseError> {
        let start = self.i;
        let digits = |p: &mut Self| {
            let from = p.i;
            while p.peek().is_some_and(|c| c.is_ascii_digit()) {
                p.i += 1;
            }
            p.i - from
        };
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        match self.peek() {
            Some(b'0') => self.i += 1,
            Some(b'1'..=b'9') => {
                digits(self);
            }
            _ => return Err(self.error("No number after minus sign")),
        }
        if self.peek() == Some(b'.') {
            self.i += 1;
            if digits(self) == 0 {
                return Err(self.error("Unterminated fractional number"));
            }
        }
        if let Some(b'e' | b'E') = self.peek() {
            self.i += 1;
            if let Some(b'+' | b'-') = self.peek() {
                self.i += 1;
            }
            if digits(self) == 0 {
                return Err(self.error("Exponent part is missing a number"));
            }
        }
        // Rust's parse rounds correctly and overflows to infinity, as JS
        // does, and keeps `-0` negative.
        let text = std::str::from_utf8(&self.b[start..self.i]).expect("ASCII");
        text.parse().map_err(|_| self.error("Bad number"))
    }
}
