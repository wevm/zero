/// The aggregate operators' synthetic columns and the `Aggregate` operator
/// (`packages/zql/src/ivm/aggregate.ts`).
package enum AggregateColumns {
  /// The aggregate's result. A view shows it as the bare value.
  package static let value = "_0_value"
  /// The constant key of a top-level aggregate's single row.
  package static let key = "_0_key"
  /// A sum or average's running sum and count of non-null values, which
  /// let the synced client adjust it optimistically.
  package static let sum = "_0_sum"
  package static let count = "_0_count"
  /// A grouped sum or average's number of rows, nulls included.
  package static let rows = "_0_rows"

  /// The payload columns, which aren't part of a row's key.
  package static let payload: Set<String> = [value, sum, count, rows]

  /// `isSyntheticColumn`.
  package static func isSynthetic(_ column: String) -> Bool {
    column == key || payload.contains(column)
  }

  /// `carriesComponents`: whether `fn`'s rows carry the sum and count.
  package static func carriesComponents(_ fn: AST.AggregateFunction) -> Bool {
    fn == .sum || fn == .avg
  }
}

extension AST.AggregateFunction {
  /// `isInvertible`: a change to one row moves the result by a fixed delta.
  package var isInvertible: Bool { self == .count || self == .sum || self == .avg }
}

/// `aggregateValue`: the value `row` contributes to `aggregate`: its field,
/// or the JSON leaf at the path when it is one the function takes (a number
/// for sum and avg, a number or string for min and max), else null.
package func aggregateValue(_ aggregate: AST.Aggregate, _ row: Row) -> JSONValue {
  let v = row[aggregate.field ?? ""] ?? .null
  guard let path = aggregate.path else { return v }
  let leaf = AST.jsonValue(at: path, in: v)
  switch leaf {
  case .number: return leaf
  case .string: return aggregate.fn == .min || aggregate.fn == .max ? leaf : .null
  default: return .null
  }
}

/// `aggregateSourceSchema`: the columns and primary key of the rows an
/// aggregate emits, from its input's columns and its group key. A top-level
/// aggregate has no group key, so its one row is keyed by `_0_key`.
package func aggregateSourceSchema(
  _ inputColumns: [String: ColumnSchema], groupKey: [String], _ aggregate: AST.Aggregate
) throws(PipelineError) -> (columns: [String: ColumnSchema], primaryKey: [String]) {
  var columns: [String: ColumnSchema] = [:]
  for column in groupKey {
    try assertNotSyntheticColumn(column)
    guard let schema = inputColumns[column] else {
      throw PipelineError("Aggregate: group key column \"\(column)\" missing")
    }
    columns[column] = schema
  }
  if groupKey.isEmpty { columns[AggregateColumns.key] = ColumnSchema(.number) }
  switch aggregate.fn {
  case .count:
    columns[AggregateColumns.value] = ColumnSchema(.number)
  case .sum, .avg:
    columns[AggregateColumns.value] = ColumnSchema(.number, optional: true)
  case .min, .max:
    if aggregate.path != nil {
      columns[AggregateColumns.value] = ColumnSchema(.json, optional: true)
    } else {
      guard let field = aggregate.field, var schema = inputColumns[field] else {
        throw PipelineError(
          "Aggregate: \(aggregate.fn.rawValue) field \"\(aggregate.field ?? "")\" missing")
      }
      schema.optional = true
      columns[AggregateColumns.value] = schema
    }
  }
  if AggregateColumns.carriesComponents(aggregate.fn) {
    columns[AggregateColumns.sum] = ColumnSchema(.number)
    columns[AggregateColumns.count] = ColumnSchema(.number)
  }
  return (columns, groupKey.isEmpty ? [AggregateColumns.key] : groupKey)
}

/// `assertNotSyntheticColumn`.
func assertNotSyntheticColumn(_ column: String) throws(PipelineError) {
  if AggregateColumns.isSynthetic(column) {
    throw PipelineError(
      "Aggregate: group key column \"\(column)\" collides with the synthetic aggregate column of the same name"
    )
  }
}

/// A group's running state (`AggState`).
struct AggState: Equatable {
  /// count(*): every row.
  var count = 0
  /// The sum of the non-null values.
  var sum: Double = 0
  /// How many values are non-null.
  var nonNull = 0
  /// The min or max of the non-null values; null when there are none.
  var extreme: JSONValue = .null
}

/// How an aggregate function reduces a group's rows (`Reducer`).
struct Reducer {
  let aggregate: AST.Aggregate

  init(_ aggregate: AST.Aggregate) throws(PipelineError) {
    let fn = aggregate.fn
    if (fn == .count) != (aggregate.field == nil) {
      throw PipelineError(
        "Aggregate: \(fn.rawValue) \(fn == .count ? "takes no" : "requires a") field")
    }
    if aggregate.path != nil && aggregate.field == nil {
      throw PipelineError("Aggregate: a JSON path requires a field")
    }
    self.aggregate = aggregate
  }

  var fn: AST.AggregateFunction { aggregate.fn }

