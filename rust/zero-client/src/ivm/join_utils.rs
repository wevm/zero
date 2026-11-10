//! Helpers shared by `Join` and `FlippedJoin` (`join-utils.ts`), and the
//! join index (`join-index.ts`).

use super::*;
use crate::json::{self, Value};
use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;

/// `generateWithOverlay`: applies the in-flight child change `overlay` to a
/// sorted child stream, so a parent that has yet to receive the change sees
/// the children as they were.
pub fn generate_with_overlay(
    stream: NodeStream,
    overlay: Change,
    schema: Rc<SourceSchema>,
) -> NodeStream {
    Box::new(OrderedOverlay {
        stream,
        overlay,
        schema,
        applied: false,
        edit_old_applied: false,
        edit_new_applied: false,
        queue: VecDeque::new(),
        finished: false,
    })
}

struct OrderedOverlay {
    stream: NodeStream,
    overlay: Change,
    schema: Rc<SourceSchema>,
    applied: bool,
    edit_old_applied: bool,
    edit_new_applied: bool,
    queue: VecDeque<Node>,
    finished: bool,
}

impl Iterator for OrderedOverlay {
    type Item = Node;

    fn next(&mut self) -> Option<Node> {
        let compare = self.schema.compare_rows.clone();
        loop {
            if let Some(n) = self.queue.pop_front() {
                return Some(n);
            }
            if self.finished {
                return None;
            }
            let Some(node) = self.stream.next() else {
                self.finished = true;
                if !self.applied {
                    match &self.overlay {
                        Change::Remove(n) => {
                            self.applied = true;
                            return Some(n.clone());
                        }
                        Change::Edit { old_node, .. } => {
                            assert!(
                                self.edit_new_applied,
                                "edit overlay: new node must be applied before old node"
                            );
                            self.edit_old_applied = true;
                            self.applied = true;
                            return Some(old_node.clone());
                        }
                        _ => {}
                    }
                }
                assert!(
                    self.applied,
                    "overlayGenerator: overlay was never applied to any fetched node"
                );
                return None;
            };
            let mut yield_node = true;
            if !self.applied {
                match &self.overlay {
                    Change::Add(n) => {
                        if compare(&n.row, &node.row) == Ordering::Equal {
                            self.applied = true;
                            yield_node = false;
                        }
                    }
                    Change::Remove(n) => {
                        if compare(&n.row, &node.row) == Ordering::Less {
                            self.applied = true;
                            self.queue.push_back(n.clone());
                        }
                    }
                    Change::Edit { node: n, old_node } => {
                        if !self.edit_old_applied
                            && compare(&old_node.row, &node.row) == Ordering::Less
                        {
                            self.edit_old_applied = true;
                            if self.edit_new_applied {
                                self.applied = true;
                            }
                            self.queue.push_back(old_node.clone());
                        }
                        if !self.edit_new_applied && compare(&n.row, &node.row) == Ordering::Equal {
                            self.edit_new_applied = true;
                            if self.edit_old_applied {
                                self.applied = true;
                            }
                            yield_node = false;
                        }
                    }
                    Change::Child(n, child) => {
                        if compare(&n.row, &node.row) == Ordering::Equal {
                            self.applied = true;
                            self.queue
                                .push_back(overlay_child(&node, child, &self.schema));
                            yield_node = false;
                        }
                    }
                }
            }
            if yield_node {
                self.queue.push_back(node);
            }
        }
    }
}

/// `node` with the child change applied to the relationship it is on.
fn overlay_child(node: &Node, child: &ChildChange, schema: &SourceSchema) -> Node {
    let name = child.relationship_name.as_str();
    let original = node
        .relationships
        .get(name)
        .expect("overlay: relationship on node")
        .clone();
    let child_schema = schema
        .relationship(name)
        .expect("overlay: relationship in schema")
        .clone();
    let change = child.change.clone();
    node.clone().with(
        name,
        Rc::new(move || generate_with_overlay(original(), change.clone(), child_schema.clone())),
    )
}

