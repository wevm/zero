//! `UnionFanOut` and `UnionFanIn`, which fork and merge a pipeline for an OR
//! with flipped subqueries (`union-fan-out.ts`, `union-fan-in.ts`), and
//! `pushAccumulatedChanges` (`push-accumulated.ts`), which `FanIn` shares.

use super::*;
use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::collections::HashSet;
use std::rc::{Rc, Weak};

/// Forks a pipeline for an OR with flipped subqueries (`union-fan-out.ts`).
pub struct UnionFanOut {
    destroy_count: Cell<usize>,
    fan_in: RefCell<Weak<UnionFanIn>>,
    input: Rc<dyn Input>,
    outputs: RefCell<Vec<Weak<dyn Output>>>,
}

impl UnionFanOut {
    pub fn new(input: Rc<dyn Input>) -> Rc<Self> {
        let op = Rc::new(UnionFanOut {
            destroy_count: Cell::new(0),
            fan_in: RefCell::new(Weak::new()),
            input: input.clone(),
            outputs: RefCell::default(),
        });
        input.set_output(Rc::downgrade(&op) as Weak<dyn Output>);
        op
    }

    fn set_fan_in(&self, fan_in: &Rc<UnionFanIn>) {
        assert!(
            self.fan_in.borrow().upgrade().is_none(),
            "FanIn already set for this FanOut"
        );
        *self.fan_in.borrow_mut() = Rc::downgrade(fan_in);
    }

    fn fan_in(&self) -> Rc<UnionFanIn> {
        self.fan_in
            .borrow()
            .upgrade()
            .expect("UnionFanOut has no fan-in")
    }

    fn outputs(&self) -> Vec<Rc<dyn Output>> {
        self.outputs
            .borrow()
            .iter()
            .filter_map(Weak::upgrade)
            .collect()
    }
}

impl InputBase for UnionFanOut {
    fn get_schema(&self) -> Rc<SourceSchema> {
        self.input.get_schema()
    }
    fn destroy(&self) {
        let n = self.outputs.borrow().len();
        assert!(
            self.destroy_count.get() < n,
            "FanOut already destroyed once for each output"
        );
        self.destroy_count.set(self.destroy_count.get() + 1);
        if self.destroy_count.get() == n {
            self.input.destroy();
        }
    }
}

impl Input for UnionFanOut {
    fn set_output(&self, output: Weak<dyn Output>) {
        self.outputs.borrow_mut().push(output);
    }
    fn fetch(&self, req: &FetchRequest) -> NodeStream {
        self.input.fetch(req)
    }
}

impl Output for UnionFanOut {
    fn push(&self, change: Change, _pusher: &dyn InputBase) {
        let fan_in = self.fan_in();
        fan_in.fan_out_started_pushing();
        let change_type = change.change_type();
        for o in self.outputs() {
            o.push(change.clone(), self);
        }
        fan_in.fan_out_done_pushing(change_type);
    }

    fn reconcile(&self, _pusher: &dyn InputBase) {
        let fan_in = self.fan_in();
        fan_in.fan_out_started_reconciling();
        for o in self.outputs() {
            o.reconcile(self);
        }
        fan_in.fan_out_done_reconciling();
    }
}

/// Merges the branches of a `UnionFanOut` (`union-fan-in.ts`).
pub struct UnionFanIn {
    inputs: Vec<Rc<dyn Input>>,
    schema: Rc<SourceSchema>,
    fan_out_push_started: Cell<bool>,
    fan_out_reconcile_started: Cell<bool>,
    output: RefCell<Weak<dyn Output>>,
    accumulated: RefCell<Vec<Change>>,
    /// Relationships the branches add (flipped joins' witnesses).
    branch_rels: HashSet<String>,
}

