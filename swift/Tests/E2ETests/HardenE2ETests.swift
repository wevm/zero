import Foundation
import Testing
import Zero

/// Chaos against the real zero-cache: connections cut, stalled, and
/// refused by a `ChaosProxy`, data changed in Postgres behind the client's
/// back, mutations replayed across outages and relaunches. The oracle is
/// convergence: afterwards the client holds what Postgres holds, every
/// mutation applied exactly once.
@Suite(.enabled(if: E2E.url != nil), .serialized, .timeLimit(.minutes(2)))
@MainActor
struct HardenE2ETests {
  let chaos = Chaos()

  /// The IDs of `chaos.prefix` in a live query of the open issues.
  func ids(_ open: LiveQuery<[TestIssue]>) -> Set<String> {
    Set(open.data.map(\.id).filter { $0.hasPrefix(chaos.prefix) })
  }

  // MARK: - Servers that don't answer

  /// A server that accepts the TCP connection and never answers the
  /// upgrade. The connect timeout (10 s, `CONNECT_TIMEOUT_MS`) gives up on
  /// it and closes the socket; JS's `#disconnect` closes the still
  /// connecting `WebSocket`, which aborts the handshake. Then the client
  /// reconnects once the server answers, and no socket is left behind.
  @Test func hungUpgradeTimesOutClosesTheSocketAndRecovers() async throws {
    defer { chaos.cleanUp() }
    let proxy = try await ChaosProxy(upstream: E2E.url!)
    await proxy.set { $0.accept = .blackhole }
    let zero = try chaos.makeZero(cacheURL: proxy.cacheURL)
    defer { zero.close() }
    while await proxy.accepted < 1 { try await Task.sleep(for: .milliseconds(20)) }
    await proxy.set { $0.accept = .pass }
    // 10 s for the timeout, 5 s of backoff.
    try await chaos.connected(zero, timeout: .seconds(25))
    #expect(await proxy.accepted == 2)
    // The hung socket was closed when its attempt timed out.
    let deadline = ContinuousClock.now + .seconds(2)
    while await proxy.openConnections > 1, ContinuousClock.now < deadline {
      try await Task.sleep(for: .milliseconds(20))
    }
    #expect(await proxy.openConnections == 1, "the timed-out socket leaked")
    zero.close()
    await proxy.shutdown()
  }

  /// A server that completes the WebSocket upgrade and never sends
  /// `connected`: the ack timeout closes the socket, and the client
  /// connects once the server behaves.
  @Test func openSocketWithoutConnectedTimesOutAndRecovers() async throws {
    defer { chaos.cleanUp() }
    let proxy = try await ChaosProxy(upstream: E2E.url!)
    await proxy.set { $0.accept = .silentAfterUpgrade }
    let zero = try chaos.makeZero(cacheURL: proxy.cacheURL)
    defer { zero.close() }
    let open = try zero.liveQuery(E2EApp.open())
    while await proxy.accepted < 1 { try await Task.sleep(for: .milliseconds(20)) }
    await proxy.set { $0.accept = .pass }
    #expect(zero.connection.state.status == .connecting)
    try await chaos.connected(zero, timeout: .seconds(25))
    try await eventually("the open issues") { open.resultType == .complete }
    let deadline = ContinuousClock.now + .seconds(2)
    while await proxy.openConnections > 1, ContinuousClock.now < deadline {
      try await Task.sleep(for: .milliseconds(20))
    }
    #expect(await proxy.openConnections == 1)
    zero.close()
    await proxy.shutdown()
  }

  // MARK: - Cuts and slow reads

