//! The database a client keeps its replica in: its client groups, clients,
//! and each group's snapshot and pending mutations (what Replicache keeps in
//! IndexedDB, as zero-swift's `ReplicaDatabase` keeps it). A memory
//! database lives for the thread and is shared by name, so a client started
//! again on the same name finds what the last one left, as JS's `kvStore:
//! 'mem'` does.

use super::replica_store::KvMap;
use super::replica_store::compare_cookies;
use crate::hash::xxhash32;
use crate::json::{self, Value};
use crate::strings;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

/// A client group: the clients that share a replica because they register
/// the same mutators (`ClientGroup`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ClientGroupRecord {
    pub(crate) id: String,
    pub(crate) mutator_names: Vec<String>,
    /// The server lost the group, so no new client joins it.
    pub(crate) disabled: bool,
}

/// A client that used the database (`ClientV6`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ClientRecord {
    pub(crate) id: String,
    pub(crate) client_group_id: String,
    /// When the client last showed it was running, in ms since 1970.
    pub(crate) heartbeat: f64,
}

/// A deleted client the server may not know about yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ClientIdPair {
    pub(crate) client_group_id: String,
    pub(crate) client_id: String,
}

impl ClientIdPair {
    /// `normalizeDeletedClients`: sorted by group then client, deduplicated.
    fn normalize(mut pairs: Vec<ClientIdPair>) -> Vec<ClientIdPair> {
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
pub(crate) type AggregateWrites = Vec<(String, Option<Value>)>;

/// A mutation that ran on a client and the server has not confirmed.
#[derive(Clone, Debug)]
pub(crate) struct StoredMutation {
    pub(crate) client_id: String,
    pub(crate) id: f64,
    pub(crate) name: String,
    pub(crate) args: Value,
    pub(crate) timestamp: f64,
    /// JS persists these in the mutation's commit; the client runs the
    /// mutation again at launch, before any query registers the aggregates
    /// it moves, and puts these back.
    pub(crate) aggregate_writes: AggregateWrites,
}

/// What a client group keeps: the last server snapshot and the mutations
/// not yet in it.
#[derive(Clone, Debug, Default)]
pub(crate) struct StoredGroup {
    pub(crate) entries: KvMap,
    pub(crate) cookie: Value,
    pub(crate) last_mutation_ids: BTreeMap<String, f64>,
    pub(crate) mutations: Vec<StoredMutation>,
}

/// A change to the database, applied in order.
pub(crate) enum StorageOp {
    PutGroup(ClientGroupRecord),
    /// Deletes the group with its snapshot and mutations.
    DeleteGroup(String),
    PutClient(ClientRecord),
    DeleteClients(Vec<String>),
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

type OnDelete = Rc<dyn Fn(&str)>;

pub(crate) struct ReplicaDatabase {
    /// The JS name of the database (`rep:<name>:<format>:<schemaVersion>`).
    pub(crate) name: String,
    groups: RefCell<Vec<ClientGroupRecord>>,
    clients: RefCell<Vec<ClientRecord>>,
    deleted_clients: RefCell<Vec<ClientIdPair>>,
    pub(crate) profile_id: String,
    data: RefCell<HashMap<String, StoredGroup>>,
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
}

impl ReplicaDatabase {
    /// The memory database named `name`, shared by name for the thread.
    pub(crate) fn memory(name: &str) -> Rc<ReplicaDatabase> {
        OPEN.with(|open| {
            if let Some(db) = open.borrow().get(name) {
                db.users.set(db.users.get() + 1);
                return db.clone();
            }
            let profile_id = MEMORY_PROFILE_ID.with(|p| {
                p.borrow_mut()
                    .get_or_insert_with(|| format!("p{}", super::engine::random_id()))
                    .clone()
            });
            let db = Rc::new(ReplicaDatabase {
                name: name.to_owned(),
                groups: RefCell::default(),
                clients: RefCell::default(),
                deleted_clients: RefCell::default(),
                profile_id,
                data: RefCell::default(),
                dropped: Cell::new(false),
                users: Cell::new(1),
                running: RefCell::default(),
                snapshot_cookies: RefCell::default(),
            });
            open.borrow_mut().insert(name.to_owned(), db.clone());
            db
        })
    }

    /// Gives up a user's hold. Memory databases live on.
    pub(crate) fn release(&self) {
        self.users.set(self.users.get().saturating_sub(1));
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

    /// Applies `ops` in order.
    pub(crate) fn apply(&self, ops: Vec<StorageOp>) {
        if self.dropped.get() {
            return;
        }
        for op in ops {
            let op = self.skipping_older_snapshot(op);
            self.apply_op(op);
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

    fn apply_op(&self, op: StorageOp) {
        let confirmed = |m: &StoredMutation, lmids: &BTreeMap<String, f64>| {
            m.id <= lmids.get(&m.client_id).copied().unwrap_or(0.0)
        };
        match op {
            StorageOp::PutGroup(g) => {
                self.data.borrow_mut().entry(g.id.clone()).or_default();
                let mut groups = self.groups.borrow_mut();
                match groups.iter_mut().find(|x| x.id == g.id) {
                    Some(x) => *x = g,
                    None => groups.push(g),
                }
            }
            StorageOp::DeleteGroup(id) => {
                self.groups.borrow_mut().retain(|g| g.id != id);
                self.snapshot_cookies.borrow_mut().remove(&id);
                self.data.borrow_mut().remove(&id);
            }
            StorageOp::PutClient(c) => {
                let mut clients = self.clients.borrow_mut();
                match clients.iter_mut().find(|x| x.id == c.id) {
                    Some(x) => *x = c,
                    None => clients.push(c),
                }
            }
            StorageOp::DeleteClients(ids) => {
                let ids: HashSet<String> = ids.into_iter().collect();
                self.clients.borrow_mut().retain(|c| !ids.contains(&c.id));
            }
            StorageOp::SetDeletedClients(pairs) => *self.deleted_clients.borrow_mut() = pairs,
            StorageOp::UpdateSnapshot {
                group,
                changes,
                cleared,
                cookie,
                last_mutation_ids,
            } => {
                let mut data = self.data.borrow_mut();
                let g = data.entry(group).or_default();
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
                self.data
                    .borrow_mut()
                    .entry(group)
                    .or_default()
                    .mutations
                    .push(mutation);
            }
            StorageOp::SetAggregateWrites {
                group,
                client_id,
                id,
                writes,
            } => {
                if let Some(m) = self.data.borrow_mut().get_mut(&group).and_then(|g| {
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
                if let Some(g) = self.data.borrow_mut().get_mut(&group) {
                    g.mutations.retain(|m| !confirmed(m, &last_mutation_ids));
                }
            }
        }
    }

    /// Adds client `client_id` to the newest client group that registers
    /// the same mutators and isn't disabled, or to a new empty one
    /// (`initClientV6` with client group forking off). Returns the group
    /// and what it holds.
    pub(crate) fn init_client(
        &self,
        client_id: &str,
        mutator_names: &[String],
        now: f64,
    ) -> (String, StoredGroup) {
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
                let group = self.data.borrow().get(&id).cloned().unwrap_or_default();
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
                (id, StoredGroup::default())
            }
        };
        ops.push(StorageOp::PutClient(ClientRecord {
            id: client_id.to_owned(),
            client_group_id: group_id.clone(),
            heartbeat: now,
        }));
        self.apply(ops);
        (group_id, group)
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
    /// starts empty; this one's later writes are dropped.
    pub(crate) fn drop_database(&self) {
        if self.dropped.replace(true) {
            return;
        }
        OPEN.with(|open| {
            let mut open = open.borrow_mut();
            if open
                .get(&self.name)
                .is_some_and(|db| std::ptr::eq(Rc::as_ptr(db), self))
            {
                open.remove(&self.name);
            }
        });
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

/// Replicache's `FormatVersion.Latest`.
const FORMAT_VERSION: u32 = 7;
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
