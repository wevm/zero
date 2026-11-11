//! The database a client keeps its replica in (zero-swift's
//! `ReplicaDatabase`): the client groups and clients, kept in memory, and
//! each group's snapshot and pending mutations, kept in memory too for
//! [`ZeroStorage::Memory`] and written to a [`StoredDatabase`] otherwise.
//!
//! Databases are shared by location and name for the thread, so a client
//! started again on the same name finds what the last one left (JS's
//! `kvStore: 'mem'` for memory), and two clients of one group (two windows)
//! share one open database.

use super::replica_store::{KvMap, compare_cookies};
pub(crate) use super::storage::{
    AggregateWrites, ClientGroupRecord, ClientIdPair, ClientRecord, StorageOp, StoredMutation,
};
use super::storage::{
    DroppedDatabases, LoadError, StorageBackend, StorageFailure, StoredDatabase, StoredGroup,
    ZeroStorage,
};
use crate::hash::xxhash32;
use crate::json::{self, Value};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

/// What a client group holds, as the client reads it.
#[derive(Clone, Debug, Default)]
pub(crate) struct GroupData {
    pub(crate) entries: KvMap,
    pub(crate) cookie: Value,
    pub(crate) last_mutation_ids: BTreeMap<String, f64>,
    pub(crate) mutations: Vec<StoredMutation>,
}

impl From<StoredGroup> for GroupData {
    fn from(g: StoredGroup) -> Self {
        GroupData {
            entries: g.entries.into(),
            cookie: g.cookie,
            last_mutation_ids: g.last_mutation_ids,
            mutations: g.mutations,
        }
    }
}

type OnDelete = Rc<dyn Fn(&str)>;
type FailureListener = Rc<dyn Fn(&StorageFailure)>;

pub(crate) struct ReplicaDatabase {
    /// The JS name of the database (`rep:<name>:<format>:<schemaVersion>`).
    pub(crate) name: String,
    /// The location and name it is shared by.
    key: String,
    groups: RefCell<Vec<ClientGroupRecord>>,
    clients: RefCell<Vec<ClientRecord>>,
    deleted_clients: RefCell<Vec<ClientIdPair>>,
    profile_id: RefCell<String>,
    /// The groups' data, for a memory database.
    memory_groups: Option<RefCell<HashMap<String, GroupData>>>,
    store: Option<(Rc<dyn StorageBackend>, Box<dyn StoredDatabase>)>,
    /// Set once the store failed; nothing more is written.
    failure: RefCell<Option<StorageFailure>>,
    /// The clients that hear about the first write failure.
    failure_listeners: RefCell<Vec<(String, FailureListener)>>,
    dropped: Cell<bool>,
    users: Cell<u32>,
    /// The clients running on this database, in the order they started
    /// (`ActiveClientsManager`). Each hears when another of its group stops.
    running: RefCell<Vec<(String, String, OnDelete)>>,
    /// The cookie of each client group's stored snapshot.
    snapshot_cookies: RefCell<HashMap<String, Value>>,
}

thread_local! {
    static OPEN: RefCell<HashMap<String, Rc<ReplicaDatabase>>> = RefCell::default();
    /// The profile ID of memory databases, one per process as in JS.
    static MEMORY_PROFILE_ID: RefCell<Option<String>> = const { RefCell::new(None) };
    /// The memory databases clients fell back to after their store failed
    /// (`fellBack` in `mem-fallback-store.ts`), with the locations they
    /// stand in for. Drops from those locations drop these too.
    static FELL_BACK: RefCell<HashMap<String, HashSet<String>>> = RefCell::default();
}

fn memory_key(name: &str) -> String {
    format!("memory:{name}")
}

fn store_key(location: &str, name: &str) -> String {
    format!("{location}\0{name}")
}

/// The backend of `storage`; `None` for memory.
fn backend(storage: &ZeroStorage) -> Result<Option<Rc<dyn StorageBackend>>, StorageFailure> {
    match storage {
        ZeroStorage::Memory => Ok(None),
        #[cfg(feature = "sqlite")]
        ZeroStorage::Sqlite { directory } => Ok(Some(Rc::new(super::sqlite::SqliteBackend::new(
            directory.clone(),
        )?))),
        ZeroStorage::Custom(b) => Ok(Some(b.clone())),
    }
}

