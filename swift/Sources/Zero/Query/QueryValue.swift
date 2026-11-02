/// What a query materializes to: its rows (a ``QueryResult``), or the value
/// of an aggregate, such as the `Int` of `count()`.
public protocol QueryValue: Sendable {
  init(view: ViewValue) throws(ZeroValueError)
  /// The value until there is a view to read: no rows, or what an
  /// aggregate is over no rows (`0` for a count, nil for the others).
  static var emptyQueryValue: Self { get }
}

/// What a query of rows materializes to: `[Row]`, or `Row?` after `one()`.
public protocol QueryResult: QueryValue {
  associatedtype Row: ZeroRow
  /// The result of rows already read, in the view's order.
  init(rows: [Row])
}

/// A row of a query's result array: a row struct, or a row of `groupBy` or
/// `distinct`.
public protocol QueryElement: Sendable {
  init(entry: ViewEntry) throws(ZeroValueError)
}

/// What an `Optional` query value wraps: the row of `one()`, or the value
/// of `sum`, `avg`, `min` or `max`, which is nil over no rows.
public protocol OptionalQueryElement: Sendable {
  static func optionalQueryValue(view: ViewValue) throws(ZeroValueError) -> Self?
}

extension Array: QueryValue where Element: QueryElement {
  @inlinable
  public init(view: ViewValue) throws(ZeroValueError) {
    let entries = view.entries
    var out: [Element] = []
    out.reserveCapacity(entries.count)
    for e in entries { out.append(try Element(entry: e)) }
    self = out
  }

  public static var emptyQueryValue: [Element] { [] }
}

extension Array: QueryResult where Element: ZeroRow {
  public typealias Row = Element

  public init(rows: [Element]) { self = rows }
}

extension Optional: QueryValue where Wrapped: OptionalQueryElement {
  public init(view: ViewValue) throws(ZeroValueError) {
    self = try Wrapped.optionalQueryValue(view: view)
  }

  public static var emptyQueryValue: Wrapped? { nil }
}

extension Optional: QueryResult where Wrapped: ZeroRow {
  public typealias Row = Wrapped

  public init(rows: [Wrapped]) { self = rows.first }
}

extension ZeroRow {
  public static func optionalQueryValue(view: ViewValue) throws(ZeroValueError) -> Self? {
    try view.entries.first.map { (e) throws(ZeroValueError) in try Self(entry: e) }
  }
}

extension ZeroValue {
  /// The value of an aggregate: nil when it's null.
  public static func optionalQueryValue(view: ViewValue) throws(ZeroValueError) -> Self? {
    let value = try aggregateValue(view)
    if case .null = value { return nil }
    return try Self(zeroValue: value)
  }
}

extension Optional: OptionalQueryElement where Wrapped: ZeroValue {}
extension Array: OptionalQueryElement where Element: ZeroValue {}

/// The value of `count()`.
extension Int: QueryValue {
  public init(view: ViewValue) throws(ZeroValueError) {
    self = try Int(zeroValue: aggregateValue(view))
  }

  public static var emptyQueryValue: Int { 0 }
}

func aggregateValue(_ view: ViewValue) throws(ZeroValueError) -> JSONValue {
  guard case .value(let value) = view else {
    throw ZeroValueError("expected the value of an aggregate, got rows")
  }
  return value
}

/// A column `sum` and `avg` can reduce: a number (`NumericSelector`).
public protocol NumericZeroValue: ZeroValue {}
extension Int: NumericZeroValue {}
extension Double: NumericZeroValue {}
extension Optional: NumericZeroValue where Wrapped: NumericZeroValue {}

/// A column `min` and `max` can reduce: a number or a string, enums
/// included (`OrderedSelector`). `Unwrapped` is the type of its non-null
/// values, the aggregate's type.
public protocol OrderedZeroValue: ZeroValue {
  associatedtype Unwrapped: ZeroValue = Self
}
extension Int: OrderedZeroValue {}
extension Double: OrderedZeroValue {}
extension String: OrderedZeroValue {}
extension Optional: OrderedZeroValue where Wrapped: OrderedZeroValue {
  public typealias Unwrapped = Wrapped.Unwrapped
}

