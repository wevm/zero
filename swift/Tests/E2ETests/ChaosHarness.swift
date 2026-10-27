import Foundation
import Synchronization
import Testing
import Zero

/// The fixture database, read and written with `psql` behind zero-cache's
/// back. `ZERO_E2E_PG_DB` overrides its URL.
enum Postgres {
  static let url =
    ProcessInfo.processInfo.environment["ZERO_E2E_PG_DB"]
    ?? "postgresql://postgres:postgres@127.0.0.1:5432/zero_e2e"

  /// Runs `sql` and returns its rows, each with its columns joined by `|`.
  @discardableResult
  static func query(_ sql: String) throws -> [String] {
    #if os(macOS) || os(Linux)
      let process = Process()
      process.executableURL = URL(fileURLWithPath: "/usr/bin/env")
      process.arguments = ["psql", url, "-v", "ON_ERROR_STOP=1", "-qAt", "-c", sql]
      let out = Pipe()
      let err = Pipe()
      process.standardOutput = out
      process.standardError = err
      try process.run()
      let data = out.fileHandleForReading.readDataToEndOfFile()
      let errors = err.fileHandleForReading.readDataToEndOfFile()
      process.waitUntilExit()
      guard process.terminationStatus == 0 else {
        throw PostgresError(message: String(decoding: errors, as: UTF8.self))
      }
      return String(decoding: data, as: UTF8.self).split(separator: "\n").map(String.init)
    #else
      // No `Process` on iOS; the chaos tests run on macOS and Linux.
      throw PostgresError(message: "psql can't run on this platform")
    #endif
  }

  struct PostgresError: Error, CustomStringConvertible {
    var message: String
    var description: String { "psql: \(message)" }
  }

  /// The IDs of the open issues whose ID starts with `prefix`.
  static func openIssueIDs(_ prefix: String) throws -> Set<String> {
    Set(try query("SELECT id FROM issues WHERE NOT closed AND id LIKE '\(prefix)%'"))
  }

  /// Every issue whose ID starts with `prefix`, as `id|title|closed`.
  static func issues(_ prefix: String) throws -> Set<String> {
    Set(
      try query("SELECT id || '|' || title || '|' || closed FROM issues WHERE id LIKE '\(prefix)%'")
    )
  }

  /// The last mutation ID zero-cache recorded for `clientID`.
  static func lastMutationID(_ clientID: String) throws -> Int? {
    try query(
      "SELECT \"lastMutationID\" FROM zero_0.clients WHERE \"clientID\" = '\(clientID)'"
    ).first.flatMap { Int($0) }
  }
}

/// Collects a client's log lines.
final class LogCollector: LogSink {
  private let lines = Mutex<[String]>([])

  func log(level: LogLevel, message: String, context: [String: String]) {
    lines.withLock { $0.append("[\(level)] \(message)") }
  }

  var all: [String] { lines.withLock { $0 } }
  func count(_ substring: String) -> Int { all.filter { $0.contains(substring) }.count }
}

/// Makes clients and waits on them.
@MainActor
struct Chaos {
  let directory = NSTemporaryDirectory() + "zero-chaos-" + UUID().uuidString
  let prefix = "chaos-\(UUID().uuidString.prefix(8))-"
  static let mutators: [any Mutator<Void>] = [E2EApp.create, E2EApp.close]

  /// A client of `cacheURL` (zero-cache itself by default).
  func makeZero(
    cacheURL: String? = nil, storageKey: String = UUID().uuidString, sqlite: Bool = true,
    logs: LogCollector? = nil, configure: (inout ZeroOptions) -> Void = { _ in }
  ) throws -> Zero<Void> {
    let origin = cacheURL ?? E2E.url!.replacingOccurrences(of: "ws://", with: "http://")
    var options = ZeroOptions(
      cacheURL: origin, storageKey: storageKey,
      storage: sqlite ? .sqlite(directory: directory) : .memory)
    options.logLevel = .info
    options.logSink = logs
    configure(&options)
    return try Zero(
      schema: .test, mutators: Self.mutators, options: options,
      socketFactory: TransportSocketFactory(E2E.transport))
  }