impl ReplicaDatabase {
    fn new(
        name: &str,
        key: String,
        store: Option<(Rc<dyn StorageBackend>, Box<dyn StoredDatabase>)>,
    ) -> ReplicaDatabase {
        ReplicaDatabase {
            name: name.to_owned(),
            key,
            groups: RefCell::default(),
            clients: RefCell::default(),
            deleted_clients: RefCell::default(),
            profile_id: RefCell::default(),
            memory_groups: store.is_none().then(RefCell::default),
            store,
            failure: RefCell::default(),
            failure_listeners: RefCell::default(),
            dropped: Cell::new(false),
            users: Cell::new(1),
            running: RefCell::default(),
            snapshot_cookies: RefCell::default(),
        }
    }

    /// Opens the database named `name` in `storage`, or the one already
    /// open on this thread. A database that can't be opened is replaced by
    /// the memory one of its name, and the failure returned. One whose data
    /// doesn't decode is deleted and a new one started, and `corrupt` set.
    pub(crate) fn open(
        storage: &ZeroStorage,
        name: &str,
    ) -> (Rc<ReplicaDatabase>, Option<StorageFailure>, bool) {
        let backend = match backend(storage) {
            Ok(None) => return (Self::memory(name), None, false),
            Ok(Some(b)) => b,
            Err(f) => return (Self::fallback(name, storage), Some(f), false),
        };
        let key = store_key(&backend.location(), name);
        if let Some(db) = OPEN.with(|o| o.borrow().get(&key).cloned()) {
            db.users.set(db.users.get() + 1);
            return (db, None, false);
        }
        let mut corrupt = false;
        let loaded = match Self::load(&backend, name, &key) {
            Err(LoadError::Corrupt(_)) => {
                corrupt = true;
                Self::load(&backend, name, &key)
            }
            r => r,
        };
        match loaded {
            Ok(db) => {
                let db = Rc::new(db);
                OPEN.with(|o| o.borrow_mut().insert(key, db.clone()));
                (db, None, corrupt)
            }
            Err(LoadError::Failure(f)) => (Self::fallback(name, storage), Some(f), false),
            Err(LoadError::Corrupt(m)) => (
                Self::fallback(name, storage),
                Some(StorageFailure::new(
                    super::storage::StorageFailureKind::IoError,
                    m,
                )),
                false,
            ),
        }
    }

    /// Opens and reads the database. One whose data doesn't decode is
    /// deleted.
    fn load(
        backend: &Rc<dyn StorageBackend>,
        name: &str,
        key: &str,
    ) -> Result<ReplicaDatabase, LoadError> {
        let store = backend.open(name)?;
        let clients = match store.load() {
            Ok(c) => c,
            Err(e) => {
                if matches!(e, LoadError::Corrupt(_)) {
                    store.drop_database();
                } else {
                    let _ = store.close();
                }
                return Err(e);
            }
        };
        let db = ReplicaDatabase::new(name, key.to_owned(), Some((backend.clone(), store)));
        *db.groups.borrow_mut() = clients.groups;
        *db.clients.borrow_mut() = clients.clients;
        *db.deleted_clients.borrow_mut() = ClientIdPair::normalize(clients.deleted_clients);
        *db.profile_id.borrow_mut() = clients.profile_id;
        Ok(db)
    }

    /// The memory database named `name`, which lives for the thread and is
    /// shared by name: [`ZeroStorage::Memory`] and the fallback of a
    /// database that fails (`MemStore`).
    pub(crate) fn memory(name: &str) -> Rc<ReplicaDatabase> {
        let key = memory_key(name);
        OPEN.with(|open| {
            if let Some(db) = open.borrow().get(&key) {
                db.users.set(db.users.get() + 1);
                return db.clone();
            }
            let db = ReplicaDatabase::new(name, key.clone(), None);
            *db.profile_id.borrow_mut() = MEMORY_PROFILE_ID.with(|p| {
                p.borrow_mut()
                    .get_or_insert_with(|| format!("p{}", super::engine::random_id()))
                    .clone()
            });
            let db = Rc::new(db);
            open.borrow_mut().insert(key, db.clone());
            db
        })
    }