impl UnionFanIn {
    pub fn new(fan_out: &Rc<UnionFanOut>, inputs: Vec<Rc<dyn Input>>) -> Rc<Self> {
        let fan_out_schema = fan_out.get_schema();
        assert!(
            fan_out_schema.sort.is_some(),
            "UnionFanIn requires sorted input"
        );
        let mut relationships = fan_out_schema.relationships.clone();
        let mut from_branches = HashSet::new();
        for input in &inputs {
            let s = input.get_schema();
            assert!(
                fan_out_schema.table_name == s.table_name,
                "Table name mismatch in union fan-in: {} !== {}",
                fan_out_schema.table_name,
                s.table_name
            );
            assert!(
                fan_out_schema.primary_key == s.primary_key,
                "Primary key mismatch in union fan-in"
            );
            assert!(
                fan_out_schema.system == s.system,
                "System mismatch in union fan-in"
            );
            for (name, rel) in &s.relationships {
                if fan_out_schema.relationship(name).is_some() {
                    continue;
                }
                assert!(
                    from_branches.insert(name.clone()),
                    "Relationship {name} exists in multiple upstream inputs to union fan-in"
                );
                relationships.push((name.clone(), rel.clone()));
            }
        }
        let mut schema = (*fan_out_schema).clone();
        schema.relationships = relationships;
        let op = Rc::new(UnionFanIn {
            inputs: inputs.clone(),
            schema: Rc::new(schema),
            fan_out_push_started: Cell::new(false),
            fan_out_reconcile_started: Cell::new(false),
            output: RefCell::new(no_output()),
            accumulated: RefCell::default(),
            branch_rels: from_branches,
        });
        fan_out.set_fan_in(&op);
        for input in &inputs {
            input.set_output(Rc::downgrade(&op) as Weak<dyn Output>);
        }
        op
    }

    fn output(&self) -> Option<Rc<dyn Output>> {
        self.output.borrow().upgrade()
    }

    /// A change from inside the fan-out/fan-in sub-graph, which only a
    /// flipped join's child produces. An add or remove is forwarded when no
    /// other branch has the row.
    fn push_internal_change(&self, change: Change, pusher: &dyn InputBase) {
        if change.change_type() == ChangeType::Child {
            if let Some(out) = self.output() {
                out.push(change, self);
            }
            return;
        }
        assert!(
            matches!(change, Change::Add(_) | Change::Remove(_)),
            "UnionFanIn: expected add or remove change type, got {}",
            change.change_type() as u8
        );
        let mut had_match = false;
        for input in &self.inputs {
            if is_same(input.as_ref(), pusher) {
                had_match = true;
                continue;
            }
            let constraint = Constraint::from_row(&change.node().row, &self.schema.primary_key);
            let req = FetchRequest {
                constraint: Some(constraint),
                ..FetchRequest::default()
            };
            if input.fetch(&req).next().is_some() {
                // Another branch keeps the row; it gained or lost this
                // branch's witnesses.
                if let Some(out) = self.output() {
                    let rels = &self.branch_rels;
                    push_witness_deltas(
                        change.node(),
                        change.change_type(),
                        &|n| rels.contains(n),
                        out.as_ref(),
                        self,
                    );
                }
                return;
            }
        }
        assert!(
            had_match,
            "Pusher was not one of the inputs to union-fan-in!"
        );
        if let Some(out) = self.output() {
            out.push(change, self);
        }
    }

    fn fan_out_started_pushing(&self) {
        assert!(
            !self.fan_out_push_started.get(),
            "UnionFanIn: fanOutStartedPushing called while already pushing"
        );
        self.fan_out_push_started.set(true);
    }

    fn fan_out_done_pushing(&self, fan_out_change_type: ChangeType) {
        assert!(
            self.fan_out_push_started.get(),
            "UnionFanIn: fanOutDonePushing called without fanOutStartedPushing"
        );
        self.fan_out_push_started.set(false);
        let changes = std::mem::take(&mut *self.accumulated.borrow_mut());
        if self.inputs.is_empty() || changes.is_empty() {
            return;
        }
        let Some(out) = self.output() else {
            return;
        };
        let schema = self.schema.clone();
        let rels = &self.branch_rels;
        push_accumulated_changes(
            changes,
            out.as_ref(),
            self,
            fan_out_change_type,
            &merge_relationships,
            &|c| make_add_empty_relationships(&schema, c),
            Some(&|n| rels.contains(n)),
        );
    }

    fn fan_out_started_reconciling(&self) {
        assert!(
            !self.fan_out_reconcile_started.get(),
            "UnionFanIn: fanOutStartedReconciling called while already reconciling"
        );
        self.fan_out_reconcile_started.set(true);
    }

    fn fan_out_done_reconciling(&self) {
        assert!(
            self.fan_out_reconcile_started.get(),
            "UnionFanIn: fanOutDoneReconciling called without fanOutStartedReconciling"
        );
        self.fan_out_reconcile_started.set(false);
        if self.inputs.is_empty() {
            return;
        }
        if let Some(out) = self.output() {
            out.reconcile(self);
        }
    }
}

