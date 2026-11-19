#if canImport(Darwin)
  import Darwin
  import Foundation
#elseif canImport(Glibc)
  import Glibc
#elseif canImport(Musl)
  import Musl
#endif

/// Where `ZeroStorage.sqlite()` keeps its databases: a folder of the app's
/// own, as IndexedDB is the origin's own in JS. Unsandboxed Mac apps, and
/// apps elsewhere than Apple platforms, used to share one folder, `legacy`,
/// where one app's collection of old databases and its `dropAllDatabases()`
/// deleted the others'. The first open of the new folder moves its database
/// out of the old one.
struct DefaultDirectory: Equatable {
  /// Where the databases are.
  var path: String
  /// The folder every app shared, which the first open of `path` moves its
  /// database from; nil where the folder is the app's own already.
  var legacy: String? = nil

  enum Platform {
    /// macOS and Mac Catalyst: the app's home is the user's unless the app
    /// is sandboxed.
    case mac
    /// iOS and the other Apple platforms: the app's home is its container.
    case container
    /// Linux and the rest.
    case other
  }

  static var platform: Platform {
    #if os(macOS) || targetEnvironment(macCatalyst)
      .mac
    #elseif canImport(Darwin)
      .container
    #else
      .other
    #endif
  }

  /// The default directory of this process.
  static func current() throws -> DefaultDirectory {
    try resolve(platform, env: environment, app: appName())
  }

  /// The default directory on `platform`, with the environment `env` and
  /// the app named `app` (`appName()`). Apple's convention is a folder named
  /// for the bundle identifier in Application Support; a sandboxed app's
  /// Application Support is its own already.
  static func resolve(
    _ platform: Platform, env: (String) -> String?, app: String?
  ) throws -> DefaultDirectory {
    func home() throws -> String {
      guard let home = env("HOME") else {
        throw StorageFailure(kind: .cannotOpen, message: "HOME is not set")
      }
      return home
    }
    let app = app.flatMap { isPathComponent($0) ? $0 : nil }
    switch platform {
    case .container:
      return DefaultDirectory(path: try home() + "/Library/Application Support/Zero")
    case .mac:
      let support = try home() + "/Library/Application Support"
      guard env("APP_SANDBOX_CONTAINER_ID") == nil, let app else {
        return DefaultDirectory(path: support + "/Zero")
      }
      return DefaultDirectory(path: support + "/" + app + "/Zero", legacy: support + "/Zero")
    case .other:
      let data = try env("XDG_DATA_HOME") ?? (home() + "/.local/share")
      guard let app else { return DefaultDirectory(path: data + "/zero") }
      return DefaultDirectory(path: data + "/" + app + "/zero", legacy: data + "/zero")
    }
  }

  /// Makes the directory. The first time, when the folder is new, it moves
  /// the database `name` there from `legacy`, with the profile ID: this
  /// app's database only, since the old folder holds other apps' too. A
  /// drop passes no name, so that what it drops isn't moved back later. On
  /// Apple platforms the folder is excluded from backups each time, as the
  /// server can sync it again, and some file operations clear the flag.
  func prepare(moving name: String?) throws {
    let new = access(path, F_OK) != 0
    try makeDirectories(path)
    if new, let legacy, Self.sameVolume(legacy, path) {
      if let name { Self.move(ReplicaDatabase.fileName(name), from: legacy, to: path) }
      Self.copy("profile-id", from: legacy, to: path)
    }
    Self.excludeFromBackup(path)
  }

  /// Moves the database `file` once SQLite has folded its WAL into it, so
  /// the move is one rename: the database can't arrive without the commits
  /// in its WAL, or leave a WAL behind for a new database to read. A
  /// database another process has open stays, and syncs again.
  private static func move(_ file: String, from source: String, to destination: String) {
    let from = source + "/" + file
    let to = destination + "/" + file
    guard access(from, F_OK) == 0, access(to, F_OK) != 0, leaveWAL(from) else { return }
    // Apple's SQLite leaves the shared memory, which holds nothing now.
    _ = unlink(from + "-shm")
    _ = rename(from, to)
  }