    /// The memory database a client whose database in `storage` failed
    /// runs on.
    pub(crate) fn fallback(name: &str, storage: &ZeroStorage) -> Rc<ReplicaDatabase> {
        let location = match storage {
            ZeroStorage::Memory => None,
            #[cfg(feature = "sqlite")]
            ZeroStorage::Sqlite { directory } => Some(super::sqlite::SqliteBackend::location_of(
                directory.as_deref(),
            )),
            ZeroStorage::Custom(b) => Some(b.location()),
        };
        if let Some(location) = location {
            FELL_BACK.with(|f| {
                f.borrow_mut()
                    .entry(name.to_owned())
                    .or_default()
                    .insert(location)
            });
        }
        Self::memory(name)
    }

    pub(crate) fn profile_id(&self) -> String {
        self.profile_id.borrow().clone()
    }

    /// Gives up a user's hold. The last user of a stored database closes it
    /// once its writes are done; memory databases live on.
    pub(crate) fn release(&self) {
        let users = self.users.get().saturating_sub(1);
        self.users.set(users);
        let Some((_, store)) = &self.store else {
            return;
        };
        if users > 0 {
            return;
        }
        self.unregister();
        if self.dropped.get() {
            return;
        }
        if let Err(f) = store.close() {
            self.fail(f);
        }
    }

    /// Takes this database out of the shared ones, unless another of its
    /// name took its place.
    fn unregister(&self) {
        OPEN.with(|open| {
            let mut open = open.borrow_mut();
            if open
                .get(&self.key)
                .is_some_and(|db| std::ptr::eq(Rc::as_ptr(db), self))
            {
                open.remove(&self.key);
            }
        });
    }

    /// Waits for the writes so far. A failure to write them is a storage
    /// failure.
    pub(crate) fn flush(&self) {
        if let Some((_, store)) = &self.store
            && self.failure.borrow().is_none()
            && !self.dropped.get()
            && let Err(f) = store.flush()
        {
            self.fail(f);
        }
    }

    /// Tells `client_id` about the first storage failure, until removed.
    pub(crate) fn add_failure_listener(&self, client_id: &str, listener: FailureListener) {
        self.failure_listeners
            .borrow_mut()
            .push((client_id.to_owned(), listener));
    }

    pub(crate) fn remove_failure_listener(&self, client_id: &str) {
        self.failure_listeners
            .borrow_mut()
            .retain(|(c, _)| c != client_id);
    }

    fn fail(&self, f: StorageFailure) {
        if self.failure.borrow().is_some() {
            return;
        }
        *self.failure.borrow_mut() = Some(f.clone());
        let listeners: Vec<FailureListener> = self
            .failure_listeners
            .borrow()
            .iter()
            .map(|(_, l)| l.clone())
            .collect();
        for l in listeners {
            l(&f);
        }
    }

    pub(crate) fn deleted_clients(&self) -> Vec<ClientIdPair> {
        self.deleted_clients.borrow().clone()
    }

    /// Records that `client_id` runs until `client_stopped`. `on_delete`
    /// gets the clients of its group that stop before it.
    pub(crate) fn client_started(
        &self,
        client_id: &str,
        client_group_id: &str,
        on_delete: OnDelete,
    ) {
        self.running.borrow_mut().push((
            client_id.to_owned(),
            client_group_id.to_owned(),
            on_delete,
        ));
    }

    /// Tells the other running clients of its group, once, that `client_id`
    /// stopped (`ActiveClientsManager.onDelete`).
    pub(crate) fn client_stopped(&self, client_id: &str) {
        let others = {
            let mut running = self.running.borrow_mut();
            let Some(i) = running.iter().position(|(c, _, _)| c == client_id) else {
                return;
            };
            let stopped = running.remove(i);
            running
                .iter()
                .filter(|(_, g, _)| *g == stopped.1)
                .map(|(_, _, d)| d.clone())
                .collect::<Vec<_>>()
        };
        for d in others {
            d(client_id);
        }
    }

