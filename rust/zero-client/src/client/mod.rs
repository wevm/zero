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
mod inspector;
mod ivm_branch;
mod log;
mod measure_push;
mod mutation_tracker;
mod planner_debug;
mod poke_handler;
mod query_context;
mod query_manager;
mod replica_store;
mod scheduler;
mod socket;
#[cfg(feature = "sqlite")]
mod sqlite;
mod storage;
mod transaction;
mod zero;

pub use connection_manager::{ConnectionState, ConnectionStatus, NeedsAuthReason};
pub use engine::{
    BatchViewUpdates, MutationOutcome, MutatorResult, StorageFailureHandler, UpdateNeededHandler,
    UpdateNeededReason, ZeroOptions,
};
pub use error::*;
pub use inspector::{
    AnalyzeQueryOptions, AnalyzeQueryResult, Inspector, InspectorClient, InspectorClientGroup,
    InspectorQuery, Metrics, PlanDebugEvent, RowCountsBySource, RowsByQuery, RowsBySource,
};
pub use log::{LogLevel, LogSink, StderrLogSink};
pub use query_context::{MaterializedView, Preload, RunType, ViewListener};
pub use scheduler::*;
pub use socket::{SocketConnection, SocketEvents, SocketFactory, SocketMessage};
pub use storage::*;
pub use transaction::{Reason, TableMutator, Transaction};
pub use zero::{Connection, MutateRequest, Mutator, Zero};
