//! Where a client keeps its replica: its client groups, clients, and each
//! group's snapshot and pending mutations, what Replicache keeps in
//! IndexedDB (zero-swift's `ZeroStorage`).
//!
//! The client owns everything but the bytes: it keeps the client groups and
//! clients in memory, makes every decision in JS's order, and hands a
//! [`StorageBackend`] an ordered stream of [`StorageOp`]s to write. The
//! memory storage is in the core; SQLite is behind the `sqlite` feature; an
//! app can bring its own backend with [`ZeroStorage::Custom`].

use crate::json::Value;
use crate::strings;
use std::collections::BTreeMap;
use std::fmt;
use std::rc::Rc;

/// Where a client keeps its replica (`ZeroStorage` in zero-swift,
/// `kvStore` in JS).
#[derive(Clone, Default)]
pub enum ZeroStorage {
    /// Memory only, kept for the life of the thread: a client created later
    /// with the same options finds what an earlier one stored, as with JS
    /// `kvStore: 'mem'`. **Nothing survives a restart.**
    #[default]
    Memory,
    /// A SQLite database file per database name in `directory`. The default
    /// directory is `~/Library/Application Support/Zero` on macOS,
    /// `%LOCALAPPDATA%\Zero` on Windows, and `$XDG_DATA_HOME/zero` (or
    /// `~/.local/share/zero`) elsewhere.
    #[cfg(feature = "sqlite")]
    Sqlite {
        directory: Option<std::path::PathBuf>,
    },
    /// A store the app brings.
    Custom(Rc<dyn StorageBackend>),
}

impl fmt::Debug for ZeroStorage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ZeroStorage::Memory => f.write_str("Memory"),
            #[cfg(feature = "sqlite")]
            ZeroStorage::Sqlite { directory } => f
                .debug_struct("Sqlite")
                .field("directory", directory)
                .finish(),
            ZeroStorage::Custom(b) => write!(f, "Custom({})", b.location()),
        }
    }
}

/// A failure of the local storage, rather than of its data
/// (`StorageFailureError`): the client keeps running on memory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageFailure {
    pub kind: StorageFailureKind,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StorageFailureKind {
    /// The device is out of space.
    Full,
    /// The database can't be opened.
    CannotOpen,
    /// A read or write failed.
    IoError,
}

impl StorageFailureKind {
    pub fn as_str(self) -> &'static str {
        match self {
            StorageFailureKind::Full => "full",
            StorageFailureKind::CannotOpen => "cannot-open",
            StorageFailureKind::IoError => "io-error",
        }
    }
}

impl StorageFailure {
    pub fn new(kind: StorageFailureKind, message: impl Into<String>) -> Self {
        StorageFailure {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for StorageFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind.as_str(), self.message)
    }
}

impl std::error::Error for StorageFailure {}

/// Why a database couldn't be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadError {
    /// The storage failed: the client runs on memory.
    Failure(StorageFailure),
    /// What it holds doesn't decode, so the client state is lost
    /// (`InvalidRefCountError` in JS): the client drops the database and
    /// starts a new one.
    Corrupt(String),
}

impl From<StorageFailure> for LoadError {
    fn from(f: StorageFailure) -> Self {
        LoadError::Failure(f)
    }
}

/// What [`Zero::delete`](crate::client::Zero::delete) and
/// [`drop_all_databases`] deleted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DroppedDatabases {
    /// The names of the databases deleted.
    pub deleted: Vec<String>,
    /// The databases that couldn't be deleted.
    pub errors: Vec<StorageFailure>,
}

/// A client group: the clients that share a replica because they register
/// the same mutators (`ClientGroup`).
#[derive(Clone, Debug, PartialEq)]
pub struct ClientGroupRecord {
    pub id: String,
    pub mutator_names: Vec<String>,
    /// The server lost the group, so no new client joins it.
    pub disabled: bool,
}

/// A client that used the database (`ClientV6`).
#[derive(Clone, Debug, PartialEq)]
pub struct ClientRecord {
    pub id: String,
    pub client_group_id: String,
    /// When the client last showed it was running, in ms since 1970.
    pub heartbeat: f64,
}

/// A deleted client the server may not know about yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientIdPair {
    pub client_group_id: String,
    pub client_id: String,
}

impl ClientIdPair {
    /// `normalizeDeletedClients`: sorted by group then client, deduplicated.
    pub fn normalize(mut pairs: Vec<ClientIdPair>) -> Vec<ClientIdPair> {
        pairs.sort_by(|a, b| {
            strings::utf16(&a.client_group_id, &b.client_group_id)
                .then_with(|| strings::utf16(&a.client_id, &b.client_id))
        });
        pairs.dedup();
        pairs
    }
}

/// The synced aggregate rows (`a/` keys) a mutation's last run wrote, with
/// their values after it (`None` for deleted).
pub type AggregateWrites = Vec<(String, Option<Value>)>;

