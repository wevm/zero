//! Bugs a bug hunt found, as tests that fail until they are fixed.
//!
//! The replica's B+tree (`KvMap` in `client/replica_store.rs`) loses keys.
//! When a delete empties the first child of an internal node, the child is
//! removed and the next one becomes the first, keeping its own lower bound,
//! which is above the bound the parent has for the node. Inside the node the
//! first bound is never read, so a key between the two goes to that child.
//! Once the node merges into its left neighbor (`a.extend(b)`), that bound is
//! read: lookups of such a key go to the neighbor's last child, so `get` and
//! `has` miss a row the map still holds and iterates. A transaction then
//! can't see or delete the row, and inserting it again writes a second entry
//! and pushes an add of a row the IVM source has, which panics.
//!
//! The steps below are the shortest construction found: ascending inserts
//! make leaves of 32 and a root over two internal nodes, P0 and P1. JS runs
//! the same mutations (through the conformance harness's `harden.ops`) and
//! deletes the row, then shows it again after the insert.

use std::rc::Rc;
use std::sync::Arc;
use zero_client::ast::System;
use zero_client::client::{MaterializedView, Mutator, Transaction, Zero, ZeroOptions};
use zero_client::json::{self, Object, Value};
use zero_client::query::AnyQuery;
use zero_client::schema::Schema;
use zero_client::testing::{ManualScheduler, RecordingLogSink};
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

    /// Leaves `i010300` in the replica where the B+tree can't find it.
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
    // Panics today: "Row already exists" in MemorySource.
    client.mutate("insert", [10300]);
    assert!(has(&view, 10300));
}
