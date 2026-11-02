/// `GroupBy`, `GroupProjection` and their helpers
/// (`packages/zql/src/ivm/group-by.ts`).

/// `groupSourceSchema`: the columns, primary key and sort of the rows
/// `GroupBy` emits: the group columns, the group's key (`_0_key`) and the
/// aggregate's payload, with a sum or average's row count (`_0_rows`).
///
/// The key is the group's values in one string, since a synced row key can't
/// hold a `null`, which is a value like any other here.
package func groupSourceSchema(
  _ inputColumns: [String: ColumnSchema], _ ordering: AST.Ordering, _ aggregate: AST.Aggregate
) throws(PipelineError) -> (
  columns: [String: ColumnSchema], primaryKey: [String], sort: AST.Ordering
) {
  var (columns, _) = try aggregateSourceSchema(
    inputColumns, groupKey: ordering.map(\.field), aggregate)
  columns[AggregateColumns.key] = ColumnSchema(.string)
  if AggregateColumns.carriesComponents(aggregate.fn) {
    columns[AggregateColumns.rows] = ColumnSchema(.number)
  }
  // The key is unique and follows from the columns: it only makes the
  // order total.
  return (columns, [AggregateColumns.key], ordering + [AST.OrderPart(AggregateColumns.key, .asc)])
}

/// `groupKey`: a group's `_0_key`, which also keys its state: `valueToKey`
/// of the value for one column, the values JSON encoded for several.
package func groupKey(_ values: [JSONValue]) -> String {
  values.count == 1 ? valueToKey(values[0]) : JSONSerializer.stringify(.array(values))
}

/// `groupOrdering`: the order of a grouped query's rows: its `orderBy`
/// restricted to the group columns (its `groupBy`, or the columns a count of
/// distinct combinations groups by), then the rest of them ascending.
package func groupOrdering(_ ast: AST) -> AST.Ordering {
  let columns = ast.groupBy ?? ast.aggregate?.distinct ?? []
  var ordering: AST.Ordering = []
  for part in ast.orderBy ?? []
  where columns.contains(part.field) && !ordering.contains(where: { $0.field == part.field }) {
    ordering.append(part)
  }
  for column in columns where !ordering.contains(where: { $0.field == column }) {
    ordering.append(AST.OrderPart(column, .asc))
  }
  return ordering
}

/// Groups its input by columns and reduces each group, as `GROUP BY` does
/// (`GroupBy`): one row per combination of the columns' values,
/// `{...columns, _0_key, _0_value}`, with a sum or average's components.
/// A distinct is a GroupBy with a count, whose value a view doesn't show.
///
/// The input is sorted by the group columns first, so a fetch folds each run
/// of a group in one pass. Every group's state is kept: a push adds a group
/// with its first row, removes it with its last, and otherwise edits it when
/// its value changes. A min or max whose extreme leaves is read again in
/// `reconcile`, as in `Aggregate`.
final class GroupBy: Operator {
  let input: any Input
  let columns: [String]
  let reducer: Reducer
  /// A sum or average row carries its group's row count.
  private let carriesRows: Bool
  private let schema: SourceSchema
  private var storage: [JSKey: AggState] = [:]
  /// Set by the first fetch: until then nothing downstream has the rows.
  private var hydrated = false
  /// min and max groups whose extreme a push removed, with a row of the
  /// group, in insertion order. Their storage keeps the value last sent,
  /// with the current count.
  private var dirtyGroups: [(key: JSKey, row: Row)] = []
  weak var output: (any Output)?

  /// `ordering` names each group column once; the input must be sorted by it
  /// first.
  init(
    _ input: any Input, ordering: AST.Ordering, tableName: String, _ reducer: Reducer
  )
    throws(PipelineError)
  {
    if ordering.isEmpty { throw PipelineError("GroupBy: needs at least one column") }
    self.input = input
    columns = ordering.map(\.field)
    self.reducer = reducer
    carriesRows = AggregateColumns.carriesComponents(reducer.fn)
    let inputSchema = input.getSchema()
    let inputSort = inputSchema.sort ?? []
    for (i, part) in ordering.enumerated()
    where i >= inputSort.count || inputSort[i].field != part.field
      || inputSort[i].direction != part.direction
    {
      throw PipelineError("GroupBy: the input must be sorted by the group columns first")
    }
    let (columns, primaryKey, sort) = try groupSourceSchema(
      inputSchema.columns, ordering, reducer.aggregate)
    schema = SourceSchema(
      tableName: tableName, columns: columns, primaryKey: primaryKey, system: inputSchema.system,
      compareRows: makeComparator(sort), sort: sort)
    input.setOutput(self)
  }

  func setOutput(_ output: any Output) { self.output = output }
  func getSchema() -> SourceSchema { schema }
  func destroy() { input.destroy() }

