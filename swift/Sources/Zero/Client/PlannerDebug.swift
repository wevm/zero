#if canImport(Darwin)
  import Darwin
#elseif canImport(Glibc)
  import Glibc
#elseif canImport(Musl)
  import Musl
#endif

/// The planner's debug events (`AnalyzeQueryResult.joinPlans`) as text, one
/// summary per planning attempt and the plan chosen (`formatPlannerEvents`
/// in `packages/zql/src/planner/planner-debug.ts`).
func formatPlanDebugEvents(_ events: [JSONValue]) -> String {
  var lines: [String] = []
  var attempts: [Double] = []
  var byAttempt: [Double: [JSONValue]] = [:]
  var best: JSONValue?
  for event in events {
    if event.objectValue?.index(forKey: "attemptNumber") != nil {
      if let attempt = event["attemptNumber"]?.numberValue {
        if byAttempt[attempt] == nil { attempts.append(attempt) }
        byAttempt[attempt, default: []].append(event)
      }
    } else if event["type"] == "best-plan-selected" {
      best = event
    }
  }
  for attempt in attempts {
    lines += attemptSummary(attempt, byAttempt[attempt]!)
    lines.append("")
  }
  if let best {
    let rule = String(repeating: "─", count: 60)
    lines.append(rule)
    lines.append(
      "✓ Best plan: Attempt \(JSNumber.format(number(best, "bestAttemptNumber") + 1)) "
        + "(cost=\(toFixed(number(best, "totalCost"), 2)))")
    let joins = best["joinStates"]?.arrayValue ?? []
    if !joins.isEmpty {
      lines.append("  Join types:")
      for j in joins {
        lines.append("    \(j["join"]?.stringValue ?? ""): \(j["type"]?.stringValue ?? "")")
      }
    }
    lines.append(rule)
  }
  return lines.joined(separator: "\n")
}

private func number(_ v: JSONValue?, _ key: String) -> Double {
  v?[key]?.numberValue ?? .nan
}

/// `formatAttemptSummary`.
private func attemptSummary(_ attempt: Double, _ events: [JSONValue]) -> [String] {
  var lines: [String] = []
  let start = events.first { $0["type"] == "attempt-start" }
  let total = start?["totalAttempts"]?.numberValue
  var bits = 1.0
  if let total {
    let b = log2(total).rounded(.up)
    bits = b == 0 || b.isNaN ? 1 : b
  }
  var pattern = binary(attempt)
  if bits.isFinite, Double(pattern.count) < bits {
    pattern = String(repeating: "0", count: Int(bits) - pattern.count) + pattern
  }
  lines.append(
    "[Attempt \(JSNumber.format(attempt + 1))/\(total.map(JSNumber.format) ?? "?")] "
      + "Pattern \(JSNumber.format(attempt)) (\(pattern))")

  func nodeCosts(_ nodeType: String) -> [JSONValue] {
    events.filter { $0["type"] == "node-cost" && $0["nodeType"]?.stringValue == nodeType }
  }
  func costLines(_ cost: JSONValue) -> [String] {
    let estimate = cost["costEstimate"]
    let limit = estimate?["limit"]?.numberValue.map(JSNumber.format) ?? "none"
    return [
      "      cost=\(toFixed(number(estimate, "cost"), 2)), "
        + "startup=\(toFixed(number(estimate, "startupCost"), 2)), "
        + "scan=\(toFixed(number(estimate, "scanEst"), 2))",
      "      rows=\(toFixed(number(estimate, "returnedRows"), 2)), "
        + "selectivity=\(toFixed(number(estimate, "selectivity"), 8)), limit=\(limit)",
      "      downstreamChildSelectivity=\(toFixed(number(cost, "downstreamChildSelectivity"), 8))",
    ]
  }

  let connections = nodeCosts("connection")
  let constraints = events.filter {
    $0["type"] == "node-constraint" && $0["nodeType"]?.stringValue == "connection"
  }
  if !connections.isEmpty {
    lines.append("  Connections:")
    for cost in connections {
      let pattern = branchPattern(cost)
      let constraint = constraints.first {
        $0["node"] == cost["node"] && branchPattern($0) == pattern
      }?["constraint"]
      lines.append("    \(cost["node"]?.stringValue ?? ""):")
      lines += costLines(cost)
      lines.append("      constraints=\(formatConstraint(constraint))")
      lines.append("      filters=\(cost["filters"].map(formatFilter) ?? "none")")
      lines.append("      ordering=\(formatOrdering(cost["ordering"]))")
    }
  }
  let joins = nodeCosts("join")
  if !joins.isEmpty {
    lines.append("  Joins:")
    for cost in joins {
      let joinType = cost["joinType"]?.stringValue ?? ""
      lines.append(
        "    \(cost["node"]?.stringValue ?? "")\(joinType.isEmpty ? "" : " (\(joinType))"):")
      lines += costLines(cost)
    }
  }
  if let complete = events.first(where: { $0["type"] == "plan-complete" }) {
    lines.append("  ✓ Plan complete: total cost = \(toFixed(number(complete, "totalCost"), 2))")
  } else if let failed = events.first(where: { $0["type"] == "plan-failed" }) {
    lines.append("  ✗ Plan failed: \(failed["reason"]?.stringValue ?? "")")
  }
  return lines
}

