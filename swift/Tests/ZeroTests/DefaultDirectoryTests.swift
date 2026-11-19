import Foundation
import Testing
import ZeroTesting

@testable import Zero

/// `ZeroStorage.sqlite()`'s folder: the app's own on every platform, and the
/// move of its database out of the folder apps used to share.
@Suite @MainActor struct DefaultDirectoryTests {
  static let home = ["HOME": "/Users/a"]

  static func resolve(
    _ platform: DefaultDirectory.Platform, _ env: [String: String] = home, app: String?
  ) throws -> DefaultDirectory {
    try DefaultDirectory.resolve(platform, env: { env[$0] }, app: app)
  }

  @Test func givesAnUnsandboxedMacAppAFolderNamedForIt() throws {
    #expect(
      try Self.resolve(.mac, app: "com.example.notes")
        == DefaultDirectory(
          path: "/Users/a/Library/Application Support/com.example.notes/Zero",
          legacy: "/Users/a/Library/Application Support/Zero"))
    // A command-line tool, named for its executable.
    #expect(
      try Self.resolve(.mac, app: "notes-sync").path
        == "/Users/a/Library/Application Support/notes-sync/Zero")
  }

  @Test func keepsTheFolderOfAnAppWhoseHomeIsItsOwn() throws {
    let sandboxed = Self.home.merging(["APP_SANDBOX_CONTAINER_ID": "com.example.notes"]) { $1 }
    let container = DefaultDirectory(path: "/Users/a/Library/Application Support/Zero")
    #expect(try Self.resolve(.mac, sandboxed, app: "com.example.notes") == container)
    #expect(try Self.resolve(.container, app: "com.example.notes") == container)
  }

  @Test func givesAnAppElsewhereAFolderNamedForItsExecutable() throws {
    #expect(
      try Self.resolve(.other, app: "notes")
        == DefaultDirectory(
          path: "/Users/a/.local/share/notes/zero", legacy: "/Users/a/.local/share/zero"))
    #expect(
      try Self.resolve(.other, ["XDG_DATA_HOME": "/data"], app: "notes")
        == DefaultDirectory(path: "/data/notes/zero", legacy: "/data/zero"))
  }

  @Test func keepsTheSharedFolderWithoutANameThatIsAFolder() throws {
    for app in [nil, "", ".", "..", "a/b", "a\\b"] {
      #expect(
        try Self.resolve(.mac, app: app)
          == DefaultDirectory(path: "/Users/a/Library/Application Support/Zero"), "\(app ?? "nil")")
      #expect(
        try Self.resolve(.other, app: app)
          == DefaultDirectory(path: "/Users/a/.local/share/zero"), "\(app ?? "nil")")
    }
    #expect(throws: StorageFailure.self) { try Self.resolve(.mac, [:], app: "notes") }
  }

  @Test func namesThisProcess() {
    let name = DefaultDirectory.appName()
    #expect(name.map(DefaultDirectory.isPathComponent) == true, "\(name ?? "nil")")
  }

  @Test func namesAnExecutableReplacedWhileItRunsAsBefore() {
    #expect(DefaultDirectory.executableName("/usr/bin/notes") == "notes")
    #expect(DefaultDirectory.executableName("/usr/bin/notes (deleted)") == "notes")
  }

  // MARK: - The move

  let root = NSTemporaryDirectory() + "zero-default-directory-" + UUID().uuidString
  var legacy: String { root + "/Zero" }
  var directory: DefaultDirectory {
    DefaultDirectory(path: root + "/com.example.notes/Zero", legacy: legacy)
  }

  let create = defineMutator("issue.create") { (tx, id: String) in
    try tx.mutate.issue.insert(
      Issue(id: id, title: "", description: "", closed: false, createdAt: 1))
  }

  /// A client that never connects, so its mutations stay pending.
  func makeZero(_ storageKey: String, in directory: String) throws -> Zero<Void> {
    var options = ZeroOptions(
      userID: "u", storageKey: storageKey, storage: .sqlite(directory: directory))
    options.logSink = RecordingLogSink()
    return try Zero(
      schema: .test, mutators: [create], options: options,
      scheduler: ManualScheduler(now: 1_000), socketFactory: nil)
  }

  static func name(_ storageKey: String) -> String {
    ReplicaDatabase.name(
      userID: "u", storageKey: storageKey, mutateURL: nil, queryURL: nil,
      schemaHash: ZeroSchema.test.clientSchema.hash)
  }

  func files(_ directory: String) -> Set<String> {
    Set(
      ((try? FileManager.default.contentsOfDirectory(atPath: directory)) ?? []).filter {
        $0.hasSuffix(".sqlite")
      })
  }

  @Test func theFirstOpenMovesTheAppsDatabaseWithItsPendingMutations() async throws {
    defer { try? FileManager.default.removeItem(atPath: root) }
    // Left as an app that was killed leaves it: the pending mutation is
    // only in the WAL.
    let file = ReplicaDatabase.fileName(Self.name("k"))
    let running = root + "/running"
    let first = try makeZero("k", in: running)
    first.mutate(create("i1"))
    first.engine.database.flush()
    try FileManager.default.createDirectory(atPath: legacy, withIntermediateDirectories: true)
    for name in [file, file + "-wal", file + "-shm", "profile-id"] {
      try FileManager.default.copyItem(atPath: running + "/" + name, toPath: legacy + "/" + name)
    }
    first.close()
    try FileManager.default.copyItem(
      atPath: legacy + "/" + file, toPath: root + "/without-wal.sqlite")
    var tables = 0
    try SQLiteDatabase(path: root + "/without-wal.sqlite").query(
      "SELECT count(*) FROM sqlite_master WHERE name = 'mutations'"
    ) { tables = Int($0.integer(0)) }
    #expect(tables == 0)
    // Another app's database, or another user's.
    let other = try makeZero("other", in: legacy)
    other.close()
    let profileID = try String(contentsOfFile: legacy + "/profile-id", encoding: .utf8)

    try directory.prepare(moving: Self.name("k"))
    #expect(files(directory.path) == [file])
    #expect(files(legacy) == [ReplicaDatabase.fileName(Self.name("other"))])
    for folder in [legacy, directory.path] {
      // The other database keeps its WAL on Apple platforms, which persist it.
      let left = try FileManager.default.contentsOfDirectory(atPath: folder)
      #expect(!left.contains { $0.hasPrefix(file + "-") }, "\(left)")
    }
    #expect(
      try String(contentsOfFile: directory.path + "/profile-id", encoding: .utf8) == profileID)

    let second = try makeZero("k", in: directory.path)
    #expect(second.clientGroupID == first.clientGroupID)
    #expect(try await second.run(zql.issue).map(\.id) == ["i1"])
    #expect(second.engine.store.pending.map(\.clientID) == [first.clientID])
    second.close()
  }

  @Test func movesNothingIntoAFolderThatExists() throws {
    defer { try? FileManager.default.removeItem(atPath: root) }
    let first = try makeZero("k", in: legacy)
    first.mutate(create("i1"))
    first.close()

    // As a drop does before the first open, so the move doesn't bring back
    // what it drops.
    try directory.prepare(moving: nil)
    try directory.prepare(moving: Self.name("k"))
    #expect(files(directory.path).isEmpty)
    #expect(files(legacy) == [ReplicaDatabase.fileName(Self.name("k"))])
  }

  @Test func leavesADatabaseThatIsOpenElsewhere() throws {
    defer { try? FileManager.default.removeItem(atPath: root) }
    // Its connection keeps the database in WAL mode, its commit in the WAL.
    let running = try makeZero("k", in: legacy)
    running.mutate(create("i1"))
    running.engine.database.flush()

    try directory.prepare(moving: Self.name("k"))
    #expect(files(directory.path).isEmpty)
    #expect(files(legacy) == [ReplicaDatabase.fileName(Self.name("k"))])
    running.close()
  }

  #if canImport(Darwin)
    @Test func excludesTheFolderFromBackups() throws {
      defer { try? FileManager.default.removeItem(atPath: root) }
      try DefaultDirectory(path: directory.path).prepare(moving: nil)
      DefaultDirectory.waitForBackupExclusion()
      let values = try URL(fileURLWithPath: directory.path).resourceValues(forKeys: [
        .isExcludedFromBackupKey
      ])
      #expect(values.isExcludedFromBackup == true)
    }
  #endif
}
