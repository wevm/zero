import CSQLite
import Foundation
import Synchronization
import Testing
import ZeroTesting

@testable import Zero

/// Storage and client-lifecycle edge cases, checked against the JS client
/// (`packages/replicache/src/persist`, `replicache-impl.ts`,
/// `packages/zero-client/src/client/zero.ts`) or, where JS has no
/// counterpart, against what DESIGN.md promises: no crash, no lost pending
/// mutations, and no silent divergence from the server.
@Suite(.serialized) @MainActor struct HardenStorageTests {
  static let day: Double = 24 * 60 * 60_000
  static let create = defineMutator("issue.create") { (tx, id: String) in
    try tx.mutate.insert(
      Issue(id: id, title: "", description: "", closed: false, createdAt: 1))
  }

  /// In a directory with `SQLiteFaults.isolatedMarker` in its name, so the
  /// faults these tests arm reach these files only, and those of other
  /// suites don't.
  let directory = NSTemporaryDirectory() + SQLiteFaults.isolatedMarker + UUID().uuidString

  final class Failures: @unchecked Sendable { var kinds: [StorageFailure.Kind] = [] }

  init() {
    _ = SQLiteFaults.installed
    SQLiteFaults.resetIsolated()
  }

  func cleanUp() {
    SQLiteFaults.resetIsolated()
    try? FileManager.default.removeItem(atPath: directory)
  }

  func makeZero(
    storageKey: String = "k", storage: ZeroStorage? = nil, at now: Double = 0,
    cacheURL: String? = nil, sockets: MockSocketFactory? = nil,
    scheduler: ManualScheduler? = nil,
    onStorageFailure: (@MainActor @Sendable (StorageFailure) -> Void)? = nil
  ) throws -> Zero<Void> {
    var options = ZeroOptions(
      cacheURL: cacheURL, userID: "u", storageKey: storageKey,
      storage: storage ?? .sqlite(directory: directory))
    options.onStorageFailure = onStorageFailure
    options.logSink = RecordingLogSink()
    return try Zero(
      schema: .test, mutators: [Self.create], options: options,
      scheduler: scheduler ?? ManualScheduler(now: now), socketFactory: sockets)
  }

  func fileName(storageKey: String = "k") -> String {
    ReplicaDatabase.fileName(
      ReplicaDatabase.name(
        userID: "u", storageKey: storageKey, mutateURL: nil, queryURL: nil,
        schemaHash: ZeroSchema.test.clientSchema.hash))
  }

  func databasePath(storageKey: String = "k") -> String {
    directory + "/" + fileName(storageKey: storageKey)
  }

  /// Lets the main actor run the tasks that report write failures.
  func settle(until done: () -> Bool = { false }) async throws {
    for _ in 0..<50 {
      if done() { return }
      try await Task.sleep(for: .milliseconds(10))
    }
  }

