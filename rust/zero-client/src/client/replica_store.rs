//! The local state Replicache keeps for Zero, without its storage
//! machinery: the last server snapshot, and the pending local mutations
//! whose effects are applied on top of it.

use crate::json::{self, Object, Value, number};
use crate::strings;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

/// A sorted key-value map, ordered by UTF-8 bytes like Replicache's B-tree.
/// Copies share the entries until one is written.
#[derive(Clone, Default, Debug, PartialEq)]
pub(crate) struct KvMap(Rc<BTreeMap<String, Value>>);

impl KvMap {
    pub(crate) fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key)
    }

    pub(crate) fn has(&self, key: &str) -> bool {
        self.0.contains_key(key)
    }

    pub(crate) fn set(&mut self, key: impl Into<String>, value: Value) {
        Rc::make_mut(&mut self.0).insert(key.into(), value);
    }

    pub(crate) fn delete(&mut self, key: &str) -> bool {
        if !self.0.contains_key(key) {
            return false;
        }
        Rc::make_mut(&mut self.0).remove(key).is_some()
    }

    pub(crate) fn clear(&mut self) {
        self.0 = Rc::default();
    }

    /// The entries whose key starts with `prefix`, in key order.
    pub(crate) fn scan<'a>(
        &'a self,
        prefix: &'a str,
    ) -> impl Iterator<Item = (&'a String, &'a Value)> {
        self.0
            .range::<str, _>((
                std::ops::Bound::Included(prefix),
                std::ops::Bound::Unbounded,
            ))
            .take_while(move |(k, _)| k.starts_with(prefix))
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.0.iter()
    }
}

/// One change between two maps (`DiffOperation` in
/// `packages/replicache/src/btree/node.ts`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum KvDiffOp {
    Add { key: String, new: Value },
    Del { key: String, old: Value },
    Change { key: String, old: Value, new: Value },
}

impl KvDiffOp {
    pub(crate) fn key(&self) -> &str {
        match self {
            KvDiffOp::Add { key, .. }
            | KvDiffOp::Del { key, .. }
            | KvDiffOp::Change { key, .. } => key,
        }
    }
}

pub(crate) type KvDiff = Vec<KvDiffOp>;

/// The diff from `old` to `new` over `keys`, which must include every key
/// whose value differs. Ordered by key, as a B-tree diff is.
pub(crate) fn diff_keys<'a>(
    old: &KvMap,
    new: &KvMap,
    keys: impl IntoIterator<Item = &'a String>,
) -> KvDiff {
    let mut keys: Vec<&String> = keys.into_iter().collect();
    keys.sort();
    keys.dedup();
    let mut diff = Vec::new();
    for key in keys {
        match (old.get(key), new.get(key)) {
            (None, None) => {}
            (None, Some(v)) => diff.push(KvDiffOp::Add {
                key: key.clone(),
                new: v.clone(),
            }),
            (Some(v), None) => diff.push(KvDiffOp::Del {
                key: key.clone(),
                old: v.clone(),
            }),
            (Some(a), Some(b)) => {
                if a != b {
                    diff.push(KvDiffOp::Change {
                        key: key.clone(),
                        old: a.clone(),
                        new: b.clone(),
                    })
                }
            }
        }
    }
    diff
}

/// The diff from `old` to `new` over every key of both.
pub(crate) fn diff_all(old: &KvMap, new: &KvMap) -> KvDiff {
    if Rc::ptr_eq(&old.0, &new.0) {
        return Vec::new();
    }
    let keys: Vec<&String> = old.0.keys().chain(new.0.keys()).collect();
    diff_keys(old, new, keys)
}

/// The ops of `diff` whose key starts with `prefix`.
pub(crate) fn with_prefix(diff: &KvDiff, prefix: &str) -> KvDiff {
    diff.iter()
        .filter(|op| op.key().starts_with(prefix))
        .cloned()
        .collect()
}

/// A Replicache write transaction over a copy of a map. Records the keys it
/// writes so a later diff only has to look at those.
#[derive(Clone, Debug)]
pub(crate) struct KvWrite {
    pub(crate) map: KvMap,
    pub(crate) writes: HashSet<String>,
    /// Set when the whole map was cleared.
    pub(crate) cleared: bool,
}

