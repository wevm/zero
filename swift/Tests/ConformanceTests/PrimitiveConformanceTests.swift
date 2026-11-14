import Testing

@testable import Zero

@Suite(.enabled(if: Vectors.available))
struct PrimitiveConformanceTests {
  struct StringCompareCase: Decodable {
    let a: String
    let b: String
    let utf8: Int
    let utf16: Int
  }

  @Test func stringCompare() throws {
    for c in try Vectors.load("string-compare", as: StringCompareCase.self) {
      #expect(
        StringCompare.utf8(c.a, c.b).signum() == c.utf8,
        "\(c.a.debugDescription) vs \(c.b.debugDescription)")
      #expect(
        StringCompare.utf16(c.a, c.b).signum() == c.utf16,
        "\(c.a.debugDescription) vs \(c.b.debugDescription)")
    }
  }

  @Test func compareValues() throws {
    for c in try Vectors.cases("compare-values") {
      let a = c["a"]!
      let b = c["b"]!
      let expected = c["compare"]!
      let got = Result { () throws(ValueCompare.IncomparableError) in
        try ValueCompare.compareChecked(a, b)
      }
      switch (got, expected) {
      case (.success(let n), .number(let want)):
        #expect(Double(n.signum()) == want, "compare(\(a), \(b))")
      case (.failure, .string("throws")):
        break
      default:
        Issue.record("compare(\(a), \(b)) = \(got), want \(expected)")
      }
      #expect(ValueCompare.equal(a, b) == c["equal"]!.boolValue!, "equal(\(a), \(b))")
    }
  }

  struct HashCase: Decodable {
    let input: String
    let h32: UInt32
    let h64: String
    let h128: String
  }

  @Test func hash() throws {
    for c in try Vectors.load("hash", as: HashCase.self) {
      #expect(XXHash32.h32(c.input) == c.h32, "h32(\(c.input.debugDescription))")
      #expect(String(XXHash32.h64(c.input)) == c.h64, "h64(\(c.input.debugDescription))")
      #expect(String(XXHash32.h128(c.input)) == c.h128, "h128(\(c.input.debugDescription))")
    }
  }

  @Test func primaryKeyString() throws {
    for c in try Vectors.cases("primary-key-string") {
      let table = c["table"]!.stringValue!
      let pk = c["primaryKey"]!.arrayValue!.map { $0.stringValue! }
      let row = c["row"]!.objectValue!
      #expect(
        try Keys.primaryKeyString(table: table, primaryKey: pk, row: row) == c["key"]!.stringValue!,
        "\(table) \(pk) \(row)")
    }
  }

  @Test func aggregateRowKey() throws {
    for c in try Vectors.cases("aggregate-row-key") {
      let table = c["table"]!.stringValue!
      let row = c["row"]!.objectValue!
      #expect(
        Keys.aggregateRowKeyColumns(table: table, columns: row.keys)
          == c["columns"]!.arrayValue!.map { $0.stringValue! }, "\(table) \(row)")
      let key = try? Keys.aggregateRowKey(table: table, row: row)
      #expect(key == c["key"]?.stringValue, "\(table) \(row)")
      #expect((key == nil) == (c["throws"]?.boolValue ?? false), "\(table) \(row)")
    }
  }

  @Test func like() throws {
    for c in try Vectors.cases("like") {
      let pattern = c["pattern"]!.stringValue!
      let ci = c["flags"]!.stringValue! == "i"
      let compiled = Result { () throws(LikePattern.InvalidPattern) in
        try LikePattern(pattern, caseInsensitive: ci)
      }
      guard case .success(let like) = compiled else {
        #expect(c["throws"]!.boolValue!, "pattern \(pattern.debugDescription) should compile")
        continue
      }
      #expect(!c["throws"]!.boolValue!, "pattern \(pattern.debugDescription) should throw")
      for r in c["results"]!.arrayValue! {
        let input = r["input"]!.stringValue!
        #expect(
          like.matches(input) == r["match"]!.boolValue!,
          "\(input.debugDescription) \(ci ? "ILIKE" : "LIKE") \(pattern.debugDescription)")
      }
    }
  }

  @Test func ttl() throws {
    let cases = try Vectors.cases("ttl")
    let ttls = try cases.map { try TTL(json: $0["ttl"]!) }
    for (c, ttl) in zip(cases, ttls) {
      #expect(ttl.parsed == c["parsed"]!.numberValue!, "parseTTL(\(ttl))")
      #expect(ttl.normalized.json == c["normalized"]!, "normalizeTTL(\(ttl))")
      #expect(ttl.clamped == c["clamped"]!.numberValue!, "clampTTL(\(ttl))")
      let signs = c["compare"]!.arrayValue!.map { $0.numberValue! }
      for (other, want) in zip(ttls, signs) {
        let got = TTL.compare(ttl, other)
        #expect((got > 0 ? 1.0 : got < 0 ? -1.0 : 0.0) == want, "compareTTL(\(ttl), \(other))")
      }
    }
  }
  /// A vector number: NaN and the infinities are strings.
  static func number(_ v: JSONValue) -> Double {
    switch v {
    case .string("NaN"): .nan
    case .string("Infinity"): .infinity
    case .string("-Infinity"): -.infinity
    default: v.numberValue ?? .nan
    }
  }

  static func encode(_ n: Double) -> JSONValue {
    n.isNaN ? "NaN" : n.isInfinite ? (n > 0 ? "Infinity" : "-Infinity") : .number(n)
  }

  /// `shared/src/tdigest.ts`: each case runs its ops on its digests, and
  /// each op's result must be JS's, number for number (JSON drops the sign
  /// of zero, so only that may differ).
  @Test func tdigest() throws {
    var passed = 0
    for c in try Vectors.cases("tdigest") {
      let name = c["name"]?.stringValue ?? "?"
      var digests = (c["compressions"]?.arrayValue ?? []).map {
        TDigest(compression: $0.numberValue ?? 100)
      }
      let expect = c["expect"]?.arrayValue ?? []
      var ok = true
      for (i, op) in (c["ops"]?.arrayValue ?? []).enumerated() {
        let d = Int(op["d"]?.numberValue ?? 0)
        let got: JSONValue
        switch op["op"]?.stringValue {
        case "add":
          for pair in op["values"]?.arrayValue ?? [] {
            digests[d].add(Self.number(pair[0] ?? .null), weight: Self.number(pair[1] ?? .null))
          }
          got = .null
        case "merge":
          digests[d].merge(digests[Int(op["from"]?.numberValue ?? 0)])
          got = .null
        case "quantile":
          got = .array(
            (op["q"]?.arrayValue ?? []).map { Self.encode(digests[d].quantile(Self.number($0))) })
        case "cdf":
          got = .array(
            (op["x"]?.arrayValue ?? []).map { Self.encode(digests[d].cdf(Self.number($0))) })
        case "count":
          got = Self.encode(digests[d].count())
        case "toJSON":
          got = .array(digests[d].toJSON().map(Self.encode))
        case "fromJSON":
          do {
            digests[d] = try TDigest(json: (op["json"]?.arrayValue ?? []).map(Self.number))
            got = "ok"
          } catch {
            got = ["threw": .string(error.description)]
          }
        case "reset":
          digests[d].reset()
          got = .null
        case let other:
          throw ClientApp.Failure("Unknown op \(other ?? "")")
        }
        let want = i < expect.count ? expect[i] : .null
        if got != want {
          Issue.record("\(name) op \(i) (\(op["op"] ?? .null)): Swift \(got), JS \(want)")
          ok = false
          break
        }
      }
      if ok { passed += 1 }
    }
    print("[tdigest] \(passed) passed")
  }
}
