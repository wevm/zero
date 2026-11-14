//! A native Rust client for [Zero](https://zero.rocicorp.dev).
//!
//! A port of the JS client (`packages/zero-client`),
//! checked against the JS implementation by the conformance vectors in
//! `packages/zero-conformance`. See `rust/PARITY.md` for what is ported so
//! far and what is planned.
//!
//! # Features
//!
//! The default build has no dependencies. It has the whole client,
//! [`client::Zero`], which an app drives with its own
//! [`Scheduler`](client::Scheduler) and
//! [`SocketFactory`](client::SocketFactory). Opt-in features add:
//!
//! - `tokio-transport`: the `tokio_transport` module, a scheduler and a
//!   WebSocket transport on tokio and `tokio-tungstenite`.
//! - `tls`: `wss://` for that transport, with rustls on `ring`.
//! - `sqlite`: SQLite storage, `ZeroStorage::Sqlite`, on the system
//!   `libsqlite3`; `sqlite-bundled` compiles SQLite in.
//! - `serde`: the `define` module, typed named queries and mutators
//!   (`define_query`, `define_mutator`) over serde argument types, and
//!   serde for `json::Value`.
//!
//! # Storage
//!
//! **A default build persists nothing across restarts**: its storage is
//! [`ZeroStorage::Memory`](client::ZeroStorage::Memory), kept for the life
//! of the thread by database name, like JS `kvStore: 'mem'`. SQLite storage,
//! which keeps synced rows and pending mutations across restarts, is behind
//! the `sqlite` feature, and an app can bring its own store with
//! [`ZeroStorage::Custom`](client::ZeroStorage::Custom).
//!
//! # Modules
//!
//! An app uses [`client`], [`typed`] (through generated code), [`query`],
//! [`ttl`], and [`testing`]; their items are all documented. The others
//! (`ast`, `ivm`, `protocol`, and the primitives) are the engine's parts,
//! public so the conformance runner can check each against JS.
//!
//! # Getting started
//!
//! A client is made from the app's schema (generated from its TypeScript
//! schema by `zero-rust codegen`), its mutators, and a scheduler and socket
//! factory for its event loop. Views stay current: when `mutate` returns,
//! they show its writes. This one runs without a server, on the manual
//! clock tests use; an app passes a `cache_url` and the
//! [`tokio_transport`](crate#features) types instead.
//!
//! ```
//! use zero_client::ast::System;
//! use zero_client::client::{Mutator, Zero, ZeroOptions};
//! use zero_client::json::{self, Object, Value};
//! use zero_client::query::AnyQuery;
//! use zero_client::schema::Schema;
//! use zero_client::testing::ManualScheduler;
//! use zero_client::ttl::Ttl;
//! use std::sync::Arc;
//!
//! let schema = Arc::new(Schema::from_json(&json::parse(
//!     r#"{"tables": {"issue": {"name": "issue", "primaryKey": ["id"],
//!         "columns": {"id": {"type": "string"}, "title": {"type": "string"}}}}}"#,
//! )?)?);
//!
//! // Runs on the client now, and on the server's API when it syncs.
//! let create = Mutator::new("issue.create", |tx, args, _ctx: &()| {
//!     let mut row = Object::new();
//!     row.insert("id", args.and_then(|a| a.get("id")).cloned().unwrap_or_default());
//!     row.insert("title", Value::from("Hello"));
//!     tx.table("issue").insert(&row)?;
//!     Ok(None)
//! });
//!
//! let scheduler = ManualScheduler::new(1_700_000_000_000.0);
//! let zero = Zero::new(schema.clone(), (), vec![create], ZeroOptions::default(), scheduler, None)?;
//!
//! let issues = AnyQuery::new(schema, "issue", System::Client);
//! let view = zero.materialize(&issues, &Ttl::default())?;
//! let mut args = Object::new();
//! args.insert("id", Value::from("i1"));
//! zero.mutate("issue.create", Some(Value::from(args)));
//!
//! let rows = view.data();
//! assert_eq!(rows.entries().len(), 1);
//! assert_eq!(rows.entries()[0].row.get("title"), Some(&Value::from("Hello")));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! With generated types, the query is `zql::issue()`, and a view's data
//! reads as row structs with `query.read(&view.data())`; see the
//! [README](https://github.com/wevm/zero/blob/main/rust/README.md#generated-types).

pub mod ast;
pub mod builder;
#[warn(missing_docs)]
pub mod client;
pub mod compare;
pub mod decode;
#[cfg(feature = "serde")]
#[warn(missing_docs)]
pub mod define;
pub mod format;
pub mod hash;
pub mod ivm;
pub mod json;
pub mod keys;
pub mod like;
pub mod map;
pub mod protocol;
#[warn(missing_docs)]
pub mod query;
pub mod schema;
pub mod sort;
pub mod strings;
#[warn(missing_docs)]
pub mod tdigest;
#[warn(missing_docs)]
pub mod testing;
#[cfg(feature = "tokio-transport")]
#[warn(missing_docs)]
pub mod tokio_transport;
#[warn(missing_docs)]
pub mod ttl;
#[warn(missing_docs)]
pub mod typed;
