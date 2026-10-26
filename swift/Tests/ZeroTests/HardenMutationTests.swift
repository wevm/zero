import Testing
import ZeroTesting

@testable import Zero

/// Mutation edge cases the client scenario harness can't express: a mutate
/// from inside a mutator or a view listener, and the Swift-only mutation
/// outcomes. Expected values come from the JS client
/// (`packages/zero-client/src/client`, `packages/replicache/src`) where it
/// has a counterpart.
@Suite @MainActor struct HardenMutationTests {
  struct Seen: Codable, Sendable {
    var mid: Int
    var seen: [String]
  }

  @MainActor enum Nested {
    static var zero: Zero<Void>?
    static var inner: MutatorResult?
  }

  static func row(_ id: String) -> Issue {
    Issue(id: id, title: id, description: "", closed: false, createdAt: 1)
  }

  static func ids(_ tx: Transaction) throws -> [String] {
    try tx.run(zql.issue).map(\.id)
  }

  static let create = defineMutator("t.create") { (tx, id: String) -> Seen in
    try tx.mutate.issue.insert(row(id))
    return Seen(mid: tx.mutationID, seen: try ids(tx))
  }
  static let createThenThrow = defineMutator("t.createThenThrow") { (tx, id: String) in
    try tx.mutate.issue.insert(row(id))
    throw ApplicationError("midway")
  }
  /// Writes, calls `zero.mutate` without waiting for it, and writes again.
  static let outer = defineMutator("t.outer") { (tx, _: String) -> Seen in
    try tx.mutate.issue.insert(row("outer1"))
    Nested.inner = Nested.zero?.mutate(create("inner"))
    try tx.mutate.issue.insert(row("outer2"))
    return Seen(mid: tx.mutationID, seen: try ids(tx))
  }

  /// Closes the `Zero` it runs in, then writes again.
  static let closing = defineMutator("t.closing") { (tx, _: String) -> String in
    try tx.mutate.issue.insert(row("a"))
    Nested.zero?.close()
    try tx.mutate.issue.insert(row("b"))
    return "done"
  }

  static let all = defineQuery("issues.all") { () in zql.issue.orderBy(\.id, .asc) }

  let scheduler = ManualScheduler(now: 1_700_000_000_000)
  let sockets = MockSocketFactory()

  func makeZero(storageKey: String = randomID()) throws -> Zero<Void> {
    var options = ZeroOptions(
      cacheURL: "http://zero.test", userID: "u", storageKey: storageKey, storage: .memory)
    options.logSink = RecordingLogSink()
    return try Zero(
      schema: .test, mutators: [Self.create, Self.createThenThrow, Self.outer, Self.closing],
      options: options, scheduler: scheduler, socketFactory: sockets)
  }