  func fetch(_ req: FetchRequest) -> NodeStream {
    NodeStream { [self] in
      // A top-level query without a bound: the view reads every row once.
      if req.constraint != nil || req.start != nil || req.reverse {
        PipelineFailure.report("GroupBy: fetch takes no constraint, start or reverse")
        return AnyIterator { nil }
      }
      hydrated = true
      // The first row of the current run, and its group's state.
      var first: Row?
      var state = AggState()
      var it = input.fetch(FetchRequest()).makeIterator()
      var done = false
      return AnyIterator { [self] in
        while !done {
          guard let node = it.next() else {
            done = true
            guard let first else { return nil }
            return store(groupValues(first), state)
          }
          if let current = first, rowEqualsForCompoundKey(current, node.row, columns) {
            reducer.contribute(&state, node.row)
            continue
          }
          let finished = first.map { store(groupValues($0), state) }
          first = node.row
          state = AggState()
          reducer.contribute(&state, node.row)
          if let finished { return finished }
        }
        return nil
      }
    }
  }

  func push(_ change: Change, pusher: any InputBase) {
    guard hydrated else { return }
    switch change {
    case .add(let node): add(node.row)
    case .remove(let node): remove(node.row)
    case .edit(let node, let oldNode):
      if rowEqualsForCompoundKey(oldNode.row, node.row, columns) {
        update(oldNode.row, node.row)
      } else {
        remove(oldNode.row)
        add(node.row)
      }
    case .child:
      // The input has no relationships the group rows show.
      break
    }
  }

  func reconcile(pusher: any InputBase) {
    if !dirtyGroups.isEmpty {
      let dirty = dirtyGroups
      dirtyGroups.removeAll()
      for (key, groupRow) in dirty {
        // Its last row may have gone in the meantime.
        guard let stored = storage[key] else { continue }
        var state = AggState()
        for node in input.fetch(FetchRequest(constraint: constraint(groupRow)))
        where rowEqualsForCompoundKey(node.row, groupRow, columns) {
          reducer.contribute(&state, node.row)
        }
        emitIfChanged(groupValues(groupRow), stored, state)
      }
    }
    output?.reconcile(pusher: self)
  }

  private func add(_ row: Row) {
    let values = groupValues(row)
    let key = JSKey(groupKey(values))
    guard let stored = storage[key] else {
      var state = AggState()
      reducer.contribute(&state, row)
      output?.push(.add(store(values, state)), pusher: self)
      return
    }
    var state = stored
    if isDirty(key) {
      // Read again in reconcile; only the count is kept up to date.
      state.count += 1
      storage[key] = state
      return
    }
    reducer.contribute(&state, row)
    emitIfChanged(values, stored, state)
  }

  private func remove(_ row: Row) {
    let values = groupValues(row)
    let key = JSKey(groupKey(values))
    guard let stored = storage[key] else {
      PipelineFailure.report("GroupBy: removed a row of a group it lacks")
      return
    }
    if stored.count == 1 {
      storage[key] = nil
      dirtyGroups.removeAll { $0.key == key }
      output?.push(.remove(makeNode(values, stored)), pusher: self)
      return
    }
    var state = stored
    if reducer.fn.isInvertible {
      reducer.applyInvertible(&state, row, -1)
    } else if isDirty(key) || !reducer.pushExtreme(&state, .remove(Node(row: row))) {
      // Keep what was last sent downstream, with the count down by one.
      markDirty(key, row)
      var kept = stored
      kept.count -= 1
      storage[key] = kept
      return
    }
    emitIfChanged(values, stored, state)
  }

  /// An edit within a group.
  private func update(_ oldRow: Row, _ row: Row) {
    let values = groupValues(row)
    let key = JSKey(groupKey(values))
    guard let stored = storage[key], !isDirty(key), !reducer.sameValue(oldRow, row) else {
      return
    }
    if !reducer.fn.isInvertible {
      // The extreme may have moved either way.
      markDirty(key, row)
      return
    }
    var state = stored
    reducer.applyInvertible(&state, oldRow, -1, fieldOnly: true)
    reducer.applyInvertible(&state, row, 1, fieldOnly: true)
    emitIfChanged(values, stored, state)
  }

  /// Stores `state` and sends the move from `stored` downstream, if any.
  private func emitIfChanged(_ values: [JSONValue], _ stored: AggState, _ state: AggState) {
    storage[JSKey(groupKey(values))] = state
    // A row whose field is null moves only a sum or average's row count.
    guard reducer.changed(stored, state) || (carriesRows && stored.count != state.count) else {
      return
    }
    output?.push(.edit(makeNode(values, state), oldNode: makeNode(values, stored)), pusher: self)
  }