/// A row of `groupBy(...)` and an aggregate: the group's columns, read by
/// key path, and the aggregate's `value`.
///
/// ```swift
/// for group in try await zero.run(zql.issue.groupBy(\.ownerId).count()) {
///   print(group[\.ownerId], group.value)
/// }
/// ```
public struct GroupRow<Row: ZeroRow, Value: ZeroValue>: QueryElement, Hashable
where Value: Hashable {
  /// The aggregate of the group's rows.
  public let value: Value
  /// The group's columns by client name.
  public let columnValues: [String: JSONValue]

  public init(entry: ViewEntry) throws(ZeroValueError) {
    // `GroupProjection` puts the value after the group columns. Its name is
    // the function's, which can be another group column's.
    var columns: [String: JSONValue] = [:]
    var last: (key: String, value: JSONValue)?
    for element in entry.row {
      if let last { columns[last.key] = last.value }
      last = (element.key, element.value)
    }
    guard let last else { throw ZeroValueError("a group row has no value") }
    value = try ViewEntry.decode(last.key, last.value)
    columnValues = columns
  }

  /// A column the query groups by. A null or a value of another type reads
  /// as the type's `valueForNull`, as a row's does; reading a column the
  /// query doesn't group by is a programming error.
  public subscript<V: ZeroValue>(column: KeyPath<Row, V>) -> V {
    readGroupColumn(columnValues, Row.self, column)
  }
}

/// A row of `distinct(...)`: the values of its columns, read by key path.
public struct DistinctRow<Row: ZeroRow>: QueryElement, Hashable {
  /// The columns by client name.
  public let columnValues: [String: JSONValue]

  public init(entry: ViewEntry) throws(ZeroValueError) {
    var columns: [String: JSONValue] = [:]
    for (name, v) in entry.row { columns[name] = v }
    columnValues = columns
  }

  /// A column of the `distinct`, read as ``GroupRow``'s are.
  public subscript<V: ZeroValue>(column: KeyPath<Row, V>) -> V {
    readGroupColumn(columnValues, Row.self, column)
  }
}

private func readGroupColumn<Row: ZeroRow, V: ZeroValue>(
  _ columns: [String: JSONValue], _: Row.Type, _ keyPath: KeyPath<Row, V>
) -> V {
  let name = Row.column(keyPath)
  guard let value = columns[name] else {
    preconditionFailure("\(name) is not a column of the group")
  }
  if let v = try? V(zeroValue: value) { return v }
  guard let fallback = V.valueForNull else {
    preconditionFailure("\(name) holds \(value), which isn't a \(V.self)")
  }
  return fallback
}

/// The aggregates a row's query asked for in `related`, by relationship:
/// `related(\.comments) { $0.count() }` gives
/// `issue.aggregates?.count(\.comments)`. Generated row structs hold them
/// in `aggregates`, nil when the query asked for none; the relationship's
/// own property stays nil.
public struct RelationshipAggregates<Row: ZeroRow>: Sendable, Hashable, Codable {
  /// The values by relationship name.
  public let values: [String: JSONValue]

  public init(values: [String: JSONValue]) {
    self.values = values
  }

  /// The aggregates of `entry`'s relationships; nil when it has none.
  public init?(entry: ViewEntry) {
    var values: [String: JSONValue] = [:]
    for (name, value) in entry.relationships {
      if case .value(let v) = value { values[name] = v }
    }
    if values.isEmpty { return nil }
    self.values = values
  }

  /// The `count()` of a relationship; nil when the query didn't ask for it.
  public func count(_ relationship: PartialKeyPath<Row>) -> Int? { value(relationship) }

  /// The `sum` of a relationship; nil when the query didn't ask for it or
  /// it has no rows.
  public func sum(_ relationship: PartialKeyPath<Row>) -> Double? { value(relationship) }

  /// The `avg` of a relationship; nil when the query didn't ask for it or
  /// it has no rows.
  public func avg(_ relationship: PartialKeyPath<Row>) -> Double? { value(relationship) }

  /// The aggregate of a relationship read as `V`, as for `min` and `max`;
  /// nil when the query didn't ask for it, it's null, or it isn't a `V`.
  public func value<V: ZeroValue>(_ relationship: PartialKeyPath<Row>) -> V? {
    guard let value = values[Row.relationship(relationship)], value != .null else { return nil }
    return try? V(zeroValue: value)
  }
}

/// A query grouped by columns, waiting for the aggregate that reduces each
/// group (`GroupedQuery` in `packages/zql/src/query/query.ts`): one
/// ``GroupRow`` per group.
public struct GroupedQuery<Row: ZeroRow>: Sendable {
  public let any: AnyGroupedQuery

  public init(_ any: AnyGroupedQuery) {
    self.any = any
  }

  public func count() -> Query<Row, [GroupRow<Row, Int>]> { Query(any.count()) }

  public func sum<V: NumericZeroValue>(
    _ column: KeyPath<Row, V>
  ) -> Query<Row, [GroupRow<Row, Double?>]> {
    Query(any.sum(Row.column(column)))
  }

  public func sum<V: ZeroValue>(
    json column: KeyPath<Row, V>, _ path: AST.JSONPathSegment...
  ) -> Query<Row, [GroupRow<Row, Double?>]> {
    Query(any.reduce(.sum, Row.column(column), path))
  }

  public func avg<V: NumericZeroValue>(
    _ column: KeyPath<Row, V>
  ) -> Query<Row, [GroupRow<Row, Double?>]> {
    Query(any.avg(Row.column(column)))
  }

