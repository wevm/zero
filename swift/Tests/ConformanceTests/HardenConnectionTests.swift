import Foundation
import Testing

@testable import Zero

/// Replays the connection hardening scenarios
/// (`packages/zero-conformance/src/client/scenarios/harden-connection.ts`,
/// `vectors/client-harden-connection.json`) with the client scenario runner.
/// Set `ZERO_CLIENT_SCENARIO` to run only the scenarios whose name contains it.
@Suite(.enabled(if: Vectors.available))
@MainActor
struct HardenConnectionTests {
  @Test func scenariosOnMemory() throws {
    try ClientConformanceTests.run(storage: .memory, vectors: "client-harden-connection")
  }

  @Test func scenariosOnSQLite() throws {
    let directory = NSTemporaryDirectory() + "zero-harden-connection-" + UUID().uuidString
    defer { try? FileManager.default.removeItem(atPath: directory) }
    try ClientConformanceTests.run(
      storage: .sqlite(directory: directory), vectors: "client-harden-connection")
  }
}
