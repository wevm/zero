import Foundation
import Testing
import ZeroTesting

@testable import Zero

/// Bad input that traps the process. The JS client throws, logs, or
/// disconnects on each of these; a trap in Swift kills the app. Each
/// scenario runs in a child process (an exit test), so a trap fails only
/// its own test.
@Suite @MainActor struct HardenCrashTests {
  static let create = defineMutator("issue.create") { (tx, id: String) in
    try tx.mutate.insert(
      Issue(id: id, title: "", description: "", closed: false, createdAt: 1))
  }

  /// A client on memory, a manual clock, and a mock socket.
  @MainActor struct Harness {
    let scheduler = ManualScheduler(now: 1_700_000_000_000)
    let sockets = MockSocketFactory()
    let zero: Zero<Void>

    init() throws {
      var options = ZeroOptions(
        cacheURL: "http://zero.test", userID: "u", storageKey: randomID(), storage: .memory)
      options.logSink = RecordingLogSink()
      zero = try Zero(
        schema: .test, mutators: [HardenCrashTests.create], options: options,
        scheduler: scheduler, socketFactory: sockets)
    }

    func connect() throws -> MockSocket { try Self.connect(sockets, scheduler) }

    func poke(_ socket: MockSocket, from base: String?, to cookie: String, _ part: String) {
      Self.poke(socket, scheduler, from: base, to: cookie, part)
    }

    /// Opens the newest socket and acknowledges it.
    static func connect(
      _ sockets: MockSocketFactory, _ scheduler: ManualScheduler
    ) throws
      -> MockSocket
    {
      scheduler.runDueTimers()
      let socket = try #require(sockets.sockets.last)
      socket.receive(text: #"["connected",{"wsid":"w"}]"#)
      scheduler.runDueTimers()
      return socket
    }

    /// A poke of one part.
    static func poke(
      _ socket: MockSocket, _ scheduler: ManualScheduler, from base: String?, to cookie: String,
      _ part: String
    ) {
      let base = base.map { "\"\($0)\"" } ?? "null"
      socket.receive(text: #"["pokeStart",{"pokeID":"\#(cookie)","baseCookie":\#(base)}]"#)
      socket.receive(text: #"["pokePart",{"pokeID":"\#(cookie)",\#(part)}]"#)
      socket.receive(text: #"["pokeEnd",{"pokeID":"\#(cookie)","cookie":"\#(cookie)"}]"#)
      scheduler.runDueTimers()
    }
  }

  // MARK: - Rows the server sends

  /// An `Int` column holding -2^63, a number JSON can carry and a bigint
  /// column of Postgres can hold. JS hands the number through. Swift:
  /// `Int(exactly:)` gives `Int.min`, and `abs(Int.min)` overflows.
  @Test func intColumnAtInt64Min() async {
    await #expect(processExitsWith: .success) {
      let decoded = try? Int(zeroValue: .number(-0x1p63))
      // Not an integer within 2^53: a decoding error, not a trap.
      precondition(decoded == nil)
    }
  }

  // MARK: - Last mutation IDs the server sends

  /// The server's last mutation ID for this client is any JSON number. JS
  /// gives the next mutation the ID `lmid + 1`, a number. Swift traps
  /// converting it to `Transaction.mutationID`, an `Int`.
  @Test func hugeLastMutationIDThenMutate() async {
    await #expect(processExitsWith: .success) {
      try await HardenCrashTests.hugeLastMutationIDThenMutateScenario()
    }
  }

  static func hugeLastMutationIDThenMutateScenario() throws {
    let h = try Harness()
    let socket = try h.connect()
    h.poke(socket, from: nil, to: "1", #""lastMutationIDChanges":{"\#(h.zero.clientID)":1e300}"#)
    _ = h.zero.mutate(Self.create("a"))
  }

  /// The same through storage: a pending mutation whose stored ID is huge
  /// (a tampered or corrupt file). Opening the client replays it. JS
  /// replays it with that ID. Swift traps converting it to an `Int`, on
  /// every launch.
  @Test func hugeStoredMutationIDReplayed() async {
    await #expect(processExitsWith: .success) {
      try await HardenCrashTests.hugeStoredMutationIDReplayedScenario()
    }
  }

  static func hugeStoredMutationIDReplayedScenario() throws {
    let directory = NSTemporaryDirectory() + "harden-" + randomID()
    defer { try? FileManager.default.removeItem(atPath: directory) }
    func open() throws -> Zero<Void> {
      var options = ZeroOptions(
        userID: "u", storageKey: "k", storage: .sqlite(directory: directory))
      options.logSink = RecordingLogSink()
      return try Zero(
        schema: .test, mutators: [HardenCrashTests.create], options: options,
        scheduler: ManualScheduler(now: 1_700_000_000_000), socketFactory: nil)
    }
    let first = try open()
    first.mutate(Self.create("a"))
    first.close()
    let path =
      directory + "/"
      + ReplicaDatabase.fileName(
        ReplicaDatabase.name(
          userID: "u", storageKey: "k", mutateURL: nil, queryURL: nil,
          schemaHash: ZeroSchema.test.clientSchema.hash))
    let db = try SQLiteDatabase(path: path)
    try db.run("UPDATE mutations SET id = 1e300")
    var ids: [Double] = []
    try db.query("SELECT id FROM mutations") { ids.append($0.real(0)) }
    precondition(ids == [1e300])
    try open().close()
  }

  // MARK: - Row patches the server sends

  /// `update` row ops whose `merge` leaves the row without its primary key
  /// (no row existed, so the merge is the whole row). Both rows have a null
  /// key, so to the table's source they are one row. JS: `MemorySource`'s
  /// `assert` throws "Row already exists", the poke fails, and the client
  /// disconnects. Swift: the same check is a `precondition`.
  @Test func updateOpsLeavingRowsWithoutPrimaryKey() async {
    await #expect(processExitsWith: .success) {
      try await HardenCrashTests.updateOpsLeavingRowsWithoutPrimaryKeyScenario()
    }
  }