    /// The running clients of group `id`.
    pub(crate) fn active_clients(&self, id: &str) -> Vec<String> {
        self.running
            .borrow()
            .iter()
            .filter(|(_, g, _)| g == id)
            .map(|(c, _, _)| c.clone())
            .collect()
    }

    /// Applies `ops` in order: to the clients in memory now, and to the
    /// groups' data in memory or in the store.
    pub(crate) fn apply(&self, ops: Vec<StorageOp>) {
        if self.dropped.get() {
            return;
        }
        let ops: Vec<StorageOp> = ops
            .into_iter()
            .map(|op| self.skipping_older_snapshot(op))
            .collect();
        for op in &ops {
            self.apply_to_mirror(op);
        }
        if let Some(memory) = &self.memory_groups {
            let mut memory = memory.borrow_mut();
            for op in ops {
                apply_to_memory(&mut memory, op);
            }
        } else if let Some((_, store)) = &self.store
            && self.failure.borrow().is_none()
            && let Err(f) = store.write(&ops)
        {
            self.fail(f);
        }
    }

    /// A snapshot no newer than the stored one, from a client of the group
    /// that is behind another, isn't written: only the mutations it
    /// confirms are deleted (`compareCookiesForSnapshots`).
    fn skipping_older_snapshot(&self, op: StorageOp) -> StorageOp {
        let StorageOp::UpdateSnapshot {
            group,
            changes,
            cleared,
            cookie,
            last_mutation_ids,
        } = op
        else {
            return op;
        };
        let mut cookies = self.snapshot_cookies.borrow_mut();
        if let Some(stored) = cookies.get(&group)
            && compare_cookies(&cookie, stored) <= 0.0
        {
            return StorageOp::DeleteConfirmedMutations {
                group,
                last_mutation_ids,
            };
        }
        cookies.insert(group.clone(), cookie.clone());
        StorageOp::UpdateSnapshot {
            group,
            changes,
            cleared,
            cookie,
            last_mutation_ids,
        }
    }

    fn apply_to_mirror(&self, op: &StorageOp) {
        match op {
            StorageOp::PutGroup(g) => {
                let mut groups = self.groups.borrow_mut();
                match groups.iter_mut().find(|x| x.id == g.id) {
                    Some(x) => *x = g.clone(),
                    None => groups.push(g.clone()),
                }
            }
            StorageOp::DeleteGroup(id) => {
                self.groups.borrow_mut().retain(|g| g.id != *id);
                self.snapshot_cookies.borrow_mut().remove(id);
            }
            StorageOp::PutClient(c) => {
                let mut clients = self.clients.borrow_mut();
                match clients.iter_mut().find(|x| x.id == c.id) {
                    Some(x) => *x = c.clone(),
                    None => clients.push(c.clone()),
                }
            }
            StorageOp::DeleteClients(ids) => {
                let ids: HashSet<&String> = ids.iter().collect();
                self.clients.borrow_mut().retain(|c| !ids.contains(&c.id));
            }
            StorageOp::SetDeletedClients(pairs) => {
                *self.deleted_clients.borrow_mut() = pairs.clone()
            }
            StorageOp::UpdateSnapshot { .. }
            | StorageOp::AddMutation { .. }
            | StorageOp::SetAggregateWrites { .. }
            | StorageOp::DeleteConfirmedMutations { .. } => {}
        }
    }

    /// The snapshot and pending mutations of group `id`.
    fn load_group(&self, id: &str) -> Result<GroupData, LoadError> {
        if let Some(memory) = &self.memory_groups {
            return Ok(memory.borrow().get(id).cloned().unwrap_or_default());
        }
        let Some((_, store)) = &self.store else {
            return Ok(GroupData::default());
        };
        match store.load_group(id) {
            Ok(g) => Ok(g.into()),
            Err(LoadError::Failure(f)) => {
                self.fail(f.clone());
                Err(LoadError::Failure(f))
            }
            Err(e) => Err(e),
        }
    }

