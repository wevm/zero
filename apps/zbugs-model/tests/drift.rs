//! The drift test: zbugs' Rust queries and mutators do what the TS ones do
//! on the samples in `apps/zbugs-swift/drift-samples.ts`, as zbugs-swift's
//! `DriftTests` checks the Swift ports, from the same record
//! (`pnpm --filter zero-swift run test` keeps it current).
//!
//! - Every name in the TS registries has a Rust definition, and the other
//!   way round.
//! - Each query sample's arguments read as the Rust argument type and
//!   encode back to the same JSON, and its AST has the same hash.
//! - Each mutator sample's arguments round-trip the same way, and the
//!   mutator, run on the sample's rows, has the same outcome and changes
//!   the same rows.

use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;
use zbugs_model::{AuthData, mutators, queries, schema};
use zero_client::ast::System;
use zero_client::client::{Mutator, RunType, Zero, ZeroOptions, ZeroStorage};
use zero_client::hash::query::hash_of_ast;
use zero_client::json::{self, Object, Value};
use zero_client::query::AnyQuery;
use zero_client::testing::{ManualScheduler, RecordingLogSink};

const VECTORS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../zbugs-swift/Tests/ZBugsTests/drift-vectors.json"
);

fn names(v: &Value, key: &str) -> BTreeSet<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn context(sample: &Value) -> Option<AuthData> {
    sample.get("context").and_then(AuthData::from_json)
}