  func connect() throws -> MockSocket {
    scheduler.runDueTimers()
    let socket = try #require(sockets.sockets.last)
    socket.receive(text: #"["connected",{"wsid":"w"}]"#)
    scheduler.runDueTimers()
    return socket
  }

  func poke(_ socket: MockSocket, from base: String?, to cookie: String, _ part: String) {
    let base = base.map { "\"\($0)\"" } ?? "null"
    socket.receive(text: #"["pokeStart",{"pokeID":"\#(cookie)","baseCookie":\#(base)}]"#)
    socket.receive(text: #"["pokePart",{"pokeID":"\#(cookie)",\#(part)}]"#)
    socket.receive(text: #"["pokeEnd",{"pokeID":"\#(cookie)","cookie":"\#(cookie)"}]"#)
    scheduler.runDueTimers()
  }

  static func serverRow(_ id: String) -> String {
    #"{"op":"put","tableName":"issues","value":{"id":"\#(id)","title":"\#(id)","description":"","closed":false,"owner_id":null,"createdAt":1}}"#
  }

  func pushed(on socket: MockSocket) -> [JSONValue] {
    socket.messages.compactMap { try? JSONParser.parse(Array($0.utf8)) }
      .filter { $0[0]?.stringValue == "push" }
      .flatMap { $0[1]?["mutations"]?.arrayValue ?? [] }
  }

  func pushedIDs(on socket: MockSocket) -> [Double] {
    pushed(on: socket).compactMap { $0["id"]?.numberValue }
  }

  func pushedNames(on socket: MockSocket) -> [String] {
    pushed(on: socket).compactMap { $0["name"]?.stringValue }
  }

  // MARK: - Reentrant mutate

  /// A mutator that calls `zero.mutate` without awaiting it. JS runs a
  /// mutation under Replicache's write lock (`#mutate` in
  /// `replicache-impl.ts` takes `withWriteNoImplicitCommit`), so the inner
  /// mutation waits for the outer one to commit and gets the next mutation
  /// ID. Checked with a throwaway TestZero test, which printed: outer
  /// `{mid: 1, seen: [outer1, outer2]}`, inner `{mid: 2, seen: [inner,
  /// outer1, outer2]}`, rows `[inner, outer1, outer2]`, and the next
  /// mutation `mid: 3`.
  @Test func mutateFromInsideAMutatorRunsAfterIt() async throws {
    let zero = try makeZero()
    Nested.zero = zero
    defer { Nested.zero = nil }
    let socket = try connect()

    let outer = zero.mutate(Self.outer("x"))
    scheduler.runDueTimers()
    #expect(
      try await outer.client.value
        == .success(data: ["mid": 1, "seen": ["outer1", "outer2"]]))
    let inner = try #require(Nested.inner)
    #expect(
      try await inner.client.value
        == .success(data: ["mid": 2, "seen": ["inner", "outer1", "outer2"]]))
    #expect(
      try await zero.run(zql.issue.orderBy(\.id, .asc)).map(\.id) == [
        "inner", "outer1", "outer2",
      ])
    #expect(zero.engine.store.pending.map(\.id) == [1, 2])

    let after = zero.mutate(Self.create("after"))
    scheduler.runDueTimers()
    #expect(
      try await after.client.value
        == .success(data: ["mid": 3, "seen": ["after", "inner", "outer1", "outer2"]]))
    #expect(pushedIDs(on: socket) == [1, 2, 3])
    #expect(pushedNames(on: socket) == ["t.outer", "t.create", "t.create"])
  }

