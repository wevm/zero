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
//!
//! # Storage
//!
//! **A default build persists nothing across restarts**: its storage is
//! [`ZeroStorage::Memory`](client::ZeroStorage::Memory), kept for the life
//! of the thread by database name, like JS `kvStore: 'mem'`. SQLite storage,
//! which keeps synced rows and pending mutations across restarts, is behind
//! the `sqlite` feature, and an app can bring its own store with
//! [`ZeroStorage::Custom`](client::ZeroStorage::Custom).

pub mod ast;
pub mod builder;
pub mod client;
pub mod compare;
pub mod decode;
pub mod format;
pub mod hash;
pub mod ivm;
pub mod json;
pub mod keys;
pub mod like;
pub mod map;
pub mod protocol;
pub mod query;
pub mod schema;
pub mod sort;
pub mod strings;
pub mod testing;
#[cfg(feature = "tokio-transport")]
pub mod tokio_transport;
pub mod ttl;
pub mod typed;
