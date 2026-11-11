//! SQLite storage, behind the `sqlite` feature: a database file per
//! database name in a directory, in zero-swift's layout
//! (`SQLiteDatabase.swift`, `SQLiteStore`), so a JS-named database maps to
//! one file whichever client wrote it.
//!
//! Each open database has a thread that owns its connection: the client's
//! thread sends it writes in order and waits only for reads, flushes, and
//! closes. WAL mode, `synchronous = NORMAL`, one transaction per write.

use super::storage::{
    ClientGroupRecord, ClientIdPair, ClientRecord, DroppedDatabases, LoadError, StorageBackend,
    StorageFailure, StorageFailureKind, StorageOp, StoredClients, StoredDatabase, StoredGroup,
    StoredMutation,
};
use crate::hash::xxhash32;
use crate::json::{self, Object, Value};
use crate::strings;
use rusqlite::{Connection, ErrorCode, OptionalExtension, params};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// The version of the file layout. A new one starts a new file, as a new
/// JS storage version starts a new database.
const LAYOUT_VERSION: u32 = 1;
/// How long a write waits for another connection's lock.
const BUSY_TIMEOUT: Duration = Duration::from_millis(5000);
/// How long opening and loading wait for one: a lock held by another
/// program fails them fast, and the client falls back to memory.
const OPEN_BUSY_TIMEOUT: Duration = Duration::from_millis(250);
const RETIRED_MARKER: &str = ".collected-";

/// The files open in this process, by path, and how many times: the
/// database GC leaves them alone.
fn open_files() -> &'static Mutex<HashMap<PathBuf, usize>> {
    static OPEN: OnceLock<Mutex<HashMap<PathBuf, usize>>> = OnceLock::new();
    OPEN.get_or_init(Mutex::default)
}

fn lock_open_files() -> std::sync::MutexGuard<'static, HashMap<PathBuf, usize>> {
    open_files().lock().unwrap_or_else(|e| e.into_inner())
}

fn failure(e: &rusqlite::Error) -> StorageFailure {
    let kind = match e.sqlite_error_code() {
        Some(ErrorCode::DiskFull) => StorageFailureKind::Full,
        Some(
            ErrorCode::CannotOpen
            | ErrorCode::NotADatabase
            | ErrorCode::DatabaseCorrupt
            | ErrorCode::PermissionDenied
            | ErrorCode::ReadOnly,
        ) => StorageFailureKind::CannotOpen,
        _ => StorageFailureKind::IoError,
    };
    StorageFailure::new(kind, e.to_string())
}

fn is_busy(e: &rusqlite::Error) -> bool {
    matches!(
        e.sqlite_error_code(),
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}

fn io(message: impl Into<String>) -> StorageFailure {
    StorageFailure::new(StorageFailureKind::IoError, message)
}

/// The default database directory.
fn default_directory() -> Result<PathBuf, StorageFailure> {
    let env = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    let no_home = || StorageFailure::new(StorageFailureKind::CannotOpen, "HOME is not set");
    if cfg!(target_os = "macos") {
        let home = env("HOME").ok_or_else(no_home)?;
        return Ok(Path::new(&home).join("Library/Application Support/Zero"));
    }
    if cfg!(windows) {
        let data = env("LOCALAPPDATA").ok_or_else(|| {
            StorageFailure::new(StorageFailureKind::CannotOpen, "LOCALAPPDATA is not set")
        })?;
        return Ok(Path::new(&data).join("Zero"));
    }
    if let Some(data) = env("XDG_DATA_HOME") {
        return Ok(Path::new(&data).join("zero"));
    }
    let home = env("HOME").ok_or_else(no_home)?;
    Ok(Path::new(&home).join(".local/share/zero"))
}

/// The file a database is stored in: names can hold any character.
fn file_name(name: &str) -> String {
    format!(
        "zero-v{LAYOUT_VERSION}-{}.sqlite",
        super::database::base36(xxhash32::h64(name))
    )
}

/// The layout version in a file name `zero-v<version>-<hash>.sqlite`.
fn layout_version(file: &str) -> Option<u32> {
    file.strip_prefix("zero-v")?.split('-').next()?.parse().ok()
}

/// The Replicache format version in `rep:<name>:<format>:<schemaVersion>`.
fn format_version(name: &str) -> Option<u32> {
    let parts: Vec<&str> = name.split(':').collect();
    if !name.starts_with("rep:") || parts.len() < 4 {
        return None;
    }
    parts[parts.len() - 2].parse().ok()
}

fn database_files(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.starts_with("zero-") && n.ends_with(".sqlite"))
        })
        .map(|e| e.path())
        .collect();
    files.sort();
    files
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// Deletes a database file with its WAL and shared memory. False when one
/// that is there can't be deleted.
fn unlink_database(path: &Path) -> bool {
    let mut deleted = true;
    for suffix in ["", "-wal", "-shm"] {
        if let Err(e) = std::fs::remove_file(with_suffix(path, suffix))
            && e.kind() != std::io::ErrorKind::NotFound
        {
            deleted = false;
        }
    }
    deleted
}