  /// The connection is cut partway through a large initial poke, several
  /// times. The partial poke must not be applied, and the client must
  /// converge on Postgres once a connection survives.
  @Test func cutsMidInitialPokeConverge() async throws {
    defer { chaos.cleanUp() }
    try await chaos.seed(3000, descriptionBytes: 300)
    let expected = try Postgres.openIssueIDs(chaos.prefix)
    #expect(expected.count == 3000)
    let proxy = try await ChaosProxy(upstream: E2E.url!)
    await proxy.set { $0.cutAfterDownstreamBytes = 150_000 }
    let zero = try chaos.makeZero(cacheURL: proxy.cacheURL, sqlite: false)
    defer { zero.close() }
    let open = try zero.liveQuery(E2EApp.open())
    while await proxy.accepted < 3 { try await Task.sleep(for: .milliseconds(20)) }
    // Never a partial result: either nothing of the seed, or all of it.
    #expect(
      ids(open).isEmpty || ids(open) == expected, "\(ids(open).count) of \(expected.count) rows")
    #expect(open.resultType != .complete)
    await proxy.set { $0.cutAfterDownstreamBytes = nil }
    try await eventually("the issues after the cuts", timeout: .seconds(40)) {
      open.resultType == .complete && ids(open) == expected
    }
    zero.close()
    await proxy.shutdown()
  }

  /// A slow reader: the client takes a ~3 MB initial poke at 600 KB/s.
  /// Each message the server sends resets the ping timer, so the client
  /// must not give up on the connection while the poke trickles in.
  @Test func slowReaderLargePokeCompletesOnOneConnection() async throws {
    defer { chaos.cleanUp() }
    try await chaos.seed(3000, descriptionBytes: 1000)
    let expected = try Postgres.openIssueIDs(chaos.prefix)
    let proxy = try await ChaosProxy(upstream: E2E.url!)
    await proxy.set { $0.downstreamRate = 600_000 }
    let zero = try chaos.makeZero(cacheURL: proxy.cacheURL, sqlite: false)
    defer { zero.close() }
    let start = ContinuousClock.now
    let open = try zero.liveQuery(E2EApp.open())
    try await eventually("the throttled poke", timeout: .seconds(50)) {
      open.resultType == .complete
    }
    let elapsed = ContinuousClock.now - start
    print("[chaos] throttled poke of \(expected.count) rows in \(elapsed)")
    expectSame(ids(open), expected)
    #expect(await proxy.accepted == 1, "the client reconnected during the slow poke")
    zero.close()
    await proxy.shutdown()
  }

  // MARK: - Mutations across outages

  /// Mutations made while zero-cache is unreachable are replayed in order
  /// once it is back, each applied exactly once: Postgres holds what the
  /// client predicted, and the server's last mutation ID for the client is
  /// the number of mutations.
  @Test func offlineMutationsReplayInOrderExactlyOnce() async throws {
    defer { chaos.cleanUp() }
    let proxy = try await ChaosProxy(upstream: E2E.url!)
    let zero = try chaos.makeZero(cacheURL: proxy.cacheURL)
    defer { zero.close() }
    let outcomes = OutcomeLog()
    let stopListening = zero.addMutationOutcomeListener { outcomes.add($0) }
    defer { stopListening() }
    let open = try zero.liveQuery(E2EApp.open())
    try await eventually("the open issues") { open.resultType == .complete }

    await proxy.set { $0.accept = .refuse }
    await proxy.cutAll()
    try await eventually("the cut") { zero.connection.state.status != .connected }

    var results: [MutatorResult] = []
    var expectedOpen: Set<String> = []
    for n in 0..<30 {
      let id = "\(chaos.prefix)\(n)"
      results.append(zero.mutate(E2EApp.create(.init(id: id, title: "Offline \(n)", ownerId: nil))))
      expectedOpen.insert(id)
      if n % 3 == 0 {
        // Only applies after the create before it.
        results.append(zero.mutate(E2EApp.close(id)))
        expectedOpen.remove(id)
      }
    }
    #expect(ids(open) == expectedOpen)
    try await Task.sleep(for: .seconds(1))
    await proxy.set { $0.accept = .pass }

    for (i, r) in results.enumerated() {
      let outcome = await settled(r.server, within: .seconds(30))
      #expect(outcome == .success(data: nil), "mutation \(i + 1)")
    }
    #expect(try Postgres.openIssueIDs(chaos.prefix) == expectedOpen)
    #expect(try Postgres.issues(chaos.prefix).count == 30)
    #expect(try Postgres.lastMutationID(zero.clientID) == results.count)
    try await eventually("the client to settle") { ids(open) == expectedOpen }
    #expect(outcomes.byID.count == results.count)
    #expect(outcomes.byID.values.allSatisfy { $0 == [.success(data: nil)] }, "\(outcomes.byID)")

    let fresh = try chaos.makeZero(sqlite: false)
    defer { fresh.close() }
    let freshOpen = try fresh.liveQuery(E2EApp.open())
    try await eventually("a fresh client") { freshOpen.resultType == .complete }
    #expect(ids(freshOpen) == expectedOpen)
    zero.close()
    await proxy.shutdown()
  }

  /// Mutations made while the connection is cut over and over: some are
  /// pushed and lost, some pushed twice, some confirmed only by the
  /// reconnect's last mutation ID. Each is applied exactly once.
  @Test func mutationsRacingConnectionCutsApplyExactlyOnce() async throws {
    defer { chaos.cleanUp() }
    let proxy = try await ChaosProxy(upstream: E2E.url!)
    let zero = try chaos.makeZero(cacheURL: proxy.cacheURL)
    defer { zero.close() }
    let open = try zero.liveQuery(E2EApp.open())
    try await eventually("the open issues") { open.resultType == .complete }
    var results: [MutatorResult] = []
    var expected: Set<String> = []
    for n in 0..<60 {
      let id = "\(chaos.prefix)\(n)"
      results.append(zero.mutate(E2EApp.create(.init(id: id, title: "Racing \(n)", ownerId: nil))))
      expected.insert(id)
      if n % 7 == 3 { await proxy.cutAll() }
      try await Task.sleep(for: .milliseconds(30))
    }
    for (i, r) in results.enumerated() {
      let outcome = await settled(r.server, within: .seconds(40))
      #expect(outcome == .success(data: nil), "mutation \(i + 1)")
    }
    #expect(try Postgres.openIssueIDs(chaos.prefix) == expected)
    #expect(try Postgres.lastMutationID(zero.clientID) == results.count)
    try await eventually("the client to settle") { ids(open) == expected }
    print("[chaos] \(await proxy.accepted) connections for 60 racing mutations")
    zero.close()
    await proxy.shutdown()
  }

  /// The server applies a mutation but its response is lost: the server
  /// stalls on the way back, the ping times out, and the connection is
  /// cut. The reconnect's last mutation ID confirms the mutation, which
  /// must not be applied again.
  @Test func lostPushResponseConfirmedByReconnect() async throws {
    defer { chaos.cleanUp() }
    let proxy = try await ChaosProxy(upstream: E2E.url!)
    let zero = try chaos.makeZero(cacheURL: proxy.cacheURL) { $0.pingTimeoutMs = 1000 }
    defer { zero.close() }
    let outcomes = OutcomeLog()
    let stopListening = zero.addMutationOutcomeListener { outcomes.add($0) }
    defer { stopListening() }
    let open = try zero.liveQuery(E2EApp.open())
    try await eventually("the open issues") { open.resultType == .complete }
    await proxy.set { $0.downstreamRate = 0 }
    let id = "\(chaos.prefix)lost-response"
    let created = zero.mutate(E2EApp.create(.init(id: id, title: "Lost response", ownerId: nil)))
    try await eventually("the server to apply it") {
      try Postgres.openIssueIDs(chaos.prefix) == [id]
    }
    // The ping fails, and the client reconnects once the server is heard.
    try await eventually("the ping timeout") { zero.connection.state.status != .connected }
    await proxy.set { $0.downstreamRate = nil }
    #expect(await settled(created.server, within: .seconds(20)) == .success(data: nil))
    try await chaos.connected(zero)
    #expect(try Postgres.lastMutationID(zero.clientID) == 1)
    try await eventually("the confirmed row") { open.data.contains { $0.id == id } }
    #expect(outcomes.byID.count <= 1)
    // Still syncing.
    let next = zero.mutate(E2EApp.close(id))
    #expect(await settled(next.server, within: .seconds(10)) == .success(data: nil))
    try await eventually("the close to sync") { !open.data.contains { $0.id == id } }
    zero.close()
    await proxy.shutdown()
  }

  /// A push that never reaches the server: what the client sends is held,
  /// then the connection is cut. The reconnect pushes it again.
  @Test func pushLostOnTheWayIsSentAgain() async throws {
    defer { chaos.cleanUp() }
    let proxy = try await ChaosProxy(upstream: E2E.url!)
    let zero = try chaos.makeZero(cacheURL: proxy.cacheURL)
    defer { zero.close() }
    let open = try zero.liveQuery(E2EApp.open())
    try await eventually("the open issues") { open.resultType == .complete }
    await proxy.set { $0.stallUpstream = true }
    let issueIDs = (0..<5).map { "\(chaos.prefix)held-\($0)" }
    let results = issueIDs.map {
      zero.mutate(E2EApp.create(.init(id: $0, title: "Held", ownerId: nil)))
    }
    try await Task.sleep(for: .milliseconds(300))
    #expect(try Postgres.openIssueIDs(chaos.prefix).isEmpty)
    await proxy.cutAll()
    await proxy.set { $0.stallUpstream = false }
    for r in results {
      #expect(await settled(r.server, within: .seconds(20)) == .success(data: nil))
    }
    #expect(try Postgres.openIssueIDs(chaos.prefix) == Set(issueIDs))
    #expect(try Postgres.lastMutationID(zero.clientID) == 5)
    zero.close()
    await proxy.shutdown()
  }

  /// The server stops sending while the socket stays open: the ping
  /// times out (`pingTimeoutMs`), the client drops the connection and
  /// opens a new one, and catches up on what changed in Postgres meanwhile.
  @Test func stalledServerIsDetectedByPingAndCaughtUp() async throws {
    defer { chaos.cleanUp() }
    let proxy = try await ChaosProxy(upstream: E2E.url!)
    let zero = try chaos.makeZero(cacheURL: proxy.cacheURL) { $0.pingTimeoutMs = 1000 }
    defer { zero.close() }
    let open = try zero.liveQuery(E2EApp.open())
    try await eventually("the open issues") { open.resultType == .complete }
    await proxy.set { $0.downstreamRate = 0 }
    let id = "\(chaos.prefix)while-stalled"
    try Postgres.query(
      """
      INSERT INTO issues (id, title, description, closed, owner_id, "createdAt")
      VALUES ('\(id)', 'Behind the back', '', false, 'u3', 7)
      """)
    // Ping after 1 s of silence, give up after 1 s more.
    try await eventually("the ping timeout", timeout: .seconds(5)) {
      zero.connection.state.status != .connected
    }
    #expect(await proxy.openConnections == 0)
    await proxy.set { $0.downstreamRate = nil }
    try await eventually("the row written behind the client's back") {
      open.data.first { $0.id == id }?.owner?.name == "Çağrı"
    }
    #expect(await proxy.accepted >= 2)
    zero.close()
    await proxy.shutdown()
  }

  // MARK: - Postgres behind the client's back

  /// Inserts, updates, and deletes made with psql arrive, including a
  /// related row with a large JSON value.
  @Test func postgresWritesArriveIncludingLargeJSON() async throws {
    defer {
      chaos.cleanUp()
      _ = try? Postgres.query("DELETE FROM users WHERE id LIKE '\(chaos.prefix)%'")
    }
    let zero = try chaos.makeZero(sqlite: false)
    defer { zero.close() }
    let open = try zero.liveQuery(E2EApp.open())
    try await eventually("the open issues") { open.resultType == .complete }
    let user = "\(chaos.prefix)user"
    let id = "\(chaos.prefix)psql"
    // ~2 MB of JSON: 20,000 contacts.
    try Postgres.query(
      """
      INSERT INTO users (id, name, metadata) SELECT '\(user)', 'Big',
        jsonb_build_object('registrar', 'google', 'email', 'big@example.com',
          'altContacts', jsonb_agg(n || ':' || repeat('é😀', 30) ORDER BY n))
      FROM generate_series(1, 20000) AS n;
      INSERT INTO issues (id, title, description, closed, owner_id, "createdAt")
      VALUES ('\(id)', 'From psql', '', false, '\(user)', 8);
      """)
    let start = ContinuousClock.now
    try await eventually("the issue and its big owner") {
      open.data.first { $0.id == id }?.owner?.metadata != nil
    }
    print("[chaos] 2 MB JSON row arrived in \(ContinuousClock.now - start)")
    let contacts = try #require(open.data.first { $0.id == id }?.owner?.metadata?.altContacts)
    #expect(contacts.count == 20000)
    #expect(contacts.last == "20000:" + String(repeating: "é😀", count: 30))
    try Postgres.query("UPDATE issues SET title = 'Renamed in psql' WHERE id = '\(id)'")
    try await eventually("the update") {
      open.data.first { $0.id == id }?.title == "Renamed in psql"
    }
    try Postgres.query("DELETE FROM issues WHERE id = '\(id)'")
    try await eventually("the delete") { !open.data.contains { $0.id == id } }
    zero.close()
  }

  /// Two clients of one user create the same issue at once. The server's
  /// insert is `ON CONFLICT DO NOTHING` (`makeServerTableCRUD` in
  /// `packages/zero-server/src/custom.ts`), so both succeed and the first
  /// wins; both clients must drop their optimistic row for Postgres's.
  @Test func twoClientsRacingOnOneRowConverge() async throws {
    defer { chaos.cleanUp() }
    let a = try chaos.makeZero(sqlite: false)
    let b = try chaos.makeZero(sqlite: false)
    defer {
      a.close()
      b.close()
    }
    let openA = try a.liveQuery(E2EApp.open())
    let openB = try b.liveQuery(E2EApp.open())
    try await eventually("both clients") {
      openA.resultType == .complete && openB.resultType == .complete
    }
    for round in 0..<10 {
      let id = "\(chaos.prefix)race-\(round)"
      let ra = a.mutate(E2EApp.create(.init(id: id, title: "A", ownerId: nil)))
      let rb = b.mutate(E2EApp.create(.init(id: id, title: "B", ownerId: nil)))
      let oa = await settled(ra.server, within: .seconds(10))
      let ob = await settled(rb.server, within: .seconds(10))
      #expect(oa == .success(data: nil), "round \(round)")
      #expect(ob == .success(data: nil), "round \(round)")
      let title = try Postgres.query("SELECT title FROM issues WHERE id = '\(id)'").first
      try await eventually("round \(round) to converge") {
        openA.data.first { $0.id == id }?.title == title
          && openB.data.first { $0.id == id }?.title == title
      }
    }
  }

  // MARK: - Queries

  /// Hundreds of queries materialized and destroyed in a burst. The server
  /// ends up with the queries still open active, the client keeps
  /// syncing, and every destroyed query is let go.
  @Test func queryChurnSettlesOnTheServer() async throws {
    defer { chaos.cleanUp() }
    let zero = try chaos.makeZero(sqlite: false)
    defer { zero.close() }
    let open = try zero.liveQuery(E2EApp.open())
    try await eventually("the open issues") { open.resultType == .complete }
    var kept: [LiveQuery<TestIssue?>] = []
    for n in 0..<300 {
      let q = try zero.liveQuery(E2EApp.byID("\(chaos.prefix)churn-\(n)"), ttl: .none)
      if n % 50 == 0 { kept.append(q) } else { q.view.destroy() }
      if n % 20 == 0 { try await Task.sleep(for: .milliseconds(1)) }
    }
    try await eventually("the kept queries") { kept.allSatisfy { $0.resultType == .complete } }
    let id = "\(chaos.prefix)churn-100"
    let created = zero.mutate(E2EApp.create(.init(id: id, title: "Churned", ownerId: nil)))
    #expect(await settled(created.server, within: .seconds(10)) == .success(data: nil))
    try await eventually("a kept query to see its row") { kept[2].data?.title == "Churned" }
    // `issues.open` and the kept `issues.byID`s.
    let client = zero.clientID
    try await eventually("the server's desired queries", timeout: .seconds(15)) {
      try Postgres.query(
        """
        SELECT count(*) FROM "zero_0/cvr".desires
        WHERE "clientID" = '\(client)' AND NOT coalesce(deleted, false)
          AND "inactivatedAt" IS NULL
        """
      ).first == String(1 + kept.count)
    }
    _ = kept
  }

  // MARK: - Storage

  /// Clients opened on one SQLite store, each making a mutation and closed
  /// at once, before it could push. The last one pushes them all, each
  /// applied once.
  @Test func rapidCloseAndReopenOnSQLiteLosesNoMutation() async throws {
    defer { chaos.cleanUp() }
    let key = UUID().uuidString
    var clientIDs: [String] = []
    for n in 0..<15 {
      let zero = try chaos.makeZero(storageKey: key)
      clientIDs.append(zero.clientID)
      _ = zero.mutate(
        E2EApp.create(.init(id: "\(chaos.prefix)\(n)", title: "Reopened \(n)", ownerId: nil)))
      zero.close()
    }
    let last = try chaos.makeZero(storageKey: key)
    defer { last.close() }
    let open = try last.liveQuery(E2EApp.open())
    let expected = Set((0..<15).map { "\(chaos.prefix)\($0)" })
    try await eventually("every mutation in Postgres", timeout: .seconds(30)) {
      try Postgres.openIssueIDs(chaos.prefix) == expected
    }
    for id in clientIDs { #expect(try Postgres.lastMutationID(id) == 1, "\(id)") }
    try await eventually("the last client to see them") { ids(open) == expected }
  }

  /// A relaunch while the server is down and mutations are pending: the
  /// new client shows them from its store, and pushes them once the
  /// server is back.
  @Test func relaunchWithPendingMutationsWhileServerIsDown() async throws {
    defer { chaos.cleanUp() }
    let proxy = try await ChaosProxy(upstream: E2E.url!)
    await proxy.set { $0.accept = .refuse }
    let key = UUID().uuidString
    let first = try chaos.makeZero(cacheURL: proxy.cacheURL, storageKey: key)
    let issueIDs = (0..<5).map { "\(chaos.prefix)pending-\($0)" }
    for id in issueIDs {
      _ = first.mutate(E2EApp.create(.init(id: id, title: "Pending", ownerId: nil)))
    }
    _ = first.mutate(E2EApp.close(issueIDs[0]))
    let firstID = first.clientID
    first.close()

    let second = try chaos.makeZero(cacheURL: proxy.cacheURL, storageKey: key)
    defer { second.close() }
    let open = try second.liveQuery(E2EApp.open())
    let expected = Set(issueIDs.dropFirst())
    try await eventually("the pending rows from the store") { ids(open) == expected }
    await proxy.set { $0.accept = .pass }
    try await eventually("the mutations in Postgres", timeout: .seconds(30)) {
      try Postgres.openIssueIDs(chaos.prefix) == expected
    }
    #expect(try Postgres.issues(chaos.prefix).count == 5)
    #expect(try Postgres.lastMutationID(firstID) == 6)
    try await eventually("the confirmed rows") {
      open.resultType == .complete && ids(open) == expected
    }
    second.close()
    await proxy.shutdown()
  }

  /// Clients opened and closed in a burst, some mid-handshake: every
  /// socket is closed.
  @Test func rapidOpenCloseLeavesNoSockets() async throws {
    defer { chaos.cleanUp() }
    let proxy = try await ChaosProxy(upstream: E2E.url!)
    for n in 0..<20 {
      let zero = try chaos.makeZero(cacheURL: proxy.cacheURL, sqlite: false)
      _ = try zero.liveQuery(E2EApp.open())
      try await Task.sleep(for: .milliseconds(n * 5))
      zero.close()
    }
    let deadline = ContinuousClock.now + .seconds(5)
    while await proxy.openConnections > 0, ContinuousClock.now < deadline {
      try await Task.sleep(for: .milliseconds(50))
    }
    #expect(await proxy.openConnections == 0)
    await proxy.shutdown()
  }

  /// A mutation too large for zero-cache (over its 10 MB message limit):
  /// the server closes with 1009. The client must not loop resending it,
  /// and must sync again (JS reloads with a new client group).
  @Test func oversizedMutationDoesNotWedgeTheClient() async throws {
    defer { chaos.cleanUp() }
    let logs = LogCollector()
    let zero = try chaos.makeZero(logs: logs)
    defer { zero.close() }
    let open = try zero.liveQuery(E2EApp.open())
    try await eventually("the open issues") { open.resultType == .complete }
    let big = zero.mutate(
      E2EApp.create(
        .init(
          id: "\(chaos.prefix)big", title: String(repeating: "x", count: 11 << 20), ownerId: nil)))
    _ = await settled(big.server, within: .seconds(15))
    try await Task.sleep(for: .seconds(2))
    let id = "\(chaos.prefix)after-big"
    let after = zero.mutate(E2EApp.create(.init(id: id, title: "After", ownerId: nil)))
    let afterOutcome = await settled(after.server, within: .seconds(20))
    print("[chaos] after the oversized mutation: \(String(describing: afterOutcome))")
    try await eventually("a mutation after the oversized one", timeout: .seconds(20)) {
      try Postgres.openIssueIDs(chaos.prefix).contains(id)
    }
    print("[chaos] 1009 closes: \(logs.count("code=1009"))")
    #expect(logs.count("code=1009") <= 1)
    #expect(try !Postgres.openIssueIDs(chaos.prefix).contains("\(chaos.prefix)big"))
  }
}
