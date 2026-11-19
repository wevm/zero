#if canImport(Darwin)
  import Darwin
  import Dispatch
#else
  import Dispatch
  #if canImport(Glibc)
    import Glibc
  #elseif canImport(Musl)
    import Musl
  #endif
#endif

/// A client's store opened, and its first queries hydrated, off the main
/// actor by ``Zero/prepare(schema:context:mutators:options:queries:)``,
/// for ``Zero/init(_:scheduler:socketFactory:)`` to take over.
public final class PreparedZero<Context: Sendable>: Sendable {
  let schema: ZeroSchema
  let context: Context
  let mutators: [any Mutator<Context>]
  let options: ZeroOptions
  /// Runs the preparation, and holds what it made until `take`.
  private let queue = DispatchQueue(label: "zero.prepare", qos: .userInitiated)
  private let result = UncheckedBox<PreparedReplica?>(nil)

  init(
    schema: ZeroSchema, context: Context, mutators: [any Mutator<Context>], options: ZeroOptions,
    queries: [AnyQuery]
  ) {
    self.schema = schema
    self.context = context
    self.mutators = mutators
    self.options = options
    let names = mutators.map(\.name).sorted()
    queue.async { [result] in
      result.value = PreparedReplica.load(
        schema: schema, mutatorNames: names, options: options, queries: queries)
    }
  }

  /// What the preparation made, once it's done, for the first client only.
  /// Waiting on the queue raises the preparation's priority to the caller's.
  func take() -> PreparedReplica? {
    queue.sync { [result] in
      defer { result.value = nil }
      return result.value
    }
  }

  /// Waits for the preparation, for tests.
  func wait() {
    queue.sync {}
  }
}

extension Zero {
  /// Opens the client's store, and hydrates `queries`, off the main actor,
  /// for ``init(_:scheduler:socketFactory:)`` to take over on it.
  ///
  /// `Zero.init` opens the SQLite store, reads the replica, and loads it
  /// into the query sources on the main actor, and the app's first
  /// `liveQuery`s hydrate there. At launch, that delays the first frame by
  /// a time that grows with the replica. `prepare` returns at once and does
  /// that work on a background queue, while the app goes on with its own
  /// launch, such as building its window. On the main actor, ``init(_:scheduler:socketFactory:)``
  /// then waits for whatever is left of it, so the client and its first
  /// views have their data synchronously, as with `Zero.init`. Call
  /// `prepare` as early as the app knows its options, such as in
  /// `App.init`, and make the client where the first frame needs it.
  ///
  /// The client then takes each view a `materialize` or `liveQuery` of one
  /// of `queries` asks for, rather than hydrating it again. The views stay
  /// current until then, and the ones no query asked for are released
  /// when the replica first changes (a poke from the server or a mutation)
  /// or the client closes. Later queries hydrate as usual.
  ///
  /// The client runs on what `Zero.init` would have read: when this
  /// process already has the store open, or another client wrote to it, or
  /// it was deleted, since `prepare` read it, the client opens it as
  /// `Zero.init` does. With
  /// `ZeroStorage.memory`, or when the store fails to open or read, there
  /// is nothing to prepare, and the client handles that as `Zero.init`
  /// does. Swift only: the JS client loads IndexedDB asynchronously.
  ///
  /// - Parameters:
  ///   - schema: The generated schema, such as `.app`.
  ///   - context: What queries and mutators are given besides their
  ///     arguments, such as the logged-in user's auth data.
  ///   - mutators: Every mutator the client may run. The client joins the
  ///     client group that registers the same ones.
  ///   - options: Where to sync from, and how.
  ///   - queries: The named queries the app's first frame shows, to
  ///     hydrate now.
  /// - Returns: The preparation, for one client.
  nonisolated public static func prepare(
    schema: ZeroSchema, context: Context, mutators: [any Mutator<Context>] = [],
    options: ZeroOptions, queries: [any PreparableQuery<Context>] = []
  ) -> PreparedZero<Context> {
    PreparedZero(
      schema: schema, context: context, mutators: mutators, options: options,
      queries: queries.map { $0.query(for: context) })
  }
}

