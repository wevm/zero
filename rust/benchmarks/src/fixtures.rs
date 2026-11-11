//! `zero.bench.ts`'s table, its mutators, and a client on a manual clock
//! that a benchmark plays the server for (`Fixtures.swift`).

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use zero_client::client::{LogLevel, MaterializedView, Mutator, Zero, ZeroOptions};
use zero_client::ivm::ViewEntry;
use zero_client::json::{self, Object, Value};
use zero_client::query::AnyQuery;
use zero_client::schema::Schema;
use zero_client::testing::{ManualScheduler, MockSocket, MockSocketFactory, RecordingLogSink};
use zero_client::ttl::Ttl;

const COLUMNS: [&str; 10] = ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"];

/// `user`: ten number columns, keyed by `a`.
pub fn bench_schema() -> Arc<Schema> {
    let columns: Vec<String> = COLUMNS
        .iter()
        .map(|c| format!(r#""{c}": {{"type": "number"}}"#))
        .collect();
    let schema = format!(
        r#"{{"tables": {{"user": {{"name": "user", "columns": {{{}}}, "primaryKey": ["a"]}}}}}}"#,
        columns.join(",")
    );
    Arc::new(Schema::from_json(&json::parse(&schema).unwrap()).unwrap())
}

pub fn users(schema: &Arc<Schema>) -> AnyQuery {
    AnyQuery::new(schema.clone(), "user", zero_client::ast::System::Client)
}

/// User `n` with `b` through `j` set to `value`.
pub fn user(n: usize, value: usize) -> Object {
    let mut row = Object::new();
    for (i, c) in COLUMNS.iter().enumerate() {
        let v = if i == 0 { n } else { value };
        row.insert(*c, Value::from(v as f64));
    }
    row
}

/// The row as `rowsPatch` puts it, with `b` through `j` set to `value`.
pub fn user_json(n: usize, value: usize) -> String {
    format!(
        r#"{{"op":"put","tableName":"user","value":{{"a":{n},"b":{value},"c":{value},"d":{value},"e":{value},"f":{value},"g":{value},"h":{value},"i":{value},"j":{value}}}}}"#
    )
}

fn number(args: Option<&Value>, key: &str) -> usize {
    args.and_then(|a| a.get(key))
        .and_then(Value::as_f64)
        .unwrap_or_default() as usize
}

/// `insertUsers({from, count})` and `updateUser({a, value})`.
pub fn mutators() -> Vec<Mutator<()>> {
    vec![
        Mutator::new("insertUsers", |tx, args, _| {
            let from = number(args, "from");
            for n in from..from + number(args, "count") {
                tx.table("user").insert(&user(n, n))?;
            }
            Ok(None)
        }),
        Mutator::new("updateUser", |tx, args, _| {
            tx.table("user")
                .upsert(&user(number(args, "a"), number(args, "value")))?;
            Ok(None)
        }),
    ]
}

pub fn update_user_args(a: usize, value: usize) -> Value {
    let mut args = Object::new();
    args.insert("a", Value::from(a as f64));
    args.insert("value", Value::from(value as f64));
    Value::from(args)
}

/// A client on memory storage with `rows` users, on a manual clock;
/// connected to a mock socket when `connected` is set. Dropping it deletes
/// its store, which memory storage otherwise keeps for the thread's life.
pub struct Client {
    pub zero: Zero,
    pub scheduler: Rc<ManualScheduler>,
    sockets: Rc<MockSocketFactory>,
    cookie: Cell<usize>,
}

static NEXT_KEY: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

impl Client {
    pub fn new(rows: usize, connected: bool) -> Self {
        Self::with(rows, connected, bench_schema(), mutators())
    }

    pub fn with(
        rows: usize,
        connected: bool,
        schema: Arc<Schema>,
        mutators: Vec<Mutator<()>>,
    ) -> Self {
        let scheduler = ManualScheduler::new(1_700_000_000_000.0);
        let sockets = MockSocketFactory::new();
        let key = NEXT_KEY.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let zero = Zero::new(
            schema,
            (),
            mutators,
            ZeroOptions {
                cache_url: connected.then(|| "http://zero.bench".to_owned()),
                user_id: Some("bench".into()),
                storage_key: Some(format!("bench-{key}")),
                log_level: LogLevel::Error,
                log_sink: Some(Rc::new(RecordingLogSink::default())),
                ..ZeroOptions::default()
            },
            scheduler.clone(),
            connected.then(|| sockets.clone() as _),
        )
        .unwrap();
        let client = Client {
            zero,
            scheduler,
            sockets,
            cookie: Cell::new(0),
        };
        if connected {
            // The rows come from the server, so no mutation is pending.
            client.scheduler.run_due_timers();
            client
                .socket()
                .receive_text(r#"["connected",{"wsid":"bench"}]"#);
            client.scheduler.run_due_timers();
            if rows > 0 {
                let rows: Vec<String> = (0..rows).map(|n| user_json(n, n)).collect();
                client.receive(&client.next_poke(&[rows], None));
            }
        } else if rows > 0 {
            let mut args = Object::new();
            args.insert("from", Value::from(0.0));
            args.insert("count", Value::from(rows as f64));
            let _ = client.zero.mutate("insertUsers", Some(Value::from(args)));
            client.scheduler.run_due_timers();
        }
        client
    }

    pub fn materialize(&self, query: &AnyQuery) -> MaterializedView {
        self.zero.materialize(query, &Ttl::default()).unwrap()
    }

    pub fn socket(&self) -> Rc<MockSocket> {
        self.sockets.sockets().last().cloned().expect("a socket")
    }

    /// The frames of the next poke, with a part for each of `parts`; the
    /// last part has the client's last mutation.
    pub fn next_poke(&self, parts: &[Vec<String>], last_mutation_id: Option<u64>) -> Vec<String> {
        let cookie = self.cookie.get();
        // Cookies compare as strings.
        let base = if cookie == 0 {
            "null".to_owned()
        } else {
            format!("\"{cookie:012}\"")
        };
        let cookie = cookie + 1;
        self.cookie.set(cookie);
        let id = format!("p{cookie}");
        let client_id = self.zero.client_id();
        let mut frames = vec![format!(
            r#"["pokeStart",{{"pokeID":"{id}","baseCookie":{base}}}]"#
        )];
        for (i, rows) in parts.iter().enumerate() {
            let mut part = format!(r#""pokeID":"{id}","rowsPatch":[{}]"#, rows.join(","));
            if let Some(lmid) = last_mutation_id
                && i == parts.len() - 1
            {
                part += &format!(r#","lastMutationIDChanges":{{"{client_id}":{lmid}}}"#);
            }
            frames.push(format!(r#"["pokePart",{{{part}}}]"#));
        }
        frames.push(format!(
            r#"["pokeEnd",{{"pokeID":"{id}","cookie":"{cookie:012}"}}]"#
        ));
        frames
    }

    /// Delivers frames from the server and lets the client apply them.
    pub fn receive(&self, frames: &[String]) {
        let socket = self.socket();
        for frame in frames {
            socket.receive_text(frame);
        }
        self.scheduler.run_due_timers();
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.zero.delete();
    }
}

/// The sum of column `a` over the view's rows, as the JS benchmarks check.
pub fn sum(view: &MaterializedView) -> f64 {
    sum_entries(&view.data().entries())
}

pub fn sum_entries(entries: &[&ViewEntry]) -> f64 {
    entries
        .iter()
        .map(|e| e.row.get("a").and_then(Value::as_f64).unwrap_or_default())
        .sum()
}
