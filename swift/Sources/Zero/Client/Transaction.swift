/// The transaction a mutator runs in on the client (`TransactionImpl` in
/// `packages/zero-client/src/client/custom.ts`). Writes go to the local
/// store and to the IVM branch the mutator reads through `run`.
@MainActor
public final class Transaction {
  public enum Reason: String, Sendable {
    /// The first run, when the app calls `mutate`.
    case optimistic
    /// A replay over newer server data while the mutation is pending.
    case rebase
  }

  /// Where the mutator runs: always `client` here.
  public let location = "client"
  public let clientID: String
  public let mutationID: Int
  public let reason: Reason
  /// The auth token the client has.
  public let token: String?

  let write: KVWrite
  let branch: IVMBranch
  let schema: ZeroSchema
  /// The rows written, by key, as the branch has them, so the main branch
  /// can take the same rows rather than a copy of each.
  private(set) var rows: [String: Row] = [:]
  private lazy var context = QueryContext.local(branch)

  init(
    clientID: String, mutationID: Int, reason: Reason, token: String?, write: KVWrite,
    branch: IVMBranch, schema: ZeroSchema
  ) {
    self.clientID = clientID
    self.mutationID = mutationID
    self.reason = reason
    self.token = token
    self.write = write
    self.branch = branch
    self.schema = schema
  }

  /// The CRUD operations, by table: `tx.mutate.issue.insert(row)`.
  public var mutate: TransactionMutate { TransactionMutate(tx: self) }

  /// The rows `query` returns over this transaction's data.
  public func run<R: ZeroRow, S: QueryResult>(_ query: Query<R, S>) throws -> S {
    try S(view: context.snapshot(query.any))
  }

  /// The rows `query` returns over this transaction's data.
  public func run(_ query: AnyQuery) throws -> ViewValue {
    try context.snapshot(query)
  }

  func tableSchema(_ table: String) throws -> TableSchema {
    guard let t = schema.tables[table] else { throw ApplicationError("Unknown table: \(table)") }
    return t
  }

  func key(_ table: TableSchema, _ value: Row) throws -> String {
    try Keys.primaryKeyString(table: table.name, primaryKey: table.primaryKey, row: value)
  }

  private func source(_ table: String) -> MemorySource {
    guard let source = branch.source(table) else {
      preconditionFailure("No source for table \(table)")
    }
    return source
  }

  /// `insert` in `crud-impl.ts`: a row that exists is left alone. Columns
  /// the value leaves out are null.
  func insert(_ tableName: String, _ value: Row) throws {
    let table = try tableSchema(tableName)
    let key = try key(table, value)
    guard !write.has(key) else { return }
    let source = source(tableName)
    var row = value.conformed(to: source.layout)
    for column in table.columnNames where row[column] == nil { row[column] = .null }
    write.set(key, JSONValue(row))
    rows[key] = row
    source.push(.add(row))
  }

  func upsert(_ tableName: String, _ value: Row) throws {
    let table = try tableSchema(tableName)
    if write.has(try key(table, value)) {
      try update(tableName, value)
    } else {
      try insert(tableName, value)
    }
  }

  /// Merges the value's columns into the row; a missing row is left alone.
  func update(_ tableName: String, _ value: Row) throws {
    let table = try tableSchema(tableName)
    let key = try key(table, value)
    guard let prev = write.get(key)?.objectValue else { return }
    var next = prev
    for (k, v) in value { next[k] = v }
    write.set(key, .object(next))
    let source = source(tableName)
    let row = Row(next, layout: source.layout)
    rows[key] = row
    source.push(.edit(row, oldRow: Row(prev, layout: source.layout)))
  }

  func delete(_ tableName: String, _ id: Row) throws {
    let table = try tableSchema(tableName)
    let key = try key(table, id)
    guard let prev = write.get(key)?.objectValue else { return }
    write.delete(key)
    rows[key] = nil
    let source = source(tableName)
    source.push(.remove(Row(prev, layout: source.layout)))
  }
}

/// `tx.mutate`: the tables of the schema. `zero-swift codegen` adds a
/// typed member per table, `tx.mutate.issue`, as in JS. A table without
/// one, or `table(_:)`, gives the untyped ``TableMutator``.
@MainActor
@dynamicMemberLookup
public struct TransactionMutate {
  let tx: Transaction

  public subscript(dynamicMember table: String) -> TableMutator {
    TableMutator(tx: tx, table: table)
  }

  /// The untyped CRUD operations on `name`, with values keyed by client
  /// column name.
  public func table(_ name: String) -> TableMutator { TableMutator(tx: tx, table: name) }
}

/// The CRUD operations on one table (`TableCRUD` in
/// `packages/zql/src/mutate/crud.ts`). Values are keyed by client column
/// name.
@MainActor
public struct TableMutator {
  let tx: Transaction
  public let table: String

