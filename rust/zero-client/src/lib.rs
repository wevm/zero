//! A native Rust client for [Zero](https://zero.rocicorp.dev).
//!
//! This is the start of a port of the JS client (`packages/zero-client`),
//! checked against the JS implementation by the conformance vectors in
//! `packages/zero-conformance`. See `rust/PARITY.md` for what is ported so
//! far and what is planned.

pub mod ast;
pub mod compare;
pub mod decode;
pub mod format;
pub mod hash;
pub mod json;
pub mod keys;
pub mod like;
pub mod protocol;
pub mod schema;
pub mod sort;
pub mod strings;
pub mod ttl;
