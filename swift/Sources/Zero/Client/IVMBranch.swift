/// The IVM sources of one state of the replica
/// (`packages/zero-client/src/client/ivm-branch.ts`). The main branch feeds
/// the app's views; mutators run against forks, so a rebase can replay them
/// over a newer server snapshot without touching the views.
final class IVMBranch {
  private var sources: [String: MemorySource?]
  private let schema: ZeroSchema
  /// The layout of each table's rows. Forks share them, so the rows a
  /// mutator writes in a fork go into this branch's sources as they are.
  private let layouts: [String: RowLayout]
  /// The synced aggregates a child row's mutation moves, shared with the
  /// forks: it is query metadata, not row state.
  private let optimism: AggregateOptimism
  /// Only the main branch registers optimistic aggregates and releases
  /// aggregate sources (see `aggregateSource`).
  private let isFork: Bool
  /// Set when an aggregate's optimism is registered for the first time
  /// since it was released, or since launch; see `takeNewRegistration`.
  private var newRegistration = false

  convenience init(schema: ZeroSchema) {
    self.init(
      schema: schema, sources: [:], layouts: schema.tables.mapValues { RowLayout($0) },
      optimism: AggregateOptimism(), isFork: false)
  }

  private init(
    schema: ZeroSchema, sources: [String: MemorySource?], layouts: [String: RowLayout],
    optimism: AggregateOptimism, isFork: Bool
  ) {
    self.schema = schema
    self.sources = sources
    self.layouts = layouts
    self.optimism = optimism
    self.isFork = isFork
  }

  func source(_ table: String) -> MemorySource? {
    if let source = sources[table] { return source }
    let source: MemorySource?
    if let tableSchema = schema.tables[table] {
      source = MemorySource(tableSchema, layout: layouts[table]!)
    } else if isTopLevelAggregateTableName(table) {
      // A synced top-level aggregate's source. Its shape is fixed, so it can
      // be made from the name alone, also before its query materializes on
      // a reload. The payload is typed as `inferAggregateSchema` types it.
      source = MemorySource(
        TableSchema(
          name: table,
          columns: [(AggregateColumns.key, ColumnSchema(.number))]
            + AggregateColumns.payloadOrder.map { ($0, ColumnSchema(.number, optional: true)) },
          primaryKey: [AggregateColumns.key]))
    } else {
      source = nil
    }
    sources[table] = .some(source)
    return source
  }

  /// `getOrCreateAggregateSource`: the source of a relationship aggregate's
  /// or a grouped query's rows, whose shape the name doesn't give. The
  /// builder gives it when a query materializes; `apply` infers it from a
  /// row when stored rows are loaded first. The first caller's shape wins.
  func aggregateSource(
    _ name: String, columns: [(String, ColumnSchema)], primaryKey: [String],
    optimistic: OptimisticAggregate? = nil
  ) -> MemorySource {
    // A fork's queries are never synced, so only the main branch, which
    // releases them, registers.
    if let optimistic, !isFork {
      if optimism.byTable[optimistic.table]?[name] == nil { newRegistration = true }
      optimism.byTable[optimistic.table, default: [:]][name] = optimistic
    }
    if let existing = sources[name] ?? nil { return existing }
    let source = MemorySource(TableSchema(name: name, columns: columns, primaryKey: primaryKey))
    sources[name] = .some(source)
    return source
  }

  /// Whether an aggregate's optimism was registered for the first time since
  /// the last call. Swift only: JS persists a pending mutation's writes, so
  /// after a restart its view still has the moves the mutation made. Swift
  /// stores the mutation and replays it at launch, before any query
  /// registers, so the client rebases the pending mutations once one does,
  /// while one replayed at launch is pending.
  func takeNewRegistration() -> Bool {
    defer { newRegistration = false }
    return newRegistration
  }

  /// `serverColumnNames`: the server names of `columns` of `table`.
  func serverColumnNames(_ table: String, _ columns: [String]) -> [String] {
    let tableColumns = schema.tables[table]?.columns
    return columns.map { tableColumns?[$0]?.serverName ?? $0 }
  }

  /// `getOptimisticAggregates`: the synced aggregates whose child rows live
  /// in `table`, by name.
  func optimisticAggregates(_ table: String) -> [OptimisticAggregate] {
    (optimism.byTable[table] ?? [:]).sorted { StringCompare.utf16Less($0.key, $1.key) }
      .map(\.value)
  }

  /// A copy whose sources share rows with these until written.
  func fork() -> IVMBranch {
    IVMBranch(
      schema: schema, sources: sources.mapValues { $0?.fork() }, layouts: layouts,
      optimism: optimism, isFork: true)
  }

