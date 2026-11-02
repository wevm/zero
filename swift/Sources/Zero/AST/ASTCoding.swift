/// JSON encoding of the AST, following `astSchema`.
extension AST: JSONCodable {
  package init(json: JSONValue) throws(DecodingError) {
    let r = try JSONObjectReader(json, "AST")
    schema = try r.optionalString("schema")
    table = try r.string("table")
    alias = try r.optionalString("alias")
    self.where = try r.optional("where", Condition.init(json:))
    related = try r.optional("related") { (v) throws(DecodingError) in
      try JSONDecode.array(v, CorrelatedSubquery.init(json:))
    }
    limit = try r.optionalNumber("limit")
    orderBy = try r.optional("orderBy") { (v) throws(DecodingError) in
      try JSONDecode.array(v) { (e) throws(DecodingError) -> OrderPart in
        guard case .array(let t) = e, t.count == 2, case .string(let field) = t[0],
          case .string(let dir) = t[1], let direction = Direction(rawValue: dir)
        else { throw DecodingError("Expected [field, 'asc' | 'desc']") }
        return OrderPart(field, direction)
      }
    }
    start = try r.optional("start") { (v) throws(DecodingError) -> Bound in
      let b = try JSONObjectReader(v, "bound")
      return Bound(
        row: try b.required("row", JSONDecode.object), exclusive: try b.bool("exclusive"))
    }
    aggregate = try r.optional("aggregate", Aggregate.init(json:))
    groupBy = try r.optional("groupBy") { (v) throws(DecodingError) in
      let columns = try JSONDecode.array(v, JSONDecode.string)
      if columns.isEmpty { throw DecodingError("expected groupBy to name a column") }
      return columns
    }
  }

  package var json: JSONValue {
    var o = JSONObjectBuilder()
    o.set("schema", schema)
    o.set("table", .string(table))
    o.set("alias", alias)
    o.set("where", self.where?.json)
    o.set("related", related.map { JSONValue.array($0.map(\.json)) })
    o.set(
      "start",
      start.map { JSONValue.object(["row": .object($0.row), "exclusive": .bool($0.exclusive)]) })
    o.set("limit", limit)
    o.set(
      "orderBy",
      orderBy.map {
        JSONValue.array($0.map { .array([.string($0.field), .string($0.direction.rawValue)]) })
      })
    o.set("aggregate", aggregate?.json)
    o.set("groupBy", groupBy.map { JSONValue.array($0.map { .string($0) }) })
    return o.value
  }
}

extension AST.JSONPathSegment {
  var json: JSONValue {
    switch self {
    case .key(let k): .string(k)
    case .index(let i): .number(Double(i))
    }
  }

  /// `jsonPathSchema`: a non-empty list of keys and array indices.
  static func path(json v: JSONValue) throws(DecodingError) -> [AST.JSONPathSegment] {
    let path = try JSONDecode.array(v) { (e) throws(DecodingError) -> AST.JSONPathSegment in
      switch e {
      case .string(let k): return .key(k)
      case .number(let n):
        guard n.rounded() == n, n >= 0, n <= Double(AST.maxJSONPathIndex) else {
          throw DecodingError("expected a non-negative integer array index")
        }
        return .index(Int(n))
      default: throw DecodingError("Expected string or number")
      }
    }
    if path.isEmpty { throw DecodingError("expected a non-empty JSON path") }
    return path
  }
}

extension AST.Aggregate: JSONCodable {
  package init(json: JSONValue) throws(DecodingError) {
    let r = try JSONObjectReader(json, "aggregate")
    let fnName = try r.string("fn")
    guard let fn = AST.AggregateFunction(rawValue: fnName) else {
      throw DecodingError("Invalid aggregate function \"\(fnName)\"", path: ["fn"])
    }
    self.fn = fn
    field = try r.optionalString("field")
    path = try r.optional("path", AST.JSONPathSegment.path(json:))
    distinct = try r.optional("distinct") { (v) throws(DecodingError) in
      try JSONDecode.array(v, JSONDecode.string)
    }
    if path != nil && (field == nil || fn == .count) {
      throw DecodingError("expected an aggregate JSON path to have a field and not be a count")
    }
    if let distinct, fn != .count || field != nil || distinct.isEmpty {
      throw DecodingError(
        "expected an aggregate over distinct columns to be a count of at least one")
    }
  }

  package var json: JSONValue {
    var o = JSONObjectBuilder()
    o.set("fn", .string(fn.rawValue))
    o.set("field", field)
    o.set("path", path.map { JSONValue.array($0.map(\.json)) })
    o.set("distinct", distinct.map { JSONValue.array($0.map { .string($0) }) })
    return o.value
  }
}

extension AST.CorrelatedSubquery: JSONCodable {
  package init(json: JSONValue) throws(DecodingError) {
    let r = try JSONObjectReader(json, "correlated subquery")
    correlation = try r.required("correlation") { (v) throws(DecodingError) in
      let c = try JSONObjectReader(v, "correlation")
      return AST.Correlation(
        parentField: try c.required("parentField", JSONDecode.compoundKey),
        childField: try c.required("childField", JSONDecode.compoundKey))
    }
    subquery = try r.required("subquery", AST.init(json:))
    system = try r.optional("system") { (v) throws(DecodingError) in
      let s = try JSONDecode.string(v)
      guard let system = AST.System(rawValue: s) else {
        throw DecodingError("Invalid system \"\(s)\"")
      }
      return system
    }
    hidden = try r.optionalBool("hidden")
    aggregate = try r.optional("aggregate", AST.Aggregate.init(json:))
  }

