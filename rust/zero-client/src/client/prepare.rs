//! `Zero::prepare` (zero-swift's `Zero.prepare`): a client's SQLite store
//! opened and read on another thread, for `Zero::from_prepared` to take
//! over on the app's thread.

use super::database::PreparedDatabase;
use super::engine::ZeroOptions;
use super::zero::{Mutator, Zero};
use super::{Error, Scheduler, SocketFactory};
use crate::schema::Schema;
use std::rc::Rc;
use std::sync::Arc;
use std::thread::JoinHandle;

/// A client's store opened and read on another thread by
/// [`Zero::prepare`], for [`Zero::from_prepared`] to take over.
pub struct PreparedZero<C = ()> {
    schema: Arc<Schema>,
    context: C,
    mutators: Vec<Mutator<C>>,
    options: ZeroOptions,
    /// Opens and reads the store; `None` when there is nothing to prepare.
    database: Option<JoinHandle<Option<PreparedDatabase>>>,
}

impl<C: 'static> Zero<C> {
    /// Opens the client's SQLite store and reads its replica on another
    /// thread, for [`from_prepared`](Self::from_prepared) to take over.
    ///
    /// [`new`](Self::new) opens the store and reads the replica on the
    /// app's thread. At launch, that delays the first frame by a time that
    /// grows with the replica. `prepare` returns at once and does it on a
    /// thread of its own, while the app goes on with its own launch, such
    /// as opening its window. [`from_prepared`](Self::from_prepared) then
    /// waits for whatever is left of it and makes the client, so the client
    /// and its first views have their data synchronously, as with `new`.
    /// Call `prepare` as early as the app knows its options, and
    /// `from_prepared` where the first frame needs the client.
    ///
    /// Loading the replica into the query sources and hydrating the first
    /// views stay on the app's thread: they build the client's `Rc`s, which
    /// can't move between threads. zero-swift's `prepare` also does those,
    /// for the queries it's given.
    ///
    /// The client runs on what `new` would have read: when this thread
    /// already has the store open, or another connection wrote to it, or
    /// it was deleted, since `prepare` read it, the client opens it as
    /// `new` does. With [`ZeroStorage::Memory`](super::ZeroStorage::Memory)
    /// or a custom backend, or when the store fails to open or read, there
    /// is nothing to prepare, and the client handles that as `new` does.
    /// Rust and Swift only: the JS client loads IndexedDB asynchronously.
    ///
    /// The arguments are [`new`](Self::new)'s. The client joins the client
    /// group that registers the same mutators.
    pub fn prepare(
        schema: Arc<Schema>,
        context: C,
        mutators: Vec<Mutator<C>>,
        options: ZeroOptions,
    ) -> PreparedZero<C> {
        let database = read_in_background(&schema, &mutators, &options);
        PreparedZero {
            schema,
            context,
            mutators,
            options,
            database,
        }
    }

    /// The client of a [`prepare`](Self::prepare)d store, as
    /// [`new`](Self::new) makes it with `prepare`'s arguments, on what
    /// `prepare` read. Waits for whatever `prepare` hasn't done yet.
    pub fn from_prepared(
        prepared: PreparedZero<C>,
        scheduler: Rc<dyn Scheduler>,
        socket_factory: Option<Rc<dyn SocketFactory>>,
    ) -> Result<Self, Error> {
        let database = prepared.database.and_then(|t| t.join().ok().flatten());
        Self::start(
            prepared.schema,
            prepared.context,
            prepared.mutators,
            prepared.options,
            scheduler,
            socket_factory,
            database,
        )
    }
}

/// Opens and reads the SQLite store of `options` on a thread of its own;
/// `None` for other storage, or for options no client starts with.
#[cfg(feature = "sqlite")]
fn read_in_background<C>(
    schema: &Arc<Schema>,
    mutators: &[Mutator<C>],
    options: &ZeroOptions,
) -> Option<JoinHandle<Option<PreparedDatabase>>> {
    use super::database::ReplicaDatabase;
    use super::engine::{Engine, database_name};
    let super::ZeroStorage::Sqlite { directory } = &options.storage else {
        return None;
    };
    Engine::check_options(options).ok()?;
    let directory = directory.clone();
    let mut names: Vec<String> = mutators.iter().map(|m| m.name.clone()).collect();
    names.sort();
    let schema = schema.clone();
    let user_id = options.user_id.clone();
    let storage_key = options.storage_key.clone();
    let mutate_url = options.mutate_url.clone();
    let query_url = options.query_url.clone();
    std::thread::Builder::new()
        .name("zero-prepare".into())
        .spawn(move || {
            let name = database_name(
                &schema.client_schema(),
                user_id.as_deref(),
                storage_key.as_deref(),
                mutate_url.as_deref(),
                query_url.as_deref(),
            );
            ReplicaDatabase::prepare(directory, name, &names)
        })
        .ok()
}