  public func avg<V: ZeroValue>(
    json column: KeyPath<Row, V>, _ path: AST.JSONPathSegment...
  ) -> Query<Row, [GroupRow<Row, Double?>]> {
    Query(any.reduce(.avg, Row.column(column), path))
  }

  public func min<V: OrderedZeroValue>(
    _ column: KeyPath<Row, V>
  ) -> Query<Row, [GroupRow<Row, V.Unwrapped?>]> where V.Unwrapped: Hashable {
    Query(any.min(Row.column(column)))
  }

  public func min<V: ZeroValue>(
    json column: KeyPath<Row, V>, _ path: AST.JSONPathSegment...
  ) -> Query<Row, [GroupRow<Row, JSONValue?>]> {
    Query(any.reduce(.min, Row.column(column), path))
  }

  public func max<V: OrderedZeroValue>(
    _ column: KeyPath<Row, V>
  ) -> Query<Row, [GroupRow<Row, V.Unwrapped?>]> where V.Unwrapped: Hashable {
    Query(any.max(Row.column(column)))
  }

  public func max<V: ZeroValue>(
    json column: KeyPath<Row, V>, _ path: AST.JSONPathSegment...
  ) -> Query<Row, [GroupRow<Row, JSONValue?>]> {
    Query(any.reduce(.max, Row.column(column), path))
  }
}

extension Query {
  /// Reduces the query to a count of the rows left after `where`, `start`
  /// and `limit`. On a query, the view holds the count; in `related`, the
  /// relationship does (read it from the row's `aggregates`). A top-level
  /// count is `0` until the server sends it.
  public func count() -> Query<Row, Int> { Query<Row, Int>(any.count()) }

  /// Reduces the query to the sum of a number column; nil over no rows.
  public func sum<V: NumericZeroValue>(_ column: KeyPath<Row, V>) -> Query<Row, Double?> {
    Query<Row, Double?>(any.sum(Row.column(column)))
  }

  /// Reduces the query to the sum of the numbers at `path` in a `json`
  /// column. A missing path, or a value that isn't a number, is skipped.
  public func sum<V: ZeroValue>(
    json column: KeyPath<Row, V>, _ path: AST.JSONPathSegment...
  ) -> Query<Row, Double?> {
    Query<Row, Double?>(any.aggregate(.sum, field: Row.column(column), path: path))
  }

  /// Reduces the query to the average of a number column; nil over no rows.
  public func avg<V: NumericZeroValue>(_ column: KeyPath<Row, V>) -> Query<Row, Double?> {
    Query<Row, Double?>(any.avg(Row.column(column)))
  }

  public func avg<V: ZeroValue>(
    json column: KeyPath<Row, V>, _ path: AST.JSONPathSegment...
  ) -> Query<Row, Double?> {
    Query<Row, Double?>(any.aggregate(.avg, field: Row.column(column), path: path))
  }

  /// Reduces the query to the least value of a number or string column;
  /// nil over no rows.
  public func min<V: OrderedZeroValue>(_ column: KeyPath<Row, V>) -> Query<Row, V.Unwrapped?> {
    Query<Row, V.Unwrapped?>(any.min(Row.column(column)))
  }

  public func min<V: ZeroValue>(
    json column: KeyPath<Row, V>, _ path: AST.JSONPathSegment...
  ) -> Query<Row, JSONValue?> {
    Query<Row, JSONValue?>(any.aggregate(.min, field: Row.column(column), path: path))
  }

  /// Reduces the query to the greatest value of a number or string column;
  /// nil over no rows.
  public func max<V: OrderedZeroValue>(_ column: KeyPath<Row, V>) -> Query<Row, V.Unwrapped?> {
    Query<Row, V.Unwrapped?>(any.max(Row.column(column)))
  }

  public func max<V: ZeroValue>(
    json column: KeyPath<Row, V>, _ path: AST.JSONPathSegment...
  ) -> Query<Row, JSONValue?> {
    Query<Row, JSONValue?>(any.aggregate(.max, field: Row.column(column), path: path))
  }

  /// Reduces the query to the distinct combinations of `columns`, of the
  /// rows `where` keeps: `zql.issue.distinct(\.ownerId)`. `count()` after it
  /// counts the combinations.
  public func distinct(_ columns: PartialKeyPath<Row>...) -> Query<Row, [DistinctRow<Row>]> {
    Query<Row, [DistinctRow<Row>]>(any.distinct(columns.map { Row.column($0) }))
  }

  /// Groups the rows by `columns`, for the aggregate that reduces each
  /// group: `zql.issue.groupBy(\.ownerId).count()`.
  public func groupBy(_ columns: PartialKeyPath<Row>...) -> GroupedQuery<Row> {
    GroupedQuery(any.groupBy(columns.map { Row.column($0) }))
  }
}