/// A query ``Zero/prepare(schema:context:mutators:options:queries:)`` can
/// hydrate: a ``QueryRequest``, a named query applied to its arguments.
public protocol PreparableQuery<Context>: Sendable {
  associatedtype Context: Sendable
  /// The query for `context`, named so the server knows it.
  func query(for context: Context) -> AnyQuery
}

extension QueryRequest: PreparableQuery {
  public func query(for context: Context) -> AnyQuery { query(context) }
}

/// A view `Zero.prepare` hydrated, for the first `materialize` of its query.
struct PreparedView {
  let queryID: String
  let ast: AST
  let format: Format
  let input: any Input
  let view: ArrayView
  /// The synced aggregates its query moves, by aggregate table, which
  /// register when a client takes it: the client replays its pending
  /// mutations first, as it does before any query registers them.
  let optimistic: [(String, OptimisticAggregate)]
  /// How long hydrating it took, in milliseconds.
  let hydrateMs: Double
}

/// Times the pushes into prepared views, once a client has them
/// (`MeasurePush`): the client sets the clock and the report.
final class PreparedMetrics: @unchecked Sendable {
  var now: () -> Double = { 0 }
  var report: ((Double, String) -> Void)?
}

/// What `Zero.prepare` read and built off the main actor: the store, opened,
/// with its records and the group the client joins, that group's snapshot
/// loaded into a branch's sources, and views of the queries over it. The
/// engine takes the store and the views; whatever it doesn't take is closed
/// and released.
final class PreparedReplica: @unchecked Sendable {
  let path: String
  /// The file the store opened, to check it is still at `path`.
  let file: FileID?
  /// SQLite's `data_version` once the store was read, which changes when
  /// another connection writes the file.
  let dataVersion: Int64
  /// Set until the engine takes it.
  var store: SQLiteStore?
  let records: ReplicaDatabase.Records
  /// The group the client joins, nil for a new one, and what it holds.
  let groupID: String?
  let group: StoredGroup
  /// The group's snapshot, loaded into the sources.
  let branch: IVMBranch
  var views: [PreparedView]
  let metrics: PreparedMetrics

  private init(
    path: String, file: FileID?, dataVersion: Int64, store: SQLiteStore,
    records: ReplicaDatabase.Records,
    groupID: String?, group: StoredGroup, branch: IVMBranch, views: [PreparedView],
    metrics: PreparedMetrics
  ) {
    self.path = path
    self.file = file
    self.dataVersion = dataVersion
    self.store = store
    self.records = records
    self.groupID = groupID
    self.group = group
    self.branch = branch
    self.views = views
    self.metrics = metrics
  }

  deinit {
    for v in views { v.input.destroy() }
    _ = store?.close()
  }