impl InputBase for UnionFanIn {
    fn get_schema(&self) -> Rc<SourceSchema> {
        self.schema.clone()
    }
    fn destroy(&self) {
        for i in &self.inputs {
            i.destroy();
        }
    }
}

impl Input for UnionFanIn {
    fn set_output(&self, output: Weak<dyn Output>) {
        *self.output.borrow_mut() = output;
    }

    fn fetch(&self, req: &FetchRequest) -> NodeStream {
        let streams: Vec<NodeStream> = self.inputs.iter().map(|i| i.fetch(req)).collect();
        let compare_rows = self.schema.compare_rows.clone();
        let reverse = req.reverse;
        let compare: NodeComparator = Rc::new(move |a: &Node, b: &Node| {
            if reverse {
                compare_rows(&b.row, &a.row)
            } else {
                compare_rows(&a.row, &b.row)
            }
        });
        let names: Vec<String> = self
            .schema
            .relationship_names()
            .into_iter()
            .map(str::to_owned)
            .collect();
        Box::new(merge_fetches(streams, compare).map(move |mut node| {
            merge_empty(&mut node.relationships, &names);
            node
        }))
    }
}

impl Output for UnionFanIn {
    fn push(&self, change: Change, pusher: &dyn InputBase) {
        if !self.fan_out_push_started.get() {
            return self.push_internal_change(change, pusher);
        }
        self.accumulated.borrow_mut().push(change);
    }

    fn reconcile(&self, _pusher: &dyn InputBase) {
        if self.fan_out_reconcile_started.get() {
            return;
        }
        if let Some(out) = self.output() {
            out.reconcile(self);
        }
    }
}

/// `mergeFetches`: merges sorted branch streams, uniting the relationships
/// of a row that several branches yield (the first branch wins a conflict).
pub fn merge_fetches(fetches: Vec<NodeStream>, compare: NodeComparator) -> NodeStream {
    lazy(move || {
        let mut iters = fetches;
        let mut current: Vec<Option<Node>> = iters.iter_mut().map(|i| i.next()).collect();
        let mut last: Option<Node> = None;
        std::iter::from_fn(move || {
            loop {
                let mut min: Option<usize> = None;
                for (i, c) in current.iter().enumerate() {
                    let Some(c) = c else { continue };
                    if min.is_none_or(|m| {
                        compare(c, current[m].as_ref().expect("head")) == Ordering::Less
                    }) {
                        min = Some(i);
                    }
                }
                let i = min?;
                let node = current[i].take().expect("head");
                current[i] = iters[i].next();
                if last
                    .as_ref()
                    .is_some_and(|l| compare(l, &node) == Ordering::Equal)
                {
                    continue;
                }
                // The same row at the head of another branch: union its
                // relationships in rather than dropping it.
                let mut merged = node.clone();
                for j in 0..iters.len() {
                    while let Some(head) = &current[j]
                        && compare(head, &node) == Ordering::Equal
                    {
                        merged.relationships = head.relationships.merging(&merged.relationships);
                        current[j] = iters[j].next();
                    }
                }
                last = Some(node);
                return Some(merged);
            }
        })
    })
}

type MergeFn<'a> = &'a dyn Fn(Change, Change) -> Change;
type FillFn<'a> = &'a dyn Fn(Change) -> Change;
type IsBranchRel<'a> = Option<&'a dyn Fn(&str) -> bool>;

