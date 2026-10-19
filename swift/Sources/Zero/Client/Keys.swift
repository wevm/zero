/// Storage key layout (`packages/zero-client/src/client/keys.ts`).
public enum Keys {
  public static let desiredQueriesPrefix = "d/"
  public static let gotQueriesPrefix = "g/"
  public static let entitiesPrefix = "e/"
  public static let mutationsPrefix = "m/"

  public struct InvalidPrimaryKeyValue: Error, CustomStringConvertible {
    public let column: String
    public let value: JSONValue?
    public var description: String {
      "Invalid primary key value for \(column): \(value.map(JSONSerializer.stringify) ?? "undefined")"
    }
  }

  /// `toPrimaryKeyString`: `e/<table>/<id>`. A single-column key is the value
  /// as JS `String()` prints it; a compound key is the decimal `h128` of the
  /// JSON array of its values.
  public static func primaryKeyString(
    table: String, primaryKey: [String], row: Row
  ) throws(InvalidPrimaryKeyValue) -> String {
    func scalar(_ column: String) throws(InvalidPrimaryKeyValue) -> JSONValue {
      let v = row[column]
      switch v {
      case .string?, .number?, .bool?: return v!
      default: throw InvalidPrimaryKeyValue(column: column, value: v)
      }
    }
    if primaryKey.count == 1 {
      let v = try scalar(primaryKey[0])
      return entitiesPrefix + table + "/" + jsString(v)
    }
    var values: [JSONValue] = []
    for column in primaryKey { values.append(try scalar(column)) }
    let hash = XXHash32.h128(JSONSerializer.stringify(.array(values)))
    return entitiesPrefix + table + "/" + String(hash)
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

  public static func desiredQueriesKey(clientID: String, hash: String) -> String {
    desiredQueriesPrefix + clientID + "/" + hash
  }

  public static func gotQueriesKey(hash: String) -> String {
    gotQueriesPrefix + hash
  }

  public static func mutationResponseKey(clientID: String, id: Double) -> String {
    mutationsPrefix + clientID + "/" + JSNumber.format(id)
  }
}