/// The name a database file holds; `None` for a file that isn't one of
/// ours.
fn stored_name(path: &Path) -> Option<String> {
    let db = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    db.query_row("SELECT value FROM meta WHERE key = 'name'", [], |r| {
        r.get(0)
    })
    .ok()
}

/// The profile ID of every database in `directory`, made the first time
/// (JS keeps one per browser profile).
fn profile_id(directory: &Path) -> Result<String, StorageFailure> {
    let path = directory.join("profile-id");
    if let Ok(id) = std::fs::read_to_string(&path) {
        let id = id.trim();
        if !id.is_empty() {
            return Ok(id.to_owned());
        }
    }
    let id = format!("p{}", super::engine::random_id());
    std::fs::write(&path, &id).map_err(|e| io(format!("Cannot write {}: {e}", path.display())))?;
    Ok(id)
}

/// A directory of SQLite databases.
pub(crate) struct SqliteBackend {
    directory: PathBuf,
}

impl SqliteBackend {
    pub(crate) fn new(directory: Option<PathBuf>) -> Result<SqliteBackend, StorageFailure> {
        let directory = match directory {
            Some(d) => d,
            None => default_directory()?,
        };
        std::fs::create_dir_all(&directory).map_err(|e| {
            StorageFailure::new(
                StorageFailureKind::CannotOpen,
                format!("Cannot create {}: {e}", directory.display()),
            )
        })?;
        Ok(SqliteBackend { directory })
    }

    /// The location of the backend of `directory`, without creating it.
    pub(crate) fn location_of(directory: Option<&Path>) -> String {
        let dir = directory
            .map(Path::to_path_buf)
            .or_else(|| default_directory().ok())
            .unwrap_or_default();
        format!("sqlite:{}", dir.display())
    }
}

impl StorageBackend for SqliteBackend {
    fn location(&self) -> String {
        Self::location_of(Some(&self.directory))
    }

    fn open(&self, name: &str) -> Result<Box<dyn StoredDatabase>, LoadError> {
        let path = self.directory.join(file_name(name));
        let connection = open_connection(&path, name).map_err(|e| failure(&e))?;
        let profile_id = profile_id(&self.directory)?;
        Ok(Box::new(SqliteDatabase::start(
            path, connection, profile_id,
        )))
    }

    fn drop_databases(&self, matches: &dyn Fn(&str) -> bool) -> DroppedDatabases {
        let mut result = DroppedDatabases::default();
        for path in database_files(&self.directory) {
            let open = lock_open_files();
            if open.get(&path).is_some_and(|n| *n > 0) {
                continue;
            }
            let Some(name) = stored_name(&path) else {
                continue;
            };
            if !matches(&name) {
                continue;
            }
            if unlink_database(&path) {
                result.deleted.push(name);
            } else {
                result
                    .errors
                    .push(io(format!("Cannot delete {}", path.display())));
            }
        }
        result
    }

