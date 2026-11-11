//! The client engine (`packages/zero-client/src/client`): the connection,
//! the replica and its pending mutations, and the query and mutation
//! bookkeeping, on memory storage.
//!
//! Everything runs synchronously on one thread, driven by a [`Scheduler`]
//! and the socket events of a [`SocketFactory`]; see `event_loop`.

mod connection_manager;
mod database;
mod engine;
mod error;
mod event_loop;
mod ivm_branch;
mod log;
mod mutation_tracker;
mod poke_handler;
mod query_context;
mod query_manager;
mod replica_store;
mod scheduler;
mod socket;
mod transaction;
mod zero;

pub use connection_manager::{ConnectionState, ConnectionStatus, NeedsAuthReason};
pub use engine::{
    MutationOutcome, MutatorResult, UpdateNeededHandler, UpdateNeededReason, ZeroOptions,
};
pub use error::*;
pub use log::{LogLevel, LogSink, StderrLogSink};
pub use query_context::{MaterializedView, Preload, RunType, ViewListener};
pub use scheduler::*;
pub use socket::{SocketConnection, SocketEvents, SocketFactory, SocketMessage};
pub use transaction::{Reason, TableMutator, Transaction};
pub use zero::{Connection, Mutator, Zero};
