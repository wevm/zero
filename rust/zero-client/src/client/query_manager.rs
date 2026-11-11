//! Tracks the queries the client wants from the server, tells the server
//! about changes in batches, and reports which ones the server has sent
//! (`packages/zero-client/src/client/query-manager.ts`).

use super::replica_store::{KvDiff, KvDiffOp, KvMap};
use super::{ConnectionError, Scheduler, Timer};
use crate::hash::query::hash_of_name_and_args;
use crate::json::{Object, Value};
use crate::keys::{DESIRED_QUERIES_PREFIX, GOT_QUERIES_PREFIX};
use crate::map::OrderedMap;
use crate::protocol::{
    ChangeDesiredQueriesBody, ErroredQuery, UpQueriesPatch, UpQueriesPatchOp, UpQueriesPut,
};
use crate::ttl::Ttl;
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::{Rc, Weak};

/// What a query's got callback hears: whether the server has confirmed the
/// query's rows on this connection (`Yes`), on an earlier one (`Cached`),
/// or not (`No`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Got {
    No,
    Cached,
    Yes,
}

pub(crate) type GotCallback = Rc<dyn Fn(Got, Option<ErroredQuery>)>;

struct Entry {
    name: String,
    args: Vec<Value>,
    count: usize,
    got_callbacks: Vec<(u64, GotCallback)>,
    ttl: f64,
}

type EntryRef = Rc<RefCell<Entry>>;

#[derive(Default)]
struct State {
    queries: OrderedMap<EntryRef>,
    recent_queries: OrderedMap<()>,
    got_queries: HashSet<String>,
    got_queries_authoritative: bool,
    cached_queries: HashSet<String>,
    awaiting_persisted_got_diff: bool,
    pending_query_changes: UpQueriesPatch,
    pending_removals: Vec<Box<dyn FnOnce()>>,
    batch_timer: Option<Box<dyn Timer>>,
    closed_error: Option<ConnectionError>,
}

pub(crate) struct QueryManager {
    me: Weak<QueryManager>,
    client_id: String,
    send: Box<dyn Fn(ChangeDesiredQueriesBody)>,
    /// The number of mutations waiting for the server.
    pending_mutations: Box<dyn Fn() -> usize>,
    scheduler: Rc<dyn Scheduler>,
    query_change_throttle_ms: f64,
    recent_queries_max_size: usize,
    state: RefCell<State>,
    next_callback_id: Cell<u64>,
}

impl QueryManager {
    pub(crate) fn new(
        client_id: String,
        scheduler: Rc<dyn Scheduler>,
        query_change_throttle_ms: f64,
        send: Box<dyn Fn(ChangeDesiredQueriesBody)>,
        pending_mutations: Box<dyn Fn() -> usize>,
    ) -> Rc<Self> {
        Rc::new_cyclic(|me| QueryManager {
            me: me.clone(),
            client_id,
            send,
            pending_mutations,
            scheduler,
            query_change_throttle_ms,
            recent_queries_max_size: 0,
            state: RefCell::new(State {
                awaiting_persisted_got_diff: true,
                ..State::default()
            }),
            next_callback_id: Cell::new(0),
        })
    }

    /// Every mutation the server had to confirm is confirmed: the removals
    /// held back meanwhile happen now.
    pub(crate) fn on_all_mutations_applied(&self) {
        let removals = std::mem::take(&mut self.state.borrow_mut().pending_removals);
        for r in removals {
            r();
        }
    }