    /// Finds the other databases of the directory no client used for
    /// `max_age` on a background thread, and moves each out of the way
    /// before deleting it, so a client opening one meanwhile starts a new
    /// file rather than writing to a deleted one. Only databases of ours
    /// are considered: of this layout version or an older one, holding a
    /// name, of a format this client knows (`gatherDatabaseInfoForCollect`
    /// never collects a newer format).
    fn collect_databases(&self, except: &str, now: f64, max_age: f64) {
        let directory = self.directory.clone();
        let own = directory.join(file_name(except));
        std::thread::spawn(move || {
            delete_retired(&directory);
            for path in database_files(&directory) {
                if path == own {
                    continue;
                }
                let file = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if layout_version(file).is_none_or(|v| v > LAYOUT_VERSION) {
                    continue;
                }
                // Opening a file takes this lock, so none opens while this
                // one is judged and moved.
                let open = lock_open_files();
                if open.get(&path).is_some_and(|n| *n > 0) {
                    continue;
                }
                if is_collectable(&path, now, max_age) {
                    retire(&path);
                }
                drop(open);
            }
        });
    }
}

fn is_collectable(path: &Path, now: f64, max_age: f64) -> bool {
    let Ok(db) = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)
    else {
        return false;
    };
    let _ = db.busy_timeout(OPEN_BUSY_TIMEOUT);
    let Ok(name) = db.query_row("SELECT value FROM meta WHERE key = 'name'", [], |r| {
        r.get::<_, String>(0)
    }) else {
        // Not a database of ours, or busy: leave it.
        return false;
    };
    if format_version(&name).is_none_or(|f| f > super::database::FORMAT_VERSION) {
        return false;
    }
    match db.query_row("SELECT max(heartbeat) FROM clients", [], |r| {
        r.get::<_, Option<f64>>(0)
    }) {
        Ok(heartbeat) => heartbeat.is_none_or(|h| now - h >= max_age),
        Err(_) => false,
    }
}

/// Moves the database at `path` out of the way and deletes it.
fn retire(path: &Path) {
    let retired = with_suffix(
        path,
        &format!("{RETIRED_MARKER}{}", super::engine::random_id()),
    );
    // The WAL first: a database without its WAL loses the latest commits,
    // a new one with an old WAL would be worse.
    for suffix in ["-wal", "-shm", ""] {
        let _ = std::fs::rename(with_suffix(path, suffix), with_suffix(&retired, suffix));
    }
    unlink_database(&retired);
}