  /// Switches the database at `path` out of WAL mode, which folds the WAL
  /// into it and deletes it, and needs every other connection closed. (A
  /// WAL left after a close shows nothing: Apple's SQLite keeps it.)
  private static func leaveWAL(_ path: String) -> Bool {
    do {
      let db = try SQLiteDatabase(path: path)
      defer { db.close() }
      db.setBusyTimeout(ms: SQLiteStore.openBusyTimeoutMs)
      var mode = ""
      try db.query("PRAGMA journal_mode = DELETE") { mode = $0.text(0) }
      return mode == "delete"
    } catch {
      return false
    }
  }

  /// Whether a rename from `a` to `b` stays on one volume, and so moves the
  /// database rather than copying it.
  private static func sameVolume(_ a: String, _ b: String) -> Bool {
    var sa = stat()
    var sb = stat()
    return stat(a, &sa) == 0 && stat(b, &sb) == 0 && sa.st_dev == sb.st_dev
  }

  private static func copy(_ file: String, from source: String, to destination: String) {
    let to = destination + "/" + file
    guard access(to, F_OK) != 0, let input = fopen(source + "/" + file, "r") else { return }
    defer { fclose(input) }
    var bytes: [UInt8] = []
    var buffer = [UInt8](repeating: 0, count: 256)
    while case let n = fread(&buffer, 1, buffer.count, input), n > 0 {
      bytes += buffer.prefix(n)
    }
    guard let output = fopen(to, "w") else { return }
    _ = fwrite(bytes, 1, bytes.count, output)
    fclose(output)
  }

  #if canImport(Darwin)
    /// Where the folder is excluded from backups: setting the flag takes a
    /// few hundred microseconds, which opening the store would otherwise
    /// spend on the main thread at launch.
    private static let backupQueue = DispatchQueue(label: "zero.storage.backup", qos: .utility)
  #endif

  private static func excludeFromBackup(_ path: String) {
    #if canImport(Darwin)
      backupQueue.async {
        var url = URL(fileURLWithPath: path, isDirectory: true)
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        try? url.setResourceValues(values)
      }
    #endif
  }

  /// Waits for the folders queued so far to be excluded from backups.
  static func waitForBackupExclusion() {
    #if canImport(Darwin)
      backupQueue.sync {}
    #endif
  }

  static func environment(_ name: String) -> String? {
    guard let v = getenv(name) else { return nil }
    let s = String(cString: v)
    return s.isEmpty ? nil : s
  }

  /// The app's name in a path: its bundle identifier, else its executable's
  /// name, as for a command-line tool.
  static func appName() -> String? {
    #if canImport(Darwin)
      Bundle.main.bundleIdentifier ?? Bundle.main.executableURL?.lastPathComponent
    #else
      var buffer = [UInt8](repeating: 0, count: 4096)
      let n = buffer.withUnsafeMutableBufferPointer { b in
        b.baseAddress!.withMemoryRebound(to: CChar.self, capacity: b.count) {
          readlink("/proc/self/exe", $0, b.count - 1)
        }
      }
      let path =
        n > 0 ? String(decoding: buffer.prefix(n), as: UTF8.self) : CommandLine.arguments.first
      return path.flatMap(executableName)
    #endif
  }

  /// The name of the executable at `path`, as `/proc/self/exe` gives it:
  /// Linux adds " (deleted)" once the file is replaced, as an upgrade does
  /// while the app runs.
  static func executableName(_ path: String) -> String? {
    let path = path.hasSuffix(" (deleted)") ? String(path.dropLast(10)) : path
    return path.split(separator: "/").last.map(String.init)
  }

  /// Whether `name` names a folder, rather than a path.
  static func isPathComponent(_ name: String) -> Bool {
    !name.isEmpty && name != "." && name != ".."
      && !name.contains { $0 == "/" || $0 == "\\" || $0 == "\0" }
  }
}
