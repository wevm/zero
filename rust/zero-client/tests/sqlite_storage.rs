//! SQLite storage on disk: what a client wrote is what the next one reads
//! (checked by editing the file in between), a file that doesn't decode is
//! dropped as lost client state, a store that can't open falls back to
//! memory, `delete` removes the file, the database GC deletes old files and
//! keeps current ones, and the default storage's first open moves its
//! database from the folder every app shared.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use zero_client::ast::System;
use zero_client::client::{
    Mutator, Transaction, Zero, ZeroOptions, ZeroStorage, drop_all_databases,
    set_default_sqlite_directories,
};
use zero_client::json::{self, Object, Value};
use zero_client::query::AnyQuery;
use zero_client::schema::Schema;
use zero_client::testing::ManualScheduler;
use zero_client::ttl::Ttl;

const DAY_MS: f64 = 24.0 * 60.0 * 60_000.0;

fn schema() -> Arc<Schema> {
    let schema = json::parse(
        r#"{"tables": {"issue": {"name": "issue", "columns": {
            "id": {"type": "string"}, "title": {"type": "string"}}, "primaryKey": ["id"]}}}"#,
    )
    .unwrap();
    Arc::new(Schema::from_json(&schema).unwrap())
}

fn directory(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("zero-sqlite-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|d| {
            d.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    let n = p.file_name().unwrap().to_str().unwrap();
                    n.starts_with("zero-v1-") && n.ends_with(".sqlite")
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

struct Client {
    zero: Zero,
    scheduler: Rc<ManualScheduler>,
    events: Rc<RefCell<Vec<String>>>,
}

fn client(storage: ZeroStorage, key: &str, now: f64) -> Client {
    let scheduler = ManualScheduler::new(now);
    let events = Rc::new(RefCell::new(Vec::new()));
    let (e1, e2) = (events.clone(), events.clone());
    let create = Mutator::new("create", |tx: &Transaction, args, _: &()| {
        let args = args.unwrap();
        let mut row = Object::new();
        row.insert("id", args.get("id").unwrap().clone());
        row.insert("title", args.get("title").unwrap().clone());
        tx.table("issue").insert(&row)?;
        Ok(None)
    });
    let zero = Zero::new(
        schema(),
        (),
        vec![create],
        ZeroOptions {
            storage,
            storage_key: Some(key.to_owned()),
            on_client_state_not_found: Some(Rc::new(move || {
                e1.borrow_mut().push("clientStateNotFound".into())
            })),
            on_storage_failure: Some(Rc::new(move |f| {
                e2.borrow_mut()
                    .push(format!("storageFailure {}", f.kind.as_str()))
            })),
            log_sink: Some(Rc::new(zero_client::testing::RecordingLogSink::default())),
            ..ZeroOptions::default()
        },
        scheduler.clone(),
        None,
    )
    .unwrap();
    scheduler.run_checkpoints();
    Client {
        zero,
        scheduler,
        events,
    }
}

impl Client {
    fn create(&self, id: &str, title: &str) {
        let mut args = Object::new();
        args.insert("id", Value::from(id));
        args.insert("title", Value::from(title));
        self.zero.mutate("create", Some(Value::Object(args)));
        self.scheduler.run_checkpoints();
    }

    /// The issues, as `id:title`.
    fn issues(&self) -> Vec<String> {
        let q = AnyQuery::new(schema(), "issue", System::Client);
        let view = self.zero.materialize(&q, &Ttl::default()).unwrap();
        self.scheduler.run_checkpoints();
        let rows: Vec<String> = view
            .data()
            .entries()
            .iter()
            .map(|e| {
                format!(
                    "{}:{}",
                    e.row.get("id").unwrap().as_str().unwrap(),
                    e.row.get("title").unwrap().as_str().unwrap()
                )
            })
            .collect();
        view.destroy();
        rows
    }
}

fn sqlite(dir: &Path) -> ZeroStorage {
    ZeroStorage::Sqlite {
        directory: Some(dir.to_owned()),
    }
}

fn edit(file: &Path, sql: &str) {
    let db = rusqlite::Connection::open(file).unwrap();
    db.execute_batch(sql).unwrap();
}

fn count(file: &Path, sql: &str) -> i64 {
    let db = rusqlite::Connection::open(file).unwrap();
    db.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn a_relaunch_reads_what_the_last_client_wrote_to_disk() {
    let dir = directory("relaunch");
    let first = client(sqlite(&dir), "k", 1_000.0);
    first.create("i1", "Written");
    assert_eq!(first.issues(), ["i1:Written"]);
    first.zero.flush_storage();
    let [file] = files(&dir).try_into().unwrap();
    assert_eq!(count(&file, "SELECT count(*) FROM mutations"), 1);
    assert_eq!(count(&file, "SELECT count(*) FROM clients"), 1);
    let group = first.zero.client_group_id();
    drop(first);

    // Edited on disk, so the next client can only show it by reading the
    // file.
    edit(
        &file,
        r#"UPDATE mutations SET args = '{"id":"i1","title":"From disk"}'"#,
    );
    let second = client(sqlite(&dir), "k", 2_000.0);
    assert_eq!(second.zero.client_group_id(), group);
    // The pending mutation runs again over the stored snapshot.
    assert_eq!(second.issues(), ["i1:From disk"]);
    second.zero.flush_storage();
    assert_eq!(count(&file, "SELECT count(*) FROM clients"), 2);
    assert!(second.events.borrow().is_empty(), "{:?}", second.events);
    drop(second);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_store_that_does_not_decode_is_dropped_as_lost_client_state() {
    let dir = directory("corrupt");
    let first = client(sqlite(&dir), "k", 1_000.0);
    first.create("i1", "Lost");
    let group = first.zero.client_group_id();
    drop(first);
    let [file] = files(&dir).try_into().unwrap();
    edit(&file, "UPDATE mutations SET args = '{not json'");

    let second = client(sqlite(&dir), "k", 2_000.0);
    assert_eq!(*second.events.borrow(), ["clientStateNotFound"]);
    assert_ne!(second.zero.client_group_id(), group);
    assert!(second.issues().is_empty());
    second.zero.flush_storage();
    let [file] = files(&dir).try_into().unwrap();
    assert_eq!(count(&file, "SELECT count(*) FROM mutations"), 0);
    assert_eq!(count(&file, "SELECT count(*) FROM client_groups"), 1);
    drop(second);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_store_that_cannot_open_falls_back_to_memory_and_says_so_once() {
    let dir = directory("cannot-open");
    std::fs::create_dir_all(&dir).unwrap();
    // A file where the directory should be.
    let not_a_directory = dir.join("file");
    std::fs::write(&not_a_directory, "x").unwrap();
    let c = client(sqlite(&not_a_directory), "k", 1_000.0);
    assert_eq!(*c.events.borrow(), ["storageFailure cannot-open"]);
    c.create("i1", "In memory");
    assert_eq!(c.issues(), ["i1:In memory"]);
    c.zero.flush_storage();
    assert_eq!(c.events.borrow().len(), 1);
    drop(c);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn delete_closes_the_client_and_removes_its_file() {
    let dir = directory("delete");
    let other = client(sqlite(&dir), "other", 1_000.0);
    other.create("o1", "Other user's");
    drop(other);
    let c = client(sqlite(&dir), "k", 1_000.0);
    c.create("i1", "Gone");
    assert_eq!(files(&dir).len(), 2);
    let dropped = c.zero.delete();
    assert!(c.zero.closed());
    assert_eq!(dropped.deleted.len(), 1, "{dropped:?}");
    assert!(dropped.errors.is_empty());
    assert_eq!(files(&dir).len(), 1);
    drop(c);
    let again = client(sqlite(&dir), "k", 2_000.0);
    assert!(again.issues().is_empty());
    let other = client(sqlite(&dir), "other", 2_000.0);
    assert_eq!(other.issues(), ["o1:Other user's"]);
    drop((again, other));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_database_gc_deletes_old_files_and_keeps_current_ones() {
    let dir = directory("gc");
    // Last used three days before the client below starts.
    let old = client(sqlite(&dir), "old", 1_000.0);
    drop(old);
    let [old_file] = files(&dir).try_into().unwrap();
    let recent = client(sqlite(&dir), "recent", 1_000.0 + 2.5 * DAY_MS);
    drop(recent);
    assert_eq!(files(&dir).len(), 2);

    let c = client(sqlite(&dir), "current", 1_000.0 + 3.0 * DAY_MS);
    c.create("i1", "Kept");
    let before = files(&dir);
    assert_eq!(before.len(), 3);
    // Five minutes in, the GC deletes databases no client used for two
    // days, on a thread of its own.
    c.scheduler.advance(5.0 * 60_000.0);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while files(&dir).len() == 3 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let after = files(&dir);
    assert_eq!(after.len(), 2, "{after:?}");
    assert!(!old_file.exists());
    assert_eq!(c.issues(), ["i1:Kept"]);
    drop(c);
    let recent = client(sqlite(&dir), "recent", 1_000.0 + 3.0 * DAY_MS);
    assert!(recent.events.borrow().is_empty());
    drop(recent);
    let _ = std::fs::remove_dir_all(&dir);
}

const DEFAULT: ZeroStorage = ZeroStorage::Sqlite { directory: None };

/// The folder every app shared, and this app's own, which doesn't exist
/// yet, as the default storage of this thread.
fn default_folders(test: &str) -> (PathBuf, PathBuf) {
    let root = directory(test);
    let (shared, own) = (root.join("Zero"), root.join("com.example.notes/Zero"));
    set_default_sqlite_directories(own.clone(), shared.clone());
    (shared, own)
}

fn with_suffix(file: &Path, suffix: &str) -> PathBuf {
    let mut s = file.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

fn contents(files: &[PathBuf]) -> Vec<Vec<u8>> {
    files.iter().map(|f| std::fs::read(f).unwrap()).collect()
}

#[test]
fn the_first_open_of_the_default_storage_moves_its_database_from_the_shared_folder() {
    let (shared, own) = default_folders("move");
    // Another app's database, and this app's of another user.
    for (key, id) in [("other-app", "o1"), ("other-user", "u1")] {
        let c = client(sqlite(&shared), key, 1_000.0);
        c.create(id, "Elsewhere");
        drop(c);
    }
    let others = files(&shared);
    assert_eq!(others.len(), 2);
    let before = contents(&others);

    // A mutation the server never confirmed, in the shared folder as an app
    // that was killed leaves it: in the WAL, not yet in the main file.
    let first = client(sqlite(&shared), "k", 1_000.0);
    first.create("i1", "Pending");
    first.zero.flush_storage();
    let group = first.zero.client_group_id();
    let [mine] = files(&shared)
        .into_iter()
        .filter(|f| !others.contains(f))
        .collect::<Vec<_>>()
        .try_into()
        .unwrap();
    let killed = shared.with_file_name("killed.sqlite");
    for suffix in ["", "-wal"] {
        std::fs::copy(with_suffix(&mine, suffix), with_suffix(&killed, suffix)).unwrap();
    }
    drop(first);
    for suffix in ["", "-wal"] {
        std::fs::copy(with_suffix(&killed, suffix), with_suffix(&mine, suffix)).unwrap();
    }
    // Without its WAL, the main file has no mutations table.
    let main_only = shared.with_file_name("main-only.sqlite");
    std::fs::copy(&killed, &main_only).unwrap();
    let tables = "SELECT count(*) FROM sqlite_master WHERE name = 'mutations'";
    assert_eq!(count(&main_only, tables), 0);
    let profile = std::fs::read_to_string(shared.join("profile-id")).unwrap();

    assert!(!own.exists());
    let c = client(DEFAULT, "k", 2_000.0);
    assert!(c.events.borrow().is_empty(), "{:?}", c.events);
    assert_eq!(c.zero.client_group_id(), group);
    // No server sent the row: the pending mutation ran again.
    assert_eq!(c.issues(), ["i1:Pending"]);
    c.zero.flush_storage();
    let moved = own.join(mine.file_name().unwrap());
    assert_eq!(files(&own), std::slice::from_ref(&moved));
    assert_eq!(count(&moved, "SELECT count(*) FROM mutations"), 1);
    for suffix in ["", "-wal", "-shm"] {
        assert!(!with_suffix(&mine, suffix).exists(), "{suffix}");
    }
    assert_eq!(files(&shared), others);
    assert_eq!(contents(&others), before);
    assert_eq!(
        std::fs::read_to_string(own.join("profile-id")).unwrap(),
        profile
    );
    drop(c);

    // Now that the folder is there, nothing moves: this app's database of
    // the other user stays, and that user syncs again from scratch.
    let again = client(DEFAULT, "other-user", 3_000.0);
    assert!(again.issues().is_empty());
    drop(again);
    assert_eq!(files(&shared), others);
    assert_eq!(contents(&others), before);
    assert_eq!(files(&own).len(), 2);
    let _ = std::fs::remove_dir_all(shared.parent().unwrap());
}

#[test]
fn a_drop_before_the_first_open_of_the_default_storage_does_not_bring_the_database_back() {
    let (shared, own) = default_folders("move-drop");
    let first = client(sqlite(&shared), "k", 1_000.0);
    first.create("i1", "Logged out");
    drop(first);
    let [old] = files(&shared).try_into().unwrap();

    let dropped = drop_all_databases(&DEFAULT);
    assert_eq!(dropped, Default::default());
    assert!(own.is_dir());
    let c = client(DEFAULT, "k", 2_000.0);
    assert!(c.issues().is_empty());
    drop(c);
    assert!(old.exists());
    let [new] = files(&own).try_into().unwrap();
    assert_eq!(count(&new, "SELECT count(*) FROM mutations"), 0);
    let _ = std::fs::remove_dir_all(shared.parent().unwrap());
}
