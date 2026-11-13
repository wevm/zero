import Testing

@testable import Zero

/// Vectors from the zql tests restored from rocicorp/mono 7ebeb2172: each
/// case has sources, an AST and pushes, with the view and the changes JS
/// recorded after hydration and after each push.
///
/// - `vectors/restored-ivm.json` (`src/ivm/restored-ivm.test.ts`): the cases
///   of the restored zql tests that build a pipeline from an AST. Each has its
///   own sources, which have no relationships, and may turn on a builder
///   setting the client leaves off. Each case is its own test.
/// - `generated/restored-chinook-decpush.json`
///   (`src/ivm/restored-chinook.test.ts`): the chinook fuzzer's decorated
///   push lane over its mini data, which the cases share.
@Suite(.enabled(if: Vectors.available))
struct RestoredIVMTests {
  final class Delegate: BuilderDelegate {
    let sources: [String: MemorySource]
    let enableNotExists: Bool
    let disableCorrelatedPredicatePushdown: Bool

    init(_ sources: [String: MemorySource], options: JSONValue?) {
      self.sources = sources
      enableNotExists = options?["enableNotExists"]?.boolValue ?? false
      disableCorrelatedPredicatePushdown =
        options?["disableCorrelatedPredicatePushdown"]?.boolValue ?? false
    }

    func getSource(_ tableName: String) -> (any Source)? { sources[tableName] }
  }

  /// Runs one case over `schema` and `data` (the case's own, or the file's);
  /// returns a description of the first mismatch.
  static func run(_ c: JSONValue, schema: JSONValue, data: JSONValue) throws -> String? {
    let schema = try ZeroSchema(json: schema)
    var sources: [String: MemorySource] = [:]
    for table in schema.tableNames {
      let source = MemorySource(schema.tables[table]!)
      for row in data[table]?.arrayValue ?? [] {
        source.push(.add(Row(row.objectValue!)))
      }
      sources[table] = source
    }
    return IVMConformanceTests.replay(
      try AST(json: c["ast"]!), try Format(json: c["format"]!), sources: sources,
      delegate: Delegate(sources, options: c["options"]), pushes: c["pushes"]!.arrayValue!,
      result: c["result"]!)
  }

  nonisolated static let names: [String] =
    ((try? Vectors.file("restored-ivm"))?["cases"]?.arrayValue ?? [])
    .compactMap { $0["name"]?.stringValue }

  @Test(arguments: names) func restored(_ name: String) throws {
    let file = try Vectors.file("restored-ivm")
    let c = try #require(file["cases"]?.arrayValue?.first { $0["name"]?.stringValue == name })
    if let mismatch = try Self.run(c, schema: c["schema"]!, data: c["data"]!) {
      Issue.record(Comment(rawValue: "\(name)\n\(mismatch)"))
    }
  }

  @Test(.enabled(if: Vectors.generatedAvailable)) func chinookDecoratedPush() throws {
    let file = try Vectors.generatedFile("restored-chinook-decpush")
    let shared = file["shared"]!
    var failed: [String] = []
    for c in file["cases"]!.arrayValue! {
      if let mismatch = try Self.run(c, schema: shared["schema"]!, data: shared["data"]!) {
        if failed.count < 5 {
          Issue.record(Comment(rawValue: "\(c["name"]!.stringValue!)\n\(mismatch)"))
        }
        failed.append(c["name"]!.stringValue!)
      }
    }
    #expect(failed.isEmpty, "\(failed.count) cases failed")
  }
}
