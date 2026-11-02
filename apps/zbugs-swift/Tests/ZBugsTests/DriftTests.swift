import Foundation
import Testing
import ZBugs
import Zero
import ZeroTesting

/// zbugs' Swift queries and mutators do what the TS ones do on the samples
/// in drift-samples.ts.
@Suite @MainActor struct DriftTests {
  @Test func matchesTheTSQueriesAndMutators() throws {
    let vectors = try String(
      contentsOf: URL(fileURLWithPath: #filePath).deletingLastPathComponent()
        .appendingPathComponent("drift-vectors.json"), encoding: .utf8)
    let failures = try Drift.failures(
      vectors: vectors, schema: .zbugs, queries: Queries.all, mutators: Mutators.all)
    // Not ported yet: issueListCount is a count(), and the Swift client has no
    // aggregates. Exact, so this fails once it is ported and must be removed.
    #expect(failures == ["query issueListCount: no Swift definition"])
  }
}
