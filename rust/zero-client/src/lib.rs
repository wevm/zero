//! A native Rust client for [Zero](https://zero.rocicorp.dev).
//!
//! This is the start of a port of the JS client (`packages/zero-client`),
//! checked against the JS implementation by the conformance vectors in
//! `packages/zero-conformance`. See `rust/DESIGN.md` for what is ported so
//! far and what is planned.

pub mod compare;
pub mod hash;
pub mod json;
pub mod keys;
pub mod like;
pub mod strings;
pub mod ttl;
