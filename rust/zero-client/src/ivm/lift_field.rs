//! `LiftField` (`packages/zql/src/ivm/lift-field.ts`).

use super::*;
use crate::ast::{Aggregate as AggregateSpec, AggregateFunction};
use crate::json::Value;
use crate::schema::{Column, ValueType};
use std::cell::RefCell;
use std::rc::{Rc, Weak};

/// The column `LiftField` writes the destination's field into. A space
/// can't start an identifier, so it never collides with a real column.
pub const LIFTED_FIELD_COLUMN: &str = " lifted";

struct Lift {
    source_relationship: String,
    field: String,
    json_aggregate: Option<AggregateSpec>,
}

impl Lift {
    /// The value to lift from a destination row.
    fn read(&self, row: &Row) -> Value {
        match &self.json_aggregate {
            Some(a) => aggregate_value(a, row),
            None => row.get(&self.field).cloned().unwrap_or(Value::Null),
        }
    }

    /// `node`'s row with the destination's field lifted onto it.
    fn lift(&self, node: &Node) -> Node {
        let mut value = Value::Null;
        if let Some(rel) = node.relationships.get(&self.source_relationship) {
            for child in rel() {
                value = self.read(&child.row);
            }
        }
        Node::new(with_column(&node.row, value))
    }

    /// The lifted value before and after a change to the destination;
    /// `None` when the destination row itself didn't change.
    fn values(&self, change: &Change) -> Option<(Value, Value)> {
        match change {
            Change::Add(n) => Some((Value::Null, self.read(&n.row))),
            Change::Remove(n) => Some((self.read(&n.row), Value::Null)),
            Change::Edit { node, old_node } => {
                Some((self.read(&old_node.row), self.read(&node.row)))
            }
            Change::Child(..) => None,
        }
    }
}

fn with_column(row: &Row, value: Value) -> Row {
    let mut row = (**row).clone();
    row.insert(LIFTED_FIELD_COLUMN, value);
    Rc::new(row)
}

/// Lifts a field of a singular child relationship onto each row.
///
/// For a junction aggregate the value (`label.points`) lives one hop past
/// the junction row (`issueLabel`). LiftField copies it onto the junction
/// row, so the `Aggregate` after it reduces a flat field. A change to the
/// destination arrives as a child change and becomes an edit of the lifted
/// column; one from below the destination leaves the value as it was. For a
/// JSON path (`json_aggregate`) it lifts the leaf, as [`aggregate_value`]
/// reads it.
///
/// The rows it emits carry no relationships.
pub struct LiftField {
    input: Rc<dyn Input>,
    lift: Rc<Lift>,
    schema: Rc<SourceSchema>,
    output: RefCell<Weak<dyn Output>>,
}

impl LiftField {
    pub fn new(
        input: Rc<dyn Input>,
        source_relationship: impl Into<String>,
        field: impl Into<String>,
        json_aggregate: Option<AggregateSpec>,
    ) -> Rc<Self> {
        let source_relationship = source_relationship.into();
        let field = field.into();
        let input_schema = input.get_schema();
        let mut columns = input_schema.columns.clone();
        // Nullable: a junction row may have no destination for a while.
        let column = match &json_aggregate {
            Some(a) => Column {
                type_: if matches!(a.fn_, AggregateFunction::Min | AggregateFunction::Max) {
                    ValueType::Json
                } else {
                    ValueType::Number
                },
                optional: true,
                server_name: None,
            },
            None => match input_schema
                .relationship(&source_relationship)
                .and_then(|r| r.columns.get(&field))
            {
                Some(c) => Column {
                    optional: true,
                    ..c.clone()
                },
                None => Column {
                    type_: ValueType::Number,
                    optional: true,
                    server_name: None,
                },
            },
        };
        columns.insert(LIFTED_FIELD_COLUMN, column);
        let schema = Rc::new(SourceSchema {
            columns,
            relationships: Vec::new(),
            ..(*input_schema).clone()
        });
        let op = Rc::new(LiftField {
            input: input.clone(),
            lift: Rc::new(Lift {
                source_relationship,
                field,
                json_aggregate,
            }),
            schema,
            output: RefCell::new(no_output()),
        });
        input.set_output(Rc::downgrade(&op) as Weak<dyn Output>);
        op
    }

    fn emit(&self, change: Change) {
        let out = self.output.borrow().upgrade();
        if let Some(out) = out {
            out.push(change, self);
        }
    }
}

impl InputBase for LiftField {
    fn get_schema(&self) -> Rc<SourceSchema> {
        self.schema.clone()
    }
    fn destroy(&self) {
        self.input.destroy()
    }
}

impl Input for LiftField {
    fn set_output(&self, output: Weak<dyn Output>) {
        *self.output.borrow_mut() = output;
    }

    fn fetch(&self, req: &FetchRequest) -> NodeStream {
        let lift = self.lift.clone();
        Box::new(self.input.fetch(req).map(move |n| lift.lift(&n)))
    }
}

impl Output for LiftField {
    fn push(&self, change: Change, _pusher: &dyn InputBase) {
        let lift = &self.lift;
        match change {
            Change::Add(n) => self.emit(Change::Add(lift.lift(&n))),
            Change::Remove(n) => self.emit(Change::Remove(lift.lift(&n))),
            Change::Edit { node, old_node } => self.emit(Change::Edit {
                node: lift.lift(&node),
                old_node: lift.lift(&old_node),
            }),
            Change::Child(node, child) => {
                // A relationship this doesn't lift from, or a change below
                // the destination.
                if child.relationship_name != lift.source_relationship {
                    return;
                }
                let Some((old, new)) = lift.values(&child.change) else {
                    return;
                };
                self.emit(Change::Edit {
                    node: Node::new(with_column(&node.row, new)),
                    old_node: Node::new(with_column(&node.row, old)),
                });
            }
        }
    }

    fn reconcile(&self, _pusher: &dyn InputBase) {
        let out = self.output.borrow().upgrade();
        if let Some(out) = out {
            out.reconcile(self);
        }
    }
}