impl KvWrite {
    pub(crate) fn new(map: KvMap) -> Self {
        KvWrite {
            map,
            writes: HashSet::new(),
            cleared: false,
        }
    }

    pub(crate) fn has(&self, key: &str) -> bool {
        self.map.has(key)
    }

    pub(crate) fn get(&self, key: &str) -> Option<&Value> {
        self.map.get(key)
    }

    pub(crate) fn set(&mut self, key: &str, value: Value) {
        self.map.set(key, value);
        self.writes.insert(key.to_owned());
    }

    pub(crate) fn delete(&mut self, key: &str) -> bool {
        self.writes.insert(key.to_owned());
        self.map.delete(key)
    }

    pub(crate) fn clear(&mut self) {
        for (k, _) in self.map.iter() {
            self.writes.insert(k.clone());
        }
        self.map.clear();
        self.cleared = true;
    }
}

/// A patch operation of a pull response (`PatchOperationInternal`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum KvPatchOp {
    Put {
        key: String,
        value: Value,
    },
    Update {
        key: String,
        merge: Option<Object>,
        constrain: Option<Vec<String>>,
    },
    Del {
        key: String,
    },
    Clear,
}

impl KvPatchOp {
    fn key(&self) -> Option<&str> {
        match self {
            KvPatchOp::Put { key, .. } | KvPatchOp::Update { key, .. } | KvPatchOp::Del { key } => {
                Some(key)
            }
            KvPatchOp::Clear => None,
        }
    }
}

/// A patch that doesn't apply.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PatchError(pub(crate) String);

/// Applies `patch` to `write` (`apply` in
/// `packages/replicache/src/sync/patch.ts`).
pub(crate) fn apply_patch(patch: &[KvPatchOp], write: &mut KvWrite) -> Result<(), PatchError> {
    for op in optimize_patch(patch) {
        match op {
            KvPatchOp::Clear => write.clear(),
            KvPatchOp::Put { key, value } => write.set(&key, value),
            KvPatchOp::Update {
                key,
                merge,
                constrain,
            } => {
                let mut next = Object::new();
                let mut entries: Vec<(String, Value)> = Vec::new();
                if let Some(existing) = write.get(&key) {
                    let Value::Object(o) = existing else {
                        return Err(PatchError(format!("Expected object for update of {key}")));
                    };
                    entries.extend(o.iter().map(|(k, v)| (k.clone(), v.clone())));
                }
                if let Some(merge) = merge {
                    entries.extend(merge.iter().map(|(k, v)| (k.clone(), v.clone())));
                }
                for (k, v) in entries {
                    let keep = match &constrain {
                        None => true,
                        Some(c) => c.is_empty() || c.contains(&k),
                    };
                    if keep {
                        next.insert(k, v);
                    }
                }
                write.set(&key, Value::Object(next));
            }
            KvPatchOp::Del { key } => {
                if write.has(&key) {
                    write.delete(&key);
                }
            }
        }
    }
    Ok(())
}

/// `optimizePatch`: drops what the last `clear` and later puts and dels of
/// the same key make irrelevant, and sorts by key. A patch with an `update`
/// is applied as sent, from its last `clear`.
pub(crate) fn optimize_patch(patch: &[KvPatchOp]) -> Vec<KvPatchOp> {
    if patch.is_empty() {
        return Vec::new();
    }
    let clear_index = patch.iter().rposition(|op| matches!(op, KvPatchOp::Clear));
    let start = clear_index.map_or(0, |i| i + 1);
    let mut result = Vec::new();
    if clear_index.is_some() {
        result.push(KvPatchOp::Clear);
    }
    let mut order: Vec<String> = Vec::new();
    let mut ops: HashMap<String, KvPatchOp> = HashMap::new();
    for op in &patch[start..] {
        match op {
            KvPatchOp::Put { key, .. } | KvPatchOp::Del { key } => {
                if ops.insert(key.clone(), op.clone()).is_none() {
                    order.push(key.clone());
                }
            }
            KvPatchOp::Update { .. } => {
                return match clear_index {
                    None => patch.to_vec(),
                    Some(i) => patch[i..].to_vec(),
                };
            }
            KvPatchOp::Clear => {}
        }
    }
    let mut rest: Vec<KvPatchOp> = Vec::new();
    for k in order {
        let op = ops.remove(&k).expect("recorded");
        if clear_index.is_some() && matches!(op, KvPatchOp::Del { .. }) {
            continue;
        }
        rest.push(op);
    }
    rest.sort_by(|a, b| a.key().cmp(&b.key()));
    result.extend(rest);
    result
}