#[cfg(not(feature = "sqlite"))]
fn read_in_background<C>(
    _: &Arc<Schema>,
    _: &[Mutator<C>],
    _: &ZeroOptions,
) -> Option<JoinHandle<Option<PreparedDatabase>>> {
    None
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    //! The client that takes over a store read on another thread is the
    //! client `new` makes on that store, also when the store changed or is
    //! open meanwhile.

    use super::*;
    use crate::ast::{Direction, System};
    use crate::client::{Transaction, ZeroStorage};
    use crate::json::{self, Object, Value};
    use crate::query::AnyQuery;
    use crate::testing::{ManualScheduler, MockSocketFactory, RecordingLogSink};
    use crate::ttl::Ttl;
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    fn schema() -> Arc<Schema> {
        let schema = json::parse(
            r#"{"tables": {"issue": {"name": "issue", "primaryKey": ["id"],
                "columns": {"id": {"type": "string"}, "title": {"type": "string"}}}}}"#,
        )
        .unwrap();
        Arc::new(Schema::from_json(&schema).unwrap())
    }

    fn create() -> Mutator<()> {
        Mutator::new("create", |tx: &Transaction, args, _: &()| {
            let mut row = Object::new();
            row.insert("id", args.unwrap().get("id").unwrap().clone());
            row.insert("title", Value::from(""));
            tx.table("issue").insert(&row)?;
            Ok(None)
        })
    }

    struct Test {
        directory: PathBuf,
        scheduler: Rc<ManualScheduler>,
        sockets: Rc<MockSocketFactory>,
        events: Rc<RefCell<Vec<String>>>,
    }

    impl Test {
        fn new(name: &str) -> Self {
            let directory =
                std::env::temp_dir().join(format!("zero-prepare-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&directory);
            Test {
                directory,
                scheduler: ManualScheduler::new(1_700_000_000_000.0),
                sockets: MockSocketFactory::new(),
                events: Rc::default(),
            }
        }

        fn options(&self, storage: Option<ZeroStorage>) -> ZeroOptions {
            let events = self.events.clone();
            ZeroOptions {
                cache_url: Some("http://zero.test".into()),
                user_id: Some("u".into()),
                storage_key: Some("k".into()),
                storage: storage.unwrap_or_else(|| ZeroStorage::Sqlite {
                    directory: Some(self.directory.clone()),
                }),
                on_client_state_not_found: Some(Rc::new(move || {
                    events.borrow_mut().push("clientStateNotFound".into())
                })),
                log_sink: Some(Rc::new(RecordingLogSink::default())),
                ..ZeroOptions::default()
            }
        }

        fn new_zero(&self) -> Zero {
            Zero::new(
                schema(),
                (),
                vec![create()],
                self.options(None),
                self.scheduler.clone(),
                Some(self.sockets.clone()),
            )
            .unwrap()
        }

        /// `prepare`, once it has read the store.
        fn prepare(&self, storage: Option<ZeroStorage>) -> PreparedZero {
            let prepared = Zero::prepare(schema(), (), vec![create()], self.options(storage));
            if let Some(t) = &prepared.database {
                while !t.is_finished() {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
            prepared
        }

        fn take_over(&self, prepared: PreparedZero) -> Zero {
            let zero =
                Zero::from_prepared(prepared, self.scheduler.clone(), Some(self.sockets.clone()))
                    .unwrap();
            self.scheduler.run_checkpoints();
            zero
        }

        /// Stores the server's issues 1 and 3, then a mutation creating
        /// issue 2 it hasn't confirmed. Returns the client group.
        fn seed(&self) -> String {
            let zero = self.new_zero();
            self.scheduler.run_due_timers();
            let socket = self.sockets.sockets().last().cloned().unwrap();
            socket.receive_text(r#"["connected",{"wsid":"w"}]"#);
            self.scheduler.run_due_timers();
            let issue = |id: &str| {
                format!(r#"{{"op":"put","tableName":"issue","value":{{"id":"{id}","title":""}}}}"#)
            };
            socket.receive_text(r#"["pokeStart",{"pokeID":"p1","baseCookie":null}]"#);
            socket.receive_text(&format!(
                r#"["pokePart",{{"pokeID":"p1","rowsPatch":[{},{}]}}]"#,
                issue("i1"),
                issue("i3")
            ));
            socket.receive_text(r#"["pokeEnd",{"pokeID":"p1","cookie":"c1"}]"#);
            self.scheduler.run_due_timers();
            create_issue(&zero, "i2");
            zero.flush_storage();
            zero.client_group_id()
        }

        fn file(&self) -> PathBuf {
            let mut files: Vec<PathBuf> = std::fs::read_dir(&self.directory)
                .unwrap()
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|e| e == "sqlite"))
                .collect();
            assert_eq!(files.len(), 1, "{files:?}");
            files.remove(0)
        }
    }

    impl Drop for Test {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    fn create_issue(zero: &Zero, id: &str) {
        let mut args = Object::new();
        args.insert("id", Value::from(id));
        zero.mutate("create", Some(Value::Object(args)));
    }

    fn issues(zero: &Zero) -> Vec<String> {
        let q = AnyQuery::new(schema(), "issue", System::Client).order_by("id", Direction::Asc);
        let view = zero.materialize(&q, &Ttl::default()).unwrap();
        let ids = view
            .data()
            .entries()
            .iter()
            .map(|e| e.row.get("id").unwrap().as_str().unwrap().to_owned())
            .collect();
        view.destroy();
        ids
    }

    fn remove_database(file: &Path) {
        for suffix in ["", "-wal", "-shm"] {
            let mut path = file.as_os_str().to_owned();
            path.push(suffix);
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    fn the_client_has_what_new_reads() {
        let t = Test::new("reads");
        let group = t.seed();
        let prepared = t.prepare(None);

        let opens = crate::client::sqlite::opens_on_this_thread();
        let zero = t.take_over(prepared);
        // The store `prepare` opened, not another.
        assert_eq!(crate::client::sqlite::opens_on_this_thread(), opens);
        assert_eq!(zero.client_group_id(), group);
        // The pending mutation runs again over the stored snapshot.
        assert_eq!(issues(&zero), ["i1", "i2", "i3"]);
        assert_eq!(zero.engine().store.borrow().pending.len(), 1);
        create_issue(&zero, "i4");
        zero.flush_storage();
        drop(zero);

        let fresh = t.new_zero();
        assert_eq!(fresh.client_group_id(), group);
        assert_eq!(issues(&fresh), ["i1", "i2", "i3", "i4"]);
        assert!(t.events.borrow().is_empty(), "{:?}", t.events);
    }

    /// A client of the same database that wrote to it, and closed, between
    /// `prepare` and the take-over: the client reads the store again.
    #[test]
    fn a_write_to_the_store_meanwhile_is_kept() {
        let t = Test::new("write");
        t.seed();
        let prepared = t.prepare(None);

        let other = t.new_zero();
        create_issue(&other, "i4");
        other.flush_storage();
        drop(other);

        let zero = t.take_over(prepared);
        assert_eq!(issues(&zero), ["i1", "i2", "i3", "i4"]);
        assert_eq!(zero.engine().store.borrow().pending.len(), 2);
    }

    /// The file `prepare` read was deleted meanwhile: the client starts a
    /// new one at its path, as `new` does, rather than write to the
    /// deleted one.
    #[test]
    fn a_deleted_store_is_opened_again() {
        let t = Test::new("deleted");
        t.seed();
        let prepared = t.prepare(None);
        remove_database(&t.file());

        let zero = t.take_over(prepared);
        assert!(issues(&zero).is_empty());
        create_issue(&zero, "i9");
        zero.flush_storage();
        drop(zero);
        assert_eq!(issues(&t.new_zero()), ["i9"]);
    }

    /// A database open on the thread when the client takes over is shared,
    /// as `new` shares it.
    #[test]
    fn a_database_open_on_the_thread_is_shared() {
        let t = Test::new("shared");
        t.seed();
        let open = t.new_zero();
        let prepared = t.prepare(None);

        let zero = t.take_over(prepared);
        assert!(Rc::ptr_eq(&zero.engine().database, &open.engine().database));
        assert_eq!(issues(&zero), ["i1", "i2", "i3"]);
    }

    /// A store that doesn't decode is lost client state, as with `new`.
    #[test]
    fn a_store_that_does_not_decode_is_dropped_as_with_new() {
        let t = Test::new("corrupt");
        let group = t.seed();
        let db = rusqlite::Connection::open(t.file()).unwrap();
        db.execute_batch("UPDATE mutations SET args = '{not json'")
            .unwrap();
        drop(db);

        let zero = t.take_over(t.prepare(None));
        assert_eq!(*t.events.borrow(), ["clientStateNotFound"]);
        assert_ne!(zero.client_group_id(), group);
        assert!(issues(&zero).is_empty());
    }

    #[test]
    fn memory_storage_has_nothing_to_prepare() {
        let t = Test::new("memory");
        let prepared = t.prepare(Some(ZeroStorage::Memory));
        assert!(prepared.database.is_none());
        let zero = t.take_over(prepared);
        create_issue(&zero, "i1");
        assert_eq!(issues(&zero), ["i1"]);
    }
}
