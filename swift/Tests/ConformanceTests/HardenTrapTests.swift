import Testing

@testable import Zero

/// Queries the JS client rejects by throwing from `materialize` (an
/// `assert` in `buildPipeline` or in `Take.fetch`), so an app can catch the
/// error, where Swift stops the process with a `precondition`. Found by
/// `packages/zero-conformance/src/ivm/harden-ivm.test.ts`; the JS errors are
/// in the comments. Each case runs in an exit test, so a trap fails only
/// that test: the expected outcome is that the process survives.
@Suite(.enabled(if: Vectors.available))
struct HardenTrapTests {
  static func materialize(_ program: String) {
    let schema = ClientApp.schema
    var sources: [String: MemorySource] = [:]
    for name in schema.tableNames { sources[name] = MemorySource(schema.tables[name]!) }
    sources["issue"]!.push(
      .add([
        "id": "i1", "title": "a", "description": "", "closed": false, "ownerId": "u1",
        "createdAt": 1,
      ]))
    sources["user"]!.push(.add(["id": "u1", "name": "a", "metadata": .null]))
    sources["comment"]!.push(
      .add(["id": "c1", "authorId": "u1", "issueId": "i1", "text": "", "createdAt": 1]))
    sources["revision"]!.push(.add(["id": "r1", "authorId": "u1", "commentId": "c1", "text": ""]))
    let query = IVMConformanceTests.build(schema, try! JSONParser.parse(Array(program.utf8)))
    do {
      let input = try buildPipeline(
        query.ast, IVMConformanceTests.Delegate(sources), queryID: "q")
      _ = ArrayView(input, format: query.format, complete: true)
    } catch {
      // What JS does: materialize throws.
    }
  }

  /// JS: `EXISTS subqueries must not have related` (thrown). Swift:
  /// `precondition` in `Builder.swift:153`.
  @Test func existsSubqueryWithRelatedThrowsRatherThanTraps() async {
    await #expect(processExitsWith: .success) {
      HardenTrapTests.materialize(
        #"{"table":"revision","ops":[{"op":"whereExists","rel":"comment","sub":[{"op":"related","rel":"issue"}]}]}"#
      )
    }
  }

  /// JS: `Partitioned take does not allow unpartitioned fetches` (thrown).
  /// Swift: `precondition` in `Take.swift:64`.
  @Test func flippedExistsOverLimitedSubqueryThrowsRatherThanTraps() async {
    await #expect(processExitsWith: .success) {
      HardenTrapTests.materialize(
        #"{"table":"issue","ops":[{"op":"whereExists","rel":"oneComment","sub":[{"op":"limit","n":2},{"op":"whereExists","rel":"issue"}],"flip":true}]}"#
      )
    }
  }
}
