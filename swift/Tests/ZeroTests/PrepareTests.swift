import Foundation
import Testing
import ZeroTesting

@testable import Zero

/// `Zero.prepare`: the client that takes over a store opened, and views
/// hydrated, off the main actor is the client `Zero.init` makes on that
/// store, also when the store changed or is open meanwhile.
@Suite @MainActor struct PrepareTests {
  static let create = defineMutator("issue.create") { (tx, id: String) in
    try tx.mutate.issue.insert(
      Issue(id: id, title: "", description: "", closed: false, createdAt: 1))
  }
  static let all = defineQuery("issues.all") { () in zql.issue.orderBy(\.id, .asc) }
  static let withComments = defineQuery("issues.withComments") { () in
    zql.issue.related(\.comments).orderBy(\.id, .desc)
  }
  static let byID = defineQuery("issues.byID") { (id: String) in zql.issue.where(\.id, id) }

  let directory = NSTemporaryDirectory() + "zero-prepare-tests-" + UUID().uuidString
  let scheduler = ManualScheduler(now: 1_700_000_000_000)
  let sockets = MockSocketFactory()

  func options(_ storage: ZeroStorage? = nil) -> ZeroOptions {
    var options = ZeroOptions(
      cacheURL: "http://zero.test", userID: "u", storageKey: "k",
      storage: storage ?? .sqlite(directory: directory))
    options.logSink = RecordingLogSink()
    return options
  }

  func makeZero(_ storage: ZeroStorage? = nil) throws -> Zero<Void> {
    try Zero(
      schema: .test, mutators: [Self.create], options: options(storage), scheduler: scheduler,
      socketFactory: sockets)
  }

  func prepare(
    _ storage: ZeroStorage? = nil, _ queries: [any PreparableQuery<Void>]
  ) -> PreparedZero<Void> {
    let prepared = Zero.prepare(
      schema: .test, mutators: [Self.create], options: options(storage), queries: queries)
    prepared.wait()
    return prepared
  }

  func takeOver(_ prepared: PreparedZero<Void>) throws -> Zero<Void> {
    try Zero(prepared, scheduler: scheduler, socketFactory: sockets)
  }

  /// Stores the server's issues 1 and 3, a comment on 1, and that it has
  /// `issues.all`, then a mutation creating issue 2 it hasn't confirmed.
  func seed() throws {
    let zero = try makeZero()
    scheduler.runDueTimers()
    let socket = try #require(sockets.sockets.last)
    socket.receive(text: #"["connected",{"wsid":"w"}]"#)
    scheduler.runDueTimers()
    let hash = QueryHash.hashOfNameAndArgs("issues.all", [])
    let issue = { (id: String) in
      #"{"op":"put","tableName":"issues","value":{"id":"\#(id)","title":"","description":"","closed":false,"owner_id":null,"createdAt":1}}"#
    }
    let comment =
      #"{"op":"put","tableName":"comments","value":{"id":"c1","authorId":"u","issue_id":"i1","text":"hi","createdAt":2}}"#
    socket.receive(text: #"["pokeStart",{"pokeID":"p1","baseCookie":null}]"#)
    socket.receive(
      text:
        #"["pokePart",{"pokeID":"p1","gotQueriesPatch":[{"op":"put","hash":"\#(hash)"}],"rowsPatch":[\#(issue("i1")),\#(issue("i3")),\#(comment)]}]"#
    )
    socket.receive(text: #"["pokeEnd",{"pokeID":"p1","cookie":"c1"}]"#)
    scheduler.runDueTimers()
    zero.mutate(Self.create("i2"))
    zero.close()
  }

  @Test func theClientHasTheDataOfOneInitMakes() throws {
    defer { try? FileManager.default.removeItem(atPath: directory) }
    try seed()
    let prepared = prepare(nil, [Self.all(), Self.withComments()])

    let zero = try takeOver(prepared)
    #expect(zero.engine.context.preparedViews.count == 2)
    let all = try zero.liveQuery(Self.all())
    let withComments = try zero.liveQuery(Self.withComments())
    // Both views are the prepared ones, with the pending mutation replayed
    // over them.
    #expect(zero.engine.context.preparedViews.isEmpty)
    #expect(all.data.map(\.id) == ["i1", "i2", "i3"])
    #expect(all.resultType == .cached)
    #expect(withComments.data.map(\.id) == ["i3", "i2", "i1"])
    #expect(withComments.data.map { $0.comments?.map(\.text) } == [[], [], ["hi"]])
    #expect(withComments.resultType == .unknown)
    #expect(zero.engine.store.pending.count == 1)
    let preparedData = (all.data, withComments.data)
    zero.close()

    let fresh = try makeZero()
    #expect(try fresh.liveQuery(Self.all()).data == preparedData.0)
    #expect(try fresh.liveQuery(Self.withComments()).data == preparedData.1)
    #expect(fresh.clientGroupID == zero.clientGroupID)
    fresh.close()
  }

  @Test func aViewIsTakenOnlyByItsQueryAndTheOthersGoAtTheFirstChange() throws {
    defer { try? FileManager.default.removeItem(atPath: directory) }
    try seed()
    let zero = try takeOver(prepare(nil, [Self.byID("i1")]))
    let source = try #require(zero.engine.ivmMain.source("issue"))

    let i3 = try zero.liveQuery(Self.byID("i3"))
    #expect(i3.data.map(\.id) == ["i3"])
    #expect(zero.engine.context.preparedViews.count == 1)
    #expect(source.connections.count == 2)

    zero.mutate(Self.create("i4"))
    #expect(zero.engine.context.preparedViews.isEmpty)
    #expect(source.connections.count == 1)
    #expect(try zero.liveQuery(Self.byID("i1")).data.map(\.id) == ["i1"])
    zero.close()
  }

  /// A client of the same database that wrote to it, and closed, between
  /// `prepare` and the take-over: the client reads the store again.
  @Test func aWriteToTheStoreMeanwhileIsKept() async throws {
    defer { try? FileManager.default.removeItem(atPath: directory) }
    try seed()
    let prepared = prepare(nil, [Self.all()])

    let other = try makeZero()
    other.mutate(Self.create("i4"))
    other.close()

    let zero = try takeOver(prepared)
    #expect(try zero.liveQuery(Self.all()).data.map(\.id) == ["i1", "i2", "i3", "i4"])
    #expect(zero.engine.store.pending.count == 2)
    zero.close()
  }

  /// A database open in the process when the client takes over is shared,
  /// as `Zero.init` shares it; so is it with a second client of one
  /// preparation.
  @Test func aDatabaseOpenInTheProcessIsShared() throws {
    defer { try? FileManager.default.removeItem(atPath: directory) }
    try seed()
    let open = try makeZero()
    let prepared = prepare(nil, [Self.all()])
    let zero = try takeOver(prepared)
    #expect(zero.engine.database === open.engine.database)
    #expect(try zero.liveQuery(Self.all()).data.map(\.id) == ["i1", "i2", "i3"])
    open.close()

    let second = try takeOver(prepared)
    #expect(second.engine.database === zero.engine.database)
    #expect(second.engine.context.preparedViews.isEmpty)
    zero.close()
    second.close()
  }

  @Test func memoryStorageHasNothingToPrepare() async throws {
    let zero = try takeOver(prepare(.memory, [Self.all()]))
    zero.mutate(Self.create("i1"))
    #expect(try await zero.run(Self.all()).map(\.id) == ["i1"])
    zero.close()
  }
}
