/// A row flowing through a pipeline, plus its relationships
/// (`packages/zql/src/ivm/data.ts`). Relationships are generated lazily as
/// they are read.
public struct Node {
  public var row: Row
  public var relationships: Relationships

  @inlinable
  public init(row: Row, relationships: Relationships = Relationships()) {
    self.row = row
    self.relationships = relationships
  }

  /// This node with `stream` as its relationship `name`.
  @inlinable
  func with(_ name: String, _ stream: @escaping () -> NodeStream) -> Node {
    var copy = self
    copy.relationships[name] = stream
    return copy
  }
}

/// A node's relationships, in insertion order like a JS object's keys. Nodes
/// carry a handful at most, so lookups are linear.
public struct Relationships: Sequence {
  public typealias Stream = () -> NodeStream
  @usableFromInline var entries: [(name: String, stream: Stream)]

  @inlinable
  public init() { entries = [] }

  @inlinable
  public var isEmpty: Bool { entries.isEmpty }

  @inlinable
  public var names: [String] { entries.map(\.name) }

  @inlinable
  public subscript(name: String) -> Stream? {
    get {
      for entry in entries where entry.name == name { return entry.stream }
      return nil
    }
    set {
      if let i = entries.firstIndex(where: { $0.name == name }) {
        if let newValue {
          entries[i].stream = newValue
        } else {
          entries.remove(at: i)
        }
      } else if let newValue {
        entries.append((name, newValue))
      }
    }
  }

  /// `{...self, ...other}`: `other` wins, keys keep their first position.
  @inlinable
  public func merging(_ other: Relationships) -> Relationships {
    var result = self
    for entry in other.entries { result[entry.name] = entry.stream }
    return result
  }

  @inlinable
  public func makeIterator() -> IndexingIterator<[(name: String, stream: Stream)]> {
    entries.makeIterator()
  }
}

/// The kinds of change (`change-type-enum.ts`).
public enum ChangeType: Int, Sendable, Hashable {
  case add = 0
  case remove = 1
  case edit = 2
  case child = 3
}

/// A change to a child relationship of a node.
public struct ChildChange {
  public var relationshipName: String
  public var change: Change

  @inlinable
  public init(relationshipName: String, change: Change) {
    self.relationshipName = relationshipName
    self.change = change
  }
}

/// A change pushed through a pipeline (`packages/zql/src/ivm/change.ts`).
public indirect enum Change {
  case add(Node)
  case remove(Node)
  case child(Node, ChildChange)
  case edit(Node, oldNode: Node)

  @inlinable
  public var type: ChangeType {
    switch self {
    case .add: .add
    case .remove: .remove
    case .child: .child
    case .edit: .edit
    }
  }

  /// The node, or the new node of an edit.
  @inlinable
  public var node: Node {
    switch self {
    case .add(let n), .remove(let n), .child(let n, _), .edit(let n, _): n
    }
  }
}

/// A change to a source's rows (`packages/zql/src/ivm/source.ts`).
public enum SourceChange: Sendable {
  case add(Row)
  case remove(Row)
  case edit(Row, oldRow: Row)

  @inlinable
  public var type: ChangeType {
    switch self {
    case .add: .add
    case .remove: .remove
    case .edit: .edit
    }
  }

  /// The row, or the new row of an edit.
  @inlinable
  public var row: Row {
    switch self {
    case .add(let r), .remove(let r), .edit(let r, _): r
    }
  }
}
