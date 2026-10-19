/// A row: column name to value. A missing column reads as `nil`, as an
/// `undefined` property does in JS.
///
/// It reads and writes like a `[String: JSONValue]`. Inside, the values sit
/// in an array, in the order of a layout the rows of a table share, so code
/// that knows the layout reads a column by index instead of hashing its
/// name.
public struct Row: Sendable {
  @usableFromInline package var layout: RowLayout
  /// A value for each of the layout's columns; nil where the row has none.
  @usableFromInline package var slots: [JSONValue?]

  @inlinable
  package init(layout: RowLayout, slots: [JSONValue?]) {
    self.layout = layout
    self.slots = slots
  }

  /// An empty row.
  public init() {
    self.init(layout: .empty, slots: [])
  }

  /// The row of `dictionary`'s columns.
  public init(_ dictionary: [String: JSONValue]) {
    let names = dictionary.keys.sorted(by: StringCompare.utf16Less)
    self.init(layout: RowLayout(names), slots: names.map { dictionary[$0] })
  }

  /// The row of `dictionary`'s columns in `layout`, with any others after
  /// them.
  package init(_ dictionary: [String: JSONValue], layout: RowLayout) {
    var slots = [JSONValue?](repeating: nil, count: layout.names.count)
    var others: [String] = []
    for (name, value) in dictionary {
      if let i = layout.index(of: name) {
        slots[i] = value
      } else {
        others.append(name)
      }
    }
    if others.isEmpty {
      self.init(layout: layout, slots: slots)
    } else {
      others.sort(by: StringCompare.utf16Less)
      self.init(
        layout: RowLayout(layout.names + others), slots: slots + others.map { dictionary[$0] })
    }
  }

  /// This row in `layout`, with any other columns after its own. The row
  /// itself when it's in `layout` already.
  package func conformed(to layout: RowLayout) -> Row {
    if self.layout === layout { return self }
    var slots = [JSONValue?](repeating: nil, count: layout.names.count)
    var others: [(String, JSONValue)] = []
    for (name, value) in self {
      if let i = layout.index(of: name) {
        slots[i] = value
      } else {
        others.append((name, value))
      }
    }
    if others.isEmpty { return Row(layout: layout, slots: slots) }
    return Row(
      layout: RowLayout(layout.names + others.map(\.0)), slots: slots + others.map(\.1))
  }

  /// The value of the column `name`.
  @inlinable
  public subscript(name: String) -> JSONValue? {
    get {
      guard let i = layout.index(of: name) else { return nil }
      return slots[i]
    }
    set {
      if let i = layout.index(of: name) {
        slots[i] = newValue
      } else if let newValue {
        // A new layout: others' rows keep theirs.
        layout = layout.appending(name)
        slots.append(newValue)
      }
    }
  }

  /// The value of the column `name`, or `defaultValue`.
  @inlinable
  public subscript(name: String, default defaultValue: @autoclosure () -> JSONValue) -> JSONValue {
    self[name] ?? defaultValue()
  }

  /// The value of `column`: by index when this row is in its layout.
  @inlinable
  package subscript(column column: ColumnRef) -> JSONValue? {
    if layout === column.layout {
      guard let i = column.index else { return nil }
      return slots[i]
    }
    return self[column.name]
  }

  /// Removes the column `name`, returning its value.
  @discardableResult
  public mutating func removeValue(forKey name: String) -> JSONValue? {
    guard let i = layout.index(of: name) else { return nil }
    defer { slots[i] = nil }
    return slots[i]
  }

  /// How many columns have a value.
  public var count: Int {
    var n = 0
    for slot in slots where slot != nil { n += 1 }
    return n
  }

  public var isEmpty: Bool { slots.allSatisfy { $0 == nil } }

  /// The names of the columns with a value, in the layout's order.
  public var keys: [String] {
    zip(layout.names, slots).compactMap { $1 == nil ? nil : $0 }
  }

  /// The row as a dictionary.
  public var dictionary: [String: JSONValue] {
    var out: [String: JSONValue] = [:]
    out.reserveCapacity(slots.count)
    for (name, value) in self { out[name] = value }
    return out
  }
}