/// `pushAccumulatedChanges` (`push-accumulated.ts`): pushes the single
/// change that the changes collected from an OR's branches amount to.
pub fn push_accumulated_changes(
    accumulated: Vec<Change>,
    output: &dyn Output,
    pusher: &dyn InputBase,
    fan_out_change_type: ChangeType,
    merge: MergeFn,
    add_empty: FillFn,
    is_branch_rel: IsBranchRel,
) {
    if accumulated.is_empty() {
        return;
    }
    // Witness changes of a nested union pass through around this fan-in's
    // change: removals before, additions after.
    let mut witnesses_left = Vec::new();
    let mut witnesses_entered = Vec::new();
    // One change per type, in the order the types first appeared.
    let mut candidates: Vec<(ChangeType, Change)> = Vec::new();
    for change in accumulated {
        if let (Some(is_branch), Change::Child(_, child)) = (is_branch_rel, &change)
            && is_branch(&child.relationship_name)
        {
            if child.change.change_type() == ChangeType::Remove {
                witnesses_left.push(change);
            } else {
                witnesses_entered.push(change);
            }
            continue;
        }
        let t = change.change_type();
        if fan_out_change_type == ChangeType::Child && t != ChangeType::Child {
            assert!(
                !candidates.iter().any(|(ct, _)| *ct == t),
                "Fan-in:child expected at most one {} when fan-out is of type child",
                t as u8
            );
        }
        match candidates.iter().position(|(ct, _)| *ct == t) {
            Some(i) => {
                let prev = candidates.remove(i).1;
                candidates.insert(i, (t, merge(prev, change)));
            }
            None => candidates.push((t, change)),
        }
    }
    assert!(
        !candidates.is_empty(),
        "Fan-in: witness changes without a change for their row"
    );
    for c in witnesses_left {
        output.push(c, pusher);
    }
    push_collapsed(
        candidates,
        output,
        pusher,
        fan_out_change_type,
        merge,
        add_empty,
        is_branch_rel,
    );
    for c in witnesses_entered {
        output.push(c, pusher);
    }
}

fn push_collapsed(
    candidates: Vec<(ChangeType, Change)>,
    output: &dyn Output,
    pusher: &dyn InputBase,
    fan_out_change_type: ChangeType,
    merge: MergeFn,
    add_empty: FillFn,
    is_branch_rel: IsBranchRel,
) {
    let types: Vec<ChangeType> = candidates.iter().map(|(t, _)| *t).collect();
    let get = |t: ChangeType| {
        candidates
            .iter()
            .find(|(ct, _)| *ct == t)
            .map(|(_, c)| c.clone())
    };
    use ChangeType::*;
    match fan_out_change_type {
        Remove => {
            assert!(types == [Remove], "Fan-in:remove expected all removes");
            output.push(add_empty(get(Remove).expect("remove")), pusher);
        }
        Add => {
            assert!(types == [Add], "Fan-in:add expected all adds");
            output.push(add_empty(get(Add).expect("add")), pusher);
        }
        Edit => {
            assert!(
                types.iter().all(|t| matches!(t, Add | Remove | Edit)),
                "Fan-in:edit expected all adds, removes, or edits"
            );
            let add = get(Add);
            let remove = get(Remove);
            let edit = get(Edit);
            // The row stays: branches it left take their witnesses, branches
            // it entered bring theirs, as child changes around the edit.
            if let Some(is_branch) = is_branch_rel
                && (edit.is_some() || (add.is_some() && remove.is_some()))
            {
                if let Some(r) = &remove {
                    push_witness_deltas(r.node(), Remove, is_branch, output, pusher);
                }
                let strip = |n: &Node| {
                    let mut r = Relationships::default();
                    for (k, v) in n.relationships.iter() {
                        if !is_branch(k) {
                            r.set(k, Some(v.clone()));
                        }
                    }
                    Node {
                        row: n.row.clone(),
                        relationships: r,
                    }
                };
                let change = edit.unwrap_or_else(|| Change::Edit {
                    node: strip(add.as_ref().expect("add").node()),
                    old_node: strip(remove.as_ref().expect("remove").node()),
                });
                output.push(add_empty(change), pusher);
                if let Some(a) = &add {
                    push_witness_deltas(a.node(), Add, is_branch, output, pusher);
                }
                return;
            }
            // An edit supersedes adds and removes.
            if let Some(mut e) = edit {
                if let Some(a) = add {
                    e = merge(e, a);
                }
                if let Some(r) = remove {
                    e = merge(e, r);
                }
                output.push(add_empty(e), pusher);
                return;
            }
            // A branch turned the edit into a remove and another into an add.
            match (add, remove) {
                (Some(a), Some(r)) => output.push(
                    add_empty(Change::Edit {
                        node: a.node().clone(),
                        old_node: r.node().clone(),
                    }),
                    pusher,
                ),
                (a, r) => output.push(add_empty(a.or(r).expect("add or remove")), pusher),
            }
        }
        Child => {
            assert!(
                types.iter().all(|t| matches!(t, Add | Remove | Child)),
                "Fan-in:child expected all adds, removes, or children"
            );
            assert!(
                types.len() <= 2,
                "Fan-in:child expected at most 2 types on a child change from fan-out"
            );
            // A branch that kept the child change takes precedence.
            if let Some(child) = get(Child) {
                let left = get(Remove);
                let entered = get(Add);
                if let (Some(is_branch), Some(l)) = (is_branch_rel, &left) {
                    push_witness_deltas(l.node(), Remove, is_branch, output, pusher);
                }
                output.push(child, pusher);
                if let (Some(is_branch), Some(e)) = (is_branch_rel, &entered) {
                    push_witness_deltas(e.node(), Add, is_branch, output, pusher);
                }
                return;
            }
            let add = get(Add);
            let remove = get(Remove);
            assert!(
                add.is_none() || remove.is_none(),
                "Fan-in:child expected either add or remove, not both"
            );
            output.push(add_empty(add.or(remove).expect("add or remove")), pusher);
        }
    }
}