  /// Opens and reads the store as `ReplicaDatabase.open` and `initClient`
  /// do, and hydrates `queries`. Nil for a memory store, and when it fails:
  /// the engine then opens the store itself, and handles the failure.
  static func load(
    schema: ZeroSchema, mutatorNames: [String], options: ZeroOptions, queries: [AnyQuery]
  ) -> PreparedReplica? {
    guard case .sqlite(let directory) = options.storage else { return nil }
    let name = ReplicaDatabase.name(
      userID: options.userID, storageKey: options.storageKey ?? "",
      mutateURL: options.mutateURL, queryURL: options.queryURL,
      schemaHash: schema.clientSchema.hash)
    let path: String
    let store: SQLiteStore
    do {
      let dir: String
      if let directory {
        dir = directory
        try makeDirectories(dir)
      } else {
        let d = try DefaultDirectory.current()
        try d.prepare(moving: name)
        dir = d.path
      }
      path = dir + "/" + ReplicaDatabase.fileName(name)
      store = try SQLiteStore(path: path, name: name, directory: dir)
    } catch {
      return nil
    }
    let file = FileID(path)
    let read: (ReplicaDatabase.Records, String?, StoredGroup, Int64)
    do {
      read = try store.sync { db in
        let records = try ReplicaDatabase.readRecords(from: db, directory: store.directory)
        let match = ReplicaDatabase.matchingGroup(records.groups, mutatorNames: mutatorNames)
        let group = try match.map { try SQLiteStore.loadGroup($0.id, from: db) } ?? StoredGroup()
        db.setBusyTimeout(ms: SQLiteDatabase.busyTimeoutMs)
        return (records, match?.id, group, try dataVersion(db))
      }
    } catch {
      _ = store.close()
      return nil
    }
    let (records, groupID, group, version) = read

    let branch = IVMBranch(schema: schema)
    branch.apply(KVDiff(adding: group.entries.entries))
    let metrics = PreparedMetrics()
    let delegate = BranchDelegate(branch)
    delegate.decorateSource = { input, queryID in
      MeasurePush(
        input, queryID: queryID, sampleRate: Int(updateSampleRate), now: { metrics.now() }
      ) { elapsed, queryID in metrics.report?(elapsed, queryID) }
    }
    var optimistic: [(String, OptimisticAggregate)] = []
    delegate.deferOptimistic = { optimistic.append(($0, $1)) }
    var views: [PreparedView] = []
    for query in queries where query.invalid == nil {
      let queryID =
        query.customQueryID.map { QueryHash.hashOfNameAndArgs($0.name, $0.args) }
        ?? QueryHash.hashOfAST(query.ast)
      optimistic = []
      let start = DispatchTime.now().uptimeNanoseconds
      let input: any Input
      do {
        let pipeline = try buildPipeline(query.ast, delegate, queryID: queryID)
        input =
          query.ast.groupBy != nil ? try groupViewInput(pipeline, query.ast, delegate) : pipeline
      } catch {
        continue
      }
      // A query that fails to hydrate is left to `materialize`, which
      // throws its error.
      guard
        let view = try? PipelineFailure.capture({
          ArrayView(input, format: query.format, error: nil, complete: false)
        })
      else {
        input.destroy()
        continue
      }
      views.append(
        PreparedView(
          queryID: queryID, ast: query.ast, format: query.format, input: input, view: view,
          optimistic: optimistic,
          hydrateMs: Double(DispatchTime.now().uptimeNanoseconds - start) / 1e6))
    }
    return PreparedReplica(
      path: path, file: file, dataVersion: version, store: store, records: records,
      groupID: groupID, group: group, branch: branch, views: views, metrics: metrics)
  }

  /// Whether the file still holds what the store read: no other connection
  /// wrote it since, such as a client of the same database that opened and
  /// closed it meanwhile, and it is still the file at `path`.
  func isCurrent() -> Bool {
    guard let store, let file, FileID(path) == file else { return false }
    return (try? store.sync(Self.dataVersion)) == dataVersion
  }

  private static func dataVersion(_ db: SQLiteDatabase) throws -> Int64 {
    var version: Int64 = -1
    try db.query("PRAGMA data_version") { version = $0.integer(0) }
    return version
  }
}

/// A file's device and inode: what a path names, while it stays there.
struct FileID: Equatable {
  let device: UInt64
  let inode: UInt64

  init?(_ path: String) {
    var s = stat()
    guard stat(path, &s) == 0 else { return nil }
    device = UInt64(truncatingIfNeeded: s.st_dev)
    inode = UInt64(truncatingIfNeeded: s.st_ino)
  }
}

/// A value one thread at a time uses, handed between them through a queue.
final class UncheckedBox<T>: @unchecked Sendable {
  var value: T
  init(_ value: T) { self.value = value }
}