/// Deletes the databases retired before the app last stopped.
fn delete_retired(directory: &Path) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for e in entries.filter_map(|e| e.ok()) {
        if e.file_name()
            .to_str()
            .is_some_and(|n| n.starts_with("zero-") && n.contains(RETIRED_MARKER))
        {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

fn open_connection(path: &Path, name: &str) -> rusqlite::Result<Connection> {
    let db = Connection::open(path)?;
    db.busy_timeout(OPEN_BUSY_TIMEOUT)?;
    db.execute_batch(
        "PRAGMA journal_mode = WAL;
        PRAGMA synchronous = NORMAL;
        CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;
        CREATE TABLE IF NOT EXISTS client_groups (
          seq INTEGER PRIMARY KEY AUTOINCREMENT,
          id TEXT NOT NULL UNIQUE,
          mutator_names TEXT NOT NULL,
          disabled INTEGER NOT NULL,
          cookie TEXT NOT NULL DEFAULT 'null',
          last_mutation_ids TEXT NOT NULL DEFAULT '{}'
        );
        CREATE TABLE IF NOT EXISTS clients (
          seq INTEGER PRIMARY KEY AUTOINCREMENT,
          id TEXT NOT NULL UNIQUE,
          client_group_id TEXT NOT NULL,
          heartbeat REAL NOT NULL
        );
        CREATE TABLE IF NOT EXISTS deleted_clients (
          client_group_id TEXT NOT NULL,
          client_id TEXT NOT NULL,
          PRIMARY KEY (client_group_id, client_id)
        ) WITHOUT ROWID;
        CREATE TABLE IF NOT EXISTS entries (
          client_group_id TEXT NOT NULL,
          key TEXT NOT NULL,
          value TEXT NOT NULL,
          PRIMARY KEY (client_group_id, key)
        ) WITHOUT ROWID;
        CREATE TABLE IF NOT EXISTS mutations (
          seq INTEGER PRIMARY KEY AUTOINCREMENT,
          client_group_id TEXT NOT NULL,
          client_id TEXT NOT NULL,
          id REAL NOT NULL,
          name TEXT NOT NULL,
          args TEXT NOT NULL,
          timestamp REAL NOT NULL
        );",
    )?;
    // Added for synced aggregates; a database zero-swift made before them
    // lacks it.
    let has_writes: bool = db.query_row(
        "SELECT count(*) FROM pragma_table_info('mutations') WHERE name = 'aggregate_writes'",
        [],
        |r| r.get::<_, i64>(0).map(|n| n > 0),
    )?;
    if !has_writes {
        db.execute_batch(
            "ALTER TABLE mutations ADD COLUMN aggregate_writes TEXT NOT NULL DEFAULT '[]'",
        )?;
    }
    db.execute(
        "INSERT INTO meta (key, value) VALUES ('name', ?1) ON CONFLICT (key) DO NOTHING",
        [name],
    )?;
    Ok(db)
}

enum Request {
    Load(mpsc::Sender<Result<StoredClients, LoadError>>),
    LoadGroup(String, mpsc::Sender<Result<StoredGroup, LoadError>>),
    Write(Vec<StorageOp>),
    Flush(mpsc::Sender<Result<(), StorageFailure>>),
    Close(mpsc::Sender<Result<(), StorageFailure>>),
    Drop(mpsc::Sender<()>),
}

/// An open SQLite database and the thread that owns its connection.
struct SqliteDatabase {
    requests: mpsc::Sender<Request>,
    /// Set once a write failed: the writes after it are dropped.
    failed: Arc<Mutex<Option<StorageFailure>>>,
    path: PathBuf,
    closed: std::cell::Cell<bool>,
}

impl SqliteDatabase {
    fn start(path: PathBuf, connection: Connection, profile_id: String) -> SqliteDatabase {
        *lock_open_files().entry(path.clone()).or_default() += 1;
        let (requests, receiver) = mpsc::channel();
        let failed = Arc::new(Mutex::new(None));
        let mut writer = Writer {
            db: Some(connection),
            path: path.clone(),
            failed: failed.clone(),
            retrying: Vec::new(),
            profile_id,
        };
        std::thread::spawn(move || {
            for request in receiver {
                if !writer.handle(request) {
                    break;
                }
            }
        });
        SqliteDatabase {
            requests,
            failed,
            path,
            closed: std::cell::Cell::new(false),
        }
    }

    fn failure(&self) -> Option<StorageFailure> {
        self.failed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn ask<T>(&self, request: impl FnOnce(mpsc::Sender<T>) -> Request) -> Option<T> {
        let (tx, rx) = mpsc::channel();
        self.requests.send(request(tx)).ok()?;
        rx.recv().ok()
    }

    fn closed_failure() -> StorageFailure {
        io("The database is closed")
    }

    /// The file is no longer open here, once.
    fn forget(&self) {
        if self.closed.replace(true) {
            return;
        }
        let mut open = lock_open_files();
        if let Some(n) = open.get_mut(&self.path) {
            *n -= 1;
            if *n == 0 {
                open.remove(&self.path);
            }
        }
    }
}

impl StoredDatabase for SqliteDatabase {
    fn load(&self) -> Result<StoredClients, LoadError> {
        self.ask(Request::Load)
            .unwrap_or_else(|| Err(Self::closed_failure().into()))
    }

    fn load_group(&self, id: &str) -> Result<StoredGroup, LoadError> {
        self.ask(|tx| Request::LoadGroup(id.to_owned(), tx))
            .unwrap_or_else(|| Err(Self::closed_failure().into()))
    }

    fn write(&self, ops: &[StorageOp]) -> Result<(), StorageFailure> {
        if let Some(f) = self.failure() {
            return Err(f);
        }
        if !ops.is_empty() {
            self.requests
                .send(Request::Write(ops.to_vec()))
                .map_err(|_| Self::closed_failure())?;
        }
        Ok(())
    }

    fn flush(&self) -> Result<(), StorageFailure> {
        self.ask(Request::Flush)
            .unwrap_or_else(|| Err(Self::closed_failure()))
    }

    fn close(&self) -> Result<(), StorageFailure> {
        let r = self.ask(Request::Close).unwrap_or(Ok(()));
        self.forget();
        r
    }

    fn drop_database(&self) {
        self.ask(Request::Drop);
        self.forget();
    }
}

impl Drop for SqliteDatabase {
    fn drop(&mut self) {
        if !self.closed.get() {
            let _ = self.close();
        }
    }
}

struct Writer {
    db: Option<Connection>,
    path: PathBuf,
    failed: Arc<Mutex<Option<StorageFailure>>>,
    /// The writes that found the database busy, written with the next.
    retrying: Vec<StorageOp>,
    profile_id: String,
}

impl Writer {
    fn failed(&self) -> bool {
        self.failed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    fn set_failed(&self, f: StorageFailure) {
        let mut failed = self.failed.lock().unwrap_or_else(|e| e.into_inner());
        if failed.is_none() {
            *failed = Some(f);
        }
    }

    /// Handles one request; false once the connection is gone.
    fn handle(&mut self, request: Request) -> bool {
        match request {
            Request::Load(reply) => {
                let r = match &self.db {
                    Some(db) => load(db, &self.profile_id),
                    None => Err(SqliteDatabase::closed_failure().into()),
                };
                if let Some(db) = &self.db {
                    let _ = db.busy_timeout(BUSY_TIMEOUT);
                }
                let _ = reply.send(r);
            }
            Request::LoadGroup(id, reply) => {
                let r = match &self.db {
                    Some(db) => load_group(db, &id),
                    None => Err(SqliteDatabase::closed_failure().into()),
                };
                let _ = reply.send(r);
            }
            Request::Write(ops) => self.write(ops),
            Request::Flush(reply) => {
                let _ = reply.send(self.write_kept());
            }
            Request::Close(reply) => {
                let r = self.write_kept();
                self.db = None;
                let _ = reply.send(r);
                return false;
            }
            Request::Drop(reply) => {
                self.db = None;
                unlink_database(&self.path);
                let _ = reply.send(());
                return false;
            }
        }
        true
    }

    /// Applies `ops` in one transaction, after the writes before. A
    /// database busy past the busy timeout is not a failure
    /// (`classifySQLiteError`): the ops are kept and written with the next
    /// write. After any other error nothing more is written.
    fn write(&mut self, ops: Vec<StorageOp>) {
        if self.failed() {
            return;
        }
        let Some(db) = &mut self.db else { return };
        let mut all = std::mem::take(&mut self.retrying);
        all.extend(ops);
        match transaction(db, &all) {
            Ok(()) => {}
            Err(e) if is_busy(&e) => self.retrying = all,
            Err(e) => self.set_failed(failure(&e)),
        }
    }

    /// Writes the ops kept because the database was busy, once more.
    fn write_kept(&mut self) -> Result<(), StorageFailure> {
        if let Some(f) = self
            .failed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            return Err(f);
        }
        let Some(db) = &mut self.db else {
            return Ok(());
        };
        if self.retrying.is_empty() {
            return Ok(());
        }
        let kept = std::mem::take(&mut self.retrying);
        match transaction(db, &kept) {
            Ok(()) => Ok(()),
            Err(e) => {
                let f = failure(&e);
                self.set_failed(f.clone());
                Err(f)
            }
        }
    }
}

fn transaction(db: &mut Connection, ops: &[StorageOp]) -> rusqlite::Result<()> {
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    for op in ops {
        apply(&tx, op)?;
    }
    tx.commit()
}

fn text(v: &Value) -> String {
    json::stringify(v)
}

fn number_record(lmids: &BTreeMap<String, f64>) -> Value {
    let mut o = Object::new();
    for (k, v) in lmids {
        o.insert(k.clone(), Value::Number(*v));
    }
    Value::Object(o)
}

/// A mutation's aggregate writes as `[[key, value or null], ...]`, by key.
fn encode_writes(writes: &[(String, Option<Value>)]) -> Value {
    let mut sorted: Vec<&(String, Option<Value>)> = writes.iter().collect();
    sorted.sort_by(|a, b| strings::utf16(&a.0, &b.0));
    Value::Array(
        sorted
            .into_iter()
            .map(|(k, v)| {
                Value::Array(vec![
                    Value::String(k.clone()),
                    v.clone().unwrap_or(Value::Null),
                ])
            })
            .collect(),
    )
}

fn decode_writes(v: &Value) -> Result<Vec<(String, Option<Value>)>, String> {
    let bad = || format!("Bad aggregate writes: {}", json::stringify(v));
    let pairs = v.as_array().ok_or_else(bad)?;
    pairs
        .iter()
        .map(|p| {
            let p = p.as_array().filter(|p| p.len() == 2).ok_or_else(bad)?;
            let key = p[0].as_str().ok_or_else(bad)?.to_owned();
            // A row is an object; null marks a deleted one.
            let value = (p[1] != Value::Null).then(|| p[1].clone());
            Ok((key, value))
        })
        .collect()
}

fn apply(db: &rusqlite::Transaction<'_>, op: &StorageOp) -> rusqlite::Result<()> {
    let delete_confirmed = |group: &str, lmids: &BTreeMap<String, f64>| {
        for (client_id, lmid) in lmids {
            db.execute(
                "DELETE FROM mutations WHERE client_group_id = ?1 AND client_id = ?2 AND id <= ?3",
                params![group, client_id, lmid],
            )?;
        }
        Ok::<_, rusqlite::Error>(())
    };
    match op {
        StorageOp::PutGroup(g) => {
            let names = Value::Array(g.mutator_names.iter().cloned().map(Value::String).collect());
            db.execute(
                "INSERT INTO client_groups (id, mutator_names, disabled) VALUES (?1, ?2, ?3)
                 ON CONFLICT (id) DO UPDATE SET disabled = excluded.disabled",
                params![g.id, text(&names), g.disabled as i64],
            )?;
        }
        StorageOp::DeleteGroup(id) => {
            db.execute("DELETE FROM client_groups WHERE id = ?1", [id])?;
            db.execute("DELETE FROM entries WHERE client_group_id = ?1", [id])?;
            db.execute("DELETE FROM mutations WHERE client_group_id = ?1", [id])?;
        }
        StorageOp::PutClient(c) => {
            db.execute(
                "INSERT INTO clients (id, client_group_id, heartbeat) VALUES (?1, ?2, ?3)
                 ON CONFLICT (id) DO UPDATE SET heartbeat = excluded.heartbeat",
                params![c.id, c.client_group_id, c.heartbeat],
            )?;
        }
        StorageOp::DeleteClients(ids) => {
            for id in ids {
                db.execute("DELETE FROM clients WHERE id = ?1", [id])?;
            }
        }
        StorageOp::SetDeletedClients(pairs) => {
            db.execute("DELETE FROM deleted_clients", [])?;
            for p in pairs {
                db.execute(
                    "INSERT OR IGNORE INTO deleted_clients (client_group_id, client_id) VALUES (?1, ?2)",
                    params![p.client_group_id, p.client_id],
                )?;
            }
        }
        StorageOp::UpdateSnapshot {
            group,
            changes,
            cleared,
            cookie,
            last_mutation_ids,
        } => {
            if *cleared {
                db.execute("DELETE FROM entries WHERE client_group_id = ?1", [group])?;
            }
            for (k, v) in changes {
                match v {
                    Some(v) => db.execute(
                        "INSERT INTO entries (client_group_id, key, value) VALUES (?1, ?2, ?3)
                         ON CONFLICT (client_group_id, key) DO UPDATE SET value = excluded.value",
                        params![group, k, text(v)],
                    )?,
                    None => db.execute(
                        "DELETE FROM entries WHERE client_group_id = ?1 AND key = ?2",
                        params![group, k],
                    )?,
                };
            }
            db.execute(
                "UPDATE client_groups SET cookie = ?1, last_mutation_ids = ?2 WHERE id = ?3",
                params![text(cookie), text(&number_record(last_mutation_ids)), group],
            )?;
            delete_confirmed(group, last_mutation_ids)?;
        }
        StorageOp::DeleteConfirmedMutations {
            group,
            last_mutation_ids,
        } => delete_confirmed(group, last_mutation_ids)?,
        StorageOp::AddMutation { group, mutation: m } => {
            db.execute(
                "INSERT INTO mutations (client_group_id, client_id, id, name, args, timestamp, aggregate_writes)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    group,
                    m.client_id,
                    m.id,
                    m.name,
                    text(&m.args),
                    m.timestamp,
                    text(&encode_writes(&m.aggregate_writes))
                ],
            )?;
        }
        StorageOp::SetAggregateWrites {
            group,
            client_id,
            id,
            writes,
        } => {
            db.execute(
                "UPDATE mutations SET aggregate_writes = ?1
                 WHERE client_group_id = ?2 AND client_id = ?3 AND id = ?4",
                params![text(&encode_writes(writes)), group, client_id, id],
            )?;
        }
    }
    Ok(())
}

/// A read error: a storage failure, unless what was read doesn't decode.
fn read_error(e: rusqlite::Error) -> LoadError {
    match e {
        rusqlite::Error::FromSqlConversionFailure(..)
        | rusqlite::Error::InvalidColumnType(..)
        | rusqlite::Error::IntegralValueOutOfRange(..) => LoadError::Corrupt(e.to_string()),
        e => LoadError::Failure(failure(&e)),
    }
}

fn parse(s: &str) -> Result<Value, LoadError> {
    json::parse(s).map_err(|e| LoadError::Corrupt(format!("{e}: {s}")))
}

fn load(db: &Connection, profile_id: &str) -> Result<StoredClients, LoadError> {
    let mut out = StoredClients {
        profile_id: profile_id.to_owned(),
        ..StoredClients::default()
    };
    let mut q = db
        .prepare("SELECT id, mutator_names, disabled FROM client_groups ORDER BY seq")
        .map_err(read_error)?;
    let rows = q
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })
        .map_err(read_error)?;
    for row in rows {
        let (id, names, disabled) = row.map_err(read_error)?;
        let names = parse(&names)?;
        let mutator_names = names
            .as_array()
            .ok_or_else(|| LoadError::Corrupt(format!("Bad mutator names of {id}")))?
            .iter()
            .filter_map(|n| n.as_str().map(str::to_owned))
            .collect();
        out.groups.push(ClientGroupRecord {
            id,
            mutator_names,
            disabled: disabled != 0,
        });
    }
    let mut q = db
        .prepare("SELECT id, client_group_id, heartbeat FROM clients ORDER BY seq")
        .map_err(read_error)?;
    for row in q
        .query_map([], |r| {
            Ok(ClientRecord {
                id: r.get(0)?,
                client_group_id: r.get(1)?,
                heartbeat: r.get(2)?,
            })
        })
        .map_err(read_error)?
    {
        out.clients.push(row.map_err(read_error)?);
    }
    let mut q = db
        .prepare("SELECT client_group_id, client_id FROM deleted_clients")
        .map_err(read_error)?;
    for row in q
        .query_map([], |r| {
            Ok(ClientIdPair {
                client_group_id: r.get(0)?,
                client_id: r.get(1)?,
            })
        })
        .map_err(read_error)?
    {
        out.deleted_clients.push(row.map_err(read_error)?);
    }
    Ok(out)
}