  private func isDirty(_ key: JSKey) -> Bool { dirtyGroups.contains { $0.key == key } }

  /// `Map.set`: a group already dirty keeps its place.
  private func markDirty(_ key: JSKey, _ row: Row) {
    if let i = dirtyGroups.firstIndex(where: { $0.key == key }) {
      dirtyGroups[i].row = row
    } else {
      dirtyGroups.append((key, row))
    }
  }

  /// Records `state` for the group and returns its row.
  private func store(_ values: [JSONValue], _ state: AggState) -> Node {
    storage[JSKey(groupKey(values))] = state
    return makeNode(values, state)
  }

  private func makeNode(_ values: [JSONValue], _ state: AggState) -> Node {
    var row = JSONObject()
    for (i, column) in columns.enumerated() { row[column] = values[i] }
    row[AggregateColumns.key] = .string(groupKey(values))
    reducer.writePayload(&row, state)
    if carriesRows { row[AggregateColumns.rows] = .number(Double(state.count)) }
    return Node(row: Row(row))
  }

  /// Reads the rows of `row`'s group. It leaves out the null values, which a
  /// constraint never matches, so it can read rows of other groups too,
  /// which the caller skips.
  private func constraint(_ row: Row) -> Constraint? {
    var constraint: Constraint?
    for column in columns {
      let value = row[column] ?? .null
      if value != .null {
        if constraint == nil { constraint = Constraint() }
        constraint!.set(column, value)
      }
    }
    return constraint
  }

  private func groupValues(_ row: Row) -> [JSONValue] {
    columns.map { row[$0] ?? .null }
  }
}

/// Turns the rows of a `GroupBy`, or of the source a synced client reads them
/// from, into the rows a view shows (`GroupProjection`): the group columns
/// under the query's names and, for a `groupBy()` with an aggregate, its
/// value under `valueName` (`count`, `sum`, …). A `distinct()` shows no
/// value, so an edit, which only moves its count, doesn't reach the view.
final class GroupProjection: Operator {
  let input: any Input
  /// The view's name and the input's name of each group column.
  let columns: [(out: String, from: String)]
  let valueName: String?
  private let schema: SourceSchema
  weak var output: (any Output)?

  init(
    _ input: any Input, columns: [(out: String, from: String)], valueName: String?
  )
    throws(PipelineError)
  {
    // The value would overwrite the group column. The query builder rejects
    // that; a hand-built AST can still name one.
    if let valueName, columns.contains(where: { $0.out == valueName }) {
      throw PipelineError(
        "groupBy(): the column \"\(valueName)\" has the same name as the \(valueName)() value")
    }
    self.input = input
    self.columns = columns
    self.valueName = valueName
    let inputSchema = input.getSchema()
    var viewName: [String: String] = [:]
    for c in columns { viewName[c.from] = c.out }
    let sort: AST.Ordering = (inputSchema.sort ?? []).compactMap { part in
      viewName[part.field].map { AST.OrderPart($0, part.direction) }
    }
    var viewColumns: [String: ColumnSchema] = [:]
    for c in columns { viewColumns[c.out] = inputSchema.columns[c.from] }
    if let valueName { viewColumns[valueName] = inputSchema.columns[AggregateColumns.value] }
    schema = SourceSchema(
      tableName: inputSchema.tableName, columns: viewColumns, primaryKey: columns.map(\.out),
      system: inputSchema.system, compareRows: makeComparator(sort), sort: sort)
    input.setOutput(self)
  }

  func setOutput(_ output: any Output) { self.output = output }
  func getSchema() -> SourceSchema { schema }
  func destroy() { input.destroy() }

  func fetch(_ req: FetchRequest) -> NodeStream {
    input.fetch(req).mapLazy { [self] in project($0) }
  }

  func push(_ change: Change, pusher: any InputBase) {
    switch change {
    case .add(let node): output?.push(.add(project(node)), pusher: self)
    case .remove(let node): output?.push(.remove(project(node)), pusher: self)
    case .edit(let node, let old):
      // A row's key is its group's values, so an edit only moves its value,
      // which a distinct doesn't show.
      guard valueName != nil,
        !ValueCompare.identical(
          node.row[AggregateColumns.value], old.row[AggregateColumns.value])
      else { return }
      output?.push(.edit(project(node), oldNode: project(old)), pusher: self)
    case .child:
      break
    }
  }

  func reconcile(pusher: any InputBase) { output?.reconcile(pusher: self) }

  private func project(_ node: Node) -> Node {
    var row = JSONObject()
    for c in columns {
      if let value = node.row[c.from] { row[c.out] = value }
    }
    if let valueName, let value = node.row[AggregateColumns.value] { row[valueName] = value }
    return Node(row: Row(row))
  }
}