extension Row: Sequence {
  public struct Iterator: IteratorProtocol {
    @usableFromInline let row: Row
    @usableFromInline var i = 0

    @inlinable
    init(_ row: Row) { self.row = row }

    @inlinable
    public mutating func next() -> (key: String, value: JSONValue)? {
      while i < row.slots.count {
        defer { i += 1 }
        if let value = row.slots[i] { return (row.layout.names[i], value) }
      }
      return nil
    }
  }

  @inlinable
  public func makeIterator() -> Iterator { Iterator(self) }

  @inlinable
  public var underestimatedCount: Int { 0 }
}

extension Row: ExpressibleByDictionaryLiteral {
  public init(dictionaryLiteral elements: (String, JSONValue)...) {
    var names: [String] = []
    var slots: [JSONValue?] = []
    var indices: [String: Int] = [:]
    for (name, value) in elements {
      if let i = indices[name] {
        slots[i] = value
      } else {
        indices[name] = names.count
        names.append(name)
        slots.append(value)
      }
    }
    self.init(layout: RowLayout(names, indices: indices), slots: slots)
  }
}

extension Row: Hashable {
  /// The same columns with equal values, whatever the layouts.
  public static func == (a: Row, b: Row) -> Bool {
    if a.layout === b.layout { return a.slots == b.slots }
    var n = 0
    for (name, value) in a {
      guard let other = b[name], other == value else { return false }
      n += 1
    }
    return n == b.count
  }

  /// Independent of the layout, as `==` is.
  public func hash(into hasher: inout Hasher) {
    var acc = 0
    var n = 0
    for (name, value) in self {
      var h = Hasher()
      h.combine(name)
      h.combine(value)
      acc ^= h.finalize()
      n += 1
    }
    hasher.combine(n)
    hasher.combine(acc)
  }
}

extension Row: CustomStringConvertible, CustomDebugStringConvertible {
  public var description: String {
    "[" + map { "\(String(reflecting: $0.key)): \($0.value)" }.joined(separator: ", ") + "]"
  }

  public var debugDescription: String { description }
}

extension JSONValue {
  /// The row as a JSON object.
  public init(_ row: Row) {
    self = .object(row.dictionary)
  }
}

/// The columns of rows, in order. The rows of a table share one, so a row
/// stores only its values, and a column's index is the same in all of them.
///
/// Immutable, so reading it needs no exclusivity checks: a row that adds a
/// column gets a new layout.
@usableFromInline
package final class RowLayout: Sendable {
  @usableFromInline let names: [String]
  @usableFromInline let indices: [String: Int]

  package init(_ names: [String]) {
    var indices: [String: Int] = [:]
    indices.reserveCapacity(names.count)
    for (i, name) in names.enumerated() where indices[name] == nil { indices[name] = i }
    self.names = names
    self.indices = indices
  }

  init(_ names: [String], indices: [String: Int]) {
    self.names = names
    self.indices = indices
  }

  package static let empty = RowLayout([])

  /// The layout of `table`'s columns, in declaration order.
  package convenience init(_ table: TableSchema) {
    self.init(table.columnNames)
  }

  @inlinable
  package func index(of name: String) -> Int? { indices[name] }

  /// A column of rows in this layout.
  package func column(_ name: String) -> ColumnRef {
    ColumnRef(layout: self, index: index(of: name), name: name)
  }

  /// This layout with `name` after its columns.
  @usableFromInline
  func appending(_ name: String) -> RowLayout {
    var indices = indices
    indices[name] = names.count
    return RowLayout(names + [name], indices: indices)
  }
}

/// A column, resolved against a layout so rows in it read it by index.
/// Rows in other layouts read it by name.
@usableFromInline
package struct ColumnRef: Sendable {
  @usableFromInline let layout: RowLayout
  /// Nil when the layout has no such column.
  @usableFromInline let index: Int?
  @usableFromInline package let name: String

  @usableFromInline
  init(layout: RowLayout, index: Int?, name: String) {
    self.layout = layout
    self.index = index
    self.name = name
  }
}