  func cleanUp() {
    try? FileManager.default.removeItem(atPath: directory)
    _ = try? Postgres.query("DELETE FROM issues WHERE id LIKE '\(prefix)%'")
  }

  /// Inserts `count` open issues `<prefix>seed-<n>` straight into Postgres,
  /// with descriptions of `descriptionBytes`.
  func seed(_ count: Int, descriptionBytes: Int = 0, tag: String = "seed") async throws {
    try Postgres.query(
      """
      INSERT INTO issues (id, title, description, closed, owner_id, "createdAt")
      SELECT '\(prefix)\(tag)-' || lpad(n::text, 6, '0'), 'Seeded ' || n,
        repeat('x', \(descriptionBytes)), false, NULL, 1000 + n
      FROM generate_series(1, \(count)) AS n
      """)
    try await replicated(prefix + tag + "-" + String(format: "%06d", count))
  }

  /// Waits until zero-cache has replicated `id`: a client straight to it
  /// sees the issue.
  func replicated(_ id: String) async throws {
    let probe = try makeZero(sqlite: false)
    defer { probe.close() }
    let issue = try probe.liveQuery(E2EApp.byID(id))
    try await eventually("\(id) to replicate") { issue.data != nil }
  }

  func connected(_ zero: Zero<Void>, timeout: Duration = .seconds(30)) async throws {
    try await eventually("\(zero.clientID) to connect", timeout: timeout) {
      zero.connection.state.status == .connected
    }
  }
}

/// Waits until `condition` holds.
@MainActor
func eventually(
  _ what: String, timeout: Duration = .seconds(30), _ condition: () throws -> Bool
) async throws {
  let deadline = ContinuousClock.now + timeout
  while try !condition() {
    guard ContinuousClock.now < deadline else {
      Issue.record("Timed out after \(timeout) waiting for \(what)")
      throw CancellationError()
    }
    try await Task.sleep(for: .milliseconds(20))
  }
}

/// Whether `condition` holds within `timeout`, without recording an issue.
@MainActor
func within(_ timeout: Duration, _ condition: () throws -> Bool) async throws -> Bool {
  let deadline = ContinuousClock.now + timeout
  while try !condition() {
    if ContinuousClock.now >= deadline { return false }
    try await Task.sleep(for: .milliseconds(20))
  }
  return true
}

/// `value` within `timeout`, or nil.
@MainActor
func settled<V: Sendable>(_ deferred: Deferred<V>, within timeout: Duration) async -> V? {
  let deadline = ContinuousClock.now + timeout
  while !deferred.isSettled {
    if ContinuousClock.now >= deadline { return nil }
    try? await Task.sleep(for: .milliseconds(20))
  }
  return try? await deferred.value
}

/// The server outcomes a client heard, by mutation ID.
@MainActor
final class OutcomeLog {
  private(set) var byID: [Int: [MutationOutcome]] = [:]
  func add(_ o: MutationServerOutcome) { byID[o.mutationID, default: []].append(o.outcome) }
}

/// Expects `got` to equal `want`, and reports a short difference.
func expectSame(
  _ got: Set<String>, _ want: Set<String>, _ what: String = "rows",
  sourceLocation: SourceLocation = #_sourceLocation
) {
  guard got != want else { return }
  let missing = want.subtracting(got).sorted()
  let extra = got.subtracting(want).sorted()
  let message =
    "\(what): \(got.count) of \(want.count); missing \(missing.count) \(missing.prefix(5)), "
    + "extra \(extra.count) \(extra.prefix(5))"
  Issue.record(Comment(rawValue: message), sourceLocation: sourceLocation)
}