#[test]
fn matches_the_ts_queries_and_mutators() {
    let v = json::parse(&std::fs::read_to_string(VECTORS).unwrap()).unwrap();
    let mut failures: Vec<String> = Vec::new();

    let qs = queries::all();
    let ms = mutators::all();
    let rust_queries: HashMap<&str, _> = qs.iter().map(|q| (q.name(), q)).collect();
    let rust_mutators: HashMap<&str, _> = ms.iter().map(|m| (m.name(), m)).collect();
    let (ts_queries, ts_mutators) = (names(&v, "queries"), names(&v, "mutators"));
    for n in &ts_queries {
        if !rust_queries.contains_key(n.as_str()) {
            failures.push(format!("query {n}: no Rust definition"));
        }
    }
    for n in rust_queries.keys() {
        if !ts_queries.contains(*n) {
            failures.push(format!("query {n}: not in the TS registry"));
        }
    }
    for n in &ts_mutators {
        if !rust_mutators.contains_key(n.as_str()) {
            failures.push(format!("mutator {n}: no Rust definition"));
        }
    }
    for n in rust_mutators.keys() {
        if !ts_mutators.contains(*n) {
            failures.push(format!("mutator {n}: not in the TS registry"));
        }
    }

    let samples = |key| {
        v.get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    for (i, sample) in samples("querySamples").iter().enumerate() {
        let name = sample.get("name").and_then(Value::as_str).unwrap_or("");
        let label = format!("query {name} sample {i}");
        let Some(q) = rust_queries.get(name) else {
            continue;
        };
        let args = sample
            .get("args")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        match q.query_from_json(&args, &context(sample)) {
            Err(e) => failures.push(format!("{label}: {e}")),
            Ok((query, sent)) => {
                if sent != args {
                    failures.push(format!(
                        "{label}: the arguments encode as {}, not {}",
                        json::stringify(&Value::Array(sent)),
                        json::stringify(&Value::Array(args))
                    ));
                }
                let any = &query;
                if let Some(e) = any.invalid() {
                    failures.push(format!("{label}: invalid query: {e}"));
                }
                let hash = hash_of_ast(any.ast());
                if sample.get("hash").and_then(Value::as_str) != Some(hash.as_str()) {
                    failures.push(format!(
                        "{label}: the AST differs.\n  TS:   {}\n  Rust: {}",
                        json::stringify(sample.get("ast").unwrap_or(&Value::Null)),
                        json::stringify(&zero_client::decode::JsonCodable::to_json(any.ast()))
                    ));
                }
            }
        }
    }

    for (i, sample) in samples("mutatorSamples").iter().enumerate() {
        let name = sample.get("name").and_then(Value::as_str).unwrap_or("");
        let label = format!("mutator {name} sample {i}");
        if !rust_mutators.contains_key(name) {
            continue;
        }
        failures.extend(check_mutator(&label, sample));
    }

    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn check_mutator(label: &str, sample: &Value) -> Vec<String> {
    let mut failures = Vec::new();
    let name = sample.get("name").and_then(Value::as_str).unwrap();
    let args = sample.get("args");
    let all = mutators::all();
    let def = all.iter().find(|m| m.name() == name).unwrap();
    match def.reencode(args) {
        Ok(sent) if sent.as_ref() == args => {}
        Ok(sent) => failures.push(format!(
            "{label}: the arguments encode as {}, not {}",
            sent.as_ref().map_or("undefined".into(), json::stringify),
            args.map_or("undefined".into(), json::stringify)
        )),
        Err(e) => failures.push(format!("{label}: the arguments don't read: {e}")),
    }

    let schema = schema::schema();
    let mut ms: Vec<Mutator<mutators::Context>> = all.iter().map(|m| m.mutator()).collect();
    // Puts the sample's rows (`{table: [row]}`) before the mutator runs.
    ms.push(Mutator::new("__drift.seed", |tx, args, _| {
        for (table, rows) in args.and_then(Value::as_object).into_iter().flatten() {
            for row in rows.as_array().into_iter().flatten() {
                tx.table(table).upsert(row.as_object().unwrap())?;
            }
        }
        Ok(None)
    }));
    let zero = Zero::new(
        schema.clone(),
        context(sample),
        ms,
        ZeroOptions {
            user_id: Some("drift".into()),
            storage_key: Some(zbugs_model::make_id()),
            storage: ZeroStorage::Memory,
            log_sink: Some(Rc::new(RecordingLogSink::default())),
            ..ZeroOptions::default()
        },
        ManualScheduler::new(0.0),
        None,
    )
    .unwrap();
    zero.mutate("__drift.seed", sample.get("seed").cloned());
    let before = rows(&zero);
    let result = zero.mutate(name, args.cloned());
    let after = rows(&zero);
    zero.close();

    let outcome = result
        .client
        .result()
        .map_or(Value::Null, |r| r.map_or(Value::Null, |o| o.to_json()));
    if let Some(expected) = sample.get("outcome")
        && outcome != *expected
    {
        failures.push(format!(
            "{label}: the outcome is {}, not {}",
            json::stringify(&outcome),
            json::stringify(expected)
        ));
    }
    let changes = diff(&before, &after);
    let expected = sample
        .get("changes")
        .cloned()
        .unwrap_or(Value::Array(Vec::new()));
    if changes != expected {
        failures.push(format!(
            "{label}: the changes differ.\n  TS:   {}\n  Rust: {}",
            json::stringify(&expected),
            json::stringify(&changes)
        ));
    }
    failures
}

/// Rows by table in schema order, then by primary key.
type Tables = Vec<(String, Vec<String>, HashMap<String, Object>)>;

fn rows(zero: &Zero<mutators::Context>) -> Tables {
    let schema = schema::schema();
    schema
        .tables
        .iter()
        .map(|(table, t)| {
            let q = AnyQuery::new(schema.clone(), table.as_str(), System::Client);
            let data = zero
                .run(&q, RunType::Unknown, None)
                .result()
                .expect("a local run settles at once")
                .unwrap();
            let mut by_key = HashMap::new();
            if let zero_client::ivm::ViewValue::Many(entries) = data {
                for e in entries.iter() {
                    let key: Vec<Value> = t
                        .primary_key
                        .iter()
                        .map(|c| e.row.get(c).cloned().unwrap_or(Value::Null))
                        .collect();
                    by_key.insert(json::stringify(&Value::Array(key)), (*e.row).clone());
                }
            }
            (table.clone(), t.primary_key.clone(), by_key)
        })
        .collect()
}

/// The changes from `before` to `after`, as `zero-swift drift record`
/// lists them: by table in schema order, then by primary key.
fn diff(before: &Tables, after: &Tables) -> Value {
    let mut changes = Vec::new();
    for ((table, pk, old), (_, _, new)) in before.iter().zip(after) {
        let mut keys: Vec<&String> = old.keys().chain(new.keys()).collect();
        keys.sort_by(|a, b| zero_client::strings::utf8(a, b));
        keys.dedup();
        for key in keys {
            let mut change = Object::new();
            change.insert("table", Value::from(table.as_str()));
            match (old.get(key), new.get(key)) {
                (o, Some(n)) if o != Some(n) => {
                    change.insert("op", Value::from("put"));
                    change.insert("row", Value::Object(n.clone()));
                }
                (Some(o), None) => {
                    let id: Object = pk
                        .iter()
                        .map(|c| (c.clone(), o.get(c).cloned().unwrap_or(Value::Null)))
                        .collect();
                    change.insert("op", Value::from("del"));
                    change.insert("id", Value::Object(id));
                }
                _ => continue,
            }
            changes.push(Value::Object(change));
        }
    }
    Value::Array(changes)
}