    /// The watch of `g/`. The first call is the got set already in the
    /// store.
    pub(crate) fn process_got_queries(&self, diff: &KvDiff) {
        let mut fire: Vec<(String, Got)> = Vec::new();
        {
            let mut s = self.state.borrow_mut();
            let persisted = std::mem::replace(&mut s.awaiting_persisted_got_diff, false);
            for op in diff {
                let hash = op.key()[GOT_QUERIES_PREFIX.len()..].to_owned();
                match op {
                    KvDiffOp::Add { .. } => {
                        s.got_queries.insert(hash.clone());
                        if s.got_queries_authoritative {
                            fire.push((hash, Got::Yes));
                        } else if persisted {
                            s.cached_queries.insert(hash.clone());
                            fire.push((hash, Got::Cached));
                        }
                    }
                    KvDiffOp::Del { .. } => {
                        s.got_queries.remove(&hash);
                        s.cached_queries.remove(&hash);
                        fire.push((hash, Got::No));
                    }
                    KvDiffOp::Change { .. } => {}
                }
            }
        }
        for (hash, got) in fire {
            self.fire_got_callbacks(&hash, got, None);
        }
    }

    fn fire_got_callbacks(&self, hash: &str, got: Got, error: Option<ErroredQuery>) {
        let callbacks: Vec<GotCallback> = {
            let s = self.state.borrow();
            let Some(entry) = s.queries.get(hash) else {
                return;
            };
            entry
                .borrow()
                .got_callbacks
                .iter()
                .map(|(_, c)| c.clone())
                .collect()
        };
        for c in callbacks {
            c(got, error.clone());
        }
    }

    /// Trusts the got set once the first poke after a connect is applied.
    pub(crate) fn mark_got_queries_authoritative(&self) {
        let hashes: Vec<String> = {
            let mut s = self.state.borrow_mut();
            if s.got_queries_authoritative {
                return;
            }
            s.got_queries_authoritative = true;
            s.cached_queries.clear();
            s.awaiting_persisted_got_diff = false;
            s.queries
                .keys()
                .filter(|h| s.got_queries.contains(*h))
                .cloned()
                .collect()
        };
        for h in hashes {
            self.fire_got_callbacks(&h, Got::Yes, None);
        }
    }

    /// On disconnect: the next connection must confirm the got set again.
    pub(crate) fn clear_got_queries_authoritative(&self) {
        let mut s = self.state.borrow_mut();
        if !s.got_queries_authoritative {
            return;
        }
        s.got_queries_authoritative = false;
        let got: Vec<String> = s.got_queries.iter().cloned().collect();
        s.cached_queries.extend(got);
    }

    /// The changes to send so the server's desired queries for this client
    /// (`d/<clientID>/` in the store) become ours, less what `last_patch`
    /// already sent.
    pub(crate) fn queries_patch(
        &self,
        main: &KvMap,
        last_patch: Option<&OrderedMap<UpQueriesPatchOp>>,
    ) -> OrderedMap<UpQueriesPatchOp> {
        let prefix = format!("{DESIRED_QUERIES_PREFIX}{}/", self.client_id);
        let mut existing: Vec<String> = Vec::new();
        let mut existing_set: HashSet<String> = HashSet::new();
        for (key, _) in main.scan(&prefix) {
            let hash = key[prefix.len()..].to_owned();
            if existing_set.insert(hash.clone()) {
                existing.push(hash);
            }
        }
        let s = self.state.borrow();
        let mut patch: OrderedMap<UpQueriesPatchOp> = OrderedMap::new();
        for hash in existing {
            if !s.queries.contains_key(&hash) {
                patch.insert(hash.clone(), UpQueriesPatchOp::Del { hash });
            }
        }
        for (hash, entry) in s.queries.iter() {
            if !existing_set.contains(hash) {
                let ttl = entry.borrow().ttl;
                patch.insert(hash.clone(), put(hash, &entry.borrow(), ttl));
            }
        }
        if let Some(last) = last_patch {
            for (hash, op) in last.iter() {
                if matches!(op, UpQueriesPatchOp::Put(_)) && !patch.contains_key(hash) {
                    patch.insert(hash.clone(), UpQueriesPatchOp::Del { hash: hash.clone() });
                }
            }
            let same: Vec<String> = patch
                .iter()
                .filter(|(hash, op)| last.get(hash).is_some_and(|l| same_op(l, op)))
                .map(|(h, _)| h.clone())
                .collect();
            for h in same {
                patch.remove(&h);
            }
        }
        patch
    }

