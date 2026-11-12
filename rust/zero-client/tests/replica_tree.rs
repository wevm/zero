//! The replica's B+tree (`KvMap` in `client/replica_store.rs`) once lost
//! keys. When a delete emptied the first child of an internal node, the child
//! was removed and the next one became the first, keeping its own lower
//! bound, above the bound the parent had for the node. Inside the node the
//! first bound is never read, so a key between the two went to that child.
//! Once the node merged into its left neighbor, that bound was read: lookups
//! of such a key went to the neighbor's last child, so `get` and `has` missed
//! a row the map still held. A mutator or a poke then couldn't delete the
//! row, and putting it again added a second entry and an add of a row the
//! IVM source had, which panicked ("Row already exists").
//!
//! The steps are the shortest construction found: ascending inserts make
//! leaves of 32 and a root over two internal nodes, P0 and P1. JS runs the
//! same mutations (through the conformance harness's `harden.ops`) and
//! deletes the row, then shows it again after the insert.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use zero_client::ast::System;
use zero_client::client::{
    MaterializedView, Mutator, SocketFactory, Transaction, Zero, ZeroOptions,
};
use zero_client::json::{self, Object, Value};
use zero_client::query::AnyQuery;
use zero_client::schema::Schema;
use zero_client::testing::{ManualScheduler, MockSocketFactory, RecordingLogSink};
use zero_client::ttl::Ttl;

fn schema() -> Arc<Schema> {
    let schema = json::parse(
        r#"{"tables": {"issue": {"name": "issue", "columns": {
            "id": {"type": "string"}, "title": {"type": "string"}}, "primaryKey": ["id"]}}}"#,
    )
    .unwrap();
    Arc::new(Schema::from_json(&schema).unwrap())
}

fn id(i: u32) -> String {
    format!("i{i:06}")
}

/// `ids` as the mutator's arguments.
fn args(ids: impl IntoIterator<Item = u32>) -> Option<Value> {
    Some(Value::Array(
        ids.into_iter().map(|i| Value::from(id(i))).collect(),
    ))
}

struct Client {
    zero: Zero,
    scheduler: Rc<ManualScheduler>,
}

impl Client {
    /// A local client with a view of every issue.
    fn new() -> (Client, MaterializedView) {
        let scheduler = ManualScheduler::new(1_700_000_000_000.0);
        let insert = Mutator::new("insert", |tx: &Transaction, args, _: &()| {
            for v in args.unwrap().as_array().unwrap() {
                let mut row = Object::new();
                row.insert("id", v.clone());
                row.insert("title", Value::from("t"));
                tx.table("issue").insert(&row)?;
            }
            Ok(None)
        });
        let delete = Mutator::new("delete", |tx: &Transaction, args, _: &()| {
            for v in args.unwrap().as_array().unwrap() {
                let mut row = Object::new();
                row.insert("id", v.clone());
                tx.table("issue").delete(&row)?;
            }
            Ok(None)
        });
        let zero = Zero::new(
            schema(),
            (),
            vec![insert, delete],
            ZeroOptions {
                log_sink: Some(Rc::new(RecordingLogSink::default())),
                ..ZeroOptions::default()
            },
            scheduler.clone(),
            None,
        )
        .unwrap();
        scheduler.run_checkpoints();
        let view = zero
            .materialize(
                &AnyQuery::new(schema(), "issue", System::Client),
                &Ttl::default(),
            )
            .unwrap();
        scheduler.run_checkpoints();
        (Client { zero, scheduler }, view)
    }

    fn mutate(&self, name: &str, ids: impl IntoIterator<Item = u32>) {
        self.zero.mutate(name, args(ids));
        self.scheduler.run_checkpoints();
    }

    /// Leaves `i010300` where the B+tree couldn't find it.
    fn lose_a_row(&self) {
        // Leaves of 32 keys; P1's first leaf has 10240..=10550, its second
        // 10560..=10870.
        self.mutate("insert", (0..2081).map(|i| i * 10));
        // The second leaf full (64), so the first can't merge into it.
        self.mutate("insert", (0..32).map(|i| 10561 + i * 10));
        // The first leaf emptied and removed: the second, bound 10560, is
        // P1's first child, and P1's bound is 10240.
        self.mutate("delete", (0..32).map(|i| 10240 + i * 10));
        self.mutate("insert", [10300]);
        // P1 shrunk until it merges into P0.
        self.mutate("delete", (0..563).map(|i| 20800 - i * 10));
    }
}

