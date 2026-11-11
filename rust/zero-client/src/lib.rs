//! A native Rust client for [Zero](https://zero.rocicorp.dev).
//!
//! This is the start of a port of the JS client (`packages/zero-client`),
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
//!
//! # Storage
//!
//! **The client persists nothing across restarts yet**: its storage is
//! memory only, kept for the life of the process by database name, like JS
//! `kvStore: 'mem'`. SQLite storage, which keeps synced rows and pending
//! mutations across restarts, comes behind the opt-in `sqlite` feature, and
//! an app will be able to bring its own store.

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
