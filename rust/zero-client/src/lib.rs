//! A native Rust client for [Zero](https://zero.rocicorp.dev).
//!
//! This is the start of a port of the JS client (`packages/zero-client`),
//! checked against the JS implementation by the conformance vectors in
//! `packages/zero-conformance`. See `rust/PARITY.md` for what is ported so
//! far and what is planned.
//!
//! # Storage
//!
//! **A default build persists nothing across restarts**: its storage is
//! memory only, and the crate has no dependencies. SQLite storage, which
//! keeps synced rows and pending mutations across restarts, comes behind the
//! opt-in `sqlite` feature, and an app can bring its own store. The quick
//! start, `cargo add zero-client --features tokio-transport,tls,sqlite`,
//! enables it. (Storage and the transport arrive with the client engine.)

pub mod ast;
pub mod builder;
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
pub mod ttl;