    /// Adds client `client_id` to the newest client group that registers
    /// the same mutators and isn't disabled, or to a new empty one
    /// (`initClientV6` with client group forking off). Returns the group
    /// and what it holds. Fails, adding nothing, when the store failed or
    /// the group can't be read or doesn't decode.
    pub(crate) fn init_client(
        &self,
        client_id: &str,
        mutator_names: &[String],
        now: f64,
    ) -> Result<(String, GroupData), LoadError> {
        if let Some(f) = self.failure.borrow().clone() {
            return Err(LoadError::Failure(f));
        }
        let names: HashSet<&String> = mutator_names.iter().collect();
        let mut ops = Vec::new();
        let matched = self
            .groups
            .borrow()
            .iter()
            .find(|g| {
                !g.disabled
                    && g.mutator_names.len() == names.len()
                    && g.mutator_names.iter().all(|n| names.contains(n))
            })
            .map(|g| g.id.clone());
        let (group_id, group) = match matched {
            Some(id) => {
                let group = self.load_group(&id)?;
                self.snapshot_cookies
                    .borrow_mut()
                    .insert(id.clone(), group.cookie.clone());
                (id, group)
            }
            None => {
                let id = super::engine::random_id();
                ops.push(StorageOp::PutGroup(ClientGroupRecord {
                    id: id.clone(),
                    mutator_names: mutator_names.to_vec(),
                    disabled: false,
                }));
                (id, GroupData::default())
            }
        };
        ops.push(StorageOp::PutClient(ClientRecord {
            id: client_id.to_owned(),
            client_group_id: group_id.clone(),
            heartbeat: now,
        }));
        self.apply(ops);
        Ok((group_id, group))
    }

    /// Records that client `client_id` is running (`writeHeartbeat`). False
    /// when the client is gone, collected by another client.
    pub(crate) fn heartbeat(&self, client_id: &str, now: f64) -> bool {
        let client = self
            .clients
            .borrow()
            .iter()
            .find(|c| c.id == client_id)
            .cloned();
        let Some(mut client) = client else {
            return false;
        };
        client.heartbeat = now;
        self.apply(vec![StorageOp::PutClient(client)]);
        true
    }

    /// Deletes the clients other than `client_id` that have not run for
    /// `max_inactive` ms (`gcClients`). Returns every deleted client the
    /// server has not confirmed, or `None` when none was deleted now.
    pub(crate) fn collect_clients(
        &self,
        client_id: &str,
        now: f64,
        max_inactive: f64,
    ) -> Option<Vec<ClientIdPair>> {
        let stale: Vec<ClientRecord> = self
            .clients
            .borrow()
            .iter()
            .filter(|c| c.id != client_id && now - c.heartbeat > max_inactive)
            .cloned()
            .collect();
        if stale.is_empty() {
            return None;
        }
        let mut deleted = self.deleted_clients();
        deleted.extend(stale.iter().map(|c| ClientIdPair {
            client_group_id: c.client_group_id.clone(),
            client_id: c.id.clone(),
        }));
        let deleted = ClientIdPair::normalize(deleted);
        self.apply(vec![
            StorageOp::DeleteClients(stale.into_iter().map(|c| c.id).collect()),
            StorageOp::SetDeletedClients(deleted.clone()),
        ]);
        Some(deleted)
    }

    /// Deletes the client groups no client is in (`gcClientGroups` without
    /// mutation recovery, so their pending mutations go too).
    pub(crate) fn collect_client_groups(&self) {
        let used: HashSet<String> = self
            .clients
            .borrow()
            .iter()
            .map(|c| c.client_group_id.clone())
            .collect();
        let ops: Vec<StorageOp> = self
            .groups
            .borrow()
            .iter()
            .filter(|g| !used.contains(&g.id))
            .map(|g| StorageOp::DeleteGroup(g.id.clone()))
            .collect();
        if !ops.is_empty() {
            self.apply(ops);
        }
    }