  /// A mutator that closes its `Zero`. JS (checked with a throwaway
  /// TestZero test) commits it: `client` is `{type: 'success', data:
  /// 'done'}`, and `server` is the zero error "Zero was explicitly closed by
  /// calling zero.close()".
  @Test func closeFromInsideAMutator() async throws {
    let zero = try makeZero()
    Nested.zero = zero
    defer { Nested.zero = nil }
    _ = try connect()
    let result = zero.mutate(Self.closing("x"))
    scheduler.runDueTimers()
    #expect(zero.closed)
    #expect(try await result.client.value == .success(data: "done"))
    #expect(
      try await result.server.value
        == .zeroError(message: "Zero was explicitly closed by calling zero.close()"))
  }

  /// A view listener that mutates while a poke rebases the pending
  /// mutations: the new mutation goes on top of the rebased ones, is pushed
  /// once after them, and the view ends with every row (in JS the listener's
  /// mutate waits for the poke's write to finish).
  @Test func mutateFromAViewListenerDuringARebase() throws {
    let zero = try makeZero()
    let socket = try connect()
    let view = try zero.materialize(Self.all())
    poke(
      socket, from: nil, to: "01",
      #""gotQueriesPatch":[{"op":"put","hash":"\#(QueryHash.hashOfNameAndArgs("issues.all", []))"}]"#
    )
    zero.mutate(Self.create("a"))
    scheduler.runDueTimers()
    var reentered = false
    _ = view.addListener { data, _, _ in
      let ids = data.json?.arrayValue?.compactMap { $0["id"]?.stringValue } ?? []
      if !reentered, ids.contains("s") {
        reentered = true
        zero.mutate(Self.create("fromListener"))
      }
    }
    poke(socket, from: "01", to: "02", #""rowsPatch":[\#(Self.serverRow("s"))]"#)
    scheduler.runDueTimers()
    #expect(reentered)
    #expect(
      view.data.json?.arrayValue?.compactMap { $0["id"]?.stringValue } == [
        "a", "fromListener", "s",
      ])
    #expect(zero.engine.store.pending.map(\.id) == [1, 2])
    #expect(pushedIDs(on: socket) == [1, 2])

    poke(
      socket, from: "02", to: "03",
      #""lastMutationIDChanges":{"\#(zero.clientID)":2},"rowsPatch":[\#(Self.serverRow("a")),\#(Self.serverRow("fromListener"))]"#
    )
    #expect(zero.engine.store.pending.isEmpty)
    #expect(
      view.data.json?.arrayValue?.compactMap { $0["id"]?.stringValue } == [
        "a", "fromListener", "s",
      ])
    view.destroy()
  }

  // MARK: - Mutation outcomes (Swift only)

  /// The outcome is reported once: by the lmid that confirms the mutation,
  /// not again by a push response that arrives after it.
  @Test func outcomeIsReportedOnceWhenAPushResponseFollowsTheLmid() throws {
    let zero = try makeZero()
    var outcomes: [MutationServerOutcome] = []
    _ = zero.addMutationOutcomeListener { outcomes.append($0) }
    let socket = try connect()
    zero.mutate(Self.create("a"))
    scheduler.runDueTimers()
    poke(socket, from: nil, to: "01", #""lastMutationIDChanges":{"\#(zero.clientID)":1}"#)
    socket.receive(
      text:
        #"["pushResponse",{"mutations":[{"id":{"clientID":"\#(zero.clientID)","id":1},"result":{"error":"app","message":"late"}}]}]"#
    )
    scheduler.runDueTimers()
    #expect(
      outcomes == [
        MutationServerOutcome(
          mutationID: 1, name: "t.create", args: "a", outcome: .success(data: nil))
      ])
  }

  /// A mutation whose mutator threw on the client was never sent, so it
  /// has no server outcome; the next mutation takes its ID and reports its
  /// own name and arguments.
  @Test func aMutationThatThrewOnTheClientHasNoOutcome() throws {
    let zero = try makeZero()
    var outcomes: [MutationServerOutcome] = []
    _ = zero.addMutationOutcomeListener { outcomes.append($0) }
    let socket = try connect()
    zero.mutate(Self.createThenThrow("x"))
    zero.mutate(Self.create("y"))
    scheduler.runDueTimers()
    socket.receive(
      text:
        #"["pushResponse",{"mutations":[{"id":{"clientID":"\#(zero.clientID)","id":1},"result":{"data":1}}]}]"#
    )
    poke(socket, from: nil, to: "01", #""lastMutationIDChanges":{"\#(zero.clientID)":1}"#)
    #expect(
      outcomes == [
        MutationServerOutcome(
          mutationID: 1, name: "t.create", args: "y", outcome: .success(data: 1))
      ])
  }

  /// Listeners added before the server lost the client hear the outcomes
  /// of the mutations of the client that replaces it.
  @Test func outcomesOfTheReplacementClientAreReported() throws {
    let zero = try makeZero()
    var outcomes: [MutationServerOutcome] = []
    _ = zero.addMutationOutcomeListener { outcomes.append($0) }
    let first = try connect()
    first.receive(
      text: #"["error",{"kind":"ClientNotFound","message":"gone","origin":"zeroCache"}]"#)
    scheduler.runDueTimers()
    let second = try connect()
    #expect(second !== first)
    zero.mutate(Self.create("a"))
    scheduler.runDueTimers()
    second.receive(
      text:
        #"["pushResponse",{"mutations":[{"id":{"clientID":"\#(zero.clientID)","id":1},"result":{"error":"app","message":"no","details":{"code":1}}}]}]"#
    )
    scheduler.runDueTimers()
    #expect(
      outcomes == [
        MutationServerOutcome(
          mutationID: 1, name: "t.create", args: "a",
          outcome: .appError(message: "no", details: ["code": 1]))
      ])
  }
}