/// A mutation that ran on a client and the server has not confirmed.
#[derive(Clone, Debug, PartialEq)]
pub struct StoredMutation {
    pub client_id: String,
    pub id: f64,
    pub name: String,
    pub args: Value,
    pub timestamp: f64,
    /// JS persists these in the mutation's commit; the client runs the
    /// mutation again at launch, before any query registers the aggregates
    /// it moves, and puts these back.
    pub aggregate_writes: AggregateWrites,
}

/// What a client group keeps: the last server snapshot and the mutations
/// not yet in it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StoredGroup {
    pub entries: BTreeMap<String, Value>,
    pub cookie: Value,
    pub last_mutation_ids: BTreeMap<String, f64>,
    /// In the order they were added.
    pub mutations: Vec<StoredMutation>,
}

/// What a database holds besides the groups' data, read when it opens.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StoredClients {
    /// In the order they were added: a client joins the first that fits.
    pub groups: Vec<ClientGroupRecord>,
    pub clients: Vec<ClientRecord>,
    pub deleted_clients: Vec<ClientIdPair>,
    /// The profile ID of the device (`profileID`): one for every database
    /// of the backend.
    pub profile_id: String,
}

/// A change to the database, applied in order.
#[derive(Clone, Debug, PartialEq)]
pub enum StorageOp {
    /// Adds the group, or updates whether it is disabled.
    PutGroup(ClientGroupRecord),
    /// Deletes the group with its snapshot and mutations.
    DeleteGroup(String),
    /// Adds the client, or updates its heartbeat.
    PutClient(ClientRecord),
    DeleteClients(Vec<String>),
    /// Replaces the deleted clients.
    SetDeletedClients(Vec<ClientIdPair>),
    /// The snapshot after a poke: the keys it changed (`None` for deleted),
    /// whether it cleared every key first, and its cookie and last mutation
    /// IDs. Drops the mutations the IDs confirm.
    UpdateSnapshot {
        group: String,
        changes: Vec<(String, Option<Value>)>,
        cleared: bool,
        cookie: Value,
        last_mutation_ids: BTreeMap<String, f64>,
    },
    AddMutation {
        group: String,
        mutation: StoredMutation,
    },
    /// What a rebase of a pending mutation wrote to synced aggregate rows.
    SetAggregateWrites {
        group: String,
        client_id: String,
        id: f64,
        writes: AggregateWrites,
    },
    /// Drops the mutations the IDs confirm, keeping the snapshot.
    DeleteConfirmedMutations {
        group: String,
        last_mutation_ids: BTreeMap<String, f64>,
    },
}

/// A place that keeps a client's databases, such as a directory of SQLite
/// files. Every call is on the client's thread.
pub trait StorageBackend {
    /// Where the databases are, such as `sqlite:/path/to/dir`. Clients on
    /// this thread with the same location and database name share one open
    /// database, as JS tabs share one IndexedDB database.
    fn location(&self) -> String;

    /// Opens the database `name`, a new empty one if there is none. `name`
    /// can hold any character.
    fn open(&self, name: &str) -> Result<Box<dyn StoredDatabase>, LoadError>;

    /// Deletes the databases that aren't open whose name `matches`.
    fn drop_databases(&self, matches: &dyn Fn(&str) -> bool) -> DroppedDatabases;

    /// Deletes, in the background, the databases other than `except` that
    /// no client used for `max_age` ms since `now` (`collectIDBDatabases`),
    /// such as those of an older schema. Optional.
    fn collect_databases(&self, except: &str, now: f64, max_age: f64) {
        let _ = (except, now, max_age);
    }
}

/// An open database of a [`StorageBackend`].
///
/// Writes may be done later, such as on another thread, but in order. A
/// failed write stops the writes after it, and is returned by the next
/// call that can return it; the client then runs on what it has in memory
/// and calls `ZeroOptions::on_storage_failure`.
pub trait StoredDatabase {
    /// The client groups and clients, read once when the database opens.
    fn load(&self) -> Result<StoredClients, LoadError>;

    /// The snapshot and pending mutations of group `id`; an empty group
    /// when it has none.
    fn load_group(&self, id: &str) -> Result<StoredGroup, LoadError>;

    /// Writes `ops`, in order, after the writes before, as one transaction.
    /// Returns the failure of an earlier write.
    fn write(&self, ops: &[StorageOp]) -> Result<(), StorageFailure>;

    /// Waits for the writes so far.
    fn flush(&self) -> Result<(), StorageFailure>;

    /// Closes the database once the writes so far are done.
    fn close(&self) -> Result<(), StorageFailure>;

    /// Deletes the database, after the writes before. Later writes do
    /// nothing.
    fn drop_database(&self);
}

/// Deletes every Zero database in `storage`, of every user
/// (`dropAllDatabases`). Clients still open on one keep running on what
/// they have in memory, and their later writes are dropped.
pub fn drop_all_databases(storage: &ZeroStorage) -> DroppedDatabases {
    super::database::ReplicaDatabase::drop_matching(storage, &|_| true)
}
