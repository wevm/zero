/// The query builder code of an AST after its table, such as
/// `.where('id', '=', 123)` (`astToZQL` in `packages/ast-to-zql/src/ast-to-zql.ts`,
/// without `redactLiterals`). The code is JS's, as the JS inspector shows it.
func astToZQL(_ ast: AST) -> String {
  var code = ""
  if let condition = ast.where {
    var args = Set<String>()
    code += zqlCondition(condition, where: true, &args)
  }
  for related in ast.related ?? [] {
    if related.hidden == true {
      if let nested = related.subquery.related?.first { code += zqlRelated(nested) }
    } else {
      code += zqlRelated(related)
    }
  }
  // distinct and groupBy go before orderBy, which then orders their rows.
  if let groupBy = ast.groupBy {
    let columns = groupBy.map(jsStringLiteral).joined(separator: ", ")
    if let aggregate = ast.aggregate {
      code += ".groupBy(\(columns)).\(aggregate.fn.rawValue)(\(zqlAggregateArg(aggregate)))"
    } else {
      code += ".distinct(\(columns))"
    }
  } else if let distinct = ast.aggregate?.distinct {
    code += ".distinct(\(distinct.map(jsStringLiteral).joined(separator: ", ")))"
  }
  if let orderBy = ast.orderBy, !orderBy.isEmpty {
    for part in orderBy { code += ".orderBy('\(part.field)', '\(part.direction.rawValue)')" }
  }
  if let limit = ast.limit { code += ".limit(\(JSNumber.format(limit)))" }
  if let start = ast.start {
    code +=
      ".start(\(JSONSerializer.stringify(.object(start.row)))"
      + (start.exclusive ? "" : ", { inclusive: true }") + ")"
  }
  // A count of the distinct values ends the chain.
  if ast.aggregate?.distinct != nil { code += ".count()" }
  return code
}

/// Whether a subquery has more than a table: JS tests these properties'
/// truthiness, so an empty ordering counts and a limit of 0 doesn't.
private func hasSubqueryProps(_ ast: AST) -> Bool {
  ast.where != nil || !(ast.related ?? []).isEmpty || ast.orderBy != nil
    || (ast.limit.map { $0 != 0 && !$0.isNaN } ?? false)
}

/// `wrap`: `code`, a builder-callback expression, at the top level of a
/// `.where` (a callback of its own) or inside a callback (its names join the
/// enclosing callback's).
private func zqlWrap(
  _ isWhere: Bool, _ args: inout Set<String>, _ needed: [String], _ code: String
) -> String {
  if isWhere {
    let names = Set(needed).sorted(by: StringCompare.utf16Less).joined(separator: ", ")
    return ".where(({\(names)}) => \(code))"
  }
  args.formUnion(needed)
  return code
}

private func zqlCondition(
  _ condition: AST.Condition, where isWhere: Bool, _ args: inout Set<String>
) -> String {
  let prefix = isWhere ? ".where" : "cmp"
  switch condition {
  case .simple(let op, let left, let right):
    let leftCode = zqlValue(left)
    let rightCode = zqlValue(right)
    let argsCode =
      op == .eq ? "\(leftCode), \(rightCode)" : "\(leftCode), '\(op.rawValue)', \(rightCode)"
    if case .json = left {
      // A JSON path operand only exists through the expression builder's `json()`.
      return zqlWrap(isWhere, &args, ["cmp", "json"], "cmp(\(argsCode))")
    }
    return "\(prefix)(\(argsCode))"
  case .and(let conditions), .or(let conditions):
    let type = condition.isAnd ? "and" : "or"
    if conditions.count == 1 { return zqlCondition(conditions[0], where: isWhere, &args) }
    if condition.isAnd {
      let parts = conditions.map { zqlCondition($0, where: isWhere, &args) }
      if isWhere { return parts.joined() }
      args.insert("and")
      return "and(" + parts.joined(separator: ", ") + ")"
    }
    // An OR gets a callback: of its own at the top level, the enclosing one
    // inside a callback.
    var own = Set<String>()
    let code: String
    if isWhere {
      code = conditions.map { zqlCondition($0, where: false, &own) }.joined(separator: ", ")
    } else {
      code = conditions.map { zqlCondition($0, where: false, &args) }.joined(separator: ", ")
      own = args
    }
    return zqlWrap(isWhere, &args, Array(own) + ["cmp", type], "\(type)(\(code))")
  case .correlatedSubquery(let related, let op, let flip, let scalar):
    let relationship = zqlRelationshipName(related)
    let next = nextExistsSubquery(related)
    let props = hasSubqueryProps(next)
    var options: [String] = []
    if let flip { options.append("flip: \(flip)") }
    if let scalar { options.append("scalar: \(scalar)") }
    let optionsCode = options.isEmpty ? "" : ", {\(options.joined(separator: ", "))}"
    let subquery = props ? ", q => q\(astToZQL(next))" : ""
    switch op {
    case .exists:
      if isWhere { return ".whereExists('\(relationship)'\(subquery)\(optionsCode))" }
      args.insert("exists")
      return "exists('\(relationship)'\(subquery)\(optionsCode))"
    case .notExists:
      return zqlWrap(
        isWhere, &args, ["exists", "not"],
        "not(exists('\(relationship)'\(subquery)\(optionsCode)))")
    }
  }
}