  /// The result for a state.
  func value(_ s: AggState) -> JSONValue {
    switch fn {
    case .count: .number(Double(s.count))
    case .sum: s.nonNull == 0 ? .null : .number(s.sum)
    case .avg: s.nonNull == 0 ? .null : .number(s.sum / Double(s.nonNull))
    case .min, .max: s.extreme
    }
  }

  private func beats(_ v: JSONValue, _ current: JSONValue) -> Bool {
    let c = compareAggregateValues(v, current)
    return fn == .max ? c > 0 : c < 0
  }

  /// The value `row` contributes, null when none.
  func read(_ row: Row) -> JSONValue {
    aggregateValue(aggregate, row)
  }

  /// Whether an edit from `old` to `new` leaves the value reduced as it was.
  func sameValue(_ old: Row, _ new: Row) -> Bool {
    aggregate.field == nil || compareAggregateValues(read(old), read(new)) == 0
  }

  /// Adds a row to a state being accumulated.
  func contribute(_ s: inout AggState, _ row: Row) {
    s.count += 1
    guard aggregate.field != nil else { return }
    let v = read(row)
    if v == .null { return }
    s.nonNull += 1
    switch fn {
    case .sum, .avg:
      guard case .number(let n) = v else {
        PipelineFailure.report("Aggregate: \(fn.rawValue) needs a number")
        return
      }
      s.sum += n
    case .min, .max:
      if s.extreme == .null || beats(v, s.extreme) { s.extreme = v }
    case .count:
      break
    }
  }

  /// The invertible delta of count, sum and avg.
  func applyInvertible(_ s: inout AggState, _ row: Row, _ sign: Int, fieldOnly: Bool = false) {
    if !fieldOnly { s.count += sign }
    guard let field = aggregate.field else { return }
    let v = read(row)
    if v == .null { return }
    guard case .number(let n) = v else {
      PipelineFailure.report("Aggregate: \(fn.rawValue)(\(field)) requires a number")
      return
    }
    s.sum += Double(sign) * n
    s.nonNull += sign
  }

  /// Min and max maintenance. Returns false when the extreme was removed or
  /// edited away: the group must be read again.
  func pushExtreme(_ s: inout AggState, _ change: Change) -> Bool {
    switch change {
    case .add(let node):
      contribute(&s, node.row)
      return true
    case .remove(let node):
      let v = read(node.row)
      s.count -= 1
      if v == .null { return true }
      if s.extreme != .null && compareAggregateValues(v, s.extreme) == 0 { return false }
      s.nonNull -= 1
      return true
    case .edit(let node, let oldNode):
      return sameValue(oldNode.row, node.row)
    case .child:
      return true
    }
  }

  /// Whether a group's row changes from `stored` to `state`: its value, or a
  /// sum or average's components.
  func changed(_ stored: AggState, _ state: AggState) -> Bool {
    !ValueCompare.identical(value(stored), value(state))
      || (AggregateColumns.carriesComponents(fn)
        && (stored.sum != state.sum || stored.nonNull != state.nonNull))
  }

  /// Writes the result, and a sum or average's components, into `row`.
  func writePayload(_ row: inout JSONObject, _ s: AggState) {
    row[AggregateColumns.value] = value(s)
    if AggregateColumns.carriesComponents(fn) {
      row[AggregateColumns.sum] = .number(s.sum)
      row[AggregateColumns.count] = .number(Double(s.nonNull))
    }
  }
}

/// `compareAggregateValues`: numbers before strings, as in JS and SQLite,
/// for the leaves of a JSON path; otherwise `compareValues`. Values JS can't
/// compare, such as the objects of a json column without a path, fail the
/// query as JS's throw does. Equal values compare equal first, as JS's
/// `a === b` does for one row's own value.
func compareAggregateValues(_ a: JSONValue, _ b: JSONValue) -> Int {
  switch (a, b) {
  case (.number, .string): return -1
  case (.string, .number): return 1
  default:
    do {
      return try ValueCompare.compareChecked(a, b)
    } catch {
      if a == b { return 0 }
      PipelineFailure.report(error.description)
      return ValueCompare.compare(a, b)
    }
  }
}

/// `valueToKey`: a string without surrogates is `s` and the string, any
/// other value its JSON.
func valueToKey(_ v: JSONValue) -> String {
  if case .string(let s) = v, !s.utf16.contains(where: { (0xD800...0xDFFF).contains($0) }) {
    return "s" + s
  }
  return JSONSerializer.stringify(v)
}

/// Reduces the rows of a relationship per parent, or of a whole query, to an
/// aggregate (`Aggregate`): one row per group key, `{...groupKey, _0_value}`,
/// so a `Join` routes it as it would the child rows.
///
/// count, sum and avg are kept in O(1) per change. min and max are too,
/// unless the extreme leaves; then the group is read again in `reconcile`.
///
/// JS's `count` fast path, which asks a SQLite table source for the number,
/// has nothing to ask here: `MemorySource` doesn't count either.
final class Aggregate: Operator {
  let input: any Input
  let groupKey: [String]
  let reducer: Reducer
  private let schema: SourceSchema
  private var storage: [JSKey: AggState] = [:]
  /// min and max groups whose extreme a push removed, in insertion order,
  /// to read again in `reconcile`. Their storage keeps the state last sent.
  private var dirtyGroups: [(key: JSKey, values: [JSONValue])] = []
  weak var output: (any Output)?