/// `generateWithOverlayUnordered`: the overlay for an unordered child
/// stream. Removed rows go first; added rows are suppressed by primary key.
pub fn generate_with_overlay_unordered(
    stream: NodeStream,
    overlay: Change,
    schema: Rc<SourceSchema>,
) -> NodeStream {
    Box::new(UnorderedOverlay {
        stream,
        overlay,
        schema,
        injected: false,
        suppressed: false,
    })
}

struct UnorderedOverlay {
    stream: NodeStream,
    overlay: Change,
    schema: Rc<SourceSchema>,
    injected: bool,
    suppressed: bool,
}

impl Iterator for UnorderedOverlay {
    type Item = Node;

    fn next(&mut self) -> Option<Node> {
        if !self.injected {
            self.injected = true;
            match &self.overlay {
                Change::Remove(n) => return Some(n.clone()),
                Change::Edit { old_node, .. } => return Some(old_node.clone()),
                _ => {}
            }
        }
        let pk = &self.schema.primary_key;
        for node in self.stream.by_ref() {
            if !self.suppressed {
                match &self.overlay {
                    Change::Add(n) | Change::Edit { node: n, .. } => {
                        if row_equals_for_compound_key(&n.row, &node.row, pk) {
                            self.suppressed = true;
                            continue;
                        }
                    }
                    Change::Child(n, child) => {
                        if row_equals_for_compound_key(&n.row, &node.row, pk) {
                            self.suppressed = true;
                            return Some(overlay_child(&node, child, &self.schema));
                        }
                    }
                    Change::Remove(_) => {}
                }
            }
            return Some(node);
        }
        assert!(
            self.suppressed || self.overlay.change_type() == ChangeType::Remove,
            "overlayGenerator: overlay was never applied to any fetched node"
        );
        None
    }
}

pub fn row_equals_for_compound_key(a: &Row, b: &Row, key: &[String]) -> bool {
    key.iter()
        .all(|k| compare_values(a.get(k), b.get(k)) == Ordering::Equal)
}

pub fn is_join_match(
    parent: &Row,
    parent_key: &[String],
    child: &Row,
    child_key: &[String],
) -> bool {
    parent_key
        .iter()
        .zip(child_key)
        .all(|(p, c)| values_equal(parent.get(p), child.get(c)))
}

/// `buildJoinConstraint`: `target_key` mapped to `source_row`'s `source_key`
/// values, or `None` if one is null (a null key matches nothing).
pub fn build_join_constraint(
    source_row: &Row,
    source_key: &[String],
    target_key: &[String],
) -> Option<Constraint> {
    let mut c = Constraint::default();
    for (s, t) in source_key.iter().zip(target_key) {
        let value = source_row.get(s);
        if matches!(value, Some(Value::Null)) {
            return None;
        }
        c.set(t, value.cloned());
    }
    Some(c)
}

/// `canonicalKey`: a string key over `keys`, tagged by type so `1` and `"1"`
/// differ.
pub fn canonical_key(row: &Row, keys: &[String]) -> String {
    canonical_key_with(keys, |k| row.get(k))
}

/// [`canonical_key`] of a constraint's values.
pub fn canonical_key_of_constraint(constraint: &Constraint, keys: &[String]) -> String {
    canonical_key_with(keys, |k| constraint.get(k))
}

fn canonical_key_with<'a>(keys: &[String], get: impl Fn(&str) -> Option<&'a Value>) -> String {
    let mut s = String::new();
    for (i, k) in keys.iter().enumerate() {
        if i > 0 {
            s.push('\0');
        }
        match get(k) {
            None | Some(Value::Null) => s.push('n'),
            Some(Value::String(v)) => {
                s.push('s');
                s.push_str(v);
            }
            Some(Value::Number(n)) => {
                s.push('d');
                s.push_str(&json::number::format(*n));
            }
            Some(Value::Bool(b)) => s.push(if *b { 't' } else { 'f' }),
            Some(v) => {
                s.push('j');
                s.push_str(&json::stringify(v));
            }
        }
    }
    s
}

/// The partitions of indexed parents that share a join key, as fetch
/// constraints; `[None]` for an unpartitioned index (`join-index.ts`).
pub type ParentPartitions = Vec<Option<Constraint>>;

struct Partition {
    constraint: Constraint,
    pks: HashSet<String>,
}