    pub(crate) fn handle_transform_errors(&self, errors: &[ErroredQuery]) {
        for error in errors {
            let id = match error {
                ErroredQuery::App { id, .. } | ErroredQuery::Parse { id, .. } => id,
            };
            self.fire_got_callbacks(id, Got::No, Some(error.clone()));
        }
    }

    pub(crate) fn handle_closed(&self, reason: &ConnectionError) {
        let entries: Vec<(String, String)> = {
            let mut s = self.state.borrow_mut();
            if s.closed_error.is_some() {
                return;
            }
            s.closed_error = Some(reason.clone());
            s.queries
                .iter()
                .map(|(h, e)| (h.clone(), e.borrow().name.clone()))
                .collect()
        };
        for (hash, name) in entries {
            let error = closed_error(&hash, Some(&name), reason);
            self.fire_got_callbacks(&hash, Got::No, Some(error));
        }
    }

    /// Registers a named query. Returns the function that unregisters it.
    pub(crate) fn add_custom(
        &self,
        name: &str,
        args: &[Value],
        ttl: &Ttl,
        got_callback: Option<GotCallback>,
    ) -> Box<dyn Fn()> {
        let hash = hash_of_name_and_args(name, args);
        let ttl = ttl.clamped();
        let callback_id = got_callback.as_ref().map(|_| {
            let id = self.next_callback_id.get() + 1;
            self.next_callback_id.set(id);
            id
        });
        let (entry, got) = {
            let mut s = self.state.borrow_mut();
            s.recent_queries.remove(&hash);
            let existing = s.queries.get(&hash).cloned();
            let entry = match existing {
                Some(entry) => {
                    entry.borrow_mut().count += 1;
                    drop(s);
                    self.update_entry(&entry, &hash, ttl);
                    s = self.state.borrow_mut();
                    if let (Some(cb), Some(id)) = (&got_callback, callback_id) {
                        entry.borrow_mut().got_callbacks.push((id, cb.clone()));
                    }
                    entry
                }
                None => {
                    let entry = Rc::new(RefCell::new(Entry {
                        name: name.to_owned(),
                        args: args.to_vec(),
                        count: 1,
                        got_callbacks: match (&got_callback, callback_id) {
                            (Some(cb), Some(id)) => vec![(id, cb.clone())],
                            _ => Vec::new(),
                        },
                        ttl,
                    }));
                    s.queries.insert(hash.clone(), entry.clone());
                    let op = put(&hash, &entry.borrow(), ttl);
                    drop(s);
                    self.queue_query_change(op);
                    s = self.state.borrow_mut();
                    entry
                }
            };
            let got = if s.got_queries_authoritative {
                if s.got_queries.contains(&hash) {
                    Got::Yes
                } else {
                    Got::No
                }
            } else if s.cached_queries.contains(&hash) {
                Got::Cached
            } else {
                Got::No
            };
            (entry, got)
        };
        if let Some(cb) = &got_callback {
            cb(got, None);
        }
        let removed = Cell::new(false);
        let me = self.me.clone();
        Box::new(move || {
            let Some(qm) = me.upgrade() else { return };
            if removed.replace(true) {
                return;
            }
            // Queries can't be removed while mutations are pending: a
            // rebase could need data they keep in scope.
            if (qm.pending_mutations)() > 0 {
                let me = me.clone();
                let entry = entry.clone();
                let hash = hash.clone();
                qm.state
                    .borrow_mut()
                    .pending_removals
                    .push(Box::new(move || {
                        if let Some(qm) = me.upgrade() {
                            qm.remove(&entry, &hash, callback_id);
                        }
                    }));
                return;
            }
            qm.remove(&entry, &hash, callback_id);
        })
    }

