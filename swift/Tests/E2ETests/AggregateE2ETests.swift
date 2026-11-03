import Foundation
import Testing

@testable import Zero

/// Synced aggregates against zero-cache, the API server, and Postgres: the
/// rows zero-cache computes and streams (`aggregates` in
/// `packages/zero-conformance/src/e2e/app.ts`), and a grouped count moved
/// optimistically by a mutation and then confirmed.
@Suite(.enabled(if: E2E.url != nil), .timeLimit(.minutes(2)))
@MainActor
struct AggregateE2ETests {
  let directory = NSTemporaryDirectory() + "zero-e2e-" + UUID().uuidString

  func makeZero() throws -> Zero<Void> {
    let origin = E2E.url!.replacingOccurrences(of: "ws://", with: "http://")
    let options = ZeroOptions(
      cacheURL: origin, storageKey: UUID().uuidString, storage: .sqlite(directory: directory))
    return try Zero(
      schema: .test, mutators: [E2EApp.create, E2EApp.close], options: options,
      socketFactory: TransportSocketFactory(E2E.transport))
  }

  func eventually(
    _ what: String, timeout: Duration = .seconds(30), _ condition: () -> Bool
  ) async throws {
    let deadline = ContinuousClock.now + timeout
    while !condition() {
      guard ContinuousClock.now < deadline else {
        Issue.record("Timed out waiting for \(what)")
        throw CancellationError()
      }
      try await Task.sleep(for: .milliseconds(20))
    }
  }

  func named(_ query: AnyQuery, _ name: String, _ args: JSONValue? = nil) -> AnyQuery {
    query.nameAndArgs(name, args.map { [$0] } ?? [])
  }

  @Test func syncsAggregatesAndMovesThemOptimistically() async throws {
    defer { try? FileManager.default.removeItem(atPath: directory) }
    let zero = try makeZero()
    defer { zero.close() }
    let zql = { (table: String) in AnyQuery(.test, table) }

    let count = try zero.materialize(
      named(zql("comment").count(), "aggregates.commentCount"))
    let perIssue = try zero.materialize(
      named(
        zql("issue").where("id", .in, ["i1", "i2", "i3", "i4"])
          .related("comments") { $0.count() }.orderBy("id", .asc),
        "aggregates.commentCounts"))
    let byAuthor = try zero.materialize(
      named(zql("comment").groupBy("authorId").count(), "aggregates.commentsByAuthor"))
    // Before the server answers, a count is 0.
    #expect(count.data == .value(0))
    try await eventually("the aggregates") {
      [count, perIssue, byAuthor].allSatisfy { $0.resultType == .complete }
    }
    #expect(count.data == .value(3))
    #expect(
      perIssue.data.entries.map { $0.relationships["comments"] }
        == [.value(2), .value(0), .value(1), .value(0)])
    #expect(
      byAuthor.data.json
        == [
          ["authorId": "u1", "count": 1], ["authorId": "u2", "count": 1],
          ["authorId": "u3", "count": 1],
        ])

    // A grouped count moves with the mutation at once; a top-level one
    // when the server confirms it.
    let title = "aggregates \(UUID().uuidString)"
    let titled = try zero.materialize(
      named(
        zql("issue").where("title", .string(title)).count(), "aggregates.titled", .string(title)))
    let owners = try zero.materialize(
      named(
        zql("issue").where("title", .string(title)).groupBy("ownerId").count(),
        "aggregates.titledOwners", .string(title)))
    try await eventually("the titled aggregates") {
      titled.resultType == .complete && owners.resultType == .complete
    }
    #expect(titled.data == .value(0))
    #expect(owners.data == .many([]))
    let id = "swift-\(UUID().uuidString)"
    let created = zero.mutate(E2EApp.create(.init(id: id, title: title, ownerId: "u2")))
    #expect(owners.data.json == [["ownerId": "u2", "count": 1]])
    #expect(titled.data == .value(0))
    #expect(try await created.server.value == .success(data: nil))
    try await eventually("the server's count") { titled.data == .value(1) }
    #expect(owners.data.json == [["ownerId": "u2", "count": 1]])
    // Closed, as the other tests leave theirs, so the fixture's open issues
    // stay the ones `WalkingSkeletonTests` expects.
    #expect(try await zero.mutate(E2EApp.close(id)).server.value == .success(data: nil))
  }
}
