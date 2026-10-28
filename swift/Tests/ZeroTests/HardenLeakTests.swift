import Testing
import ZeroTesting

@testable import Zero

/// Long sessions compressed with a manual clock: reconnects, views and live queries made and dropped, preloads, mutations, a replaced client, and close. Each test asserts, with weak references, that what it dropped is freed, and that a closed client leaves no timer running.
@Suite @MainActor struct HardenLeakTests {
  static let create = defineMutator("issue.create") { (tx, id: String) in
    try tx.mutate.issue.insert(
      Issue(id: id, title: "", description: "", closed: false, createdAt: 1))
  }
  static let all = defineQuery("issues.all") { () in zql.issue.orderBy(\.id, .asc) }
  static let byID = defineQuery("issues.byID") { (id: String) in zql.issue.where(\.id, id) }

  let scheduler = ManualScheduler(now: 1_700_000_000_000)

  let log = RecordingLogSink()

  func makeZero(_ sockets: any SocketFactory) throws -> Zero<Void> {
    var options = ZeroOptions(
      cacheURL: "http://zero.test", userID: "u", storageKey: randomID(), storage: .memory)
    options.logSink = log
    options.onUpdateNeeded = { _ in }
    return try Zero(
      schema: .test, mutators: [Self.create], options: options, scheduler: scheduler,
      socketFactory: sockets)
  }

