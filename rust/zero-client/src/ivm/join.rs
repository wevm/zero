//! `Join`, the `TakeGate` that caps its parent fetches, and the
//! `JoinIndexTap` that keeps EXISTS joins' parent indexes
//! (`packages/zql/src/ivm/join.ts`, `take-gate.ts`, and
//! `join-index-tap.ts`).

use super::*;
use crate::ast::{CorrelatedSubqueryOperator, System};
use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::collections::HashSet;
use std::rc::{Rc, Weak};

fn upgrade<T: ?Sized>(w: &RefCell<Weak<T>>) -> Option<Rc<T>> {
    w.borrow().upgrade()
}

/// Set when a join feeds an EXISTS or NOT EXISTS condition (`ExistsJoin` in
/// `join.ts`). Its parent index is kept by a [`JoinIndexTap`] at the end of
/// the pipeline.
#[derive(Clone)]
pub struct ExistsJoin {
    pub op: CorrelatedSubqueryOperator,
    pub parent_index: Rc<JoinIndex>,
}

/// What [`Join::new`] joins, and how.
pub struct JoinArgs {
    pub parent: Rc<dyn Input>,
    pub child: Rc<dyn Input>,
    pub parent_key: Vec<String>,
    pub child_key: Vec<String>,
    pub relationship_name: String,
    pub hidden: bool,
    pub system: System,
    pub parent_partition_key: Option<Vec<String>>,
    pub bound_provider: Option<Rc<dyn TakeBoundProvider>>,
    pub exists: Option<ExistsJoin>,
}

/// The child change a join is pushing to its parents.
#[derive(Default)]
struct Inprogress {
    change: Option<Change>,
    /// The last parent row the change reached.
    position: Option<Row>,
    /// Primary keys of the parents the change reached, kept when the parent
    /// input is unordered.
    reached_parents: Option<HashSet<String>>,
    parent_fetch_bounds: Option<Vec<ParentFetchBound>>,
}

/// Joins a parent and a child input into hierarchical output: each parent
/// node gets a relationship streaming its children (`join.ts`).
pub struct Join {
    me: Weak<Join>,
    parent: Rc<dyn Input>,
    child: Rc<dyn Input>,
    parent_key: Vec<String>,
    child_key: Vec<String>,
    relationship_name: String,
    schema: Rc<SourceSchema>,
    parent_index: Rc<JoinIndex>,
    /// Whether this join maintains `parent_index` itself; not for EXISTS
    /// joins.
    indexes_parents: bool,
    /// The child change type still pushed when no indexed parent has its
    /// key.
    qualifying_child_change: Option<ChangeType>,
    bound_provider: Option<Rc<dyn TakeBoundProvider>>,
    output: RefCell<Weak<dyn Output>>,
    parent_output: Rc<ClosureOutput>,
    child_output: Rc<ClosureOutput>,
    inprogress: RefCell<Inprogress>,
}

