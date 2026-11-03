/// Storage key layout (`packages/zero-client/src/client/keys.ts`).
package enum Keys {
  package static let desiredQueriesPrefix = "d/"
  package static let gotQueriesPrefix = "g/"
  package static let entitiesPrefix = "e/"
  /// Synced aggregate rows live outside `e/`, which a client from before
  /// them loads into its schema's tables: both can share a database.
  package static let aggregatesPrefix = "a/"
  /// The row-version column the server stamps on synced rows; not part of
  /// an aggregate row's key.
  package static let rowVersionColumn = "_0_version"
  package static let mutationsPrefix = "m/"

  package struct InvalidPrimaryKeyValue: Error, CustomStringConvertible {
    package let column: String
    package let value: JSONValue?
    /// valita's message for `primaryKeyValueSchema`, which JS throws as
    /// a `TypeError`.
    package var description: String {
      "Expected string, number or boolean. Got \(JSONDecode.valitaDisplay(value))"
    }
  }

  /// `toPrimaryKeyString`: `e/<table>/<id>`. A single-column key is the value
  /// as JS `String()` prints it; a compound key is the decimal `h128` of the
  /// JSON array of its values.
  package static func primaryKeyString(
    table: String, primaryKey: [String], row: Row
  ) throws(InvalidPrimaryKeyValue) -> String {
    try primaryKeyString(table: table, primaryKey: primaryKey) { row[$0] }
  }

  package static func primaryKeyString(
    table: String, primaryKey: [String], row: RowRecord
  ) throws(InvalidPrimaryKeyValue) -> String {
    try primaryKeyString(table: table, primaryKey: primaryKey) { row[$0] }
  }

  /// `aggregateRowKey`: `a/`, the table name without its `aggregate:`, and
  /// the row's key, from `aggregateRowKeyColumns`. A `put`'s full row, a
  /// `del`'s key and an optimistic delta all give the same key.
  package static func aggregateRowKey(
    table: String, row: RowRecord
  ) throws(InvalidPrimaryKeyValue) -> String {
    try primaryKeyString(
      prefix: aggregatesPrefix, table: String(table.dropFirst(aggregateTablePrefix.count)),
      primaryKey: aggregateRowKeyColumns(table: table, columns: row.keys)
    ) { row[$0] }
  }

  /// `aggregateRowKeyColumns`: `_0_key` for a top-level aggregate's or a
  /// grouped query's row, and otherwise every column but the payload and
  /// the row version, sorted. The table name decides, not the row.
  package static func aggregateRowKeyColumns(
    table: String, columns: some Sequence<String>
  ) -> [String] {
    if isKeyedAggregateTableName(table) { return [AggregateColumns.key] }
    return columns.filter { !AggregateColumns.payload.contains($0) && $0 != rowVersionColumn }
      .sorted(by: StringCompare.utf16Less)
  }

  private static func primaryKeyString(
    prefix: String = entitiesPrefix, table: String, primaryKey: [String],
    value: (String) -> JSONValue?
  ) throws(InvalidPrimaryKeyValue) -> String {
    func scalar(_ column: String) throws(InvalidPrimaryKeyValue) -> JSONValue {
      let v = value(column)
      switch v {
      case .string?, .number?, .bool?: return v!
      default: throw InvalidPrimaryKeyValue(column: column, value: v)
      }
    }
    if primaryKey.count == 1 {
      let v = try scalar(primaryKey[0])
      return prefix + table + "/" + jsString(v)
    }
    var values: [JSONValue] = []
    for column in primaryKey { values.append(try scalar(column)) }
    let hash = XXHash32.h128(JSONSerializer.stringify(.array(values)))
    return prefix + table + "/" + String(hash)
  }

  /// JS `String(v)` for a scalar.
  static func jsString(_ v: JSONValue) -> String {
    switch v {
    case .string(let s): s
    case .number(let n): JSNumber.format(n)
    case .bool(let b): b ? "true" : "false"
    case .null: "null"
    case .array, .object: JSONSerializer.stringify(v)
    }
  }

  /// `sourceNameFromKey`: the source of an `e/` or `a/` row key. An `a/`
  /// key leaves out the `aggregate:` of the table's name.
  package static func sourceName(_ key: String) -> String {
    // Both prefixes are two characters long.
    let rest = key.utf8.dropFirst(entitiesPrefix.utf8.count)
    let end = rest.firstIndex(of: UInt8(ascii: "/")) ?? rest.endIndex
    let name = String(rest[rest.startIndex..<end])!
    return key.utf8.starts(with: aggregatesPrefix.utf8) ? aggregateTablePrefix + name : name
  }

  package static func desiredQueriesKey(clientID: String, hash: String) -> String {
    desiredQueriesPrefix + clientID + "/" + hash
  }

  package static func gotQueriesKey(hash: String) -> String {
    gotQueriesPrefix + hash
  }

  package static func mutationResponseKey(clientID: String, id: Double) -> String {
    mutationsPrefix + clientID + "/" + JSNumber.format(id)
  }
}