fn has(view: &MaterializedView, i: u32) -> bool {
    view.data()
        .entries()
        .iter()
        .any(|e| e.row.get("id").and_then(Value::as_str) == Some(&id(i)))
}

#[test]
fn a_row_after_a_merge_of_the_replica_tree_can_be_deleted() {
    let (client, view) = Client::new();
    client.lose_a_row();
    assert!(has(&view, 10300));
    client.mutate("delete", [10300]);
    assert!(!has(&view, 10300), "the deleted row is still in the view");
}

#[test]
fn a_row_after_a_merge_of_the_replica_tree_can_be_inserted_again() {
    let (client, view) = Client::new();
    client.lose_a_row();
    client.mutate("delete", [10300]);
    client.mutate("insert", [10300]);
    assert!(has(&view, 10300));
}

/// The steps of [`Client::lose_a_row`] as pokes: puts and dels of the
/// server's rows, then a del and a put of the row.
#[test]
fn a_row_after_a_merge_of_the_replica_tree_follows_the_server() {
    let scheduler = ManualScheduler::new(1_700_000_000_000.0);
    let factory = MockSocketFactory::new();
    let zero = Zero::new(
        schema(),
        (),
        Vec::new(),
        ZeroOptions {
            cache_url: Some("http://zero.test".into()),
            user_id: Some("u1".into()),
            log_sink: Some(Rc::new(RecordingLogSink::default())),
            ..ZeroOptions::default()
        },
        scheduler.clone(),
        Some(factory.clone() as Rc<dyn SocketFactory>),
    )
    .unwrap();
    let view = zero
        .materialize(
            &AnyQuery::new(schema(), "issue", System::Client),
            &Ttl::default(),
        )
        .unwrap();
    scheduler.advance(0.0);
    let socket = factory.sockets()[0].clone();
    socket.receive_text(r#"["connected",{"wsid":"w1"}]"#);
    let cookie = Cell::new(0);
    let poke = |patch: Vec<String>| {
        let n = cookie.get() + 1;
        let base = if n == 1 {
            "null".to_owned()
        } else {
            format!(r#""{:02}""#, n - 1)
        };
        cookie.set(n);
        socket.receive_text(&format!(
            r#"["pokeStart",{{"pokeID":"p{n}","baseCookie":{base}}}]"#
        ));
        socket.receive_text(&format!(
            r#"["pokePart",{{"pokeID":"p{n}","rowsPatch":[{}]}}]"#,
            patch.join(",")
        ));
        socket.receive_text(&format!(
            r#"["pokeEnd",{{"pokeID":"p{n}","cookie":"{n:02}"}}]"#
        ));
        scheduler.advance(20.0);
    };
    let put = |ids: Vec<u32>| {
        poke(
            ids.into_iter()
                .map(|i| {
                    format!(
                        r#"{{"op":"put","tableName":"issue","value":{{"id":"{}","title":"t"}}}}"#,
                        id(i)
                    )
                })
                .collect(),
        )
    };
    let del = |ids: Vec<u32>| {
        poke(
            ids.into_iter()
                .map(|i| {
                    format!(
                        r#"{{"op":"del","tableName":"issue","id":{{"id":"{}"}}}}"#,
                        id(i)
                    )
                })
                .collect(),
        )
    };
    put((0..2081).map(|i| i * 10).collect());
    put((0..32).map(|i| 10561 + i * 10).collect());
    del((0..32).map(|i| 10240 + i * 10).collect());
    put(vec![10300]);
    del((0..563).map(|i| 20800 - i * 10).collect());
    assert!(has(&view, 10300));
    del(vec![10300]);
    assert!(
        !has(&view, 10300),
        "the row the server deleted is still in the view"
    );
    put(vec![10300]);
    assert!(has(&view, 10300));
    assert_eq!(view.data().entries().len(), 2081 + 32 - 32 + 1 - 563);
}
