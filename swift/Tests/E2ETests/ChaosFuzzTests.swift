import Foundation
import Testing
import Zero

/// Random chaos against the real zero-cache. Each seed runs two clients, on
/// stores of their own, through a `ChaosProxy`, and takes 40 random steps:
/// mutations, connections cut, refused, stalled, or cut partway through
/// what the server sends, writes to Postgres behind the clients' back, and
/// relaunches on the same store. Then the network heals, and the oracle is
/// convergence: Postgres holds every mutation once, each client's last
/// mutation ID counts all of its mutations, and both clients see what
/// Postgres holds.
///
/// PRs run `ZERO_CHAOS_SEEDS` seeds (2 by default) from
/// `ZERO_CHAOS_FIRST_SEED` (0); the nightly workflow runs more. To replay
/// one: `ZERO_CHAOS_SEED=<seed> ZERO_E2E_URL=... swift test --filter ChaosFuzzTests`.
@Suite(.enabled(if: E2E.url != nil), .serialized, .timeLimit(.minutes(60)))
@MainActor
struct ChaosFuzzTests {
  @Test func randomSeeds() async throws {
    let env = ProcessInfo.processInfo.environment
    let seeds: Range<UInt64>
    if let only = env["ZERO_CHAOS_SEED"].flatMap(UInt64.init) {
      seeds = only..<only + 1
    } else {
      let first = env["ZERO_CHAOS_FIRST_SEED"].flatMap(UInt64.init) ?? 0
      seeds = first..<first + (env["ZERO_CHAOS_SEEDS"].flatMap(UInt64.init) ?? 2)
    }
    var failed = 0
    for seed in seeds {
      let start = ContinuousClock.now
      let failure = try await ChaosRun(seed: seed).run()
      print(
        "[chaos fuzz] seed \(seed): \(failure ?? "converged") in \(ContinuousClock.now - start)")
      guard let failure else { continue }
      Issue.record("seed \(seed): \(failure)")
      failed += 1
      if failed == 3 { return }
    }
  }
}

/// SplitMix64: the same steps for a seed on every platform.
struct ChaosRandom: RandomNumberGenerator {
  var state: UInt64
  mutating func next() -> UInt64 {
    state &+= 0x9E37_79B9_7F4A_7C15
    var z = state
    z = (z ^ (z >> 30)) &* 0xBF58_476D_1CE4_E5B9
    z = (z ^ (z >> 27)) &* 0x94D0_49BB_1331_11EB
    return z ^ (z >> 31)
  }
}

/// One seed's clients, proxy, and model of what Postgres should end up with.
@MainActor
final class ChaosRun {
  /// A client and the relaunches on its store.
  @MainActor final class Lineage {
    let storageKey: String
    var zero: Zero<Void>
    var open: LiveQuery<[TestIssue]>
    /// What the client now running created and hasn't closed: only it may
    /// close them, since its mutations apply in order.
    var closable: [String] = []

    init(zero: Zero<Void>, storageKey: String) throws {
      self.zero = zero
      self.storageKey = storageKey
      open = try zero.liveQuery(E2EApp.open())
    }
  }

  let seed: UInt64
  var rng: ChaosRandom
  let chaos = Chaos()
  var proxy: ChaosProxy!
  var lineages: [Lineage] = []
  /// The open issues Postgres should end with.
  var expectedOpen: Set<String> = []
  var created = 0
  /// Mutations made by each client ID.
  var mutations: [String: Int] = [:]
  /// Issues written straight to Postgres and still open.
  var written: [String] = []
  var steps: [String] = []

  init(seed: UInt64) {
    self.seed = seed
    rng = ChaosRandom(state: seed)
  }

  func makeZero(_ storageKey: String) throws -> Zero<Void> {
    try chaos.makeZero(cacheURL: proxy.cacheURL, storageKey: storageKey) {
      $0.pingTimeoutMs = 2000
    }
  }

  /// The problems at the end; nil when everything converged.
  func run() async throws -> String? {
    defer { chaos.cleanUp() }
    proxy = try await ChaosProxy(upstream: E2E.url!)
    for _ in 0..<2 {
      let key = UUID().uuidString
      lineages.append(try Lineage(zero: try makeZero(key), storageKey: key))
    }
    defer {
      for l in lineages { l.zero.close() }
    }
    for l in lineages { try await chaos.connected(l.zero) }
    for n in 0..<40 {
      steps.append("\(n): " + (try await step(n)))
      try await Task.sleep(for: .milliseconds(Int.random(in: 0...100, using: &rng)))
    }
    await proxy.set {
      $0 = ChaosProxy.Knobs()
    }
    let problems = try await converge()
    await proxy.shutdown()
    guard !problems.isEmpty else { return nil }
    return problems.joined(separator: "; ")
      + " (last steps: \(steps.suffix(8).joined(separator: " | ")))"
  }

  func mutate(_ l: Lineage, _ request: MutateRequest<Void>) {
    _ = l.zero.mutate(request)
    mutations[l.zero.clientID, default: 0] += 1
  }

