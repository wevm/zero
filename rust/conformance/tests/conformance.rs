//! Every replayed vector case matches JS. Files no suite replays yet are
//! listed in the report, not failed; `cargo run -p zero-conformance` prints
//! it.

#[test]
fn replayed_vectors_match_js() {
    // The mirror (wevm/zero-rust) has rust/ without the monorepo's vectors.
    // In the monorepo they're always there, and CI's conformance step fails
    // without them.
    if !zero_conformance::vectors_dir().exists() {
        eprintln!(
            "skipped: no vectors at {}; set ZERO_CONFORMANCE_VECTORS",
            zero_conformance::vectors_dir().display()
        );
        return;
    }
    let report = zero_conformance::run(None).expect("vectors");
    assert!(report.ok(), "{}", report.markdown());
}

use zero_client::json::{self, Object, Value};

fn object(v: &mut Value) -> &mut Object {
    match v {
        Value::Object(o) => o,
        v => panic!("{v} is not an object"),
    }
}

fn result(case: &mut Value) -> &mut Object {
    object(object(case).get_mut("result").expect("a result"))
}

/// A case's `key` (`changes` or `data`) after each push.
fn steps<'a>(case: &'a mut Value, key: &str) -> Vec<&'a mut Value> {
    match result(case).get_mut("steps") {
        Some(Value::Array(steps)) => steps
            .iter_mut()
            .filter_map(|s| object(s).get_mut(key))
            .collect(),
        _ => Vec::new(),
    }
}

/// Drops the last element of the first non-empty list `at` finds in a case
/// not doctored yet, and returns that case.
fn cut(
    cases: &mut [Value],
    doctored: &mut Vec<usize>,
    at: impl Fn(&mut Value) -> Vec<&mut Value>,
) -> usize {
    for (i, c) in cases.iter_mut().enumerate() {
        if doctored.contains(&i) {
            continue;
        }
        for v in at(c) {
            if let Value::Array(a) = v
                && a.pop().is_some()
            {
                doctored.push(i);
                return i;
            }
        }
    }
    panic!("no case to doctor");
}

/// The restored zql vectors (`vectors/restored-ivm.json` and, when it is
/// there, `generated/restored-chinook-decpush.json`) fail when JS's answers
/// are wrong: in a copy of each, a case with a row cut from its initial
/// view, another with a change cut after a push, and a third with a row cut
/// from its view after a push fail, and only those.
#[test]
fn doctored_restored_vectors_fail() {
    let vectors = zero_conformance::vectors_dir();
    if !vectors.exists() {
        eprintln!("skipped: no vectors at {}", vectors.display());
        return;
    }
    let dir =
        std::env::temp_dir().join(format!("zero-conformance-doctored-{}", std::process::id()));
    let copies = [
        (vectors.join("restored-ivm.json"), dir.join("vectors")),
        (
            zero_conformance::generated_dir().join("restored-chinook-decpush.json"),
            dir.join("generated"),
        ),
    ];
    let mut expected = Vec::new();
    for (path, into) in &copies {
        std::fs::create_dir_all(into).unwrap();
        let Ok(text) = std::fs::read_to_string(path) else {
            eprintln!("not doctored: no {}", path.display());
            continue;
        };
        let mut file = json::parse(&text).unwrap();
        let Some(Value::Array(cases)) = object(&mut file).get_mut("cases") else {
            panic!("no cases");
        };
        let mut doctored = Vec::new();
        let picked = [
            (
                cut(cases, &mut doctored, |c| {
                    result(c).get_mut("initial").into_iter().collect()
                }),
                "initial view",
            ),
            (
                cut(cases, &mut doctored, |c| steps(c, "changes")),
                "changes at push",
            ),
            // The view, with the changes as JS recorded them.
            (
                cut(cases, &mut doctored, |c| steps(c, "data")),
                "view at push",
            ),
        ];
        // The generated corpus is large; ten cases past the doctored ones
        // show the rest still pass.
        cases.truncate(doctored.iter().max().unwrap() + 11);
        let name = path.file_stem().unwrap().to_str().unwrap().to_owned();
        expected.push((name, cases.len(), picked));
        std::fs::write(into.join(path.file_name().unwrap()), json::stringify(&file)).unwrap();
    }
    let report = zero_conformance::run_in(
        &dir.join("vectors"),
        &dir.join("generated"),
        Some("restored"),
    );
    std::fs::remove_dir_all(&dir).unwrap();
    let report = report.unwrap();
    for (name, total, mut picked) in expected {
        let f = report.files.iter().find(|f| f.file == name).unwrap();
        assert_eq!(
            (f.cases, f.passed, f.failed),
            (total, total - picked.len(), picked.len()),
            "{}",
            report.markdown()
        );
        picked.sort();
        for ((i, what), failure) in picked.iter().zip(&f.failures) {
            let prefix = format!("case {i}: {what}");
            assert!(
                failure.starts_with(&prefix),
                "{name}: {prefix}...: {failure}"
            );
        }
    }
    assert!(!report.ok());
}
