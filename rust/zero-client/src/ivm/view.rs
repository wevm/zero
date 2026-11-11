//! Materialized views: the entries a view holds, how changes apply to them
//! (`packages/zql/src/ivm/view-apply-change.ts`), and `ArrayView`
//! (`array-view.ts`).

use super::*;
use crate::ast::{Aggregate as AggregateSpec, AggregateFunction};
use crate::format::Format;
use crate::json::{Object, Value};
use crate::keys::aggregate_columns as cols;
use crate::protocol::ErroredQuery;
use crate::strings;
use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::rc::{Rc, Weak};

/// A row in a materialized view, with the relationships the query asked
/// for (`Entry` in `view.ts`).
#[derive(Clone, Debug)]
pub struct ViewEntry {
    pub row: Row,
    /// By name, in UTF-16 code unit order.
    pub relationships: Vec<(String, ViewValue)>,
    /// How many edges of the relationship reach this row.
    ref_count: usize,
}

impl PartialEq for ViewEntry {
    /// Equal rows with equal relationships. How many edges reach the row
    /// doesn't matter.
    fn eq(&self, other: &Self) -> bool {
        self.row == other.row && self.relationships == other.relationships
    }
}

impl ViewEntry {
    fn new(row: Row) -> Self {
        ViewEntry {
            row,
            relationships: Vec::new(),
            ref_count: 1,
        }
    }

    pub fn relationship(&self, name: &str) -> Option<&ViewValue> {
        self.position(name).ok().map(|i| &self.relationships[i].1)
    }

    fn position(&self, name: &str) -> Result<usize, usize> {
        self.relationships
            .binary_search_by(|(k, _)| strings::utf16(k, name))
    }

    fn relationship_mut(&mut self, name: &str) -> Option<&mut ViewValue> {
        self.position(name)
            .ok()
            .map(|i| &mut self.relationships[i].1)
    }

    fn set(&mut self, name: &str, value: ViewValue) {
        match self.position(name) {
            Ok(i) => self.relationships[i].1 = value,
            Err(i) => self.relationships.insert(i, (name.to_owned(), value)),
        }
    }

    /// The row with its relationships, as JS views present it: the
    /// relationships after the row's columns, by name.
    pub fn to_json(&self) -> Value {
        let mut o: Object = (*self.row).clone();
        for (name, value) in &self.relationships {
            match value.to_json() {
                Some(v) => {
                    o.insert(name.clone(), v);
                }
                None => {
                    o.remove(name);
                }
            }
        }
        Value::Object(o)
    }
}

/// A relationship in a view: several rows, at most one, or an aggregate's
/// value.
#[derive(Clone, Debug, PartialEq)]
pub enum ViewValue {
    /// Shared with earlier snapshots until changed.
    Many(Rc<Vec<ViewEntry>>),
    One(Option<Box<ViewEntry>>),
    /// The value of an aggregate (`count()`, `sum(...)`, …): a number, or
    /// `null` for an empty `sum`, `avg`, `min` or `max`.
    Value(Value),
}

impl ViewValue {
    /// The value of an aggregate over no rows, as in SQL: `count` is 0, the
    /// rest null (`emptyViewValue`).
    pub fn empty(format: &Format) -> ViewValue {
        if let Some(a) = &format.aggregate {
            return ViewValue::Value(empty_aggregate_value(a));
        }
        if format.singular {
            ViewValue::One(None)
        } else {
            ViewValue::Many(Rc::default())
        }
    }

    /// The value as JS views present it; `None` for a missing singular row.
    pub fn to_json(&self) -> Option<Value> {
        match self {
            ViewValue::Many(entries) => Some(Value::Array(
                entries.iter().map(ViewEntry::to_json).collect(),
            )),
            ViewValue::One(entry) => entry.as_ref().map(|e| e.to_json()),
            ViewValue::Value(v) => Some(v.clone()),
        }
    }

    /// The rows; none for an aggregate's value.
    pub fn entries(&self) -> Vec<&ViewEntry> {
        match self {
            ViewValue::Many(e) => e.iter().collect(),
            ViewValue::One(e) => e.iter().map(|e| e.as_ref()).collect(),
            ViewValue::Value(_) => Vec::new(),
        }
    }
}

