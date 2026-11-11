//! zbugs on the Zero Rust client, for the Rust zbugs apps
//! (`apps/zbugs-gpui`, `apps/zbugs-rune`): the schema `zero-rust codegen`
//! generates from `apps/zbugs/shared/schema.ts`, and ports of its queries
//! (`queries.ts`), mutators (`mutators.ts`), and auth checks (`auth.ts`).
//!
//! The drift test (`tests/drift.rs`) checks the ports against what the TS
//! definitions do, with the vectors zbugs-swift's drift test reads too.
//! It has no UI and no transport: each app brings its own.

pub mod args;
pub mod auth;
pub mod mutators;
pub mod queries;
#[rustfmt::skip]
pub mod schema;

pub use auth::{AuthData, make_id};
pub use mutators::{Context, MutateRequest, client_mutators};
pub use queries::ZERO_PROJECT_NAME;
