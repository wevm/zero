//! `FlippedJoin` (`packages/zql/src/ivm/flipped-join.ts`).

use super::*;
use crate::ast::System;
use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

/// The most entries in one batched parent fetch (`flipped-join.ts`).
pub const MULTI_CONSTRAINT_CHUNK_SIZE: usize = 256;

/// What [`FlippedJoin::new`] joins, and how.
pub struct FlippedJoinArgs {
    pub parent: Rc<dyn Input>,
    pub child: Rc<dyn Input>,
    pub parent_key: Vec<String>,
    pub child_key: Vec<String>,
    pub relationship_name: String,
    pub hidden: bool,
    pub system: System,
    pub parent_partition_key: Option<Vec<String>>,
    pub bound_provider: Option<Rc<dyn TakeBoundProvider>>,
    /// An index kept elsewhere (by a `JoinIndexTap`); the join keeps its
    /// own when `None`.
    pub parent_index: Option<Rc<JoinIndex>>,
}

#[derive(Default)]
struct Inprogress {
    change: Option<Change>,
    position: Option<Row>,
    parent_fetch_bounds: Option<Vec<ParentFetchBound>>,
}

/// A child node, and whether it is the in-progress removed child spliced
/// back in (JS compares it by identity).
#[derive(Clone)]
struct ChildNode {
    node: Node,
    is_removed_overlay: bool,
}

/// An inner join that fetches children first, then their parents. Outputs
/// parents, in parent order, that have at least one related child.
pub struct FlippedJoin {
    me: Weak<FlippedJoin>,
    parent: Rc<dyn Input>,
    child: Rc<dyn Input>,
    parent_key: Vec<String>,
    child_key: Vec<String>,
    relationship_name: String,
    schema: Rc<SourceSchema>,
    bound_provider: Option<Rc<dyn TakeBoundProvider>>,
    /// The parents this join has output.
    parent_index: Rc<JoinIndex>,
    indexes_parents: bool,
    output: RefCell<Weak<dyn Output>>,
    parent_output: Rc<ClosureOutput>,
    child_output: Rc<ClosureOutput>,
    inprogress: RefCell<Inprogress>,
}

impl FlippedJoin {
    pub fn new(args: FlippedJoinArgs) -> Rc<Self> {
        let FlippedJoinArgs {
            parent,
            child,
            parent_key,
            child_key,
            relationship_name,
            hidden,
            system,
            parent_partition_key,
            bound_provider,
            parent_index,
        } = args;
        assert!(
            !is_same(parent.as_ref(), child.as_ref()),
            "Parent and child must be different operators"
        );
        assert!(
            parent_key.len() == child_key.len(),
            "The parentKey and childKey keys must have same length"
        );
        let parent_schema = parent.get_schema();
        let (parent_index, indexes_parents) = match parent_index {
            Some(index) => (index, false),
            None => (
                JoinIndex::new(
                    parent_key.clone(),
                    parent_schema.primary_key.clone(),
                    parent_partition_key,
                ),
                true,
            ),
        };
        let mut child_schema = (*child.get_schema()).clone();
        child_schema.is_hidden = hidden;
        child_schema.system = system;
        let schema = Rc::new(parent_schema.adding(&relationship_name, Rc::new(child_schema)));
        let op = Rc::new_cyclic(|me: &Weak<FlippedJoin>| {
            let (push_parent, reconcile_parent) = (me.clone(), me.clone());
            let (push_child, reconcile_child) = (me.clone(), me.clone());
            FlippedJoin {
                me: me.clone(),
                parent: parent.clone(),
                child: child.clone(),
                parent_key,
                child_key,
                relationship_name,
                schema,
                bound_provider,
                parent_index,
                indexes_parents,
                output: RefCell::new(no_output()),
                parent_output: ClosureOutput::new(
                    move |c| {
                        if let Some(j) = push_parent.upgrade() {
                            j.push_parent(c)
                        }
                    },
                    move || {
                        if let Some(j) = reconcile_parent.upgrade() {
                            j.reconcile_output()
                        }
                    },
                ),
                child_output: ClosureOutput::new(
                    move |c| {
                        if let Some(j) = push_child.upgrade() {
                            j.push_child(c)
                        }
                    },
                    move || {
                        if let Some(j) = reconcile_child.upgrade() {
                            j.reconcile_output()
                        }
                    },
                ),
                inprogress: RefCell::default(),
            }
        });
        parent.set_output(Rc::downgrade(&op.parent_output) as Weak<dyn Output>);
        child.set_output(Rc::downgrade(&op.child_output) as Weak<dyn Output>);
        op
    }