impl Join {
    pub fn new(args: JoinArgs) -> Rc<Self> {
        let JoinArgs {
            parent,
            child,
            parent_key,
            child_key,
            relationship_name,
            hidden,
            system,
            parent_partition_key,
            bound_provider,
            exists,
        } = args;
        assert!(
            !is_same(parent.as_ref(), child.as_ref()),
            "Parent and child must be different operators"
        );
        assert!(
            parent_key.len() == child_key.len(),
            "The parentKey and childKey keys must have same length"
        );
        assert!(
            exists.is_none() || parent_partition_key.is_none(),
            "EXISTS joins are not partitioned"
        );
        let parent_schema = parent.get_schema();
        let (parent_index, indexes_parents, qualifying_child_change) = match &exists {
            Some(exists) => (
                exists.parent_index.clone(),
                false,
                Some(match exists.op {
                    CorrelatedSubqueryOperator::Exists => ChangeType::Add,
                    CorrelatedSubqueryOperator::NotExists => ChangeType::Remove,
                }),
            ),
            None => (
                JoinIndex::new(
                    parent_key.clone(),
                    parent_schema.primary_key.clone(),
                    parent_partition_key,
                ),
                true,
                None,
            ),
        };
        let mut child_schema = (*child.get_schema()).clone();
        child_schema.is_hidden = hidden;
        child_schema.system = system;
        let schema = Rc::new(parent_schema.adding(&relationship_name, Rc::new(child_schema)));

        let op = Rc::new_cyclic(|me: &Weak<Join>| {
            let (push_parent, reconcile_parent) = (me.clone(), me.clone());
            let (push_child, reconcile_child) = (me.clone(), me.clone());
            Join {
                me: me.clone(),
                parent: parent.clone(),
                child: child.clone(),
                parent_key,
                child_key,
                relationship_name,
                schema,
                parent_index,
                indexes_parents,
                qualifying_child_change,
                bound_provider,
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
        if let Some(out) = upgrade(&self.output) {
            out.reconcile(self);
        }
    }

    fn emit(&self, change: Change) {
        if let Some(out) = upgrade(&self.output) {
            out.push(change, self);
        }
    }

    fn push_parent(&self, change: Change) {
        match change {
            Change::Add(node) => {
                self.index_parent_row(&node.row);
                self.emit(Change::Add(self.process_parent_node(&node)));
            }
            Change::Remove(node) => {
                self.unindex_parent_row(&node.row);
                self.emit(Change::Remove(self.process_parent_node(&node)));
            }
            Change::Child(node, child) => {
                self.emit(Change::Child(self.process_parent_node(&node), child));
            }
            Change::Edit { node, old_node } => {
                assert!(
                    row_equals_for_compound_key(&old_node.row, &node.row, &self.parent_key),
                    "Parent edit must not change relationship."
                );
                self.unindex_parent_row(&old_node.row);
                self.index_parent_row(&node.row);
                self.emit(Change::Edit {
                    node: self.process_parent_node(&node),
                    old_node: self.process_parent_node(&old_node),
                });
            }
        }
    }

    fn push_child(&self, change: Change) {
        if let Change::Edit { node, old_node } = &change {
            assert!(
                row_equals_for_compound_key(&old_node.row, &node.row, &self.child_key),
                "Child edit must not change relationship."
            );
        }
        let row = change.node().row.clone();
        {
            let mut p = self.inprogress.borrow_mut();
            p.change = Some(change.clone());
            p.position = None;
            p.reached_parents = self.parent.get_schema().sort.is_none().then(HashSet::new);
        }
        self.push_child_change(&row, change);
        *self.inprogress.borrow_mut() = Inprogress::default();
    }

    fn push_child_change(&self, child_row: &Row, change: Change) {
        let Some(constraint) = build_join_constraint(child_row, &self.child_key, &self.parent_key)
        else {
            return;
        };
        let fetch_constraints: Vec<Constraint> =
            match self.parent_index.lookup(child_row, &self.child_key) {
                Some(partitions) => partitions
                    .into_iter()
                    .map(|p| match p {
                        Some(p) => constraint.merging(&p),
                        None => constraint.clone(),
                    })
                    .collect(),
                None if Some(change.change_type()) == self.qualifying_child_change => {
                    vec![constraint]
                }
                None => return,
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
        for parent_node in parent_nodes {
            {
                let mut p = self.inprogress.borrow_mut();
                p.position = Some(parent_node.row.clone());
                if let Some(reached) = &mut p.reached_parents {
                    reached.insert(canonical_key(&parent_node.row, &self.schema.primary_key));
                }
            }
            self.emit(Change::child(
                self.process_parent_node(&parent_node),
                self.relationship_name.clone(),
                change.clone(),
            ));
        }
    }

    /// Whether the in-progress child change has yet to reach `row` in the
    /// parent stream.
    fn is_after_inprogress_position(&self, p: &Inprogress, row: &Row) -> bool {
        let Some(position) = &p.position else {
            return false;
        };
        if let Some(reached) = &p.reached_parents {
            return !reached.contains(&canonical_key(row, &self.schema.primary_key));
        }
        (self.schema.compare_rows)(row, position) == Ordering::Greater
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

    fn process_parent_node(&self, parent: &Node) -> Node {
        let me = self.me.clone();
        let row = parent.row.clone();
        let child_stream = Rc::new(move || -> NodeStream {
            let Some(me) = me.upgrade() else {
                return empty();
            };
            let stream = match build_join_constraint(&row, &me.parent_key, &me.child_key) {
                Some(c) => me.child.fetch(&FetchRequest {
                    constraint: Some(c),
                    ..FetchRequest::default()
                }),
                None => empty(),
            };
            // The parent has yet to get the in-progress child change if it
            // comes after the current position and a parent fetch of the push
            // yields it.
            let overlay = {
                let p = me.inprogress.borrow();
                p.change.as_ref().and_then(|inprogress| {
                    let in_push_queue = me.is_after_inprogress_position(&p, &row)
                        && p.parent_fetch_bounds.as_ref().is_none_or(|bounds| {
                            is_in_parent_fetch(bounds, &row, &me.schema.compare_rows)
                        });
                    (in_push_queue
                        && is_join_match(
                            &row,
                            &me.parent_key,
                            &inprogress.node().row,
                            &me.child_key,
                        ))
                    .then(|| inprogress.clone())
                })
            };
            let Some(overlay) = overlay else {
                return stream;
            };
            let child_schema = me.child.get_schema();
            if child_schema.sort.is_none() {
                generate_with_overlay_unordered(stream, overlay, child_schema)
            } else {
                generate_with_overlay(stream, overlay, child_schema)
            }
        });
        let mut node = parent.clone();
        node.relationships
            .set(&self.relationship_name, Some(child_stream));
        node
    }
}

impl InputBase for Join {
    fn get_schema(&self) -> Rc<SourceSchema> {
        self.schema.clone()
    }
    fn destroy(&self) {
        self.parent.destroy();
        self.child.destroy();
    }
}

impl Input for Join {
    fn set_output(&self, output: Weak<dyn Output>) {
        *self.output.borrow_mut() = output;
    }

    fn fetch(&self, req: &FetchRequest) -> NodeStream {
        let me = self.me.clone();
        let req = req.clone();
        lazy(move || -> NodeStream {
            let Some(me) = me.upgrade() else {
                return empty();
            };
            let parents = me.parent.fetch(&req);
            Box::new(parents.map(move |node| {
                me.index_parent_row(&node.row);
                me.process_parent_node(&node)
            }))
        })
    }
}

/// Provides `Take`'s bound to upstream operators (`take-gate.ts`).
pub trait TakeBoundProvider {
    fn get_bound(&self, constraint: Option<&Constraint>) -> Option<Row>;
}

/// The bound a [`TakeGate`] caps one parent fetch of a child push at.
#[derive(Clone, Debug)]
pub struct ParentFetchBound {
    pub constraint: Constraint,
    pub bound: Option<Row>,
}

/// Reads the bounds for a child push's parent fetches before they start.
pub fn read_parent_fetch_bounds(
    provider: &dyn TakeBoundProvider,
    constraints: &[Constraint],
) -> Vec<ParentFetchBound> {
    constraints
        .iter()
        .map(|c| ParentFetchBound {
            constraint: c.clone(),
            bound: provider.get_bound(Some(c)),
        })
        .collect()
}

/// Whether one of the parent fetches of a push yields `row`.
pub fn is_in_parent_fetch(bounds: &[ParentFetchBound], row: &Row, compare: &Comparator) -> bool {
    match bounds.iter().find(|b| b.constraint.matches(row)) {
        None => false,
        Some(ParentFetchBound { bound: None, .. }) => true,
        Some(ParentFetchBound {
            bound: Some(bound), ..
        }) => compare(row, bound) != Ordering::Greater,
    }
}

/// Caps upstream parent fetches at a downstream `Take`'s bound.
pub struct TakeGate {
    me: Weak<TakeGate>,
    input: Rc<dyn Input>,
    comparator: Comparator,
    bound_provider: RefCell<Option<Weak<dyn TakeBoundProvider>>>,
    output: RefCell<Weak<dyn Output>>,
    open_depth: Cell<usize>,
}

impl TakeGate {
    pub fn new(input: Rc<dyn Input>) -> Rc<Self> {
        let schema = input.get_schema();
        assert!(schema.sort.is_some(), "TakeGate requires sorted input");
        let op = Rc::new_cyclic(|me| TakeGate {
            me: me.clone(),
            input: input.clone(),
            comparator: schema.compare_rows.clone(),
            bound_provider: RefCell::new(None),
            output: RefCell::new(no_output()),
            open_depth: Cell::new(0),
        });
        input.set_output(Rc::downgrade(&op) as Weak<dyn Output>);
        op
    }

    pub fn open(&self) {
        self.open_depth.set(self.open_depth.get() + 1);
    }

    pub fn close(&self) {
        let depth = self.open_depth.get();
        assert!(depth > 0, "TakeGate.close called without matching open");
        self.open_depth.set(depth - 1);
    }

    pub fn set_bound_provider(&self, provider: Weak<dyn TakeBoundProvider>) {
        *self.bound_provider.borrow_mut() = Some(provider);
    }

    fn provider_bound(&self, constraint: Option<&Constraint>) -> Option<Row> {
        let provider = self.bound_provider.borrow().as_ref()?.upgrade()?;
        provider.get_bound(constraint)
    }
}

impl TakeBoundProvider for TakeGate {
    fn get_bound(&self, constraint: Option<&Constraint>) -> Option<Row> {
        if self.open_depth.get() > 0 {
            return None;
        }
        self.provider_bound(constraint)
    }
}

impl InputBase for TakeGate {
    fn get_schema(&self) -> Rc<SourceSchema> {
        self.input.get_schema()
    }
    fn destroy(&self) {
        self.input.destroy()
    }
}

impl Input for TakeGate {
    fn set_output(&self, output: Weak<dyn Output>) {
        *self.output.borrow_mut() = output;
    }

    fn fetch(&self, req: &FetchRequest) -> NodeStream {
        let me = self.me.clone();
        let req = req.clone();
        lazy(move || -> NodeStream {
            let Some(me) = me.upgrade() else {
                return empty();
            };
            let stream = me.input.fetch(&req);
            if me.open_depth.get() > 0 || req.reverse {
                return stream;
            }
            let Some(bound) = me.provider_bound(req.constraint.as_ref()) else {
                return stream;
            };
            let comparator = me.comparator.clone();
            Box::new(stream.take_while(move |n| comparator(&n.row, &bound) != Ordering::Greater))
        })
    }
}

impl Output for TakeGate {
    fn push(&self, change: Change, _pusher: &dyn InputBase) {
        if let Some(out) = upgrade(&self.output) {
            out.push(change, self);
        }
    }
    fn reconcile(&self, _pusher: &dyn InputBase) {
        if let Some(out) = upgrade(&self.output) {
            out.reconcile(self);
        }
    }
}

/// Keeps the parent indexes of a pipeline's EXISTS joins equal to the rows
/// the pipeline emits (`join-index-tap.ts`).
pub struct JoinIndexTap {
    input: Rc<dyn Input>,
    indexes: Rc<Vec<Rc<JoinIndex>>>,
    output: RefCell<Weak<dyn Output>>,
}

impl JoinIndexTap {
    pub fn new(input: Rc<dyn Input>, indexes: Vec<Rc<JoinIndex>>) -> Rc<Self> {
        let op = Rc::new(JoinIndexTap {
            input: input.clone(),
            indexes: Rc::new(indexes),
            output: RefCell::new(no_output()),
        });
        input.set_output(Rc::downgrade(&op) as Weak<dyn Output>);
        op
    }
}

impl InputBase for JoinIndexTap {
    fn get_schema(&self) -> Rc<SourceSchema> {
        self.input.get_schema()
    }
    fn destroy(&self) {
        self.input.destroy()
    }
}

impl Input for JoinIndexTap {
    fn set_output(&self, output: Weak<dyn Output>) {
        *self.output.borrow_mut() = output;
    }

    fn fetch(&self, req: &FetchRequest) -> NodeStream {
        let indexes = self.indexes.clone();
        Box::new(self.input.fetch(req).inspect(move |node| {
            for index in indexes.iter() {
                index.add(&node.row);
            }
        }))
    }
}

impl Output for JoinIndexTap {
    fn push(&self, change: Change, _pusher: &dyn InputBase) {
        match &change {
            Change::Add(node) => self.indexes.iter().for_each(|i| i.add(&node.row)),
            Change::Remove(node) => self.indexes.iter().for_each(|i| i.remove(&node.row)),
            Change::Edit { node, old_node } => {
                for index in self.indexes.iter() {
                    index.remove(&old_node.row);
                    index.add(&node.row);
                }
            }
            Change::Child(..) => {}
        }
        if let Some(out) = upgrade(&self.output) {
            out.push(change, self);
        }
    }
    fn reconcile(&self, _pusher: &dyn InputBase) {
        if let Some(out) = upgrade(&self.output) {
            out.reconcile(self);
        }
    }
}
