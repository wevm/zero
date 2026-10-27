import Testing
import ZeroTesting

@testable import Zero

/// Long sessions compressed with a manual clock: reconnects, views and
/// live queries made and dropped, preloads, mutations, a replaced client,
/// and close. Each test asserts, with weak references, that what it dropped
/// is freed, and that a closed client leaves no timer running.
@Suite @MainActor struct HardenLeakTests {
  static let create = defineMutator("issue.create") { (tx, id: String) in
    try tx.mutate.issue.insert(
      Issue(id: id, title: "", description: "", closed: false, createdAt: 1))
  }
  static let all = defineQuery("issues.all") { () in zql.issue.orderBy(\.id, .asc) }
  static let byID = defineQuery("issues.byID") { (id: String) in zql.issue.where(\.id, id) }

  let scheduler = ManualScheduler(now: 1_700_000_000_000)

  func makeZero(_ sockets: any SocketFactory) throws -> Zero<Void> {
    var options = ZeroOptions(
      cacheURL: "http://zero.test", userID: "u", storageKey: randomID(), storage: .memory)
    options.logSink = RecordingLogSink()
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

  /// A client used and closed, then dropped, is freed with its engine, and
  /// leaves no timer behind.
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

  /// A client dropped without `close()`, while connecting or connected,
  /// is freed, closes its socket, leaves no timer running, and settles
  /// what was waiting on it, as `close()` does.
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

  /// A view dropped while a mutation is pending, on a client then closed
  /// and dropped, is freed.
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

  /// After `replaceClient`, the old engine is freed and the views moved to
  /// the new one keep working.
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
}