    /// The server deleted these clients and client groups
    /// (`confirmDeletedClients`).
    pub(crate) fn confirm_deleted_clients(
        &self,
        client_ids: &[String],
        client_group_ids: &[String],
    ) {
        let gone: Vec<String> = self
            .clients
            .borrow()
            .iter()
            .filter(|c| client_ids.contains(&c.id) || client_group_ids.contains(&c.client_group_id))
            .map(|c| c.id.clone())
            .collect();
        let remaining: Vec<ClientIdPair> = self
            .deleted_clients()
            .into_iter()
            .filter(|p| {
                !client_ids.contains(&p.client_id) && !client_group_ids.contains(&p.client_group_id)
            })
            .collect();
        self.apply(vec![
            StorageOp::DeleteClients(gone),
            StorageOp::SetDeletedClients(remaining),
        ]);
    }

    /// Keeps new clients out of group `id` (`disableClientGroup`).
    pub(crate) fn disable_client_group(&self, id: &str) {
        let group = self.groups.borrow().iter().find(|g| g.id == id).cloned();
        if let Some(mut g) = group {
            g.disabled = true;
            self.apply(vec![StorageOp::PutGroup(g)]);
        }
    }

    /// Deletes the database (`dropDatabase`). The next client to open it
    /// starts empty; this one's later writes are dropped. Dropping it again
    /// does nothing: the store may be a new database by then.
    pub(crate) fn drop_database(&self) {
        if self.dropped.replace(true) {
            return;
        }
        let replaced = OPEN.with(|open| {
            open.borrow()
                .get(&self.key)
                .is_some_and(|db| !std::ptr::eq(Rc::as_ptr(db), self))
        });
        if let Some((_, store)) = &self.store {
            // Another database of this name opened since owns the store.
            if replaced {
                let _ = store.close();
                return;
            }
            store.drop_database();
        }
        self.unregister();
    }

    /// Deletes, in the background, the other databases of the store no
    /// client used for `max_age` ms (`collectIDBDatabases`).
    pub(crate) fn collect_databases(&self, now: f64, max_age: f64) {
        if let Some((backend, _)) = &self.store
            && self.failure.borrow().is_none()
            && !self.dropped.get()
        {
            backend.collect_databases(&self.name, now, max_age);
        }
    }

    /// Deletes the databases in `storage` whose Replicache name `matches`
    /// (`dropMatchingDatabases`): those open on this thread, whose clients'
    /// later writes are dropped, and the others.
    pub(crate) fn drop_matching(
        storage: &ZeroStorage,
        matches: &dyn Fn(&str) -> bool,
    ) -> DroppedDatabases {
        let mut result = DroppedDatabases::default();
        let backend = match backend(storage) {
            Ok(b) => b,
            Err(f) => {
                result.errors.push(f);
                return result;
            }
        };
        let prefix = match &backend {
            None => "memory:".to_owned(),
            Some(b) => format!("{}\0", b.location()),
        };
        let open: Vec<Rc<ReplicaDatabase>> = OPEN.with(|o| {
            o.borrow()
                .iter()
                .filter(|(k, db)| k.starts_with(&prefix) && matches(&replicache_name(&db.name)))
                .map(|(_, db)| db.clone())
                .collect()
        });
        for db in open {
            db.drop_database();
            result.deleted.push(db.name.clone());
        }
        let Some(backend) = backend else {
            return result;
        };
        // The memory databases clients of this location fell back to go
        // too (`dropTargets`); those of memory clients stay.
        let location = backend.location();
        let fell_back: Vec<String> = FELL_BACK.with(|f| {
            let mut f = f.borrow_mut();
            let names: Vec<String> = f
                .iter()
                .filter(|(name, l)| l.contains(&location) && matches(&replicache_name(name)))
                .map(|(name, _)| name.clone())
                .collect();
            for n in &names {
                f.remove(n);
            }
            names
        });
        for name in fell_back {
            let db = OPEN.with(|o| o.borrow().get(&memory_key(&name)).cloned());
            if let Some(db) = db {
                db.drop_database();
                if !result.deleted.contains(&name) {
                    result.deleted.push(name);
                }
            }
        }
        let files = backend.drop_databases(&|name| matches(&replicache_name(name)));
        result.deleted.extend(files.deleted);
        result.errors.extend(files.errors);
        result
    }