fn load_group(db: &Connection, id: &str) -> Result<StoredGroup, LoadError> {
    let mut g = StoredGroup::default();
    let head = db
        .query_row(
            "SELECT cookie, last_mutation_ids FROM client_groups WHERE id = ?1",
            [id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(read_error)?;
    if let Some((cookie, lmids)) = head {
        g.cookie = parse(&cookie)?;
        let lmids = parse(&lmids)?;
        for (k, v) in lmids.as_object().into_iter().flat_map(|o| o.iter()) {
            g.last_mutation_ids
                .insert(k.clone(), v.as_f64().unwrap_or(0.0));
        }
    }
    let mut q = db
        .prepare("SELECT key, value FROM entries WHERE client_group_id = ?1")
        .map_err(read_error)?;
    for row in q
        .query_map([id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(read_error)?
    {
        let (k, v) = row.map_err(read_error)?;
        g.entries.insert(k, parse(&v)?);
    }
    let mut q = db
        .prepare(
            "SELECT client_id, id, name, args, timestamp, aggregate_writes FROM mutations
             WHERE client_group_id = ?1 ORDER BY seq",
        )
        .map_err(read_error)?;
    for row in q
        .query_map([id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, f64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, f64>(4)?,
                r.get::<_, String>(5)?,
            ))
        })
        .map_err(read_error)?
    {
        let (client_id, mid, name, args, timestamp, writes) = row.map_err(read_error)?;
        g.mutations.push(StoredMutation {
            client_id,
            id: mid,
            name,
            args: parse(&args)?,
            timestamp,
            aggregate_writes: decode_writes(&parse(&writes)?).map_err(LoadError::Corrupt)?,
        });
    }
    Ok(g)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_of_files_and_formats() {
        let name = "rep:zero-u1-abc:7:52.xyz";
        assert!(file_name(name).starts_with("zero-v1-"));
        assert!(file_name(name).ends_with(".sqlite"));
        assert_eq!(layout_version(&file_name(name)), Some(1));
        assert_eq!(layout_version("zero-v12-x.sqlite"), Some(12));
        assert_eq!(layout_version("other.sqlite"), None);
        assert_eq!(format_version(name), Some(7));
        assert_eq!(format_version("rep:a:b"), None);
        assert_eq!(super::super::database::replicache_name(name), "zero-u1-abc");
    }

    #[test]
    fn aggregate_writes_round_trip_sorted_with_deletes() {
        let mut row = Object::new();
        row.insert("count", Value::Number(2.0));
        let writes = vec![
            ("b".to_owned(), None),
            ("a".to_owned(), Some(Value::Object(row))),
        ];
        let encoded = encode_writes(&writes);
        assert_eq!(
            json::stringify(&encoded),
            r#"[["a",{"count":2}],["b",null]]"#
        );
        let decoded = decode_writes(&encoded).unwrap();
        assert_eq!(decoded, vec![writes[1].clone(), writes[0].clone()]);
        assert!(decode_writes(&Value::from("x")).is_err());
    }
}