#[derive(Default)]
struct JoinIndexState {
    unpartitioned: HashMap<String, HashSet<String>>,
    /// Join key, then partition key. Partitions keep insertion order.
    partitioned: HashMap<String, (Vec<String>, HashMap<String, Partition>)>,
    size: usize,
}

/// An index from join key to the parent rows that have it. `Join` and
/// `FlippedJoin` skip the parent fetch of a child change when no indexed
/// parent can see it.
pub struct JoinIndex {
    parent_key: Vec<String>,
    primary_key: Vec<String>,
    join_key_is_pk: bool,
    partition_key: Option<Vec<String>>,
    state: RefCell<JoinIndexState>,
}

impl JoinIndex {
    pub fn new(
        parent_key: Vec<String>,
        primary_key: Vec<String>,
        partition_key: Option<Vec<String>>,
    ) -> Rc<Self> {
        Rc::new(JoinIndex {
            join_key_is_pk: parent_key == primary_key,
            parent_key,
            primary_key,
            partition_key,
            state: RefCell::default(),
        })
    }

    /// The number of parent rows indexed.
    pub fn size(&self) -> usize {
        self.state.borrow().size
    }

    fn pk_of(&self, row: &Row, join_key: &str) -> String {
        if self.join_key_is_pk {
            join_key.to_owned()
        } else {
            canonical_key(row, &self.primary_key)
        }
    }

    pub fn add(&self, row: &Row) {
        let Some(join_key) = join_key_of(row, &self.parent_key) else {
            return;
        };
        let pk = self.pk_of(row, &join_key);
        let mut state = self.state.borrow_mut();
        let state = &mut *state;
        let Some(partition_key) = &self.partition_key else {
            if state.unpartitioned.entry(join_key).or_default().insert(pk) {
                state.size += 1;
            }
            return;
        };
        let part_key = canonical_key(row, partition_key);
        let (order, partitions) = state.partitioned.entry(join_key).or_default();
        let partition = partitions.entry(part_key.clone()).or_insert_with(|| {
            order.push(part_key);
            Partition {
                constraint: Constraint::from_row(row, partition_key),
                pks: HashSet::new(),
            }
        });
        if partition.pks.insert(pk) {
            state.size += 1;
        }
    }

    pub fn remove(&self, row: &Row) {
        let Some(join_key) = join_key_of(row, &self.parent_key) else {
            return;
        };
        let pk = self.pk_of(row, &join_key);
        let mut state = self.state.borrow_mut();
        let state = &mut *state;
        let Some(partition_key) = &self.partition_key else {
            if let Some(pks) = state.unpartitioned.get_mut(&join_key)
                && pks.remove(&pk)
            {
                state.size -= 1;
                if pks.is_empty() {
                    state.unpartitioned.remove(&join_key);
                }
            }
            return;
        };
        let Some((order, partitions)) = state.partitioned.get_mut(&join_key) else {
            return;
        };
        let part_key = canonical_key(row, partition_key);
        let Some(partition) = partitions.get_mut(&part_key) else {
            return;
        };
        if partition.pks.remove(&pk) {
            state.size -= 1;
            if partition.pks.is_empty() {
                partitions.remove(&part_key);
                order.retain(|k| *k != part_key);
                if partitions.is_empty() {
                    state.partitioned.remove(&join_key);
                }
            }
        }
    }

    /// The partitions of the indexed parents whose join key matches
    /// `child_row` over `child_key`, or `None` if there are none.
    pub fn lookup(&self, child_row: &Row, child_key: &[String]) -> Option<ParentPartitions> {
        let join_key = join_key_of(child_row, child_key)?;
        let state = self.state.borrow();
        if self.partition_key.is_none() {
            return state
                .unpartitioned
                .contains_key(&join_key)
                .then(|| vec![None]);
        }
        let (order, partitions) = state.partitioned.get(&join_key)?;
        Some(
            order
                .iter()
                .map(|k| Some(partitions[k].constraint.clone()))
                .collect(),
        )
    }
}

/// The canonical join key of `row`, or `None` if part of it is null.
fn join_key_of(row: &Row, key: &[String]) -> Option<String> {
    if key.iter().any(|k| matches!(row.get(k), Some(Value::Null))) {
        return None;
    }
    Some(canonical_key(row, key))
}
