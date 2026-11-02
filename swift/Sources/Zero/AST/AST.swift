/// The ZQL abstract syntax tree (`packages/zero-protocol/src/ast.ts`).
public struct AST: Sendable, Hashable {
  public var schema: String?
  public var table: String
  /// Set on subqueries, to tell apart two subqueries of the same table.
  public var alias: String?
  public var `where`: Condition?
  public var related: [CorrelatedSubquery]?
  public var start: Bound?
  public var limit: Double?
  public var orderBy: Ordering?
  /// Reduces the query to one aggregate over the rows left after `where`,
  /// `start`, and `limit`; with `groupBy`, to one per group.
  public var aggregate: Aggregate?
  /// Groups the rows that pass `where` by these columns: alone a
  /// `SELECT DISTINCT` of them, with `aggregate` that aggregate per group.
  public var groupBy: [String]?

  public init(
    schema: String? = nil,
    table: String,
    alias: String? = nil,
    where: Condition? = nil,
    related: [CorrelatedSubquery]? = nil,
    start: Bound? = nil,
    limit: Double? = nil,
    orderBy: Ordering? = nil,
    aggregate: Aggregate? = nil,
    groupBy: [String]? = nil
  ) {
    self.schema = schema
    self.table = table
    self.alias = alias
    self.where = `where`
    self.related = related
    self.start = start
    self.limit = limit
    self.orderBy = orderBy
    self.aggregate = aggregate
    self.groupBy = groupBy
  }

  /// An aggregate function (`AggregateFunction`).
  public enum AggregateFunction: String, Sendable, Hashable, CaseIterable {
    case count, sum, avg, min, max
  }

  /// One step of a JSON path: an object key or an array index.
  public enum JSONPathSegment: Sendable, Hashable, ExpressibleByStringLiteral,
    ExpressibleByIntegerLiteral
  {
    case key(String)
    case index(Int)

    public init(stringLiteral value: String) { self = .key(value) }
    public init(integerLiteral value: Int) { self = .index(value) }
  }

  /// `MAX_JSON_PATH_INDEX`: the largest array index a path may hold.
  public static let maxJSONPathIndex = Int(Int32.max)

  /// An aggregate of a query or a relationship (`Aggregate`).
  public struct Aggregate: Sendable, Hashable {
    public var fn: AggregateFunction
    /// The column to aggregate; nil for `count`.
    public var field: String?
    /// A path into `field`, then a `json` column, to a scalar leaf.
    public var path: [JSONPathSegment]?
    /// For `count`: counts the distinct combinations of these columns.
    public var distinct: [String]?

    public init(
      _ fn: AggregateFunction, field: String? = nil, path: [JSONPathSegment]? = nil,
      distinct: [String]? = nil
    ) {
      self.fn = fn
      self.field = field
      self.path = path
      self.distinct = distinct
    }
  }

  public enum System: String, Sendable, Hashable {
    case permissions, client, test
  }

  public struct Bound: Sendable, Hashable {
    /// The cursor's columns: those the query orders by.
    public var row: JSONObject
    public var exclusive: Bool

    public init(row: JSONObject, exclusive: Bool) {
      self.row = row
      self.exclusive = exclusive
    }
  }

  public enum Direction: String, Codable, Sendable, Hashable {
    case asc, desc
  }

  public struct OrderPart: Sendable, Hashable {
    public var field: String
    public var direction: Direction

    public init(_ field: String, _ direction: Direction) {
      self.field = field
      self.direction = direction
    }
  }

  public typealias Ordering = [OrderPart]

  public enum SimpleOperator: String, Sendable, Hashable, CaseIterable {
    case eq = "="
    case ne = "!="
    case `is` = "IS"
    case isNot = "IS NOT"
    case lt = "<"
    case gt = ">"
    case le = "<="
    case ge = ">="
    case like = "LIKE"
    case notLike = "NOT LIKE"
    case ilike = "ILIKE"
    case notIlike = "NOT ILIKE"
    case `in` = "IN"
    case notIn = "NOT IN"
  }

  public enum CorrelatedSubqueryOperator: String, Sendable, Hashable {
    case exists = "EXISTS"
    case notExists = "NOT EXISTS"
  }

  public enum Anchor: String, Sendable, Hashable {
    case authData
    case preMutationRow
  }

  public enum StaticField: Sendable, Hashable {
    case single(String)
    case path([String])
  }

  /// One side of a simple condition. A literal is a string, number, boolean,
  /// null, or an array of strings, numbers, and booleans.
  public enum ValuePosition: Sendable, Hashable {
    case literal(JSONValue)
    case column(String)
    /// A value inside the `json` column `column`, at `path`.
    case json(column: String, path: [JSONPathSegment])
    case `static`(anchor: Anchor, field: StaticField)
  }

  public indirect enum Condition: Sendable, Hashable {
    case simple(op: SimpleOperator, left: ValuePosition, right: ValuePosition)
    case and([Condition])
    case or([Condition])
    case correlatedSubquery(
      related: CorrelatedSubquery, op: CorrelatedSubqueryOperator, flip: Bool?, scalar: Bool?)
  }

  public struct Correlation: Sendable, Hashable {
    public var parentField: [String]
    public var childField: [String]

    public init(parentField: [String], childField: [String]) {
      self.parentField = parentField
      self.childField = childField
    }
  }

  public struct CorrelatedSubquery: Sendable, Hashable {
    public var correlation: Correlation
    public var subquery: AST
    public var system: System?
    /// Hidden hops (junction tables) are left out of the output view; their
    /// children are not.
    public var hidden: Bool?
    /// Reduces the subquery to one aggregate per parent row.
    public var aggregate: Aggregate?

    public init(
      correlation: Correlation, subquery: AST, system: System? = nil, hidden: Bool? = nil,
      aggregate: Aggregate? = nil
    ) {
      self.correlation = correlation
      self.subquery = subquery
      self.system = system
      self.hidden = hidden
      self.aggregate = aggregate
    }
  }
}

extension AST.Condition {
  public var conditions: [AST.Condition]? {
    switch self {
    case .and(let c), .or(let c): c
    default: nil
    }
  }
}

extension AST {
  /// `jsonValueAtPath`: the value at `path` in `value`, strictly: a key reads
  /// an object's own key and an index an array's element. Any other step,
  /// and a missing value, is null.
  package static func jsonValue(at path: [JSONPathSegment], in value: JSONValue?) -> JSONValue {
    var v = value ?? .null
    for segment in path {
      switch (v, segment) {
      case (.object(let o), .key(let k)):
        guard let next = o[k] else { return .null }
        v = next
      case (.array(let a), .index(let i)):
        guard i < a.count else { return .null }
        v = a[i]
      default:
        return .null
      }
    }
    return v
  }

  /// `formatJsonPathReference`: `metadata.tags[0]`.
  static func formatJSONPath(_ column: String, _ path: [JSONPathSegment]) -> String {
    column
      + path.map {
        switch $0 {
        case .key(let k): ".\(k)"
        case .index(let i): "[\(i)]"
        }
      }.joined()
  }
}