/// `compareCookies` (`packages/replicache/src/cookies.ts`) for the cookies
/// Zero uses: null, strings, numbers, and objects with an `order`.
pub(crate) fn compare_cookies(a: &Value, b: &Value) -> f64 {
    if a == b {
        return 0.0;
    }
    if a.is_null() {
        return -1.0;
    }
    if b.is_null() {
        return 1.0;
    }
    fn value(c: &Value) -> Value {
        match c {
            Value::Object(o) => o.get("order").cloned().unwrap_or(Value::Null),
            c => c.clone(),
        }
    }
    let (va, vb) = (value(a), value(b));
    if let (Value::Number(x), Value::Number(y)) = (&va, &vb) {
        return x - y;
    }
    fn string(v: &Value) -> String {
        match v {
            Value::String(s) => s.clone(),
            Value::Number(n) => number::format(*n),
            v => json::stringify(v),
        }
    }
    match strings::utf16(&string(&va), &string(&vb)) {
        Ordering::Less => -1.0,
        Ordering::Equal => 0.0,
        Ordering::Greater => 1.0,
    }
}

/// The last server snapshot.
#[derive(Clone, Debug, Default)]
pub(crate) struct Snapshot {
    pub(crate) map: KvMap,
    /// The server's cookie for the snapshot, null before the first poke.
    pub(crate) cookie: Value,
    pub(crate) last_mutation_ids: BTreeMap<String, f64>,
}

/// A local mutation the server hasn't confirmed.
#[derive(Clone, Debug)]
pub(crate) struct PendingMutation {
    pub(crate) id: f64,
    pub(crate) client_id: String,
    pub(crate) name: String,
    pub(crate) args: Value,
    /// `performance.now()` when the mutation was made.
    pub(crate) timestamp: f64,
    /// The keys its last run wrote.
    pub(crate) writes: HashSet<String>,
}

/// The snapshot, the pending mutations, and `main`: the snapshot with their
/// effects applied.
pub(crate) struct ReplicaStore {
    pub(crate) snapshot: Snapshot,
    pub(crate) main: KvMap,
    pub(crate) pending: Vec<PendingMutation>,
}

impl ReplicaStore {
    pub(crate) fn new(snapshot: Snapshot, pending: Vec<PendingMutation>) -> Self {
        ReplicaStore {
            main: snapshot.map.clone(),
            snapshot,
            pending,
        }
    }

    pub(crate) fn cookie(&self) -> &Value {
        &self.snapshot.cookie
    }

    /// The ID the next mutation of `client_id` gets.
    pub(crate) fn next_mutation_id(&self, client_id: &str) -> f64 {
        if let Some(last) = self.pending.iter().rev().find(|m| m.client_id == client_id) {
            return last.id + 1.0;
        }
        self.snapshot
            .last_mutation_ids
            .get(client_id)
            .copied()
            .unwrap_or(0.0)
            + 1.0
    }

    /// Commits a mutation that ran in `write` over `main`.
    pub(crate) fn commit_mutation(&mut self, mutation: PendingMutation, write: &KvWrite) -> KvDiff {
        let diff = diff_keys(&self.main, &write.map, &write.writes);
        self.pending.push(mutation);
        self.main = write.map.clone();
        diff
    }

    /// The keys whose value in `main` may differ from the snapshot's.
    pub(crate) fn pending_writes(&self) -> HashSet<String> {
        self.pending
            .iter()
            .flat_map(|m| m.writes.iter().cloned())
            .collect()
    }

    /// Replaces the snapshot and the pending mutations after a poke. Returns
    /// the diff of `main`.
    pub(crate) fn advance(
        &mut self,
        snapshot: Snapshot,
        main: KvMap,
        pending: Vec<PendingMutation>,
        touched: Option<&HashSet<String>>,
    ) -> KvDiff {
        let diff = match touched {
            Some(t) => diff_keys(&self.main, &main, t),
            None => diff_all(&self.main, &main),
        };
        self.snapshot = snapshot;
        self.pending = pending;
        self.main = main;
        diff
    }
}
