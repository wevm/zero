import Foundation
import Testing

@testable import Zero

/// Replays the hardening client scenarios
/// (`packages/zero-conformance/src/client/scenarios/harden-queries.ts`):
/// listeners that re-enter the client from a notification, TTLs JS parses
/// oddly, query change throttling, and pokes that leave a view unchanged.
/// Set `ZERO_CLIENT_SCENARIO` to run the scenarios whose name contains it.
@Suite(.enabled(if: Vectors.available))
@MainActor
struct HardenQueryTests {
  @Test func scenariosOnMemory() throws {
    try ClientConformanceTests.run(storage: .memory, vectors: "harden-queries")
  }

  @Test func scenariosOnSQLite() throws {
    let directory = NSTemporaryDirectory() + "zero-harden-scenarios-" + UUID().uuidString
    defer { try? FileManager.default.removeItem(atPath: directory) }
    try ClientConformanceTests.run(
      storage: .sqlite(directory: directory), vectors: "harden-queries")
  }
}