    fn reconcile_output(&self) {
        let out = self.output.borrow().upgrade();
        if let Some(out) = out {
            out.reconcile(self);
        }
    }

    fn emit(&self, change: Change) {
        let out = self.output.borrow().upgrade();
        if let Some(out) = out {
            out.push(change, self);
        }
    }

    fn fetch_now(self: &Rc<Self>, req: &FetchRequest) -> NodeStream {
        // Constraints on the parent's join key become child constraints.
        let mut child_constraint = Constraint::default();
        if let Some(constraint) = &req.constraint {
            for (k, v) in &constraint.entries {
                if let Some(i) = self.parent_key.iter().position(|p| p == k) {
                    child_constraint.set(&self.child_key[i], v.clone());
                }
            }
        }
        let child_req = FetchRequest {
            constraint: (!child_constraint.is_empty()).then_some(child_constraint),
            ..FetchRequest::default()
        };
        let mut child_nodes: Vec<ChildNode> = self
            .child
            .fetch(&child_req)
            .map(|node| ChildNode {
                node,
                is_removed_overlay: false,
            })
            .collect();

        // Undo an in-progress remove here; it is re-applied below to the
        // parents the push has reached.
        let removed = match &self.inprogress.borrow().change {
            Some(Change::Remove(n)) => Some(n.clone()),
            _ => None,
        };
        if let Some(removed) = removed {
            let compare = self.child.get_schema().compare_rows.clone();
            let pos = binary_search(child_nodes.len(), |i| {
                compare(&removed.row, &child_nodes[i].node.row)
            });
            child_nodes.insert(
                pos,
                ChildNode {
                    node: removed,
                    is_removed_overlay: true,
                },
            );
        }
        self.fetch_batched(req, child_nodes)
    }

    fn fetch_batched(
        self: &Rc<Self>,
        req: &FetchRequest,
        child_nodes: Vec<ChildNode>,
    ) -> NodeStream {
        let mut computed_multi: Vec<Constraint> = Vec::new();
        let mut child_indexes_by_key: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, child) in child_nodes.iter().enumerate() {
            let Some(from_child) =
                build_join_constraint(&child.node.row, &self.child_key, &self.parent_key)
            else {
                continue;
            };
            if let Some(c) = &req.constraint
                && !from_child.is_compatible(c)
            {
                continue;
            }
            let key = canonical_key_of_constraint(&from_child, &self.parent_key);
            match child_indexes_by_key.get_mut(&key) {
                Some(idxs) => idxs.push(i),
                None => {
                    child_indexes_by_key.insert(key, vec![i]);
                    computed_multi.push(from_child);
                }
            }
        }
        if computed_multi.is_empty() {
            return empty();
        }

        let incoming = req.multi_constraints.clone().unwrap_or_default();
        let with_multi = |chunk: &[Constraint]| {
            let mut multi = incoming.clone();
            multi.push(chunk.to_vec());
            self.parent.fetch(&FetchRequest {
                multi_constraints: Some(multi),
                ..req.clone()
            })
        };
        let parent_stream = if computed_multi.len() <= MULTI_CONSTRAINT_CHUNK_SIZE {
            with_multi(&computed_multi)
        } else {
            let compare_rows = self.schema.compare_rows.clone();
            let reverse = req.reverse;
            merge_sorted_streams(
                computed_multi
                    .chunks(MULTI_CONSTRAINT_CHUNK_SIZE)
                    .map(with_multi)
                    .collect(),
                Rc::new(move |a: &Node, b: &Node| {
                    if reverse {
                        compare_rows(&b.row, &a.row)
                    } else {
                        compare_rows(&a.row, &b.row)
                    }
                }),
            )
        };