  /// Inserts a row unless one with its primary key exists. Columns it
  /// leaves out are null.
  public func insert(_ value: Row) throws { try tx.insert(table, value) }
  /// Inserts a row, or merges the value into the existing one.
  public func upsert(_ value: Row) throws { try tx.upsert(table, value) }
  /// Merges the value's columns into the row with its primary key, if any.
  public func update(_ value: Row) throws { try tx.update(table, value) }
  /// Deletes the row with the primary key in `id`, if any.
  public func delete(_ id: Row) throws { try tx.delete(table, id) }

  public func insert(_ value: [String: JSONValue]) throws { try insert(Row(value)) }
  public func upsert(_ value: [String: JSONValue]) throws { try upsert(Row(value)) }
  public func update(_ value: [String: JSONValue]) throws { try update(Row(value)) }
  public func delete(_ id: [String: JSONValue]) throws { try delete(Row(id)) }

  public func insert(_ value: JSONObject) throws { try insert(Row(value)) }
  public func upsert(_ value: JSONObject) throws { try upsert(Row(value)) }
  public func update(_ value: JSONObject) throws { try update(Row(value)) }
  public func delete(_ id: JSONObject) throws { try delete(Row(id)) }
}

/// A row struct with typed writes, as `zero-swift codegen` generates it:
/// `Update` is what `update` takes (`UpdateValue` in JS) and `Key` what
/// `delete` takes (`DeleteID`).
public protocol ZeroTable: ZeroRow {
  associatedtype Update: ZeroColumnValues
  associatedtype Key: ZeroColumnValues
}

/// Some of a table's columns, keyed by client column name.
public protocol ZeroColumnValues: Sendable {
  var columnValues: Row { get }
}

/// The CRUD operations on one table, typed by its row struct (`TableCRUD`
/// in `packages/zql/src/mutate/crud.ts`): `tx.mutate.issue`.
@MainActor
public struct TypedTableMutator<R: ZeroTable> {
  let tx: Transaction

  public init(_ mutate: TransactionMutate) { tx = mutate.tx }

  /// Inserts a row unless one with its primary key exists.
  public func insert(_ row: R) throws { try tx.insert(R.tableName, row.columnValues) }
  /// Inserts a row, or replaces the columns of the existing one. Every
  /// column is written, so an optional one left nil sets null; JS keeps a
  /// column the value leaves out. Use `update` to set only some columns.
  public func upsert(_ row: R) throws { try tx.upsert(R.tableName, row.columnValues) }
  /// Sets the columns of the row with the primary key in `value` that
  /// `value` sets, if the row exists.
  public func update(_ value: R.Update) throws { try tx.update(R.tableName, value.columnValues) }
  /// Deletes the row with the primary key `id`, if any.
  public func delete(_ id: R.Key) throws { try tx.delete(R.tableName, id.columnValues) }
}

/// An optional column in an update: left as it is (the default), set to
/// null, or set to a value. A literal sets the value: `ownerID: "u1"`.
public enum ColumnUpdate<Value: ZeroValue & Hashable>: Sendable, Hashable {
  case unchanged
  case null
  case set(Value)

  /// Sets the column to `value`, or to null when it is nil.
  public init(_ value: Value?) {
    self = value.map { .set($0) } ?? .null
  }

  /// What the update writes to the column; nil when it leaves it.
  public var zeroValue: JSONValue? {
    switch self {
    case .unchanged: nil
    case .null: .null
    case .set(let value): value.zeroValue
    }
  }
}

extension ColumnUpdate: ExpressibleByUnicodeScalarLiteral
where Value: ExpressibleByStringLiteral, Value.StringLiteralType == String {
  public init(unicodeScalarLiteral value: String) { self = .set(Value(stringLiteral: value)) }
}

extension ColumnUpdate: ExpressibleByExtendedGraphemeClusterLiteral
where Value: ExpressibleByStringLiteral, Value.StringLiteralType == String {
  public init(extendedGraphemeClusterLiteral value: String) {
    self = .set(Value(stringLiteral: value))
  }
}

extension ColumnUpdate: ExpressibleByStringLiteral
where Value: ExpressibleByStringLiteral, Value.StringLiteralType == String {
  public init(stringLiteral value: String) { self = .set(Value(stringLiteral: value)) }
}

extension ColumnUpdate: ExpressibleByIntegerLiteral where Value: ExpressibleByIntegerLiteral {
  public init(integerLiteral value: Value.IntegerLiteralType) {
    self = .set(Value(integerLiteral: value))
  }
}

extension ColumnUpdate: ExpressibleByFloatLiteral where Value: ExpressibleByFloatLiteral {
  public init(floatLiteral value: Value.FloatLiteralType) {
    self = .set(Value(floatLiteral: value))
  }
}

extension ColumnUpdate: ExpressibleByBooleanLiteral where Value: ExpressibleByBooleanLiteral {
  public init(booleanLiteral value: Value.BooleanLiteralType) {
    self = .set(Value(booleanLiteral: value))
  }
}