/// The row stays, but gained (`add`) or lost (`remove`) every witness in
/// its branch relationships: one child change per witness.
pub fn push_witness_deltas(
    node: &Node,
    change_type: ChangeType,
    is_branch_rel: &dyn Fn(&str) -> bool,
    output: &dyn Output,
    pusher: &dyn InputBase,
) {
    for (name, children) in node.relationships.iter() {
        if !is_branch_rel(name) {
            continue;
        }
        for child in children() {
            let change = if change_type == ChangeType::Add {
                Change::Add(child)
            } else {
                Change::Remove(child)
            };
            output.push(Change::child(node.clone(), name, change), pusher);
        }
    }
}

/// `mergeRelationships`: puts `right`'s relationships into `left` where
/// `left` lacks them.
pub fn merge_relationships(left: Change, right: Change) -> Change {
    let merged = |l: &Node, r: &Node| Node {
        row: l.row.clone(),
        relationships: r.relationships.merging(&l.relationships),
    };
    match (left, right) {
        (Change::Add(l), Change::Add(r)) => Change::Add(merged(&l, &r)),
        (Change::Remove(l), Change::Remove(r)) => Change::Remove(merged(&l, &r)),
        (
            Change::Edit {
                node: l,
                old_node: lo,
            },
            Change::Edit {
                node: r,
                old_node: ro,
            },
        ) => Change::Edit {
            node: merged(&l, &r),
            old_node: merged(&lo, &ro),
        },
        (Change::Child(l, child), Change::Child(r, _)) => Change::Child(merged(&l, &r), child),
        (
            Change::Edit {
                node: l,
                old_node: lo,
            },
            Change::Add(r),
        ) => Change::Edit {
            node: merged(&l, &r),
            old_node: lo,
        },
        (
            Change::Edit {
                node: l,
                old_node: lo,
            },
            Change::Remove(r),
        ) => Change::Edit {
            node: l,
            old_node: merged(&lo, &r),
        },
        (left, right) if left.change_type() != ChangeType::Edit => panic!(
            "mergeRelationships: when types differ, left.type must be edit, got left.type={}, right.type={}",
            left.change_type() as u8,
            right.change_type() as u8
        ),
        _ => panic!("mergeRelationships: unexpected right change"),
    }
}

/// `makeAddEmptyRelationships(schema)(change)`.
pub fn make_add_empty_relationships(schema: &SourceSchema, change: Change) -> Change {
    if schema.relationships.is_empty() {
        return change;
    }
    let names: Vec<String> = schema
        .relationship_names()
        .into_iter()
        .map(str::to_owned)
        .collect();
    let fill = |mut n: Node| {
        merge_empty(&mut n.relationships, &names);
        n
    };
    match change {
        Change::Add(n) => Change::Add(fill(n)),
        Change::Remove(n) => Change::Remove(fill(n)),
        Change::Edit { node, old_node } => Change::Edit {
            node: fill(node),
            old_node: fill(old_node),
        },
        // Only the path to the change has relationships.
        c @ Change::Child(..) => c,
    }
}

/// Adds an empty stream for each of `names` that `relationships` lacks.
pub fn merge_empty(relationships: &mut Relationships, names: &[String]) {
    for name in names {
        if relationships.get(name).is_none() {
            relationships.set(name, Some(Rc::new(empty)));
        }
    }
}