/// `emptyAggregateValue`.
pub fn empty_aggregate_value(aggregate: &AggregateSpec) -> Value {
    if aggregate.fn_ == AggregateFunction::Count {
        Value::Number(0.0)
    } else {
        Value::Null
    }
}

fn child_format<'a>(format: &'a Format, name: &str) -> Option<&'a Format> {
    format
        .relationships
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, f)| f)
}

fn child_schema<'a>(schema: &'a SourceSchema, name: &str) -> &'a SourceSchema {
    schema
        .relationship(name)
        .unwrap_or_else(|| panic!("missing relationship schema {name}"))
}

/// Reports what JS asserts here (the view and its input disagree) and
/// leaves the tree as it was: the push or hydration then fails.
fn fail(message: &str) -> bool {
    report(message);
    false
}

fn list_mut<'a>(parent: &'a mut ViewEntry, relationship: &str) -> &'a mut Vec<ViewEntry> {
    match parent.relationship_mut(relationship) {
        Some(ViewValue::Many(list)) => Rc::make_mut(list),
        _ => panic!("expected relationship array"),
    }
}

fn singular_mut<'a>(parent: &'a mut ViewEntry, relationship: &str) -> Option<&'a mut ViewEntry> {
    match parent.relationship_mut(relationship) {
        Some(ViewValue::One(Some(entry))) => Some(entry),
        _ => None,
    }
}

/// `{...existing, ...row}`.
fn apply_edit(entry: &mut ViewEntry, row: &Row) {
    let existing = Rc::make_mut(&mut entry.row);
    for (k, v) in row.iter() {
        existing.insert(k.clone(), v.clone());
    }
}

/// The index of `target`, or its insertion point. Probes the end first:
/// hydration appends in order.
fn search(view: &[ViewEntry], target: &Row, comparator: &Comparator) -> Result<usize, usize> {
    let Some(last) = view.last() else {
        return Err(0);
    };
    match comparator(&last.row, target) {
        Ordering::Less => return Err(view.len()),
        Ordering::Equal => return Ok(view.len() - 1),
        Ordering::Greater => {}
    }
    view[..view.len() - 1].binary_search_by(|e| comparator(&e.row, target))
}

/// A new entry for `node`, with its relationships filled in.
fn new_entry(node: &Node, schema: &SourceSchema, format: &Format) -> ViewEntry {
    let mut entry = ViewEntry::new(node.row.clone());
    initialize_relationships(&mut entry, node, schema, format);
    entry
}

/// Fills in a new entry's relationships from its node.
fn initialize_relationships(
    entry: &mut ViewEntry,
    node: &Node,
    schema: &SourceSchema,
    format: &Format,
) {
    for (name, stream) in node.relationships.iter() {
        let child_schema = child_schema(schema, name);
        let Some(child_format) = child_format(format, name) else {
            continue;
        };
        if child_schema.is_hidden || child_format.singular {
            // An aggregate starts at its empty value: a parent whose
            // correlation value is null gets no row from the Join, but SQL
            // still gives it 0 or null.
            entry.set(name, ViewValue::empty(child_format));
            for child in stream() {
                apply(entry, &Change::Add(child), child_schema, name, child_format);
            }
        } else {
            let mut list: Vec<ViewEntry> = Vec::new();
            for child in stream() {
                match search(&list, &child.row, &child_schema.compare_rows) {
                    Ok(pos) => list[pos].ref_count += 1,
                    Err(pos) => list.insert(pos, new_entry(&child, child_schema, child_format)),
                }
            }
            entry.set(name, ViewValue::Many(Rc::new(list)));
        }
    }
}

