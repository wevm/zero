import Foundation
import Testing
import ZeroTesting

@testable import Zero

/// Replays the mutation hardening scenarios
/// (`packages/zero-conformance/src/client/scenarios/harden-mutations.ts`,
/// vectors in `client-harden-mutations.json`) with the client scenario
/// runner, one test per scenario.
@Suite(.enabled(if: Vectors.available))
@MainActor
struct HardenMutationConformanceTests {
  nonisolated static let names: [String] =
    ((try? Vectors.file("client-harden-mutations"))?["cases"]?.arrayValue ?? [])
    .compactMap { $0["name"]?.stringValue }

  @Test(arguments: names) func onMemory(_ name: String) throws {
    try Self.run(name, storage: .memory)
  }

  @Test(arguments: names) func onSQLite(_ name: String) throws {
    let directory = NSTemporaryDirectory() + "zero-harden-mutations-" + UUID().uuidString
    defer { try? FileManager.default.removeItem(atPath: directory) }
    try Self.run(name, storage: .sqlite(directory: directory))
  }

  static func run(_ name: String, storage: ZeroStorage) throws {
    let file = try Vectors.file("client-harden-mutations")
    let startTime = file["shared"]?["startTime"]?.numberValue ?? 0
    let scenario = try #require(
      file["cases"]?.arrayValue?.first { $0["name"]?.stringValue == name })
    let runner = try ScenarioRunner(scenario, startTime: startTime, storage: storage)
    let actual = runner.run()
    if let mismatch = ClientConformanceTests.firstMismatch(
      scenario, scenario["expect"]?.arrayValue ?? [], actual)
    {
      Issue.record(Comment(rawValue: "\(name)\n\(mismatch)"))
    }
  }
}
