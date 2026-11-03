import Foundation
import Testing
import ZeroTesting

@testable import Zero

/// The aggregate bug hunt (`packages/zero-conformance/src/ivm/bughunt.test.ts`):
/// JS-generated aggregate, distinct and groupBy cases over datasets with the
/// values a port most likely gets wrong. Each checked-in case is its own test.
@Suite(.enabled(if: Vectors.available))
@MainActor
struct BugHuntTests {
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
}