  func connect(_ sockets: MockSocketFactory) throws -> MockSocket {
    scheduler.runDueTimers()
    let socket = try #require(sockets.sockets.last)
    socket.receive(text: #"["connected",{"wsid":"w"}]"#)
    scheduler.runDueTimers()
    return socket
  }

  /// A client used and closed, then dropped, is freed with its engine, and leaves no timer behind.
  @Test func closedClientIsFreed() throws {
    let sockets = MockSocketFactory()
    weak var weakZero: Zero<Void>?
    weak var weakEngine: Engine?
    try {
      let zero = try makeZero(sockets)
      weakZero = zero
      weakEngine = zero.engine
      _ = try connect(sockets)
      let view = try zero.materialize(Self.all())
      _ = view.addListener { _, _, _ in }
      let live = try zero.liveQuery(Self.byID("a"))
      _ = live
      zero.mutate(Self.create("a"))
      _ = zero.preload(Self.byID("b"))
      scheduler.advance(by: 30_000)
      zero.close()
    }()
    scheduler.advance(by: 60_000)
    #expect(weakZero == nil)
    #expect(weakEngine == nil)
    #expect(scheduler.pendingTimers == 0)
  }

  /// A client dropped without `close()`, while connecting or connected, is freed, closes its socket, leaves no timer running, and settles what was waiting on it, as `close()` does.
  @Test(arguments: [false, true]) func droppedClientIsFreed(connected: Bool) throws {
    let sockets = MockSocketFactory()
    weak var weakZero: Zero<Void>?
    weak var weakEngine: Engine?
    var mutation: MutatorResult?
    var preload: Preload?
    try {
      let zero = try makeZero(sockets)
      weakZero = zero
      weakEngine = zero.engine
      if connected { _ = try connect(sockets) } else { scheduler.runDueTimers() }
      let view = try zero.materialize(Self.all())
      _ = view
      mutation = zero.mutate(Self.create("a"))
      preload = zero.preload(Self.byID("a"))
    }()
    #expect(mutation?.server.isSettled == true)
    #expect(preload?.complete.isSettled == true)
    #expect(weakZero == nil)
    #expect(weakEngine == nil)
    #expect(sockets.sockets.allSatisfy { $0.closed })
    scheduler.advance(by: 120_000)
    #expect(scheduler.pendingTimers == 0)
  }

  /// A view dropped while a mutation is pending, on a client then closed and dropped, is freed.
  @Test func viewDroppedWithPendingMutationIsFreed() throws {
    let sockets = MockSocketFactory()
    weak var weakArrayView: ArrayView?
    weak var weakEngine: Engine?
    try {
      let zero = try makeZero(sockets)
      weakEngine = zero.engine
      _ = try connect(sockets)
      zero.mutate(Self.create("a"))
      try {
        let view = try zero.materialize(Self.all())
        weakArrayView = view.view
      }()
      zero.close()
    }()
    #expect(weakEngine == nil)
    #expect(weakArrayView == nil)
  }

  /// After `replaceClient`, the old engine is freed and the views moved to the new one keep working.
  @Test func replacedEngineIsFreed() throws {
    let sockets = MockSocketFactory()
    let zero = try makeZero(sockets)
    _ = try connect(sockets)
    let view = try zero.materialize(Self.all())
    let live = try zero.liveQuery(Self.byID("a"))
    let preload = zero.preload(Self.byID("b"))
    weak var weakEngine: Engine?
    weak var weakArrayView: ArrayView?
    for _ in 0..<20 {
      weakEngine = zero.engine
      weakArrayView = view.view
      zero.replaceClient()
      scheduler.runDueTimers()
      #expect(weakEngine == nil)
      #expect(weakArrayView == nil)
    }
    zero.mutate(Self.create("a"))
    #expect(view.data.entries.count == 1)
    #expect(live.data.count == 1)
    preload.cleanup()
    zero.close()
  }

  /// An hour connected, answering every ping, leaves the engine with what it had after the first minutes: the run loop's waits don't pile up.
  @Test func anHourConnectedDoesNotGrow() throws {
    let sockets = MockSocketFactory()
    let zero = try makeZero(sockets)
    let socket = try connect(sockets)
    let manager = zero.engine.connectionManager
    var pings = 0
    func run(minutes: Int) {
      for _ in 0..<(minutes * 60) {
        scheduler.advance(by: 1_000)
        if socket.messages.last?.hasPrefix(#"["ping""#) == true {
          pings += 1
          socket.receive(text: #"["pong",{}]"#)
          scheduler.runDueTimers()
        }
      }
    }
    run(minutes: 1)
    let waiters = manager.stateWaiters.count
    let timers = scheduler.pendingTimers
    run(minutes: 60)
    #expect(pings > 600)
    #expect(zero.connection.state.status == .connected)
    #expect(manager.stateWaiters.count == waiters)
    #expect(waiters <= 1)
    #expect(scheduler.pendingTimers == timers)
    zero.close()
  }

  /// The sizes of the collections the engine and its parts hold, by path, read by reflection so private state counts too.
  func collectionSizes(_ engine: Engine) -> [String: Int] {
    var sizes: [String: Int] = [:]
    func visit(_ value: Any, _ path: String, depth: Int) {
      let mirror = Mirror(reflecting: value)
      switch mirror.displayStyle {
      case .collection, .dictionary, .set:
        sizes[path] = mirror.children.count
        return
      case .optional:
        if let some = mirror.children.first { visit(some.value, path, depth: depth) }
        return
      default: break
      }
      guard depth > 0 else { return }
      for child in mirror.children {
        visit(child.value, path + "." + (child.label ?? "?"), depth: depth - 1)
      }
    }
    visit(engine, "engine", depth: 3)
    return sizes
  }

  /// Hours of churn: the server drops the connection every few minutes, and between drops views and live queries come and go, preloads expire, and mutations are confirmed. What the engine holds stays the same from one cycle to the next.
  @Test func reconnectAndViewChurnDoesNotGrow() throws {
    let sockets = MockSocketFactory()
    let zero = try makeZero(sockets)
    var cookie = 0
    var lmid = 0
    func cycle(_ i: Int) throws {
      let socket = try connect(sockets)
      #expect(
        zero.connection.state.status == .connected,
        "\(log.entries.filter { $0.level == .error }.map(\.message).prefix(3))")
      let view = try zero.materialize(Self.all())
      let live = try zero.liveQuery(Self.byID("x\(i % 3)"))
      _ = live
      let preload = zero.preload(Self.byID("p\(i % 5)"), ttl: .milliseconds(1_000))
      zero.mutate(Self.create("m\(i % 4)"))
      lmid += 1
      cookie += 1
      func c(_ n: Int) -> String { String(repeating: "0", count: 6 - String(n).count) + String(n) }
      let base = cookie == 1 ? "null" : #""\#(c(cookie - 1))""#
      socket.receive(text: #"["pokeStart",{"pokeID":"p\#(cookie)","baseCookie":\#(base)}]"#)
      socket.receive(
        text:
          #"["pokePart",{"pokeID":"p\#(cookie)","lastMutationIDChanges":{"\#(zero.clientID)":\#(lmid)}}]"#
      )
      socket.receive(text: #"["pokeEnd",{"pokeID":"p\#(cookie)","cookie":"\#(c(cookie))"}]"#)
      scheduler.runDueTimers()
      view.destroy()
      preload.cleanup()
      scheduler.advance(by: 120_000)
      socket.serverClose()
      scheduler.runDueTimers()
      scheduler.advance(by: 5_000)
    }
    for i in 0..<10 { try cycle(i) }
    let before = collectionSizes(zero.engine)
    let timers = scheduler.pendingTimers
    for i in 10..<210 { try cycle(i) }
    let after = collectionSizes(zero.engine)
    // The test's socket factory keeps every socket.
    for (path, size) in after where size > (before[path] ?? 0) && !path.contains("socketFactory") {
      Testing.Issue.record("\(path) grew from \(before[path] ?? 0) to \(size)")
    }
    #expect(after.count > 20)
    #expect(scheduler.pendingTimers <= timers)
    zero.close()
  }

  /// A snapshot stream iterated a little and dropped, and a mutation outcome stream likewise, let go of the view and the listener.
  @Test func droppedStreamsLetGo() async throws {
    let sockets = MockSocketFactory()
    let zero = try makeZero(sockets)
    weak var weakView: MaterializedView?
    var outcomes = 0
    do {
      let view = try zero.materialize(Self.all())
      weakView = view
      var seen = 0
      for await _ in view.snapshots {
        seen += 1
        if seen == 1 { zero.mutate(Self.create("a")) } else { break }
      }
      // Made and never iterated.
      _ = zero.mutationOutcomes
    }
    for _ in 0..<100 where weakView != nil { await Task.yield() }
    #expect(weakView == nil)
    let listeners = Mirror(reflecting: zero).children.first { $0.label == "outcomeListeners" }
    outcomes = (listeners?.value as? [Any])?.count ?? -1
    #expect(outcomes == 0)
    zero.close()
  }
}