  package var json: JSONValue {
    var o = JSONObjectBuilder()
    o.set(
      "correlation",
      .object([
        "parentField": correlation.parentField.json, "childField": correlation.childField.json,
      ]))
    o.set("subquery", subquery.json)
    o.set("system", system?.rawValue)
    o.set("hidden", hidden)
    o.set("aggregate", aggregate?.json)
    return o.value
  }
}

extension AST.ValuePosition: JSONCodable {
  package init(json: JSONValue) throws(DecodingError) {
    let r = try JSONObjectReader(json, "value position")
    let type = try r.string("type")
    switch type {
    case "literal":
      let value = try r.required("value") { (v) throws(DecodingError) -> JSONValue in
        switch v {
        case .string, .number, .bool, .null: return v
        case .array(let a):
          for e in a {
            switch e {
            case .string, .number, .bool: continue
            default: throw DecodingError("Invalid literal array element")
            }
          }
          return v
        case .object: throw DecodingError("Invalid literal value")
        }
      }
      self = .literal(value)
    case "column":
      self = .column(try r.string("name"))
    case "json":
      let column = try r.required("value") { (v) throws(DecodingError) -> String in
        let c = try JSONObjectReader(v, "column reference")
        try c.literal("type", "column")
        return try c.string("name")
      }
      self = .json(column: column, path: try r.required("path", AST.JSONPathSegment.path(json:)))
    case "static":
      let anchorName = try r.string("anchor")
      guard let anchor = AST.Anchor(rawValue: anchorName) else {
        throw DecodingError("Invalid anchor \"\(anchorName)\"", path: ["anchor"])
      }
      let field = try r.required("field") { (v) throws(DecodingError) -> AST.StaticField in
        switch v {
        case .string(let s): return .single(s)
        case .array: return .path(try JSONDecode.array(v, JSONDecode.string))
        default: throw DecodingError("Expected string or string array")
        }
      }
      self = .static(anchor: anchor, field: field)
    default:
      throw DecodingError("Invalid value position type \"\(type)\"", path: ["type"])
    }
  }

  package var json: JSONValue {
    switch self {
    case .literal(let v): ["type": "literal", "value": v]
    case .column(let name): ["type": "column", "name": .string(name)]
    case .json(let column, let path):
      [
        "type": "json", "value": ["type": "column", "name": .string(column)],
        "path": .array(path.map(\.json)),
      ]
    case .static(let anchor, let field):
      ["type": "static", "anchor": .string(anchor.rawValue), "field": field.json]
    }
  }
}

extension AST {
  /// `isHomogeneousList`: every element has the JS type of the first.
  static func isHomogeneous(_ list: [JSONValue]) -> Bool {
    guard let first = list.first else { return true }
    return list.allSatisfy { $0.typeName == first.typeName }
  }
}

extension AST.Condition: JSONCodable {
  package init(json: JSONValue) throws(DecodingError) {
    let r = try JSONObjectReader(json, "condition")
    let type = try r.string("type")
    switch type {
    case "simple":
      let opName = try r.string("op")
      guard let op = AST.SimpleOperator(rawValue: opName) else {
        throw DecodingError("Invalid operator \"\(opName)\"", path: ["op"])
      }
      let left = try r.required("left", AST.ValuePosition.init(json:))
      let right = try r.required("right", AST.ValuePosition.init(json:))
      switch right {
      case .column, .json:
        throw DecodingError("The right side of a condition can't be a column", path: ["right"])
      default: break
      }
      if case .json = left, case .literal(let value) = right {
        // `hasJsonLeafLiteralShape` and `hasHomogeneousJsonInList`.
        let isList = value.arrayValue != nil
        if op == .in || op == .notIn ? !(isList || value == .null) : isList {
          throw DecodingError(
            "expected a list for IN/NOT IN against a JSON path and a scalar for other operators")
        }
        if (op == .in || op == .notIn), let list = value.arrayValue, !AST.isHomogeneous(list) {
          throw DecodingError(
            "expected a list of values of one type for IN/NOT IN against a JSON path")
        }
      }
      self = .simple(op: op, left: left, right: right)
    case "and", "or":
      let conditions = try r.required("conditions") { (v) throws(DecodingError) in
        try JSONDecode.array(v, AST.Condition.init(json:))
      }
      self = type == "and" ? .and(conditions) : .or(conditions)
    case "correlatedSubquery":
      let opName = try r.string("op")
      guard let op = AST.CorrelatedSubqueryOperator(rawValue: opName) else {
        throw DecodingError("Invalid operator \"\(opName)\"", path: ["op"])
      }
      self = .correlatedSubquery(
        related: try r.required("related", AST.CorrelatedSubquery.init(json:)),
        op: op,
        flip: try r.optionalBool("flip"),
        scalar: try r.optionalBool("scalar"))
    default:
      throw DecodingError("Invalid condition type \"\(type)\"", path: ["type"])
    }
  }

  package var json: JSONValue {
    switch self {
    case .simple(let op, let left, let right):
      return ["type": "simple", "op": .string(op.rawValue), "left": left.json, "right": right.json]
    case .and(let c):
      return ["type": "and", "conditions": .array(c.map(\.json))]
    case .or(let c):
      return ["type": "or", "conditions": .array(c.map(\.json))]
    case .correlatedSubquery(let related, let op, let flip, let scalar):
      var o = JSONObjectBuilder()
      o.set("type", "correlatedSubquery")
      o.set("related", related.json)
      o.set("op", .string(op.rawValue))
      o.set("flip", flip)
      o.set("scalar", scalar)
      return o.value
    }
  }
}