/// Applies `change` to `parent`'s `relationship`. Returns whether the tree
/// changed (JS returns the same entry when it did not).
pub fn apply(
    parent: &mut ViewEntry,
    change: &Change,
    schema: &SourceSchema,
    relationship: &str,
    format: &Format,
) -> bool {
    if schema.is_hidden {
        return match change {
            Change::Add(node) | Change::Remove(node) => {
                let is_add = change.change_type() == ChangeType::Add;
                let mut changed = false;
                for (name, stream) in node.relationships.iter() {
                    let child_schema = child_schema(schema, name);
                    for child in stream() {
                        let c = if is_add {
                            Change::Add(child)
                        } else {
                            Change::Remove(child)
                        };
                        changed = apply(parent, &c, child_schema, name, format) || changed;
                    }
                }
                changed
            }
            // A hidden row changed; a change to its relationships would have
            // been split into a remove and an add.
            Change::Edit { .. } => false,
            Change::Child(_, child) => apply(
                parent,
                &child.change,
                child_schema(schema, &child.relationship_name),
                relationship,
                format,
            ),
        };
    }

    // An aggregate relationship shows the bare value rather than its row:
    // an add or edit sets it, a remove resets it to the empty value.
    if let Some(aggregate) = &format.aggregate {
        let value = match change {
            Change::Add(node) | Change::Edit { node, .. } => {
                node.row.get(cols::VALUE).cloned().unwrap_or(Value::Null)
            }
            Change::Remove(_) => empty_aggregate_value(aggregate),
            Change::Child(..) => return false,
        };
        parent.set(relationship, ViewValue::Value(value));
        return true;
    }

    let compare = &schema.compare_rows;
    match change {
        Change::Add(node) => {
            if format.singular {
                if let Some(old) = singular_mut(parent, relationship) {
                    // Server data can do this, with two rows for a `one`
                    // relationship.
                    if compare(&old.row, &node.row) != Ordering::Equal {
                        return fail(&format!(
                            "Singular relationship '{relationship}' should not have multiple rows. You may need to declare this relationship with the `many` helper instead of the `one` helper in your schema."
                        ));
                    }
                    old.ref_count += 1;
                } else {
                    let entry = new_entry(node, schema, format);
                    parent.set(relationship, ViewValue::One(Some(Box::new(entry))));
                }
            } else {
                let list = list_mut(parent, relationship);
                match search(list, &node.row, compare) {
                    Ok(pos) => list[pos].ref_count += 1,
                    Err(pos) => list.insert(pos, new_entry(node, schema, format)),
                }
            }
            true
        }

        Change::Remove(node) => {
            if format.singular {
                let Some(old) = singular_mut(parent, relationship) else {
                    return fail("node does not exist");
                };
                if old.ref_count == 1 {
                    parent.set(relationship, ViewValue::One(None));
                } else {
                    old.ref_count -= 1;
                }
            } else {
                let list = list_mut(parent, relationship);
                let Ok(pos) = search(list, &node.row, compare) else {
                    return fail("node does not exist");
                };
                if list[pos].ref_count == 1 {
                    list.remove(pos);
                } else {
                    list[pos].ref_count -= 1;
                }
            }
            true
        }

        Change::Child(node, child) => {
            let name = child.relationship_name.as_str();
            let child_schema = child_schema(schema, name);
            let Some(child_format) = child_format(format, name) else {
                return false; // not in the view
            };
            if format.singular {
                let Some(existing) = singular_mut(parent, relationship) else {
                    return fail("node does not exist");
                };
                return apply(existing, &child.change, child_schema, name, child_format);
            }
            let list = list_mut(parent, relationship);
            let Ok(pos) = search(list, &node.row, compare) else {
                return fail("node does not exist");
            };
            apply(
                &mut list[pos],
                &child.change,
                child_schema,
                name,
                child_format,
            )
        }

        Change::Edit { node, old_node } => {
            if format.singular {
                let Some(existing) = singular_mut(parent, relationship) else {
                    return fail("node does not exist");
                };
                apply_edit(existing, &node.row);
                return true;
            }
            let list = list_mut(parent, relationship);
            if compare(&old_node.row, &node.row) == Ordering::Equal {
                let Ok(pos) = search(list, &old_node.row, compare) else {
                    return fail("node does not exist");
                };
                apply_edit(&mut list[pos], &node.row);
                return true;
            }
            // The sort key changed, so the row may move.
            let Ok(old_pos) = search(list, &old_node.row, compare) else {
                return fail("old node does not exist");
            };
            let raw = search(list, &node.row, compare);
            let (found, pos) = match raw {
                Ok(p) => (true, p),
                Err(p) => (false, p),
            };
            // With one reference, a row landing where it was is only edited.
            if list[old_pos].ref_count == 1 && (pos == old_pos || pos == old_pos + 1) {
                apply_edit(&mut list[old_pos], &node.row);
                return true;
            }
            // Move the row. With several references an edit arrives for
            // each: a copy stays behind until the last one moves.
            let old_entry = list[old_pos].clone();
            let new_ref_count = old_entry.ref_count - 1;
            let mut adjusted = pos;
            if new_ref_count == 0 {
                list.remove(old_pos);
                if old_pos < pos {
                    adjusted = pos - 1;
                }
            } else {
                list[old_pos].ref_count = new_ref_count;
            }
            if found {
                let rc = list[adjusted].ref_count;
                apply_edit(&mut list[adjusted], &node.row);
                list[adjusted].ref_count = rc + 1;
            } else {
                let mut moved = old_entry;
                apply_edit(&mut moved, &node.row);
                moved.ref_count = 1;
                list.insert(adjusted, moved);
            }
            true
        }
    }
}