    pub(crate) fn update_custom(&self, name: &str, args: &[Value], ttl: &Ttl) {
        let hash = hash_of_name_and_args(name, args);
        let entry = self.state.borrow().queries.get(&hash).cloned();
        if let Some(entry) = entry {
            self.update_entry(&entry, &hash, ttl.clamped());
        }
    }

    fn update_entry(&self, entry: &EntryRef, hash: &str, ttl: f64) {
        let current = entry.borrow().ttl;
        if Ttl::compare(&Ttl::Milliseconds(ttl), &Ttl::Milliseconds(current)) > 0.0 {
            entry.borrow_mut().ttl = ttl;
            let op = put(hash, &entry.borrow(), ttl);
            self.queue_query_change(op);
        }
    }

    fn queue_query_change(&self, op: UpQueriesPatchOp) {
        let mut s = self.state.borrow_mut();
        s.pending_query_changes.push(op);
        if s.batch_timer.is_none() {
            let me = self.me.clone();
            s.batch_timer = Some(self.scheduler.schedule(
                self.query_change_throttle_ms,
                Box::new(move || {
                    if let Some(qm) = me.upgrade() {
                        qm.flush_batch();
                    }
                }),
            ));
        }
    }

    pub(crate) fn flush_batch(&self) {
        let changes = {
            let mut s = self.state.borrow_mut();
            if let Some(t) = s.batch_timer.take() {
                t.cancel();
            }
            if s.pending_query_changes.is_empty() {
                return;
            }
            std::mem::take(&mut s.pending_query_changes)
        };
        (self.send)(ChangeDesiredQueriesBody {
            desired_queries_patch: changes,
            traceparent: None,
        });
    }

    fn remove(&self, entry: &EntryRef, hash: &str, callback_id: Option<u64>) {
        {
            let mut e = entry.borrow_mut();
            if let Some(id) = callback_id {
                e.got_callbacks.retain(|(i, _)| *i != id);
            }
            e.count -= 1;
            if e.count > 0 {
                return;
            }
        }
        let evicted = {
            let mut s = self.state.borrow_mut();
            s.recent_queries.insert(hash.to_owned(), ());
            if s.recent_queries.len() > self.recent_queries_max_size {
                let lru = s.recent_queries.get_index(0).map(|(k, _)| k.clone());
                if let Some(lru) = &lru {
                    s.queries.remove(lru);
                    s.recent_queries.remove(lru);
                }
                lru
            } else {
                None
            }
        };
        if let Some(lru) = evicted {
            self.queue_query_change(UpQueriesPatchOp::Del { hash: lru });
        }
    }
}

fn put(hash: &str, entry: &Entry, ttl: f64) -> UpQueriesPatchOp {
    UpQueriesPatchOp::Put(Box::new(UpQueriesPut {
        hash: hash.to_owned(),
        ttl: Some(ttl),
        ast: None,
        name: Some(entry.name.clone()),
        args: Some(entry.args.clone()),
    }))
}

fn same_op(a: &UpQueriesPatchOp, b: &UpQueriesPatchOp) -> bool {
    matches!(
        (a, b),
        (UpQueriesPatchOp::Put(_), UpQueriesPatchOp::Put(_))
            | (UpQueriesPatchOp::Del { .. }, UpQueriesPatchOp::Del { .. })
            | (UpQueriesPatchOp::Clear, UpQueriesPatchOp::Clear)
    )
}

/// The error of a query that waited for the server when the client closed
/// (or, in this client, that waits for it once closed).
pub(crate) fn closed_error(id: &str, name: Option<&str>, reason: &ConnectionError) -> ErroredQuery {
    let mut details = Object::new();
    details.insert("kind", Value::from(reason.kind_name()));
    ErroredQuery::App {
        id: id.to_owned(),
        name: name.unwrap_or("legacy").to_owned(),
        message: Some(reason.message().to_owned()),
        details: Some(Value::Object(details)),
    }
}
