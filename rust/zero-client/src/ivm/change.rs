//! Nodes and the changes pushed through a pipeline
//! (`packages/zql/src/ivm/data.ts`, `change.ts`, and `source.ts`).

use super::{NodeStream, Row};
use std::rc::Rc;

/// A relationship of a node: a stream of its child nodes, made each time it
/// is read.
pub type RelationshipStream = Rc<dyn Fn() -> NodeStream>;

/// A row flowing through a pipeline, plus its relationships. Relationships
/// are generated lazily as they are read.
#[derive(Clone)]
pub struct Node {
    pub row: Row,
    pub relationships: Relationships,
}

impl Node {
    pub fn new(row: Row) -> Self {
        Node {
            row,
            relationships: Relationships::default(),
        }
    }

    /// This node with `stream` as its relationship `name`.
    pub fn with(mut self, name: &str, stream: RelationshipStream) -> Self {
        self.relationships.set(name, Some(stream));
        self
    }
}

impl std::fmt::Debug for Node {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Node")
            .field("row", &self.row)
            .field("relationships", &self.relationships.names())
            .finish()
    }
}

/// A node's relationships, in insertion order like a JS object's keys. Nodes
/// carry a handful at most, so lookups are linear.
#[derive(Clone, Default)]
pub struct Relationships {
    entries: Vec<(String, RelationshipStream)>,
}

impl Relationships {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn names(&self) -> Vec<&str> {
        self.entries.iter().map(|(n, _)| n.as_str()).collect()
    }

    pub fn get(&self, name: &str) -> Option<&RelationshipStream> {
        self.entries.iter().find(|(n, _)| n == name).map(|(_, s)| s)
    }

    /// Sets, replaces (keeping its position), or with `None` removes the
    /// relationship `name`.
    pub fn set(&mut self, name: &str, stream: Option<RelationshipStream>) {
        let i = self.entries.iter().position(|(n, _)| n == name);
        match (i, stream) {
            (Some(i), Some(s)) => self.entries[i].1 = s,
            (Some(i), None) => {
                self.entries.remove(i);
            }
            (None, Some(s)) => self.entries.push((name.to_owned(), s)),
            (None, None) => {}
        }
    }

    /// `{...self, ...other}`: `other` wins, keys keep their first position.
    pub fn merging(&self, other: &Relationships) -> Relationships {
        let mut result = self.clone();
        for (name, stream) in &other.entries {
            result.set(name, Some(stream.clone()));
        }
        result
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &RelationshipStream)> {
        self.entries.iter().map(|(n, s)| (n.as_str(), s))
    }
}

/// The kinds of change (`change-type-enum.ts`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChangeType {
    Add = 0,
    Remove = 1,
    Edit = 2,
    Child = 3,
}

/// A change to a child relationship of a node.
#[derive(Clone, Debug)]
pub struct ChildChange {
    pub relationship_name: String,
    pub change: Change,
}

/// A change pushed through a pipeline (`change.ts`).
#[derive(Clone, Debug)]
pub enum Change {
    Add(Node),
    Remove(Node),
    Child(Node, Box<ChildChange>),
    Edit { node: Node, old_node: Node },
}

impl Change {
    pub fn change_type(&self) -> ChangeType {
        match self {
            Change::Add(_) => ChangeType::Add,
            Change::Remove(_) => ChangeType::Remove,
            Change::Child(..) => ChangeType::Child,
            Change::Edit { .. } => ChangeType::Edit,
        }
    }

    /// The node, or the new node of an edit.
    pub fn node(&self) -> &Node {
        match self {
            Change::Add(n) | Change::Remove(n) | Change::Child(n, _) => n,
            Change::Edit { node, .. } => node,
        }
    }

    /// A child change of `node`.
    pub fn child(node: Node, relationship_name: impl Into<String>, change: Change) -> Change {
        Change::Child(
            node,
            Box::new(ChildChange {
                relationship_name: relationship_name.into(),
                change,
            }),
        )
    }
}

/// A change to a source's rows (`source.ts`).
#[derive(Clone, Debug, PartialEq)]
pub enum SourceChange {
    Add(Row),
    Remove(Row),
    Edit { row: Row, old_row: Row },
}

impl SourceChange {
    pub fn change_type(&self) -> ChangeType {
        match self {
            SourceChange::Add(_) => ChangeType::Add,
            SourceChange::Remove(_) => ChangeType::Remove,
            SourceChange::Edit { .. } => ChangeType::Edit,
        }
    }

    /// The row, or the new row of an edit.
    pub fn row(&self) -> &Row {
        match self {
            SourceChange::Add(r) | SourceChange::Remove(r) => r,
            SourceChange::Edit { row, .. } => row,
        }
    }
}