/// `branchPattern.join(',')`.
private func branchPattern(_ event: JSONValue) -> String {
  (event["branchPattern"]?.arrayValue ?? []).map { $0.numberValue.map(JSNumber.format) ?? "" }
    .joined(separator: ",")
}

/// `attemptNum.toString(2)`, for the integers attempts are numbered with.
private func binary(_ n: Double) -> String {
  guard n == n.rounded(), abs(n) < 9_007_199_254_740_992 else { return JSNumber.format(n) }
  let digits = String(Int64(abs(n)), radix: 2)
  return n < 0 ? "-" + digits : digits
}

private func formatConstraint(_ constraint: JSONValue?) -> String {
  guard let keys = constraint?.objectValue?.map(\.key), !keys.isEmpty else { return "{}" }
  return "{" + keys.joined(separator: ", ") + "}"
}

private func formatValuePosition(_ value: JSONValue) -> String {
  switch value["type"]?.stringValue {
  case "column":
    return value["name"]?.stringValue ?? ""
  case "json":
    // `formatJsonPathReference`.
    let path = (value["path"]?.arrayValue ?? []).map { s in
      s.numberValue.map { "[\(JSNumber.format($0))]" } ?? ".\(s.stringValue ?? "")"
    }
    return (value["value"]?["name"]?.stringValue ?? "") + path.joined()
  case "literal":
    let literal = value["value"] ?? .null
    if let s = literal.stringValue { return "'\(s)'" }
    return JSONSerializer.stringify(literal)
  case "static":
    let field = value["field"]
    let name =
      field?.arrayValue.map { $0.map { $0.stringValue ?? "" }.joined(separator: ".") }
      ?? field?.stringValue ?? ""
    return "@\(value["anchor"]?.stringValue ?? "").\(name)"
  default:
    return JSONSerializer.stringify(value)
  }
}

private func formatFilter(_ filter: JSONValue) -> String {
  switch filter["type"]?.stringValue {
  case "simple":
    return "\(formatValuePosition(filter["left"] ?? .null)) \(filter["op"]?.stringValue ?? "") "
      + formatValuePosition(filter["right"] ?? .null)
  case "and", "or":
    let joiner = filter["type"] == "and" ? " AND " : " OR "
    return "("
      + (filter["conditions"]?.arrayValue ?? []).map(formatFilter).joined(
        separator: joiner) + ")"
  case "correlatedSubquery":
    return "EXISTS(\(filter["related"]?["subquery"]?["table"]?.stringValue ?? ""))"
  default:
    return JSONSerializer.stringify(filter)
  }
}

private func formatOrdering(_ ordering: JSONValue?) -> String {
  guard let parts = ordering?.arrayValue, !parts.isEmpty else { return "none" }
  return parts.map { "\($0[0]?.stringValue ?? "") \($0[1]?.stringValue ?? "")" }.joined(
    separator: ", ")
}

/// `Number.prototype.toFixed(digits)`: the value rounded to `digits`
/// decimals, a tie away from zero, from its exact binary value.
func toFixed(_ x: Double, _ digits: Int) -> String {
  if x.isNaN { return "NaN" }
  if abs(x) >= 1e21 { return JSNumber.format(x) }
  let negative = x < 0
  let v = abs(x)
  // v = significand * 2^shift, exactly.
  var significand = UInt128(v.significandBitPattern)
  if v.isNormal { significand |= 1 << 52 }
  let shift = v.isNormal ? Int(v.exponent) - 52 : -1074
  var scale: UInt128 = 1
  for _ in 0..<digits { scale *= 10 }
  let n: UInt128
  if v == 0 {
    n = 0
  } else if shift >= 0 {
    n = (significand << UInt128(shift)) * scale
  } else if -shift > 100 {
    // Below 2^53 * 10^8 / 2^100: rounds to 0.
    n = 0
  } else {
    let down = UInt128(-shift)
    let scaled = significand * scale
    n = (scaled + (1 << (down - 1))) >> down
  }
  var text = String(n)
  if digits > 0 {
    if text.count <= digits {
      text = String(repeating: "0", count: digits + 1 - text.count) + text
    }
    text.insert(".", at: text.index(text.endIndex, offsetBy: -digits))
  }
  return negative ? "-" + text : text
}