  static func updateOpsLeavingRowsWithoutPrimaryKeyScenario() throws {
    let h = try Harness()
    let view = try h.zero.materialize(zql.issue)
    defer { view.destroy() }
    let socket = try h.connect()
    h.poke(
      socket, from: nil, to: "1",
      #""rowsPatch":[{"op":"update","tableName":"issues","id":{"id":"a"},"merge":{"title":"x"}},{"op":"update","tableName":"issues","id":{"id":"b"},"merge":{"title":"y"}}]"#
    )
  }

  /// An `update` that moves a row to the primary key of another, then
  /// deletes of both. JS: `MemorySource` throws "Row not found". Swift
  /// traps.
  @Test func updateOpChangingPrimaryKeyThenDeletes() async {
    await #expect(processExitsWith: .success) {
      try await HardenCrashTests.updateOpChangingPrimaryKeyThenDeletesScenario()
    }
  }

  static func updateOpChangingPrimaryKeyThenDeletesScenario() throws {
    let h = try Harness()
    let view = try h.zero.materialize(zql.issue)
    defer { view.destroy() }
    let socket = try h.connect()
    func row(_ id: String) -> String {
      #"{"op":"put","tableName":"issues","value":{"id":"\#(id)","title":"","description":"","closed":false,"owner_id":null,"createdAt":1}}"#
    }
    h.poke(socket, from: nil, to: "1", #""rowsPatch":[\#(row("a")),\#(row("b"))]"#)
    h.poke(
      socket, from: "1", to: "2",
      #""rowsPatch":[{"op":"update","tableName":"issues","id":{"id":"a"},"merge":{"id":"b"}}]"#)
    h.poke(
      socket, from: "2", to: "3",
      #""rowsPatch":[{"op":"del","tableName":"issues","id":{"id":"a"}},{"op":"del","tableName":"issues","id":{"id":"b"}}]"#
    )
  }

  /// Rows that don't match the schema: wrong types, missing and extra
  /// columns, and a table the schema doesn't have. JS keeps them as sent;
  /// a typed read of a row that doesn't fit throws rather than traps.
  @Test func rowsThatDoNotMatchTheSchema() async throws {
    let h = try Harness()
    let view = try h.zero.materialize(zql.issue.orderBy(\.id, .asc))
    defer { view.destroy() }
    let socket = try h.connect()
    h.poke(
      socket, from: nil, to: "1",
      #""rowsPatch":[{"op":"put","tableName":"issues","value":{"id":1,"title":2,"description":null,"closed":"yes","owner_id":{},"createdAt":"x","extra":[1]}},{"op":"put","tableName":"issues","value":{"id":"m"}},{"op":"put","tableName":"issues","value":{"id":true,"createdAt":1e400}},{"op":"put","tableName":"nope","value":{"id":"z"}}]"#
    )
    #expect(!socket.closed)
    #expect(view.data.entries.count == 3)
    await #expect(throws: ZeroValueError.self) { try await h.zero.run(zql.issue) }
    let one = try await h.zero.run(zql.issue.where(\.id, "m").one())
    #expect(one?.title == "" && one?.createdAt == 0 && one?.closed == false)
  }

  /// A row without its primary key, or with a null one. JS: parsing the key
  /// throws, the poke fails, and the client reconnects.
  @Test(arguments: [#"{"title":"t"}"#, #"{"id":null}"#, #"{"id":[1]}"#])
  func rowWithoutAUsablePrimaryKey(_ value: String) throws {
    let h = try Harness()
    let socket = try h.connect()
    h.poke(
      socket, from: nil, to: "1",
      #""rowsPatch":[{"op":"put","tableName":"issues","value":\#(value)}]"#
    )
    #expect(socket.closed)
  }

  // MARK: - Timers

  /// A backoff error from zero-cache with a huge `minBackoffMs`. JS passes
  /// it to `setTimeout`, which fires at once for a delay past 2^31 - 1 ms.
  /// Swift: `SystemScheduler` makes a `Duration` of it, which traps.
  @Test(.timeLimit(.minutes(1))) func hugeServerBackoff() async {
    await #expect(processExitsWith: .success) {
      try await HardenCrashTests.hugeServerBackoffScenario()
    }
  }

  static func hugeServerBackoffScenario() async throws {
    let sockets = MockSocketFactory()
    let zero = try Zero(
      schema: .test,
      options: ZeroOptions(
        cacheURL: "http://zero.test", userID: "u", storageKey: randomID(), storage: .memory),
      scheduler: SystemScheduler(), socketFactory: sockets)
    try await Task.sleep(for: .milliseconds(100))
    let socket = try #require(sockets.sockets.last)
    socket.receive(
      text:
        #"["error",{"kind":"Rebalance","message":"m","origin":"zeroCache","minBackoffMs":1e300}]"#)
    try await Task.sleep(for: .milliseconds(100))
    zero.close()
  }

  /// `pingTimeoutMs: .infinity`. JS: `setTimeout(f, Infinity)` fires at
  /// once. Swift traps making a `Duration` of it.
  @Test(.timeLimit(.minutes(1))) func infinitePingTimeout() async {
    await #expect(processExitsWith: .success) {
      try await HardenCrashTests.infinitePingTimeoutScenario()
    }
  }

  static func infinitePingTimeoutScenario() async throws {
    let sockets = MockSocketFactory()
    var options = ZeroOptions(
      cacheURL: "http://zero.test", userID: "u", storageKey: randomID(), storage: .memory)
    options.pingTimeoutMs = .infinity
    let zero = try Zero(
      schema: .test, options: options, scheduler: SystemScheduler(), socketFactory: sockets)
    try await Task.sleep(for: .milliseconds(100))
    let socket = try #require(sockets.sockets.last)
    socket.receive(text: #"["connected",{"wsid":"w"}]"#)
    try await Task.sleep(for: .milliseconds(100))
    zero.close()
  }

  // MARK: - Query arguments the app passes

  /// `limit(-1)`. JS throws "Limit must be non-negative" from `limit`.
  /// Swift: a `precondition`.
  @Test func negativeLimit() async {
    await #expect(processExitsWith: .success) {
      _ = await MainActor.run { zql.issue.limit(-1) }
    }
  }

  /// A limit past 2^31 - 1, such as `Int.max` for "no limit". JS throws
  /// "Limit must be an integer" (`(limit | 0) !== limit`). Swift: a
  /// `precondition`.
  @Test func limitPastInt32() async {
    await #expect(processExitsWith: .success) {
      _ = await MainActor.run { zql.issue.limit(Int.max) }
    }
  }

  /// `whereExists` whose subquery asks for `related` rows. JS: the
  /// builder's `assert` throws "EXISTS subqueries must not have related"
  /// when the query is materialized. Swift: a `precondition`.
  @Test func existsSubqueryWithRelated() async {
    await #expect(processExitsWith: .success) {
      try await HardenCrashTests.existsSubqueryWithRelatedScenario()
    }
  }

  static func existsSubqueryWithRelatedScenario() throws {
    let h = try Harness()
    let view = try? h.zero.materialize(zql.issue.whereExists(\.comments) { $0.related(\.author) })
    view?.destroy()
  }

  /// `limit` on a many-to-many relationship (through a junction table),
  /// such as the first label of each issue. JS throws `NotImplementedError`
  /// ("Limit is not supported in junction relationships yet") from the
  /// query builder. Swift: a `preconditionFailure`.
  @Test func limitOnAJunctionRelationship() async {
    await #expect(processExitsWith: .success) {
      _ = await MainActor.run { zql.issue.related(\.labels) { $0.limit(1) } }
    }
  }
}