extension AST.Condition {
  fileprivate var isAnd: Bool {
    if case .and = self { return true }
    return false
  }
}

/// `getNextExistsSubquery`: an `exists` over a junction edge has both hops
/// as exists conditions with the same alias; the code names the far one.
private func nextExistsSubquery(_ related: AST.CorrelatedSubquery) -> AST {
  if case .correlatedSubquery(let inner, _, _, _)? = related.subquery.where,
    inner.subquery.alias?.contains(subqueryAliasPrefix + "zhidden_") == true
  {
    return nextExistsSubquery(inner)
  }
  return related.subquery
}

private func zqlRelationshipName(_ related: AST.CorrelatedSubquery) -> String {
  let alias = related.subquery.alias ?? ""
  return alias.hasPrefix(subqueryAliasPrefix)
    ? String(alias.dropFirst(subqueryAliasPrefix.count)) : alias
}

private func zqlRelated(_ related: AST.CorrelatedSubquery) -> String {
  guard let alias = related.subquery.alias else { return "" }
  var code = ".related('\(alias)'"
  if hasSubqueryProps(related.subquery) { code += ", q => q" + astToZQL(related.subquery) }
  return code + ")"
}

private func zqlValue(_ value: AST.ValuePosition) -> String {
  switch value {
  case .literal(let v): return zqlLiteral(v)
  case .column(let name): return "'\(name)'"
  case .json(let column, let path):
    return "json('\(column)', \(zqlPath(path)))"
  case .static(_, let field):
    switch field {
    case .single(let f): return "authParam('\(f)')"
    case .path(let fields):
      return "authParam([\(fields.map { "'\($0)'" }.joined(separator: ", "))])"
    }
  }
}

private func zqlPath(_ path: [AST.JSONPathSegment]) -> String {
  path.map { segment in
    switch segment {
    case .key(let k): jsStringLiteral(k)
    case .index(let i): String(i)
    }
  }.joined(separator: ", ")
}

/// `transformLiteral`: what `String()` prints, with strings quoted.
private func zqlLiteral(_ value: JSONValue) -> String {
  switch value {
  case .null: "null"
  case .array: JSONSerializer.stringify(value)
  case .string(let s): jsStringLiteral(s)
  case .number(let n): JSNumber.format(n)
  case .bool(let b): b ? "true" : "false"
  case .object: "[object Object]"
  }
}

/// `transformAggregateArg`: nothing for a count, a column, or a callback
/// that picks a leaf of a `json()` column.
private func zqlAggregateArg(_ aggregate: AST.Aggregate) -> String {
  guard let field = aggregate.field else { return "" }
  let name = jsStringLiteral(field)
  guard let path = aggregate.path else { return name }
  return "({json}) => json(\(name), \(zqlPath(path)))"
}

/// A single-quoted JS string literal of `s`, escaped as `JSON.stringify`
/// escapes it (`jsStringLiteral`).
private func jsStringLiteral(_ s: String) -> String {
  let quoted = Array(JSONSerializer.stringify(.string(s)).unicodeScalars)
  var out = String.UnicodeScalarView()
  var i = 1
  let end = quoted.count - 1
  while i < end {
    let c = quoted[i]
    if c == "\\", i + 1 < end, quoted[i + 1] == "\"" {
      out.append("\"")
      i += 2
      continue
    }
    if c == "'" {
      out.append("\\")
      out.append("'")
    } else {
      out.append(c)
    }
    i += 1
  }
  return "'" + String(out) + "'"
}
