import Foundation
import Testing

@testable import Zero

/// Replays the inspector scenarios
/// (`packages/zero-conformance/src/client/scenarios/inspector.ts`): the RPCs
/// over the socket and how their answers resolve, the local store as the
/// inspector reads it, and the query metrics and materialization logs.
/// Where zero-cache refuses an unauthenticated request, JS prompts for the
/// password and fails with `Authentication failed` without one; Swift throws
/// `InspectorError.authenticationRequired`, which the runner reports as JS's
/// message (`ScenarioRunner.inspectError`).
/// Set `ZERO_CLIENT_SCENARIO` to run the scenarios whose name contains it.
@Suite(.enabled(if: Vectors.available))
@MainActor
struct InspectorConformanceTests {
  @Test func scenariosOnMemory() async throws {
    try await ClientConformanceTests.run(storage: .memory, vectors: "client-inspector")
  }

  @Test func scenariosOnSQLite() async throws {
    let directory = NSTemporaryDirectory() + "zero-inspector-scenarios-" + UUID().uuidString
    defer { try? FileManager.default.removeItem(atPath: directory) }
    try await ClientConformanceTests.run(
      storage: .sqlite(directory: directory), vectors: "client-inspector")
  }
}
