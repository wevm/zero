//! The local state Replicache keeps for Zero, without its storage
//! machinery: the last server snapshot, and the pending local mutations
//! whose effects are applied on top of it.

use crate::json::{self, Object, Value, number};
use crate::strings;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

/// The most entries or children a node of a [`KvMap`] holds.
const MAX_NODE: usize = 64;
/// A node with fewer after a delete merges with a neighbor that has room.
const MIN_NODE: usize = MAX_NODE / 4;

/// A node of a [`KvMap`]'s B+tree. Each entry is behind an `Rc`, so copying
/// a leaf to write it doesn't copy its values.
#[derive(Clone)]
enum Node {
    Leaf(Vec<Rc<(String, Value)>>),
    /// Each child with a lower bound of its keys, which also bounds the keys
    /// of the child before it from above; the first child's is unused.
    Internal(Vec<(String, Rc<Node>)>),
}

impl Node {
    fn len(&self) -> usize {
        match self {
            Node::Leaf(e) => e.len(),
            Node::Internal(c) => c.len(),
        }
    }

    /// The child whose keys `key` falls among.
    fn child_index(children: &[(String, Rc<Node>)], key: &str) -> usize {
        children
            .partition_point(|(bound, _)| bound.as_str() <= key)
            .saturating_sub(1)
    }

    fn get(&self, key: &str) -> Option<&Value> {
        let mut node = self;
        loop {
            match node {
                Node::Leaf(entries) => {
                    let i = entries.binary_search_by(|e| e.0.as_str().cmp(key)).ok()?;
                    return Some(&entries[i].1);
                }
                Node::Internal(children) => {
                    node = &children[Node::child_index(children, key)].1;
                }
            }
        }
    }

    /// Sets `key`, copying the shared nodes on its path. Returns whether the
    /// key is new, and the node's new right sibling with its lower bound if
    /// it split.
    fn insert(&mut self, key: String, value: Value) -> (bool, Option<(String, Rc<Node>)>) {
        let added = match self {
            Node::Leaf(entries) => match entries.binary_search_by(|e| e.0.as_str().cmp(&key)) {
                Ok(i) => {
                    entries[i] = Rc::new((key, value));
                    false
                }
                Err(i) => {
                    entries.insert(i, Rc::new((key, value)));
                    true
                }
            },
            Node::Internal(children) => {
                let i = Node::child_index(children, &key);
                let (added, split) = Rc::make_mut(&mut children[i].1).insert(key, value);
                if let Some(sibling) = split {
                    children.insert(i + 1, sibling);
                }
                added
            }
        };
        if self.len() <= MAX_NODE {
            return (added, None);
        }
        let mid = self.len() / 2;
        let sibling = match self {
            Node::Leaf(entries) => {
                let right = entries.split_off(mid);
                (right[0].0.clone(), Node::Leaf(right))
            }
            Node::Internal(children) => {
                let right = children.split_off(mid);
                (right[0].0.clone(), Node::Internal(right))
            }
        };
        (added, Some((sibling.0, Rc::new(sibling.1))))
    }

    /// Removes `key`, which the node has, copying the shared nodes on its
    /// path, and merges a child left with few entries into a neighbor.
    fn remove(&mut self, key: &str) {
        match self {
            Node::Leaf(entries) => {
                if let Ok(i) = entries.binary_search_by(|e| e.0.as_str().cmp(key)) {
                    entries.remove(i);
                }
            }
            Node::Internal(children) => {
                let i = Node::child_index(children, key);
                Rc::make_mut(&mut children[i].1).remove(key);
                let len = children[i].1.len();
                if len == 0 {
                    children.remove(i);
                } else if len < MIN_NODE && children.len() > 1 {
                    let left = if i + 1 < children.len() { i } else { i - 1 };
                    if children[left].1.len() + children[left + 1].1.len() <= MAX_NODE {
                        let (_, right) = children.remove(left + 1);
                        let right = Rc::unwrap_or_clone(right);
                        match (Rc::make_mut(&mut children[left].1), right) {
                            (Node::Leaf(a), Node::Leaf(b)) => a.extend(b),
                            (Node::Internal(a), Node::Internal(b)) => a.extend(b),
                            _ => unreachable!("the children of a node are at one depth"),
                        }
                    }
                }
            }
        }
    }
}

/// A sorted key-value map, ordered by UTF-8 bytes, as a persistent B+tree
/// like Replicache's (`packages/replicache/src/btree`): copies share their
/// nodes, and a write copies only the nodes on its key's path, so taking a
/// snapshot and writing to it costs O(log n), not a copy of every row.
#[derive(Clone, Default)]
pub(crate) struct KvMap {
    root: Option<Rc<Node>>,
    len: usize,
}

impl From<BTreeMap<String, Value>> for KvMap {
    fn from(map: BTreeMap<String, Value>) -> Self {
        let mut out = KvMap::default();
        for (k, v) in map {
            out.set(k, v);
        }
        out
    }
}

impl PartialEq for KvMap {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len && self.iter().eq(other.iter())
    }
}

impl std::fmt::Debug for KvMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl KvMap {
    pub(crate) fn get(&self, key: &str) -> Option<&Value> {
        self.root.as_ref()?.get(key)
    }

