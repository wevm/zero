//! `Ast`'s JSON through the API an app uses to check its queries against a
//! server's: `Ast::from_json` reads the protocol's JSON, and
//! `json::stringify` of `to_json` writes it back as JS does, byte for byte.

use zero_client::ast::Ast;
use zero_client::decode::JsonCodable;
use zero_client::json::{self, Value};

/// JS's `normalizeAST` writes every node's fields in a fixed order, as its
/// query builder does, so that equal queries have the same JSON text. Each
/// normalized AST, read and written again, is JS's text of it.
#[test]
fn writes_the_protocol_json_byte_for_byte() {
    let vectors = zero_conformance::vectors_dir();
    // The mirror (wevm/zero-rust) has rust/ without the monorepo's vectors.
    if !vectors.exists() {
        eprintln!("skipped: no vectors at {}", vectors.display());
        return;
    }
    let mut files = vec![vectors.join("ast.json"), vectors.join("ast-features.json")];
    let fuzz = zero_conformance::generated_dir().join("ast-fuzz.json");
    if fuzz.exists() {
        files.push(fuzz);
    }
    let mut differ = Vec::new();
    let mut count = 0;
    for path in files {
        let text = std::fs::read_to_string(&path).unwrap();
        let file = json::parse(&text).unwrap();
        let cases: Vec<&Value> = file
            .get("cases")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .filter_map(|c| c.get("normalized"))
            .collect();
        let texts = case_values(&text, "normalized");
        assert!(
            !cases.is_empty() && texts.len() == cases.len(),
            "{}: {} normalized ASTs, {} texts",
            path.display(),
            cases.len(),
            texts.len()
        );
        for (normalized, js) in cases.into_iter().zip(texts) {
            let ast = Ast::from_json(normalized)
                .unwrap_or_else(|e| panic!("{}: rejected {js}: {e}", path.display()));
            let rust = json::stringify(&ast.to_json());
            if rust != js {
                differ.push(format!("{}:\n  JS   {js}\n  Rust {rust}", path.display()));
            }
            count += 1;
        }
    }
    assert!(
        differ.is_empty(),
        "{} of {count} ASTs differ from JS's text; the first:\n{}",
        differ.len(),
        differ[0]
    );
}

/// The text of each case's `key` as JS wrote it (`JSON.stringify`): the
/// vector file holds `JSON.stringify(file, null, 2)`, or one
/// `JSON.stringify(case)` per line, which differ from it only by the
/// whitespace between tokens.
fn case_values(file: &str, key: &str) -> Vec<String> {
    let mut text = Vec::with_capacity(file.len());
    let (mut in_string, mut escaped) = (false, false);
    for &b in file.as_bytes() {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
        } else if b == b'"' {
            in_string = true;
        } else if matches!(b, b' ' | b'\n' | b'\r' | b'\t') {
            continue;
        }
        text.push(b);
    }
    // The cases are the objects at depth 3: the file, `cases`, a case.
    let key = format!("\"{key}\":");
    let mut values = Vec::new();
    let mut depth = 0;
    let mut i = 0;
    in_string = false;
    while i < text.len() {
        let b = text[i];
        if in_string {
            if b == b'\\' {
                i += 1;
            } else if b == b'"' {
                in_string = false;
            }
        } else if depth == 3 && text[i..].starts_with(key.as_bytes()) {
            let start = i + key.len();
            let end = end_of_value(&text, start);
            values.push(String::from_utf8(text[start..end].to_vec()).unwrap());
            i = end;
            continue;
        } else if b == b'"' {
            in_string = true;
        } else if b == b'{' || b == b'[' {
            depth += 1;
        } else if b == b'}' || b == b']' {
            depth -= 1;
        }
        i += 1;
    }
    values
}

/// Where the object that starts at `start` ends.
fn end_of_value(text: &[u8], start: usize) -> usize {
    let mut depth = 0;
    let mut in_string = false;
    let mut i = start;
    loop {
        let b = text[i];
        if in_string {
            if b == b'\\' {
                i += 1;
            } else if b == b'"' {
                in_string = false;
            }
        } else if b == b'"' {
            in_string = true;
        } else if b == b'{' || b == b'[' {
            depth += 1;
        } else if b == b'}' || b == b']' {
            depth -= 1;
        }
        i += 1;
        if depth == 0 {
            return i;
        }
    }
}
