//! Operators, their inputs and outputs, and sources
//! (`packages/zql/src/ivm/operator.ts`, `schema.ts`, `source.ts`, and
//! `filter-operators.ts`).

use super::{Change, Comparator, Constraint, Node, NodeStream, PipelineError, Row, SourceChange};
use crate::ast::{Condition, OrderPart, System};
use crate::map::OrderedMap;
use crate::schema::{Column, Table};
use std::collections::HashSet;
use std::rc::{Rc, Weak};

/// The schema of an operator's output (`SourceSchema` in `schema.ts`).
#[derive(Clone)]
pub struct SourceSchema {
    pub table_name: String,
    pub columns: OrderedMap<Column>,
    pub primary_key: Vec<String>,
    /// Relationship schemas, in the order they were added.
    pub relationships: Vec<(String, Rc<SourceSchema>)>,
    pub is_hidden: bool,
    pub system: System,
    pub compare_rows: Comparator,
    /// `None` for an unordered input.
    pub sort: Option<Vec<OrderPart>>,
}

impl SourceSchema {
    pub fn relationship(&self, name: &str) -> Option<&Rc<SourceSchema>> {
        self.relationships
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, s)| s)
    }

    pub fn relationship_names(&self) -> Vec<&str> {
        self.relationships.iter().map(|(n, _)| n.as_str()).collect()
    }

    /// This schema with `schema` added (or replaced) as relationship `name`.
    pub fn adding(&self, name: &str, schema: Rc<SourceSchema>) -> SourceSchema {
        let mut copy = self.clone();
        match copy.relationships.iter_mut().find(|(n, _)| n == name) {
            Some(entry) => entry.1 = schema,
            None => copy.relationships.push((name.to_owned(), schema)),
        }
        copy
    }
}

impl std::fmt::Debug for SourceSchema {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceSchema")
            .field("table_name", &self.table_name)
            .field("primary_key", &self.primary_key)
            .field("relationships", &self.relationship_names())
            .field("is_hidden", &self.is_hidden)
            .field("system", &self.system)
            .field("sort", &self.sort)
            .finish()
    }
}

/// Whether a fetch starts at its start row or after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Basis {
    At,
    After,
}

/// Where a fetch starts (`Start` in `operator.ts`).
#[derive(Clone, Debug)]
pub struct Start {
    pub row: Row,
    pub basis: Basis,
}

/// `FetchRequest` in `operator.ts`.
#[derive(Clone, Debug, Default)]
pub struct FetchRequest {
    pub constraint: Option<Constraint>,
    /// Multi-row IN clauses, all ANDed together and with `constraint`.
    pub multi_constraints: Option<Vec<Vec<Constraint>>>,
    /// If supplied, `start.row` must have previously been output by fetch or
    /// push.
    pub start: Option<Start>,
    /// Whether to fetch in reverse order of the schema's sort.
    pub reverse: bool,
    /// A predicate without subqueries for the source to apply. Only
    /// `FilterStart` introduces one; pass-through operators keep it.
    pub filter: Option<Condition>,
}

/// `InputBase` in `operator.ts`.
pub trait InputBase {
    fn get_schema(&self) -> Rc<SourceSchema>;
    fn destroy(&self);
}

/// An operator's input: something that can be fetched from and pushes to an
/// output.
pub trait Input: InputBase {
    fn set_output(&self, output: Weak<dyn Output>);
    fn fetch(&self, req: &FetchRequest) -> NodeStream;
}

/// An operator's output (`Output` in `operator.ts`).
///
/// Pushing is two-phase. `push` propagates a change; once every change of a
/// source push has propagated, `reconcile` lets bounded operators (`Take`)
/// refill and push the rows that refill them.
pub trait Output {
    fn push(&self, change: Change, pusher: &dyn InputBase);
    /// Phase two. Sinks can ignore it; operators must forward it.
    fn reconcile(&self, _pusher: &dyn InputBase) {}
    /// Whether the output has a phase two: false for a sink whose JS
    /// counterpart has no `reconcile`, such as a view, which pushes timed
    /// for the inspector skip (`MeasurePushOperator`).
    fn has_reconcile(&self) -> bool {
        true
    }
}

/// An operator in a pipeline: an input to the next and an output of the
/// previous.
pub trait Operator: Input + Output {}

impl<T: Input + Output> Operator for T {}

/// Whether `a` and `b` are the same object, as JS's `===` on operators.
pub fn is_same(a: &dyn InputBase, b: &dyn InputBase) -> bool {
    std::ptr::addr_eq(a, b)
}

/// A weak reference that upgrades to nothing, for an input whose output is
/// not set yet.
pub fn no_output() -> Weak<dyn Output> {
    Weak::<ClosureOutput>::new()
}

/// A weak filter output that upgrades to nothing.
pub fn no_filter_output() -> Weak<dyn FilterOutput> {
    Weak::<NoFilterOutput>::new()
}

/// Never constructed: the type behind [`no_filter_output`].
pub struct NoFilterOutput;

impl Output for NoFilterOutput {
    fn push(&self, _: Change, _: &dyn InputBase) {}
}

impl FilterOutput for NoFilterOutput {
    fn begin_filter(&self) {}
    fn filter(&self, _: &Node) -> bool {
        false
    }
    fn end_filter(&self) {}
}

/// Routes an input's pushes to closures, as JS operators do with object
/// literals. The operator owns it: inputs hold their outputs weakly.
pub struct ClosureOutput {
    on_push: Box<dyn Fn(Change)>,
    on_reconcile: Box<dyn Fn()>,
}

impl ClosureOutput {
    pub fn new(push: impl Fn(Change) + 'static, reconcile: impl Fn() + 'static) -> Rc<Self> {
        Rc::new(ClosureOutput {
            on_push: Box::new(push),
            on_reconcile: Box::new(reconcile),
        })
    }
}

impl Output for ClosureOutput {
    fn push(&self, change: Change, _pusher: &dyn InputBase) {
        (self.on_push)(change)
    }

    fn reconcile(&self, _pusher: &dyn InputBase) {
        (self.on_reconcile)()
    }
}

/// The `where` sub-graph's input (`FilterInput` in `filter-operators.ts`).
pub trait FilterInput: InputBase {
    fn set_filter_output(&self, output: Weak<dyn FilterOutput>);
}

/// The `where` sub-graph's output. Filter operators have no `fetch`; a fetch
/// through the sub-graph asks `filter` whether each node passes.
pub trait FilterOutput: Output {
    fn begin_filter(&self);
    fn filter(&self, node: &Node) -> bool;
    fn end_filter(&self);
}

pub trait FilterOperator: FilterInput + FilterOutput {}

impl<T: FilterInput + FilterOutput> FilterOperator for T {}

/// A source of rows for pipelines (`Source` in `source.ts`).
pub trait Source {
    fn table_schema(&self) -> &Table;
    /// Connects a pipeline. `sort` is `None` for an unordered connection.
    fn connect(
        &self,
        sort: Option<Vec<OrderPart>>,
        filters: Option<&Condition>,
        split_edit_keys: Option<&HashSet<String>>,
    ) -> Result<Rc<dyn SourceInput>, PipelineError>;
    fn push(&self, change: SourceChange);
}

/// A source's connection to one pipeline.
pub trait SourceInput: Input {
    /// Whether the source applies all of the connection's filters, so no
    /// `Filter` operators are needed above it.
    fn fully_applied_filters(&self) -> bool;
}
