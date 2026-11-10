//! The Rust `zero-rust codegen` generates (packages/zero-rust-cli), checked
//! in so it compiles with the workspace's lints. `pnpm --filter
//! zero-rust-cli generate` regenerates it; its test fails when it is stale.

#[rustfmt::skip]
pub mod fixture;
pub mod sweep;
#[rustfmt::skip]
pub mod test_schema;
#[rustfmt::skip]
pub mod zbugs;