  /// Applies the row changes of `diff` (`applyDiffs`): the aggregate rows
  /// under `a/`, then the table rows under `e/`, which is key order. A run
  /// of adds to one source is pushed together. A row in `rows`, by key, is
  /// the one a transaction wrote, which is the value the diff has for it.
  /// The main branch then releases the aggregate sources nothing uses.
  func apply(_ diff: KVDiff, rows written: [String: Row] = [:]) {
    var i = 0
    // JS's throw ends `applyDiffs` at the change an operator failed on.
    while i < diff.count, !PipelineFailure.hasFailed {
      let op = diff[i]
      let isAggregate = op.key.utf8.starts(with: Keys.aggregatesPrefix.utf8)
      guard isAggregate || op.key.utf8.starts(with: Keys.entitiesPrefix.utf8) else {
        i += 1
        continue
      }
      let name = Keys.sourceName(op.key)
      var source = source(name)
      if source == nil, isAggregate {
        // A relationship aggregate's or a grouped query's rows replayed
        // before its query materializes (a reload): the source takes the
        // row's shape, and the builder reuses it.
        let row: JSONValue
        switch op {
        case .del(_, let old): row = old
        case .add(_, let new), .change(_, _, let new): row = new
        }
        let (columns, primaryKey) = Self.inferAggregateSchema(name, row.objectValue ?? [:])
        source = aggregateSource(name, columns: columns, primaryKey: primaryKey)
      }
      guard let source else {
        i += 1
        continue
      }
      switch op {
      case .del(_, let old):
        source.push(.remove(Row(old.objectValue ?? [:], layout: source.layout)))
        i += 1
      case .change(let key, let old, let new):
        source.push(
          .edit(
            written[key] ?? Row(new.objectValue ?? [:], layout: source.layout),
            oldRow: Row(old.objectValue ?? [:], layout: source.layout)))
        i += 1
      case .add:
        // The source's own prefix: the key up to the slash after its name.
        let keyPrefix = isAggregate ? Keys.aggregatesPrefix : Keys.entitiesPrefix
        let sourcePrefix =
          keyPrefix
          + (isAggregate ? String(name.dropFirst(aggregateTablePrefix.count)) : name) + "/"
        var rows: [Row] = []
        while i < diff.count, case .add(let key, let value) = diff[i],
          key.utf8.starts(with: sourcePrefix.utf8)
        {
          rows.append(written[key] ?? Row(value.objectValue ?? [:], layout: source.layout))
          i += 1
        }
        source.pushAdds(rows)
      }
    }
    if !isFork && !PipelineFailure.hasFailed { releaseAggregateSources() }
  }

  /// `#releaseAggregateSources`: drops the aggregate sources that are empty
  /// and that no view reads, with their optimistic registrations. The server
  /// deletes an aggregate's rows when its query goes, so this is how the
  /// query's client state goes too. Every source is checked: the server can
  /// empty one a view still reads, and one can lose its view with no rows.
  private func releaseAggregateSources() {
    for (name, source) in sources {
      guard let source, isAggregateTableName(name), source.data.isEmpty,
        source.connections.isEmpty
      else { continue }
      sources[name] = nil
      for table in optimism.byTable.keys { optimism.byTable[table]![name] = nil }
    }
  }

  /// `inferAggregateSchema`: an aggregate source's shape from one of its
  /// rows: the payload columns are nullable numbers, the others typed by
  /// their value; the key is `aggregateRowKeyColumns`.
  static func inferAggregateSchema(
    _ name: String, _ row: JSONObject
  ) -> (columns: [(String, ColumnSchema)], primaryKey: [String]) {
    var columns: [(String, ColumnSchema)] = []
    for (column, value) in row where column != Keys.rowVersionColumn {
      columns.append(
        (
          column,
          AggregateColumns.payload.contains(column)
            ? ColumnSchema(.number, optional: true) : ColumnSchema(value.valueType)
        ))
    }
    if !columns.contains(where: { $0.0 == AggregateColumns.value }) {
      columns.append((AggregateColumns.value, ColumnSchema(.number, optional: true)))
    }
    return (columns, Keys.aggregateRowKeyColumns(table: name, columns: row.keys))
  }
}

/// `OptimisticAggregate`: how a mutation of a child row moves a synced
/// aggregate's row. Registered when a query with a relationship aggregate
/// or a grouping materializes.
package struct OptimisticAggregate {
  /// The aggregate's table, `aggregate:<queryID>:<alias>` or `…:#group`.
  package let aggregateTable: String
  /// The child table whose mutations move it.
  package let table: String
  /// The correlation (or group) columns of a child row, by client name.
  package let childField: [String]
  /// The same columns as the aggregate rows name them: server names.
  package let keyField: [String]
  package let aggregate: AST.Aggregate
  /// The aggregate's `where` as a row predicate; nil when every child row
  /// counts.
  package let predicate: RowPredicate?
  /// The rows are a grouped query's groups, keyed by `_0_key`: a group is
  /// added with its first row and removed with its last.
  package let grouped: Bool
}

/// The registry of optimistic aggregates, by child table and then by
/// aggregate table, shared by a branch and its forks.
final class AggregateOptimism {
  var byTable: [String: [String: OptimisticAggregate]] = [:]
}

extension JSONValue {
  /// `valueTypeOf`: the column type of a value.
  var valueType: ValueType {
    switch self {
    case .string: .string
    case .number: .number
    case .bool: .boolean
    case .null: .null
    case .array, .object: .json
    }
  }
}