/// Applies an add for each node.
fn add_all(
    parent: &mut ViewEntry,
    nodes: impl Iterator<Item = Node>,
    schema: &SourceSchema,
    relationship: &str,
    format: &Format,
) {
    if schema.is_hidden || format.singular {
        for node in nodes {
            apply(parent, &Change::Add(node), schema, relationship, format);
        }
        return;
    }
    let list = list_mut(parent, relationship);
    for node in nodes {
        match search(list, &node.row, &schema.compare_rows) {
            Ok(pos) => list[pos].ref_count += 1,
            Err(pos) => list.insert(pos, new_entry(&node, schema, format)),
        }
    }
}

/// The result type of a view (`ResultType` in `typed-view.ts`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ResultType {
    Unknown,
    Cached,
    Complete,
    Error,
}

impl ResultType {
    pub fn as_str(self) -> &'static str {
        match self {
            ResultType::Unknown => "unknown",
            ResultType::Cached => "cached",
            ResultType::Complete => "complete",
            ResultType::Error => "error",
        }
    }
}

/// Called with a view's data and result type.
pub type Listener = Rc<dyn Fn(&ViewValue, ResultType)>;

struct ViewState {
    root: ViewEntry,
    dirty: bool,
    committed_root: Option<ViewEntry>,
    held: bool,
    held_result_type: ResultType,
    held_error: Option<ErroredQuery>,
    result_type: ResultType,
    error: Option<ErroredQuery>,
}

/// A materialized view of a pipeline's output (`array-view.ts`).
///
/// Listeners are notified on [`flush`](Self::flush), which the engine
/// calls when a transaction commits.
pub struct ArrayView {
    input: Rc<dyn Input>,
    format: Format,
    schema: Rc<SourceSchema>,
    state: RefCell<ViewState>,
    listeners: Rc<RefCell<Vec<(u64, Listener)>>>,
    next_listener_id: Cell<u64>,
    on_destroy: RefCell<Option<Box<dyn Fn()>>>,
}

impl ArrayView {
    /// Hydrates from `input`, starting as failed when `error` is set.
    /// `complete` marks the result complete from the start.
    pub fn new(
        input: Rc<dyn Input>,
        format: Format,
        error: Option<ErroredQuery>,
        complete: bool,
    ) -> Rc<Self> {
        let mut root = ViewEntry::new(Rc::new(Object::new()));
        root.set("", ViewValue::empty(&format));
        let result_type = if complete {
            ResultType::Complete
        } else if error.is_some() {
            ResultType::Error
        } else {
            ResultType::Unknown
        };
        let error = if complete { None } else { error };
        let view = Rc::new(ArrayView {
            schema: input.get_schema(),
            input: input.clone(),
            format,
            state: RefCell::new(ViewState {
                root,
                dirty: false,
                committed_root: None,
                held: false,
                held_result_type: ResultType::Unknown,
                held_error: None,
                result_type,
                error,
            }),
            listeners: Rc::default(),
            next_listener_id: Cell::new(0),
            on_destroy: RefCell::new(None),
        });
        input.set_output(Rc::downgrade(&view) as Weak<dyn Output>);
        view.hydrate();
        view
    }

    pub fn format(&self) -> &Format {
        &self.format
    }

    /// The schema of the rows.
    pub fn schema(&self) -> &SourceSchema {
        &self.schema
    }

    /// The current rows.
    pub fn data(&self) -> ViewValue {
        let state = self.state.borrow();
        let root = state.committed_root.as_ref().unwrap_or(&state.root);
        root.relationship("").expect("view root").clone()
    }

    pub fn result_type(&self) -> ResultType {
        self.state.borrow().result_type
    }

