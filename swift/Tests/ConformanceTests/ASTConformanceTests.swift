import Testing

@testable import Zero

@Suite(.enabled(if: Vectors.available))
struct ASTConformanceTests {
  static func check(_ c: JSONValue) throws {
    let ast = try AST(json: c["ast"]!)
    // Decoding then encoding is lossless.
    #expect(ast.json == c["ast"]!)
    let normalized = ast.normalized()
    #expect(normalized.json == c["normalized"]!, "normalize \(c["ast"]!)")
    #expect(QueryHash.hashOfAST(ast) == c["hash"]!.stringValue!, "hash \(c["ast"]!)")
  }

  @Test func curated() throws {
    for c in try Vectors.cases("ast") { try Self.check(c) }
  }

  @Test(.enabled(if: Vectors.generatedAvailable)) func fuzz() throws {
    for c in try Vectors.generatedCases("ast-fuzz") { try Self.check(c) }
  }

  /// Aggregates, groupBy and JSON paths: the wire schema's verdict, and for
  /// the ASTs it accepts, the normalized form, the hash, and the server
  /// names.
  @Test func features() throws {
    let mapper = NameMapper.clientToServer(.test)
    for c in try Vectors.cases("ast-features") {
      let json = c["ast"]!
      guard c["valid"] == .bool(true) else {
        #expect(throws: DecodingError.self, "accepts \(json)") { try StrictAST.decode(json) }
        continue
      }
      let ast = try StrictAST.decode(json)
      try Self.check(c)
      do {
        let mapped = try mapper.map(ast)
        #expect(mapped.json == c["mapped"], "map \(json)")
      } catch {
        #expect(error.description == c["mapError"]?.stringValue, "map \(json)")
      }
    }
  }

  @Test func queryInternalsHash() throws {
    for c in try Vectors.cases("query-internals-hash") {
      let ast = try AST(json: c["ast"]!)
      let format = try Format(json: c["format"]!)
      #expect(format.json == c["format"]!)
      let hash = QueryHash.hashOfQueryInternals(
        ast: ast, format: format.json, system: .client, currentJunction: nil,
        customQueryName: nil, customQueryArgs: nil)
      #expect(hash == c["hash"]!.stringValue!, "\(c["format"]!)")
    }
  }

  @Test func nameAndArgsHash() throws {
    for c in try Vectors.cases("name-args-hash") {
      let name = c["name"]!.stringValue!
      let args = c["args"]!.arrayValue!
      #expect(QueryHash.hashOfNameAndArgs(name, args) == c["hash"]!.stringValue!, "\(name) \(args)")
    }
  }
  /// `astToZQL` (`packages/ast-to-zql/src/ast-to-zql.ts`) of each AST, as
  /// the inspector shows a query. Swift has no `redactLiterals`, which only
  /// zero-cache uses, so the cases' `redacted` code isn't checked.
  @Test func astToZQLCode() throws {
    var passed = 0
    for c in try Vectors.cases("ast-to-zql") {
      let ast = try AST(json: c["ast"]!)
      let code = astToZQL(ast)
      #expect(code == c["zql"]?.stringValue, "astToZQL(\(c["ast"]!))")
      if code == c["zql"]?.stringValue { passed += 1 }
    }
    print("[ast-to-zql] \(passed) passed")
  }
}
