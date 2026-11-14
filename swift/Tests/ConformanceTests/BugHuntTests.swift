import Foundation
import Testing
import ZeroTesting

@testable import Zero

/// The aggregate bug hunt (`packages/zero-conformance/src/client/bughunt.test.ts`
/// and `src/ivm/bughunt.test.ts`): JS-generated aggregate, distinct and
/// groupBy cases over the values and client histories (restarts, pending
/// mutations) a port most likely gets wrong. Each checked-in case is its own
/// test.
@Suite(.serialized, .enabled(if: Vectors.available))
@MainActor
struct BugHuntTests {
  /// The random client scenarios, all of them: reports the first mismatches.
  @Test(.enabled(if: Vectors.generatedAvailable)) func clientFuzzOnMemory() async throws {
    try await Self.runFuzz(storage: .memory)
  }

  @Test(.enabled(if: Vectors.generatedAvailable)) func clientFuzzOnSQLite() async throws {
    let directory = NSTemporaryDirectory() + "zero-bughunt-" + UUID().uuidString
    defer { try? FileManager.default.removeItem(atPath: directory) }
    try await Self.runFuzz(storage: .sqlite(directory: directory))
  }

  nonisolated static let ivmNames: [String] =
    ((try? Vectors.file("bughunt-ivm"))?["cases"]?.arrayValue ?? [])
    .compactMap { $0["name"]?.stringValue }

  /// One targeted IVM case (`vectors/bughunt-ivm.json`).
  @Test(arguments: ivmNames) func ivm(_ name: String) throws {
    let file = try Vectors.file("bughunt-ivm")
    let schema = try ZeroSchema(json: file["shared"]!["schema"]!)
    let c = try #require(file["cases"]?.arrayValue?.first { $0["name"]?.stringValue == name })
    if let mismatch = try IVMConformanceTests.run(
      c, schema: schema, datasets: file["shared"]!["datasets"]!.arrayValue!)
    {
      Issue.record(Comment(rawValue: "\(name)\n\(mismatch)"))
    }
  }

  nonisolated static let clientNames: [String] =
    ((try? Vectors.file("bughunt-client"))?["cases"]?.arrayValue ?? [])
    .compactMap { $0["name"]?.stringValue }

  /// One targeted client scenario (`vectors/bughunt-client.json`).
  @Test(arguments: clientNames) func clientOnMemory(_ name: String) async throws {
    try await Self.runClient(name, storage: .memory)
  }

  @Test(arguments: clientNames) func clientOnSQLite(_ name: String) async throws {
    let directory = NSTemporaryDirectory() + "zero-bughunt-" + UUID().uuidString
    defer { try? FileManager.default.removeItem(atPath: directory) }
    try await Self.runClient(name, storage: .sqlite(directory: directory))
  }

  static func runClient(_ name: String, storage: ZeroStorage) async throws {
    let file = try Vectors.file("bughunt-client")
    let startTime = file["shared"]?["startTime"]?.numberValue ?? 0
    let scenario = try #require(
      file["cases"]?.arrayValue?.first { $0["name"]?.stringValue == name })
    let runner = try ScenarioRunner(scenario, startTime: startTime, storage: storage)
    if let mismatch = ClientConformanceTests.firstMismatch(
      scenario, scenario["expect"]?.arrayValue ?? [], await runner.run())
    {
      Issue.record(Comment(rawValue: "\(name)\n\(mismatch)"))
    }
  }

  static func runFuzz(storage: ZeroStorage) async throws {
    let file = try Vectors.generatedFile("client-aggregates-fuzz")
    let startTime = file["shared"]?["startTime"]?.numberValue ?? 0
    var failed: [String] = []
    for scenario in file["cases"]?.arrayValue ?? [] {
      let name = scenario["name"]?.stringValue ?? "?"
      let runner = try ScenarioRunner(scenario, startTime: startTime, storage: storage)
      let actual = await runner.run()
      if let mismatch = ClientConformanceTests.firstMismatch(
        scenario, scenario["expect"]?.arrayValue ?? [], actual)
      {
        if failed.count < 5 { Issue.record(Comment(rawValue: "\(name)\n\(mismatch)")) }
        failed.append(name)
      }
    }
    print(
      "[aggregate fuzz on \(storage)] \(failed.count) failed: \(failed.joined(separator: ", "))")
    #expect(failed.isEmpty, "\(failed.count) scenarios failed")
  }
}