  func step(_ n: Int) async throws -> String {
    let i = Int.random(in: 0..<lineages.count, using: &rng)
    let l = lineages[i]
    switch Int.random(in: 0..<100, using: &rng) {
    case 0..<35:
      let id = "\(chaos.prefix)c\(n)"
      mutate(l, E2EApp.create(.init(id: id, title: "Chaos \(n)", ownerId: nil)))
      l.closable.append(id)
      expectedOpen.insert(id)
      created += 1
      return "client \(i) creates \(id)"
    case 35..<45:
      guard !l.closable.isEmpty else { return "client \(i) has nothing to close" }
      let id = l.closable.remove(at: Int.random(in: 0..<l.closable.count, using: &rng))
      mutate(l, E2EApp.close(id))
      expectedOpen.remove(id)
      return "client \(i) closes \(id)"
    case 45..<53:
      await proxy.cutAll()
      return "cut every connection"
    case 53..<58:
      let ms = Int.random(in: 100...1500, using: &rng)
      await proxy.set { $0.accept = .refuse }
      await proxy.cutAll()
      try await Task.sleep(for: .milliseconds(ms))
      await proxy.set { $0.accept = .pass }
      return "down for \(ms) ms"
    case 58..<63:
      let ms = Int.random(in: 200...3000, using: &rng)
      await proxy.set { $0.downstreamRate = 0 }
      try await Task.sleep(for: .milliseconds(ms))
      await proxy.set { $0.downstreamRate = nil }
      return "server stalled for \(ms) ms"
    case 63..<68:
      let bytes = Int.random(in: 500...50_000, using: &rng)
      let ms = Int.random(in: 200...1500, using: &rng)
      await proxy.set { $0.cutAfterDownstreamBytes = bytes }
      try await Task.sleep(for: .milliseconds(ms))
      await proxy.set { $0.cutAfterDownstreamBytes = nil }
      return "cuts after \(bytes) bytes for \(ms) ms"
    case 68..<76:
      let id = "\(chaos.prefix)pg\(n)"
      try Postgres.query(
        """
        INSERT INTO issues (id, title, description, closed, owner_id, "createdAt")
        VALUES ('\(id)', 'Written \(n)', '', false, NULL, 2000)
        """)
      written.append(id)
      expectedOpen.insert(id)
      created += 1
      return "Postgres inserts \(id)"
    case 76..<82:
      guard !written.isEmpty else { return "Postgres has nothing to close" }
      let id = written.remove(at: Int.random(in: 0..<written.count, using: &rng))
      try Postgres.query("UPDATE issues SET closed = true WHERE id = '\(id)'")
      expectedOpen.remove(id)
      return "Postgres closes \(id)"
    case 82..<90:
      l.zero.close()
      l.zero = try makeZero(l.storageKey)
      l.open = try l.zero.liveQuery(E2EApp.open())
      l.closable = []
      return "client \(i) relaunches"
    default:
      let ms = Int.random(in: 0...500, using: &rng)
      try await Task.sleep(for: .milliseconds(ms))
      return "wait \(ms) ms"
    }
  }

  func ids(_ l: Lineage) -> Set<String> {
    Set(l.open.data.map(\.id).filter { $0.hasPrefix(chaos.prefix) })
  }

  func short(_ got: Set<String>, _ want: Set<String>) -> String {
    let missing = want.subtracting(got).sorted()
    let extra = got.subtracting(want).sorted()
    return "missing \(missing.prefix(5)), extra \(extra.prefix(5))"
  }

  /// Waits for the clients and Postgres to settle, then compares them with
  /// the model.
  func converge() async throws -> [String] {
    var problems: [String] = []
    // Every mutation pushed first: until then, a create closed in the same
    // run leaves the open issues right but is missing from Postgres.
    for (clientID, count) in mutations.sorted(by: { $0.key < $1.key }) {
      let ok = try await within(.seconds(30)) { try Postgres.lastMutationID(clientID) == count }
      if !ok {
        let last = try Postgres.lastMutationID(clientID).map(String.init) ?? "none"
        problems.append(
          "client \(clientID) made \(count) mutations; its last mutation ID is \(last)")
      }
    }
    let inPostgres = try await within(.seconds(30)) {
      try Postgres.openIssueIDs(self.chaos.prefix) == self.expectedOpen
        && Postgres.issues(self.chaos.prefix).count == self.created
    }
    if !inPostgres {
      let open = try Postgres.openIssueIDs(chaos.prefix)
      if open != expectedOpen {
        problems.append("Postgres's open issues: \(short(open, expectedOpen))")
      }
      let all = try Postgres.issues(chaos.prefix).count
      if all != created { problems.append("\(all) issues in Postgres, \(created) created") }
    }
    for (i, l) in lineages.enumerated() {
      let ok = try await within(.seconds(60)) {
        l.open.resultType == .complete && self.ids(l) == self.expectedOpen
      }
      if !ok {
        problems.append(
          "client \(i) (\(l.zero.connection.state.status)) sees \(short(ids(l), expectedOpen))")
      }
    }
    return problems
  }
}