    /// The database name of a client (`makeIDBName` in `zero.ts`).
    pub(crate) fn name(
        user_id: Option<&str>,
        storage_key: &str,
        mutate_url: Option<&str>,
        query_url: Option<&str>,
        schema_hash: &str,
    ) -> String {
        // `JSON.stringify` keeps the keys in this order.
        let s = |v: &str| json::stringify(&Value::String(v.to_owned()));
        let name_key = format!(
            "{{\"storageKey\":{},\"mutateUrl\":{},\"queryUrl\":{}}}",
            s(storage_key),
            s(mutate_url.unwrap_or("")),
            s(query_url.unwrap_or(""))
        );
        let hashed = base36(xxhash32::h64(&name_key));
        format!(
            "rep:zero-{}-{hashed}:{FORMAT_VERSION}:{STORAGE_VERSION}.{schema_hash}",
            user_id.unwrap_or("__anonymous__")
        )
    }
}

fn apply_to_memory(memory: &mut HashMap<String, GroupData>, op: StorageOp) {
    let confirmed = |m: &StoredMutation, lmids: &BTreeMap<String, f64>| {
        m.id <= lmids.get(&m.client_id).copied().unwrap_or(0.0)
    };
    match op {
        StorageOp::PutGroup(g) => {
            memory.entry(g.id).or_default();
        }
        StorageOp::DeleteGroup(id) => {
            memory.remove(&id);
        }
        StorageOp::UpdateSnapshot {
            group,
            changes,
            cleared,
            cookie,
            last_mutation_ids,
        } => {
            let g = memory.entry(group).or_default();
            if cleared {
                g.entries.clear();
            }
            for (k, v) in changes {
                match v {
                    Some(v) => g.entries.set(k, v),
                    None => {
                        g.entries.delete(&k);
                    }
                }
            }
            g.cookie = cookie;
            g.mutations.retain(|m| !confirmed(m, &last_mutation_ids));
            g.last_mutation_ids = last_mutation_ids;
        }
        StorageOp::AddMutation { group, mutation } => {
            memory.entry(group).or_default().mutations.push(mutation);
        }
        StorageOp::SetAggregateWrites {
            group,
            client_id,
            id,
            writes,
        } => {
            if let Some(m) = memory.get_mut(&group).and_then(|g| {
                g.mutations
                    .iter_mut()
                    .find(|m| m.client_id == client_id && m.id == id)
            }) {
                m.aggregate_writes = writes;
            }
        }
        StorageOp::DeleteConfirmedMutations {
            group,
            last_mutation_ids,
        } => {
            if let Some(g) = memory.get_mut(&group) {
                g.mutations.retain(|m| !confirmed(m, &last_mutation_ids));
            }
        }
        StorageOp::PutClient(_) | StorageOp::DeleteClients(_) | StorageOp::SetDeletedClients(_) => {
        }
    }
}

/// The Replicache name a database name holds (`replicacheName`):
/// `zero-<userID>-<hash>`, from `rep:<name>:<format>:<schemaVersion>`.
pub(crate) fn replicache_name(name: &str) -> String {
    let rest = name.strip_prefix("rep:").unwrap_or(name);
    let mut parts: Vec<&str> = rest.split(':').collect();
    let n = parts.len().saturating_sub(2);
    parts.truncate(n);
    parts.join(":")
}

/// Replicache's `FormatVersion.Latest`.
pub(crate) const FORMAT_VERSION: u32 = 7;
/// `STORAGE_VERSION` in `packages/zero-client/src/client/storage-version.ts`.
const STORAGE_VERSION: u32 = 52;

/// `n.toString(36)`.
pub(crate) fn base36(mut n: u64) -> String {
    if n == 0 {
        return "0".into();
    }
    let digits = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    while n > 0 {
        out.push(digits[(n % 36) as usize]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8(out).expect("ascii")
}