    /// Why the query failed, when the result type is `Error`.
    pub fn error(&self) -> Option<ErroredQuery> {
        self.state.borrow().error.clone()
    }

    pub fn set_on_destroy(&self, f: impl Fn() + 'static) {
        *self.on_destroy.borrow_mut() = Some(Box::new(f));
    }

    /// Adds a listener, calls it with the current data, and returns a
    /// function that removes it.
    pub fn add_listener(
        &self,
        listener: impl Fn(&ViewValue, ResultType) + 'static,
    ) -> Box<dyn Fn()> {
        let id = self.next_listener_id.get();
        self.next_listener_id.set(id + 1);
        let listener: Listener = Rc::new(listener);
        self.listeners.borrow_mut().push((id, listener.clone()));
        let (data, ty) = self.listener_args();
        listener(&data, ty);
        let listeners = Rc::downgrade(&self.listeners);
        Box::new(move || {
            if let Some(l) = listeners.upgrade() {
                l.borrow_mut().retain(|(i, _)| *i != id);
            }
        })
    }

    fn listener_args(&self) -> (ViewValue, ResultType) {
        let ty = {
            let s = self.state.borrow();
            if s.committed_root.is_some() {
                s.held_result_type
            } else {
                s.result_type
            }
        };
        (self.data(), ty)
    }

    /// Calls each listener, reading the set again after every call, as JS
    /// iterates a `Set` that changes while it is iterated: one removed
    /// before its turn is skipped, one added is called in the same pass.
    fn fire_listeners(&self) {
        let (data, ty) = self.listener_args();
        let mut last: Option<u64> = None;
        loop {
            let next = self
                .listeners
                .borrow()
                .iter()
                .find(|(id, _)| last.is_none_or(|l| *id > l))
                .cloned();
            let Some((id, listener)) = next else {
                break;
            };
            last = Some(id);
            listener(&data, ty);
        }
    }

    pub fn destroy(&self) {
        let f = self.on_destroy.borrow_mut().take();
        if let Some(f) = f {
            f();
        }
    }

    fn hydrate(&self) {
        let nodes = self.input.fetch(&FetchRequest::default());
        {
            let mut s = self.state.borrow_mut();
            s.dirty = true;
            add_all(&mut s.root, nodes, &self.schema, "", &self.format);
        }
        self.flush();
    }

    /// Keeps exposing the current snapshot until `release_data` or the next
    /// `flush`.
    pub fn hold_data(&self) {
        let mut s = self.state.borrow_mut();
        s.committed_root = Some(s.root.clone());
        s.held = true;
        s.held_result_type = s.result_type;
        s.held_error = s.error.clone();
    }

    /// The result type and error listeners are told about now.
    pub fn listener_result_type(&self) -> (ResultType, Option<ErroredQuery>) {
        let s = self.state.borrow();
        if s.committed_root.is_some() {
            (s.held_result_type, s.held_error.clone())
        } else {
            (s.result_type, s.error.clone())
        }
    }

    pub fn release_data(&self) {
        self.state.borrow_mut().committed_root = None;
    }

    /// Notifies listeners if the view changed since the last flush.
    pub fn flush(&self) {
        {
            let mut s = self.state.borrow_mut();
            s.committed_root = None;
            s.held = false;
            if !s.dirty {
                return;
            }
            s.dirty = false;
        }
        self.fire_listeners();
    }

    pub fn set_result_type(&self, ty: ResultType, error: Option<ErroredQuery>) {
        let fire = {
            let mut s = self.state.borrow_mut();
            s.result_type = ty;
            s.error = error;
            if s.held {
                s.dirty = true;
                false
            } else {
                !s.dirty
            }
        };
        if fire {
            self.fire_listeners();
        }
    }

    /// The server-confirmed result of an earlier sync is in the store.
    pub fn mark_cached(&self) {
        if self.result_type() == ResultType::Unknown {
            self.set_result_type(ResultType::Cached, None);
        }
    }

    pub fn unmark_cached(&self) {
        if self.result_type() == ResultType::Cached {
            self.set_result_type(ResultType::Unknown, None);
        }
    }
}

impl Output for ArrayView {
    fn push(&self, change: Change, _pusher: &dyn InputBase) {
        let mut s = self.state.borrow_mut();
        if apply(&mut s.root, &change, &self.schema, "", &self.format) {
            s.dirty = true;
        }
    }
}