  init(
    _ input: any Input, groupKey: [String], _ aggregate: AST.Aggregate, tableName: String? = nil
  ) throws(PipelineError) {
    reducer = try Reducer(aggregate)
    self.input = input
    self.groupKey = groupKey
    let inputSchema = input.getSchema()
    let (columns, primaryKey) = try aggregateSourceSchema(
      inputSchema.columns, groupKey: groupKey, aggregate)
    let sort = primaryKey.map { AST.OrderPart($0, .asc) }
    schema = SourceSchema(
      tableName: tableName ?? inputSchema.tableName, columns: columns, primaryKey: primaryKey,
      system: inputSchema.system, compareRows: makeComparator(sort), sort: sort)
    input.setOutput(self)
  }

  func setOutput(_ output: any Output) { self.output = output }
  func getSchema() -> SourceSchema { schema }
  func destroy() { input.destroy() }

  private func compute(_ constraint: Constraint?) -> AggState {
    var s = AggState()
    for node in input.fetch(FetchRequest(constraint: constraint)) {
      reducer.contribute(&s, node.row)
    }
    return s
  }

  func fetch(_ req: FetchRequest) -> NodeStream {
    NodeStream { [self] in
      guard let values = groupValues(req.constraint) else { return AnyIterator { nil } }
      let key = stateKey(values)
      let state: AggState
      if let stored = storage[key] {
        state = stored
      } else {
        state = compute(req.constraint)
        storage[key] = state
      }
      return AnyIterator([makeNode(values, state)].makeIterator())
    }
  }

  func reconcile(pusher: any InputBase) {
    if !dirtyGroups.isEmpty {
      let dirty = dirtyGroups
      dirtyGroups.removeAll()
      for (key, values) in dirty {
        guard let stored = storage[key] else { continue }
        let state = compute(constraint(values))
        storage[key] = state
        emitIfChanged(values, stored, state)
      }
    }
    output?.reconcile(pusher: self)
  }

  func push(_ change: Change, pusher: any InputBase) {
    let values = groupKey.map { change.node.row[$0] ?? .null }
    let key = stateKey(values)
    // Nothing downstream has fetched the group: a later fetch computes it.
    guard let stored = storage[key] else { return }
    var state = stored
    if reducer.fn.isInvertible {
      switch change {
      case .add(let node): reducer.applyInvertible(&state, node.row, 1)
      case .remove(let node): reducer.applyInvertible(&state, node.row, -1)
      case .edit(let node, let oldNode):
        // Subtracting and adding again a value that didn't change would
        // only add rounding error.
        if reducer.sameValue(oldNode.row, node.row) { return }
        reducer.applyInvertible(&state, oldNode.row, -1, fieldOnly: true)
        reducer.applyInvertible(&state, node.row, 1, fieldOnly: true)
      case .child:
        return
      }
    } else if dirtyGroups.contains(where: { $0.key == key })
      || !reducer.pushExtreme(&state, change)
    {
      if !dirtyGroups.contains(where: { $0.key == key }) { dirtyGroups.append((key, values)) }
      return
    }
    if state.count < 0 {
      PipelineFailure.report("Aggregate: removed a row from an empty group")
      return
    }
    storage[key] = state
    emitIfChanged(values, stored, state)
  }

  private func emitIfChanged(_ values: [JSONValue], _ stored: AggState, _ state: AggState) {
    guard reducer.changed(stored, state) else { return }
    output?.push(.edit(makeNode(values, state), oldNode: makeNode(values, stored)), pusher: self)
  }

  private func makeNode(_ values: [JSONValue], _ state: AggState) -> Node {
    var row = JSONObject()
    for (i, column) in groupKey.enumerated() { row[column] = values[i] }
    if groupKey.isEmpty { row[AggregateColumns.key] = 0 }
    reducer.writePayload(&row, state)
    return Node(row: Row(row))
  }

  private func constraint(_ values: [JSONValue]) -> Constraint {
    Constraint(groupKey.enumerated().map { ($0.element, values[$0.offset]) })
  }

  /// The group a fetch is for, or nil when its constraint lacks the group
  /// key, which JS asserts.
  private func groupValues(_ constraint: Constraint?) -> [JSONValue]? {
    if groupKey.isEmpty { return [] }
    guard let constraint else {
      PipelineFailure.report(
        "Aggregate requires a constraint on the group key (relationship aggregate only)")
      return nil
    }
    var values: [JSONValue] = []
    for column in groupKey {
      guard constraint.has(column) else {
        PipelineFailure.report("Aggregate: constraint missing group key column \"\(column)\"")
        return nil
      }
      values.append(constraint[column] ?? .null)
    }
    return values
  }

  /// `stateKey`.
  private func stateKey(_ values: [JSONValue]) -> JSKey {
    JSKey(values.count == 1 ? valueToKey(values[0]) : JSONSerializer.stringify(.array(values)))
  }
}