  /// Opens the newest socket and acknowledges it.
  func connect(_ sockets: MockSocketFactory, _ scheduler: ManualScheduler) throws -> MockSocket {
    scheduler.runDueTimers()
    let socket = try #require(sockets.sockets.last)
    socket.receive(text: #"["connected",{"wsid":"w"}]"#)
    scheduler.runDueTimers()
    return socket
  }

  func poke(
    _ socket: MockSocket, _ scheduler: ManualScheduler, _ id: String, from base: String?,
    to cookie: String, rows: [String]
  ) {
    let base = base.map { "\"\($0)\"" } ?? "null"
    socket.receive(text: #"["pokeStart",{"pokeID":"\#(id)","baseCookie":\#(base)}]"#)
    socket.receive(
      text: #"["pokePart",{"pokeID":"\#(id)","rowsPatch":[\#(rows.joined(separator: ","))]}]"#)
    socket.receive(text: #"["pokeEnd",{"pokeID":"\#(id)","cookie":"\#(cookie)"}]"#)
    scheduler.runDueTimers()
  }

  static func row(_ id: String, title: String = "") -> String {
    #"{"op":"put","tableName":"issues","value":{"id":"\#(id)","title":"\#(title)","description":"","closed":false,"owner_id":null,"createdAt":1}}"#
  }

  /// The page offset of `table`'s root page in the file at `path`.
  func rootPageOffset(of table: String, at path: String) throws -> Int64 {
    let db = try SQLiteDatabase(path: path)
    defer { db.close() }
    var pageSize: Int64 = 0
    var root: Int64 = 0
    try db.query("PRAGMA page_size") { pageSize = $0.integer(0) }
    try db.query("SELECT rootpage FROM sqlite_master WHERE name = ?", [.text(table)]) {
      root = $0.integer(0)
    }
    #expect(root > 1)
    return (root - 1) * pageSize
  }

  // MARK: - Collecting databases

  /// The collector decides from a database's heartbeats and then deletes
  /// its files, while the main actor may open that database in between: a
  /// client that opens a database no client used for two days, at the
  /// moment the collector of another database reads it, keeps writing to
  /// files that are gone. Its pending mutation is lost at the next launch.
  /// JS can't lose it this way: IndexedDB doesn't delete a database with
  /// an open connection under it, and Replicache's store then fails over
  /// to memory and says so (`idb-store.ts`, `#withReopen`).
  @Test(.timeLimit(.minutes(1)))
  func collectingADatabaseAClientJustOpenedKeepsItsMutations() async throws {
    defer { cleanUp() }
    let old = try makeZero(storageKey: "old", at: 0)
    old.close()
    let oldPath = databasePath(storageKey: "old")
    let offset = try rootPageOffset(of: "clients", at: oldPath)

    let now = 2 * Self.day + 60_000
    let scheduler = ManualScheduler(now: now)
    let current = try makeZero(storageKey: "current", scheduler: scheduler)
    // The collector's read of the old database's clients waits.
    SQLiteFaults.pauseRead(ofFile: fileName(storageKey: "old"), atOffset: offset)
    scheduler.advance(by: 5 * 60_000)
    for _ in 0..<500 where !SQLiteFaults.readPaused.load(ordering: .sequentiallyConsistent) {
      try await Task.sleep(for: .milliseconds(10))
    }
    let paused = SQLiteFaults.readPaused.load(ordering: .sequentiallyConsistent)
    try #require(paused)

    // Meanwhile the app opens the old database and makes a mutation.
    let reopened = try makeZero(storageKey: "old", at: now + 5 * 60_000)
    // It opened the file, not a memory fallback.
    #expect(reopened.clientGroupID == old.clientGroupID)
    reopened.mutate(Self.create("i1"))
    reopened.engine.database.flush()
    SQLiteFaults.resumeRead()
    current.engine.database.flush()
    #expect(FileManager.default.fileExists(atPath: oldPath), "deleted while open")
    reopened.close()
    current.close()

    let next = try makeZero(storageKey: "old", at: now + 6 * 60_000)
    #expect(next.engine.store.pending.map(\.id) == [1])
    #expect(next.clientGroupID == reopened.clientGroupID)
    next.close()
  }

  /// JS never collects a database of a newer format than it knows
  /// (`gatherDatabaseInfoForCollect` in `collect-idb-databases.ts`:
  /// `db.replicacheFormatVersion > FormatVersion.Latest` returns
  /// `[false]`), so an older app binary sharing the directory, an app
  /// extension or a downgrade, doesn't delete what a newer one wrote. Here
  /// every `zero-*.sqlite` file whose `clients` look old is deleted,
  /// whatever its layout version.
  @Test(.timeLimit(.minutes(1)))
  func doesNotCollectADatabaseOfANewerLayout() throws {
    defer { cleanUp() }
    let old = try makeZero(storageKey: "old", at: 0)
    old.close()
    // What a later layout version could write: the same tables, and more.
    let newer = directory + "/zero-v\(ReplicaDatabase.sqliteLayoutVersion + 1)-abc.sqlite"
    let db = try SQLiteDatabase(path: newer)
    try db.execute(
      """
      CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;
      INSERT INTO meta VALUES ('name', 'rep:zero-u-abc:8:53.x');
      CREATE TABLE clients (id TEXT, client_group_id TEXT, heartbeat REAL);
      INSERT INTO clients VALUES ('c', 'g', 0);
      CREATE TABLE pending (id INTEGER, mutation TEXT);
      INSERT INTO pending VALUES (1, '{}');
      """)
    db.close()
    // A file of the app's that happens to match `zero-*.sqlite`.
    let unrelated = directory + "/zero-notes.sqlite"
    let notes = try SQLiteDatabase(path: unrelated)
    try notes.execute("CREATE TABLE notes (body TEXT); INSERT INTO notes VALUES ('x');")
    notes.close()

    let scheduler = ManualScheduler(now: 2 * Self.day + 60_000)
    let current = try makeZero(storageKey: "current", scheduler: scheduler)
    scheduler.advance(by: 5 * 60_000)
    current.engine.database.flush()
    // The collection ran: the old database of this layout is gone.
    #expect(!FileManager.default.fileExists(atPath: databasePath(storageKey: "old")))
    #expect(FileManager.default.fileExists(atPath: newer))
    #expect(FileManager.default.fileExists(atPath: unrelated))
    current.close()
  }

  // MARK: - Two clients of one group

  /// JS persists a client's snapshot only when its cookie is newer than the
  /// stored one (`persistDD31` in `persist/persist.ts`: both
  /// `compareCookiesForSnapshots(memdagBaseSnapshot, perdagBaseSnapshot) > 0`
  /// checks), so a client that is behind never writes an older snapshot
  /// over a newer one. Here every poke's changes and cookie are written
  /// through: a client of the group that applies poke 01→02 after another
  /// applied 02→03 leaves 03's rows with 02's version of the others and
  /// cookie 02, a state the server never had, which the next launch shows
  /// offline.
  @Test(.timeLimit(.minutes(1)), arguments: [false, true])
  func aClientBehindDoesNotWriteAnOlderSnapshotOverANewerOne(inMemory: Bool) async throws {
    defer { cleanUp() }
    let key = randomID()
    let storage: ZeroStorage = inMemory ? .memory : .sqlite(directory: directory)
    let scheduler = ManualScheduler(now: 1_700_000_000_000)
    let sockets = MockSocketFactory()
    let first = try makeZero(
      storageKey: key, storage: storage, cacheURL: "http://zero.test", sockets: sockets,
      scheduler: scheduler)
    let a = try connect(sockets, scheduler)
    let second = try makeZero(
      storageKey: key, storage: storage, cacheURL: "http://zero.test", sockets: sockets,
      scheduler: scheduler)
    let b = try connect(sockets, scheduler)
    #expect(first.clientGroupID == second.clientGroupID)

    for s in [a, b] {
      poke(s, scheduler, "p1", from: nil, to: "01", rows: [Self.row("i1", title: "v1")])
    }
    poke(a, scheduler, "p2", from: "01", to: "02", rows: [Self.row("i1", title: "v2")])
    poke(
      a, scheduler, "p3", from: "02", to: "03", rows: [Self.row("i1", title: "v3"), Self.row("i3")])
    // The second client is behind.
    poke(b, scheduler, "p2", from: "01", to: "02", rows: [Self.row("i1", title: "v2")])
    #expect(try await first.run(zql.issue).map(\.title) == ["v3", ""])
    first.close()
    second.close()

    let next = try makeZero(storageKey: key, storage: storage, at: 1_700_000_100_000)
    #expect(next.engine.store.cookie == "03")
    let rows = try await next.run(zql.issue.orderBy(\.id, .asc))
    #expect(rows.map(\.id) == ["i1", "i3"])
    #expect(rows.map(\.title) == ["v3", ""])
    next.close()
  }

  // MARK: - Dropping databases

  /// JS drops the memory stores instances fell back to along with the
  /// configured store (`dropTargets` in `collect-idb-databases.ts`, used by
  /// `zero.delete()` through `dropMatchingDatabases` and by
  /// `dropAllDatabases`). Here a client whose SQLite store failed to open
  /// runs on a memory database that `delete()` leaves: the next client of
  /// that user, falling back again, finds the deleted replica and pending
  /// mutations, after the app logged the user out.
  @Test(.timeLimit(.minutes(1)), arguments: [false, true])
  func deleteAlsoDropsTheMemoryFallback(dropAll: Bool) async throws {
    let storage = ZeroStorage.sqlite(directory: "/dev/null/" + UUID().uuidString)
    let key = randomID()
    let first = try makeZero(storageKey: key, storage: storage)
    first.mutate(Self.create("i1"))
    if dropAll {
      first.close()
      await dropAllDatabases(storage)
    } else {
      await first.delete()
    }

    let second = try makeZero(storageKey: key, storage: storage, at: 1000)
    #expect(second.engine.store.pending.isEmpty)
    #expect(try await second.run(zql.issue).isEmpty)
    second.close()
  }

  // MARK: - Bad data

  /// JS treats a store whose data is corrupt as lost client state: it drops
  /// the database and calls `onClientStateNotFound`, so the next instance
  /// starts on a fresh, persisted store (`#handleInvalidRefCount` and
  /// `#dropDatabaseAndFireOnClientStateNotFound` in `replicache-impl.ts`).
  /// A row that doesn't decode must not make every launch fall back to
  /// memory, forever.
  @Test(.timeLimit(.minutes(1)), arguments: ["mutations", "client_groups"])
  func aRowThatDoesNotDecodeDoesNotDisablePersistenceForever(table: String) async throws {
    defer { cleanUp() }
    let first = try makeZero()
    first.mutate(Self.create("i1"))
    first.close()
    let db = try SQLiteDatabase(path: databasePath())
    try db.execute(
      table == "mutations"
        ? "UPDATE mutations SET args = '{not json'"
        : "UPDATE client_groups SET mutator_names = '[not json'")
    db.close()

    final class Calls: @unchecked Sendable { var notFound = 0 }
    let calls = Calls()
    let failures = Failures()
    var options = ZeroOptions(userID: "u", storageKey: "k", storage: .sqlite(directory: directory))
    options.logSink = RecordingLogSink()
    options.onStorageFailure = { failures.kinds.append($0.kind) }
    options.onClientStateNotFound = { calls.notFound += 1 }
    let second = try Zero(
      schema: .test, mutators: [Self.create], options: options,
      scheduler: ManualScheduler(now: 1000), socketFactory: nil)
    try await settle(until: { calls.notFound > 0 })
    #expect(calls.notFound == 1)
    #expect(failures.kinds == [])
    #expect(second.engine.store.pending.isEmpty)
    second.mutate(Self.create("i2"))
    second.close()

    let third = try makeZero(at: 2000, onStorageFailure: { failures.kinds.append($0.kind) })
    try await settle()
    #expect(failures.kinds == [])
    #expect(third.clientGroupID == second.clientGroupID)
    #expect(try await third.run(zql.issue).map(\.id) == ["i2"])
    third.close()
  }

  /// A client whose database was dropped (`InvalidConnectionRequest*`) and
  /// replaced: another client of the group, still on the dropped database,
  /// gets the same error and drops it again. That must not delete the file
  /// the replacement started, with its pending mutation.
  @Test(.timeLimit(.minutes(1)))
  func droppingADroppedDatabaseAgainKeepsTheNewOne() async throws {
    defer { cleanUp() }
    let scheduler = ManualScheduler(now: 1_700_000_000_000)
    let sockets = MockSocketFactory()
    let first = try makeZero(cacheURL: "http://zero.test", sockets: sockets, scheduler: scheduler)
    let a = try connect(sockets, scheduler)
    let second = try makeZero(cacheURL: "http://zero.test", sockets: sockets, scheduler: scheduler)
    let b = try connect(sockets, scheduler)
    #expect(first.clientGroupID == second.clientGroupID)
    let error =
      #"["error",{"kind":"InvalidConnectionRequestBaseCookie","message":"x","origin":"zeroCache"}]"#

    a.receive(text: error)
    scheduler.runDueTimers()
    let replacedGroup = first.clientGroupID
    #expect(replacedGroup != second.clientGroupID)
    first.mutate(Self.create("i1"))
    first.engine.database.flush()
    #expect(FileManager.default.fileExists(atPath: databasePath()))

    b.receive(text: error)
    scheduler.runDueTimers()
    first.engine.database.flush()
    #expect(FileManager.default.fileExists(atPath: databasePath()))
    first.close()
    second.close()

    let next = try makeZero(at: 1_700_000_100_000)
    #expect(next.clientGroupID == replacedGroup)
    #expect(next.engine.store.pending.map(\.id) == [1])
    next.close()
  }

  // MARK: - Writes that found the database busy

  /// The writes kept because the database was busy (`SQLiteStore.write`,
  /// "written with the next") are dropped when the client closes before
  /// another write, and `flush()`, which the app lifecycle calls before
  /// suspension, returns without them. The mutation is gone at the next
  /// launch, though no storage failure was reported. JS's SQLite store
  /// doesn't count a busy database as a failure either
  /// (`classifySQLiteError`), and keeps the mutation for the next persist.
  @Test(.timeLimit(.minutes(1)))
  func aBusyWriteIsNotLostWhenTheClientClosesBeforeTheNextWrite() async throws {
    defer { cleanUp() }
    let failures = Failures()
    let first = try makeZero(onStorageFailure: { failures.kinds.append($0.kind) })
    first.engine.database.flush()

    let other = Box(try SQLiteDatabase(path: databasePath()))
    try other.value.execute("BEGIN IMMEDIATE")
    let release = Task.detached {
      try? await Task.sleep(for: .seconds(5.5))
      try? other.value.execute("COMMIT")
      other.value.close()
    }
    first.mutate(Self.create("i1"))
    first.engine.database.flush()
    await release.value
    first.close()
    try await settle()
    #expect(failures.kinds == [])

    let second = try makeZero(at: 1000)
    #expect(second.engine.store.pending.map(\.id) == [1])
    second.close()
  }

  /// When the writes kept because the database was busy still can't be
  /// written as the client closes, that is reported as a storage failure,
  /// not dropped without a word.
  @Test(.timeLimit(.minutes(1)))
  func aBusyWriteThatStillFailsAtCloseIsReported() async throws {
    defer { cleanUp() }
    let failures = Failures()
    let first = try makeZero(onStorageFailure: { failures.kinds.append($0.kind) })
    first.engine.database.flush()

    let other = try SQLiteDatabase(path: databasePath())
    try other.execute("BEGIN IMMEDIATE")
    first.mutate(Self.create("i1"))
    first.close()
    try? other.execute("COMMIT")
    other.close()
    try await settle(until: { !failures.kinds.isEmpty })
    #expect(failures.kinds == [.ioError])
  }

  // MARK: - A full disk

  /// A disk that fills up while a large poke's snapshot is written: the
  /// transaction rolls back, so the next launch has the previous snapshot,
  /// cookie and rows together, and the pending mutation still to push.
  @Test(.timeLimit(.minutes(1)))
  func aFullDiskDuringALargePokeLeavesThePreviousSnapshotWhole() async throws {
    defer { cleanUp() }
    let scheduler = ManualScheduler(now: 1_700_000_000_000)
    let sockets = MockSocketFactory()
    let failures = Failures()
    let first = try makeZero(
      cacheURL: "http://zero.test", sockets: sockets, scheduler: scheduler,
      onStorageFailure: { failures.kinds.append($0.kind) })
    let socket = try connect(sockets, scheduler)
    poke(socket, scheduler, "p1", from: nil, to: "01", rows: [Self.row("i0")])
    first.mutate(Self.create("m1"))
    scheduler.runDueTimers()
    first.engine.database.flush()

    SQLiteFaults.isolatedWriteFault.store(SQLITE_FULL, ordering: .sequentiallyConsistent)
    let big = String(repeating: "x", count: 2000)
    poke(
      socket, scheduler, "p2", from: "01", to: "02",
      rows: (1...500).map { Self.row("r\($0)", title: big) })
    first.engine.database.flush()
    try await settle(until: { !failures.kinds.isEmpty })
    #expect(failures.kinds == [.full])
    // The client keeps running on what it has.
    #expect(try await first.run(zql.issue).count == 502)
    SQLiteFaults.resetIsolated()
    first.close()

    let next = try makeZero(at: 1_700_000_100_000)
    #expect(next.engine.store.cookie == "01")
    #expect(try await next.run(zql.issue).map(\.id).sorted() == ["i0", "m1"])
    #expect(next.engine.store.pending.map(\.id) == [1])
    next.close()
  }
}