    pub(crate) fn has(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    pub(crate) fn set(&mut self, key: impl Into<String>, value: Value) {
        let root = self
            .root
            .get_or_insert_with(|| Rc::new(Node::Leaf(Vec::new())));
        let (added, split) = Rc::make_mut(root).insert(key.into(), value);
        if let Some(sibling) = split {
            let left = std::mem::replace(root, Rc::new(Node::Leaf(Vec::new())));
            *root = Rc::new(Node::Internal(vec![(String::new(), left), sibling]));
        }
        if added {
            self.len += 1;
        }
    }

    pub(crate) fn delete(&mut self, key: &str) -> bool {
        if !self.has(key) {
            return false;
        }
        let Some(root) = &mut self.root else {
            return false;
        };
        Rc::make_mut(root).remove(key);
        self.len -= 1;
        // A root with one child gives way to it.
        loop {
            let child = match &**root {
                Node::Internal(children) if children.len() == 1 => children[0].1.clone(),
                _ => break,
            };
            *root = child;
        }
        if root.len() == 0 {
            self.root = None;
        }
        true
    }

    pub(crate) fn clear(&mut self) {
        *self = KvMap::default();
    }

    /// Whether the two maps are the same copy, so they hold the same
    /// entries without comparing them.
    pub(crate) fn same(&self, other: &KvMap) -> bool {
        match (&self.root, &other.root) {
            (Some(a), Some(b)) => Rc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        }
    }

    /// The entries from the first key at or after `start`, in key order.
    fn iter_from(&self, start: Option<&str>) -> Iter<'_> {
        let mut stack = Vec::new();
        let mut node = self.root.as_deref();
        while let Some(n) = node {
            match n {
                Node::Leaf(entries) => {
                    let i = start.map_or(0, |s| entries.partition_point(|e| e.0.as_str() < s));
                    stack.push((n, i));
                    node = None;
                }
                Node::Internal(children) => {
                    let i = start.map_or(0, |s| Node::child_index(children, s));
                    stack.push((n, i + 1));
                    node = Some(&children[i].1);
                }
            }
        }
        Iter { stack }
    }

    /// The entries whose key starts with `prefix`, in key order.
    pub(crate) fn scan<'a>(
        &'a self,
        prefix: &'a str,
    ) -> impl Iterator<Item = (&'a String, &'a Value)> {
        self.iter_from(Some(prefix))
            .take_while(move |(k, _)| k.starts_with(prefix))
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.iter_from(None)
    }

    pub(crate) fn keys(&self) -> impl Iterator<Item = &String> {
        self.iter().map(|(k, _)| k)
    }
}

/// The entries of a [`KvMap`] in key order: the path to the next one, with
/// the position to go on from in each node.
struct Iter<'a> {
    stack: Vec<(&'a Node, usize)>,
}

impl<'a> Iterator for Iter<'a> {
    type Item = (&'a String, &'a Value);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let (node, i) = self.stack.last_mut()?;
            let node: &'a Node = node;
            match node {
                Node::Leaf(entries) if *i < entries.len() => {
                    let e = &entries[*i];
                    *i += 1;
                    return Some((&e.0, &e.1));
                }
                Node::Internal(children) if *i < children.len() => {
                    let child = &children[*i].1;
                    *i += 1;
                    self.stack.push((child, 0));
                }
                _ => {
                    self.stack.pop();
                }
            }
        }
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
    if old.same(new) {
        return Vec::new();
    }
    let keys: Vec<&String> = old.keys().chain(new.keys()).collect();
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Random writes to a map and to copies of it agree with a `BTreeMap`
    /// at every step, deep enough for three levels of nodes, and leave the
    /// earlier copies as they were.
    #[test]
    fn kv_map_matches_btree_map() {
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut random = |n: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % n
        };
        let mut map = KvMap::default();
        let mut model = BTreeMap::new();
        let mut snapshots: Vec<(KvMap, BTreeMap<String, Value>)> = Vec::new();
        for step in 0..60_000u64 {
            // Grow to about 6,000 keys, then shrink to none.
            let growing = step < 40_000;
            let key = format!(
                "{}/{:05}",
                ["e/a", "e/b", "m"][random(3) as usize],
                random(9000)
            );
            if random(10) < if growing { 7 } else { 1 } {
                map.set(key.clone(), Value::from(step as f64));
                model.insert(key, Value::from(step as f64));
            } else {
                let removed = model.remove(&key).is_some();
                assert_eq!(map.delete(&key), removed, "{key}");
            }
            if step % 997 == 0 {
                snapshots.push((map.clone(), model.clone()));
            }
            if step % 101 == 0 {
                assert_eq!(map.len, model.len());
                assert!(map.iter().eq(model.iter()), "step {step}");
                let probe = format!("e/b/{:05}", random(9000));
                assert_eq!(map.get(&probe), model.get(&probe));
                let prefix = ["e/a", "e/b/0", "m/", "e/", "z", ""][random(6) as usize];
                assert!(
                    map.scan(prefix)
                        .eq(model.iter().filter(|(k, _)| k.starts_with(prefix))),
                    "scan {prefix} at {step}"
                );
            }
        }
        // Deleting the rest empties it.
        for k in model.keys().cloned().collect::<Vec<_>>() {
            assert!(map.delete(&k));
        }
        assert_eq!(map, KvMap::default());
        for (copy, model) in &snapshots {
            assert!(copy.iter().eq(model.iter()));
        }
    }
}
