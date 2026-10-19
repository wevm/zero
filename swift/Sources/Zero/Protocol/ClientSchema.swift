/// The schema a client was built with, sent so zero-cache can check it
/// against the replica (`clientSchemaSchema` in
/// `packages/zero-protocol/src/client-schema.ts`).
public struct ClientSchema: Sendable, Hashable, JSONCodable {
  /// `valueTypeSchema`.
  public enum ValueType: String, Sendable, Hashable, CaseIterable {
    case string, number, boolean, null, json
  }

  /// `tableSchemaSchema`. Each column maps to its `{type}`.
  public struct Table: Sendable, Hashable {
    public var columns: [String: ValueType]
    public var primaryKey: [String]

    public init(columns: [String: ValueType], primaryKey: [String]) {
      self.columns = columns
      self.primaryKey = primaryKey
    }
  }

  public var tables: [String: Table]

  public init(tables: [String: Table]) {
    self.tables = tables
  }

  public init(json: JSONValue) throws(DecodingError) {
    let r = try JSONObjectReader(json, "client schema")
    tables = try r.required("tables") { (v) throws(DecodingError) in
      try JSONDecode.record(v) { (t) throws(DecodingError) -> Table in
        let table = try JSONObjectReader(t, "table schema")
        return Table(
          columns: try table.required("columns") { (v) throws(DecodingError) in
            try JSONDecode.record(v) { (c) throws(DecodingError) in
              try JSONObjectReader(c, "column schema").literalUnion("type", ValueType.self)
            }
          },
          primaryKey: try table.required("primaryKey", JSONDecode.strings))
      }
    }
  }

  public var json: JSONValue {
    .object([
      "tables": .object(
        tables.mapValues { t in
          [
            "columns": .object(t.columns.mapValues { ["type": .string($0.rawValue)] }),
            "primaryKey": t.primaryKey.json,
          ]
        })
    ])
  }

  /// The hash of the normalized schema that names a client's local
  /// database (`clientSchemaFrom`): a schema change starts a new one.
  public var hash: String {
    String(XXHash32.h64(JSONSerializer.stringify(normalized().json)), radix: 36)
  }

  /// `normalizeClientSchema`: sorts each primary key, as JS `sort()` does,
  /// by UTF-16 code units. JS also orders the table and column entries by
  /// `<`; ``json`` has no order of its own and ``JSONSerializer`` writes keys
  /// in that same order.
  public func normalized() -> ClientSchema {
    ClientSchema(
      tables: tables.mapValues { t in
        Table(columns: t.columns, primaryKey: t.primaryKey.sorted(by: StringCompare.utf16Less))
      })
  }
}
