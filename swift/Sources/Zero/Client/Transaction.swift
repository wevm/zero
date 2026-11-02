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
  /// A mutator's query computes its aggregates from the branch's rows,
  /// unless the server has answered it: then it reads the synced rows, with
  /// this client's pending changes applied, as a view of it does
  /// (`MutatorContext` in `custom.ts`).
  private lazy var context = QueryContext.local(branch, aggregatesFromSource: false)
  private lazy var syncedContext = QueryContext.local(branch, aggregatesFromSource: true)

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
    try S(view: run(query.any))
  }

  /// The rows `query` returns over this transaction's data.
  public func run(_ query: AnyQuery) throws -> ViewValue {
    let queryID =
      query.customQueryID.map { QueryHash.hashOfNameAndArgs($0.name, $0.args) }
      ?? QueryHash.hashOfAST(query.ast)
    return try (write.has(Keys.gotQueriesKey(hash: queryID)) ? syncedContext : context)
      .snapshot(query)
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
    try applyOptimisticAggregateDeltas(tableName, old: nil, new: row)
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
    let oldRow = Row(prev, layout: source.layout)
    rows[key] = row
    source.push(.edit(row, oldRow: oldRow))
    try applyOptimisticAggregateDeltas(tableName, old: oldRow, new: row)
  }

  func delete(_ tableName: String, _ id: Row) throws {
    let table = try tableSchema(tableName)
    let key = try key(table, id)
    guard let prev = write.get(key)?.objectValue else { return }
    write.delete(key)
    rows[key] = nil
    let source = source(tableName)
    let oldRow = Row(prev, layout: source.layout)
    source.push(.remove(oldRow))
    try applyOptimisticAggregateDeltas(tableName, old: oldRow, new: nil)
  }

  // MARK: Optimistic aggregates (`crud-impl.ts`)

  /// `applyOptimisticAggregateDeltas`: moves the synced aggregates whose
  /// child rows live in `table` by a child row's change, in this
  /// transaction, so views and `run` see it at once. The rebase replays it
  /// until the server confirms the mutation, whose value then replaces it.
  /// An aggregate row not on the client is left alone, except that a
  /// grouped query's group is added by its first row.
  private func applyOptimisticAggregateDeltas(_ table: String, old: Row?, new: Row?) throws {
    for agg in branch.optimisticAggregates(table) {
      let oldKey = try old.flatMap { try aggregateKey(agg, $0) }
      let newKey = try new.flatMap { try aggregateKey(agg, $0) }
      // JS's `===`: by code unit.
      let sameKey =
        switch (oldKey, newKey) {
        case (nil, nil): true
        case (let a?, let b?): StringCompare.equal(a, b)
        default: false
        }
      if sameKey {
        if let oldKey { applyAggregateDelta(agg, oldKey, old, new) }
      } else {
        // The child leaves one aggregate and joins another; either may be
        // none.
        if let oldKey { applyAggregateDelta(agg, oldKey, old, nil) }
        if let newKey { applyAggregateDelta(agg, newKey, nil, new) }
      }
    }
  }

  /// `aggregateKeyFor`: the key of the aggregate row `child` counts in; nil
  /// when a correlation value is null, which gives the child no parent. A
  /// grouped query's group can have null values.
  private func aggregateKey(_ agg: OptimisticAggregate, _ child: Row) throws -> String? {
    if agg.grouped {
      let values = agg.childField.map { child[$0] ?? .null }
      return try Keys.aggregateRowKey(
        table: agg.aggregateTable, row: [AggregateColumns.key: .string(groupKey(values))])
    }
    var keyRow = JSONObject()
    for (i, column) in agg.childField.enumerated() {
      guard let value = child[column], value != .null else { return nil }
      keyRow[agg.keyField[i]] = value
    }
    return try Keys.aggregateRowKey(table: agg.aggregateTable, row: keyRow)
  }

  /// `applyAggregateDelta`: writes the aggregate row at `key` as one child
  /// change moves it, if the row is on the client, and pushes it to the
  /// branch so a later `run` reads it. A grouped query's row is added by a
  /// group's first row and removed with its last.
  private func applyAggregateDelta(
    _ agg: OptimisticAggregate, _ key: String, _ old: Row?, _ new: Row?
  ) {
    // The branch holds the row when a view of its query reads it.
    let source = branch.source(agg.aggregateTable)
    guard let cur = write.get(key)?.objectValue else {
      if agg.grouped, let added = newGroupRow(agg, old, new) {
        write.set(key, .object(added))
        source?.push(.add(Row(added, layout: source!.layout)))
      }
      return
    }
    guard let updated = nextAggregateRow(agg, cur, old, new) else { return }
    let curRow = source.map { Row(cur, layout: $0.layout) }
    let inBranch = curRow.map { source!.data.contains($0) } ?? false
    if agg.grouped, Self.groupRowCount(agg, updated) == 0 {
      write.delete(key)
      if inBranch { source!.push(.remove(curRow!)) }
      return
    }
    write.set(key, .object(updated))
    if inBranch { source!.push(.edit(Row(updated, layout: source!.layout), oldRow: curRow!)) }
  }

  /// `groupRowCount`: the number of rows of a group, nil for a sum or
  /// average row stored before the rows carried it.
  private static func groupRowCount(_ agg: OptimisticAggregate, _ row: JSONObject) -> JSONValue? {
    row[agg.aggregate.fn == .count ? AggregateColumns.value : AggregateColumns.rows]
  }

  /// `newGroupRow`: the row of a group not on the client that `new` is the
  /// first row of: it passes the `where`, and `old` wasn't counted already.
  private func newGroupRow(_ agg: OptimisticAggregate, _ old: Row?, _ new: Row?) -> JSONObject? {
    func counted(_ row: Row?) -> Bool {
      guard let row else { return false }
      return agg.predicate?(row) ?? true
    }
    guard let new, counted(new), !counted(old) else { return nil }
    let values = agg.childField.map { new[$0] ?? .null }
    // The group with no rows, which `new` then joins.
    var empty = JSONObject()
    for (i, column) in agg.keyField.enumerated() { empty[column] = values[i] }
    empty[AggregateColumns.key] = .string(groupKey(values))
    if agg.aggregate.fn == .count {
      empty[AggregateColumns.value] = 0
    } else {
      empty[AggregateColumns.value] = .null
      empty[AggregateColumns.sum] = 0
      empty[AggregateColumns.count] = 0
      empty[AggregateColumns.rows] = 0
    }
    return nextAggregateRow(agg, empty, nil, new)
  }

  /// `nextAggregateRow`: the aggregate row after a child changes from `old`
  /// to `new`, nil when it doesn't move. A row the `where` rejects counts
  /// as absent, so a row entering or leaving the filtered set moves it too.
  private func nextAggregateRow(
    _ agg: OptimisticAggregate, _ cur: JSONObject, _ oldIn: Row?, _ newIn: Row?
  ) -> JSONObject? {
    let old = oldIn.flatMap { agg.predicate?($0) == false ? nil : $0 }
    let new = newIn.flatMap { agg.predicate?($0) == false ? nil : $0 }
    func number(_ v: JSONValue?) -> Double { v?.numberValue ?? 0 }
    func contribution(_ row: Row?) -> JSONValue? { row.map { aggregateValue(agg.aggregate, $0) } }
    func contributes(_ row: Row?) -> Bool { contribution(row)?.numberValue != nil }
    var next = cur
    switch agg.aggregate.fn {
    case .count:
      let delta = (new != nil ? 1.0 : 0) - (old != nil ? 1.0 : 0)
      if delta == 0 { return nil }
      next[AggregateColumns.value] = .number(number(cur[AggregateColumns.value]) + delta)
      return next
    case .sum, .avg:
      guard agg.aggregate.field != nil else { return nil }
      // The row carries the sum of the non-null values and their count, so
      // both move and the value follows: SQL's sum and avg skip nulls and
      // are null over none. A group's row also carries its number of rows,
      // nulls included, unless it was stored before it did.
      let sumDelta = number(contribution(new)) - number(contribution(old))
      let countDelta = (contributes(new) ? 1.0 : 0) - (contributes(old) ? 1.0 : 0)
      let hasRows = cur[AggregateColumns.rows] != nil
      let rowsDelta = hasRows ? (new != nil ? 1.0 : 0) - (old != nil ? 1.0 : 0) : 0
      if sumDelta == 0 && countDelta == 0 && rowsDelta == 0 { return nil }
      let sum = number(cur[AggregateColumns.sum]) + sumDelta
      let count = number(cur[AggregateColumns.count]) + countDelta
      let result = agg.aggregate.fn == .sum ? sum : sum / count
      next[AggregateColumns.value] = count > 0 ? .number(result) : .null
      next[AggregateColumns.sum] = .number(sum)
      next[AggregateColumns.count] = .number(count)
      if hasRows {
        next[AggregateColumns.rows] = .number(number(cur[AggregateColumns.rows]) + rowsDelta)
      }
      return next
    case .min, .max:
      // Not invertible: the server's next poke moves them.
      return nil
    }
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