        let me = self.clone();
        Box::new(parent_stream.filter_map(move |node| {
            // A parent that matches none of the computed constraints comes
            // from an operator that passes multiConstraints through
            // unfiltered.
            let idxs = child_indexes_by_key.get(&canonical_key(&node.row, &me.parent_key))?;
            let related: Vec<&ChildNode> = idxs.iter().map(|&i| &child_nodes[i]).collect();
            me.parent_with_overlay(node, &related)
        }))
    }

    fn parent_with_overlay(&self, min_parent: Node, related: &[&ChildNode]) -> Option<Node> {
        let mut overlaid: Vec<Node> = related.iter().map(|c| c.node.clone()).collect();
        let inprogress = {
            let p = self.inprogress.borrow();
            p.position
                .clone()
                .map(|position| (p.change.clone(), position, p.parent_fetch_bounds.clone()))
        };
        if let Some((Some(change), position, bounds)) = inprogress {
            // The parent has yet to get the in-progress child change if it
            // comes after the current position and a parent fetch of the
            // push yields it.
            let compare_rows = self.parent.get_schema().compare_rows.clone();
            let in_push_queue = compare_rows(&min_parent.row, &position) == Ordering::Greater
                && bounds
                    .as_ref()
                    .is_none_or(|b| is_in_parent_fetch(b, &min_parent.row, &compare_rows));
            if is_join_match(
                &change.node().row,
                &self.child_key,
                &min_parent.row,
                &self.parent_key,
            ) {
                if change.change_type() == ChangeType::Remove {
                    if !in_push_queue {
                        // The removed child was spliced into the children
                        // above.
                        overlaid = related
                            .iter()
                            .filter(|c| !c.is_removed_overlay)
                            .map(|c| c.node.clone())
                            .collect();
                    }
                } else if in_push_queue {
                    overlaid =
                        generate_with_overlay(from_vec(overlaid), change, self.child.get_schema())
                            .collect();
                }
            }
        }
        if overlaid.is_empty() {
            return None;
        }
        self.index_parent_row(&min_parent.row);
        let children = Rc::new(overlaid);
        Some(min_parent.with(
            &self.relationship_name,
            Rc::new(move || from_vec(children.as_ref().clone())),
        ))
    }

    fn push_child(&self, change: Change) {
        match &change {
            Change::Add(_) | Change::Remove(_) => self.push_child_change(change, false),
            Change::Edit { node, old_node } => {
                assert!(
                    row_equals_for_compound_key(&old_node.row, &node.row, &self.child_key),
                    "Child edit must not change relationship."
                );
                self.push_child_change(change, true)
            }
            Change::Child(..) => self.push_child_change(change, true),
        }
    }

    fn push_child_change(&self, change: Change, exists: bool) {
        {
            let mut p = self.inprogress.borrow_mut();
            p.change = Some(change.clone());
            p.position = None;
        }
        self.push_child_change_inner(change, exists);
        *self.inprogress.borrow_mut() = Inprogress::default();
    }

    /// The `child_key`-to-`parent_key` fetch of a parent's children.
    fn child_stream(&self, parent_row: &Row) -> RelationshipStream {
        let me = self.me.clone();
        let row = parent_row.clone();
        Rc::new(move || -> NodeStream {
            let Some(me) = me.upgrade() else {
                return empty();
            };
            match build_join_constraint(&row, &me.parent_key, &me.child_key) {
                Some(c) => me.child.fetch(&FetchRequest {
                    constraint: Some(c),
                    ..FetchRequest::default()
                }),
                None => empty(),
            }
        })
    }

    fn push_child_change_inner(&self, change: Change, exists: bool) {
        let child_row = change.node().row.clone();
        let Some(constraint) = build_join_constraint(&child_row, &self.child_key, &self.parent_key)
        else {
            return;
        };
        let change_type = change.change_type();
        let adds = matches!(change_type, ChangeType::Add | ChangeType::Child);
        let partitions = self.parent_index.lookup(&child_row, &self.child_key);
        // Without a resident parent, a remove or edit cannot affect the view.
        let fetch_constraints: Vec<Constraint> = match partitions {
            _ if adds => vec![constraint],
            None => return,
            Some(partitions) => partitions
                .into_iter()
                .map(|p| match p {
                    Some(p) => constraint.merging(&p),
                    None => constraint.clone(),
                })
                .collect(),
        };
        if let Some(provider) = &self.bound_provider {
            self.inprogress.borrow_mut().parent_fetch_bounds = Some(read_parent_fetch_bounds(
                provider.as_ref(),
                &fetch_constraints,
            ));
        }
        let fetch = |c: Constraint| {
            self.parent.fetch(&FetchRequest {
                constraint: Some(c),
                ..FetchRequest::default()
            })
        };
        let parent_nodes: NodeStream = if fetch_constraints.len() == 1 {
            fetch(fetch_constraints.into_iter().next().unwrap())
        } else {
            let compare = self.schema.compare_rows.clone();
            merge_sorted_streams(
                fetch_constraints.into_iter().map(fetch).collect(),
                Rc::new(move |a: &Node, b: &Node| compare(&a.row, &b.row)),
            )
        };
        let child_compare = self.child.get_schema().compare_rows.clone();
        for parent_node in parent_nodes {
            {
                let mut p = self.inprogress.borrow_mut();
                p.change = Some(change.clone());
                p.position = Some(parent_node.row.clone());
            }
            let child_stream = self.child_stream(&parent_node.row);
            let parent_exists = exists
                || child_stream().any(|n| child_compare(&n.row, &child_row) != Ordering::Equal);
            if parent_exists {
                self.emit(Change::child(
                    parent_node.with(&self.relationship_name, child_stream),
                    self.relationship_name.clone(),
                    change.clone(),
                ));
            } else {
                let changed = change.node().clone();
                let new_node = parent_node.clone().with(
                    &self.relationship_name,
                    Rc::new(move || from_vec(vec![changed.clone()])),
                );
                if change_type == ChangeType::Add {
                    self.index_parent_row(&parent_node.row);
                    self.emit(Change::Add(new_node));
                } else {
                    self.unindex_parent_row(&parent_node.row);
                    self.emit(Change::Remove(new_node));
                }
            }
        }
    }

    fn index_parent_row(&self, row: &Row) {
        if self.indexes_parents {
            self.parent_index.add(row);
        }
    }

    fn unindex_parent_row(&self, row: &Row) {
        if self.indexes_parents {
            self.parent_index.remove(row);
        }
    }

    fn push_parent(&self, change: Change) {
        let flip = |node: Node| {
            let stream = self.child_stream(&node.row);
            node.with(&self.relationship_name, stream)
        };
        // An inner join: no related child, no push.
        let Some(c) = build_join_constraint(&change.node().row, &self.parent_key, &self.child_key)
        else {
            return;
        };
        let has_child = self
            .child
            .fetch(&FetchRequest {
                constraint: Some(c),
                ..FetchRequest::default()
            })
            .next()
            .is_some();
        if !has_child {
            return;
        }
        match change {
            Change::Add(node) => {
                self.index_parent_row(&node.row);
                self.emit(Change::Add(flip(node)));
            }
            Change::Remove(node) => {
                self.unindex_parent_row(&node.row);
                self.emit(Change::Remove(flip(node)));
            }
            Change::Child(node, child) => self.emit(Change::Child(flip(node), child)),
            Change::Edit { node, old_node } => {
                assert!(
                    row_equals_for_compound_key(&old_node.row, &node.row, &self.parent_key),
                    "Parent edit must not change relationship."
                );
                self.unindex_parent_row(&old_node.row);
                self.index_parent_row(&node.row);
                self.emit(Change::Edit {
                    node: flip(node),
                    old_node: flip(old_node),
                });
            }
        }
    }
}

impl InputBase for FlippedJoin {
    fn get_schema(&self) -> Rc<SourceSchema> {
        self.schema.clone()
    }
    fn destroy(&self) {
        self.child.destroy();
        self.parent.destroy();
    }
}

impl Input for FlippedJoin {
    fn set_output(&self, output: Weak<dyn Output>) {
        *self.output.borrow_mut() = output;
    }

    fn fetch(&self, req: &FetchRequest) -> NodeStream {
        let me = self.me.clone();
        let req = req.clone();
        lazy(move || -> NodeStream {
            match me.upgrade() {
                Some(me) => me.fetch_now(&req),
                None => empty(),
            }
        })
    }
}

/// `binarySearch(high, compare)` (`shared/src/binary-search.ts`): the index
/// where `compare` is equal, or the first index past the elements it is
/// greater for.
pub fn binary_search(high: usize, compare: impl Fn(usize) -> Ordering) -> usize {
    let (mut low, mut high) = (0, high);
    while low < high {
        let mid = low + ((high - low) >> 1);
        match compare(mid) {
            Ordering::Equal => return mid,
            Ordering::Greater => low = mid + 1,
            Ordering::Less => high = mid,
        }
    }
    low
}
