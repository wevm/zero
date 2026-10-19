/// A Zero client: syncs the rows of named queries from zero-cache into a
/// local replica, keeps materialized views of them current, and runs
/// mutators optimistically before the server confirms them
/// (`Zero` in `packages/zero-client/src/client/zero.ts`).
///
/// `Context` is what queries and mutators are given besides their
/// arguments, such as the logged-in user's auth data.
@MainActor
public final class Zero<Context: Sendable> {
  let engine: Engine
  public let context: Context
  public let connection: Connection

  /// - Parameters:
  ///   - mutators: Every mutator the client may run. Pending mutations are
  ///     replayed by name, so each must be registered.
  ///   - scheduler: The clock and timers; tests pass a manual one.
  ///   - socketFactory: Opens the sync socket.
  public init(
    schema: ZeroSchema, context: Context, mutators: [any Mutator<Context>] = [],
    options: ZeroOptions, scheduler: any Scheduler = SystemScheduler(),
    socketFactory: (any SocketFactory)?
  ) throws {
    var functions: [String: MutatorFunction] = [:]
    for m in mutators {
      guard functions[m.name] == nil else {
        throw ApplicationError("Mutator \"\(m.name)\" is registered twice")
      }
      functions[m.name] = { tx, args in try m.run(tx, args: args, context: context) }
    }
    engine = try Engine(
      schema: schema, options: options, mutators: functions, scheduler: scheduler,
      socketFactory: socketFactory)
    self.context = context
    connection = Connection(engine)
  }

  public var clientID: String { engine.clientID }
  public var clientGroupID: String { engine.clientGroupID }
  public var userID: String? { engine.userID }
  public var schema: ZeroSchema { engine.schema }
  public var closed: Bool { engine.closed }

  /// A view of the query's rows that stays current. Named queries are
  /// synced from the server; an unnamed one only sees local data.
  public func materialize<R: ZeroRow, S: QueryResult>(
    _ request: QueryRequest<R, S, Context>, ttl: TTL = .defaultTTL
  ) throws -> MaterializedView {
    try materialize(request.query(context), ttl: ttl)
  }

  /// A view of an unnamed query, which only sees local data.
  public func materialize<R: ZeroRow, S: QueryResult>(
    _ query: Query<R, S>, ttl: TTL = .defaultTTL
  ) throws -> MaterializedView {
    try materialize(query.any, ttl: ttl)
  }

  public func materialize(_ query: AnyQuery, ttl: TTL = .defaultTTL) throws -> MaterializedView {
    try engine.loop.call { try engine.context.materialize(query, ttl: ttl) }
  }

  /// The query's rows: what the replica holds now (`.unknown`), or once the
  /// server has confirmed them on this connection (`.complete`) or any
  /// (`.cached`).
  public func run<R: ZeroRow, S: QueryResult>(
    _ request: QueryRequest<R, S, Context>, type: RunType = .unknown, ttl: TTL? = nil
  ) async throws -> S {
    let data = try await run(request.query(context), type: type, ttl: ttl).value
    return try S(view: data)
  }

  public func run<R: ZeroRow, S: QueryResult>(
    _ query: Query<R, S>, type: RunType = .unknown, ttl: TTL? = nil
  ) async throws -> S {
    let data = try await run(query.any, type: type, ttl: ttl).value
    return try S(view: data)
  }

  public func run(
    _ query: AnyQuery, type: RunType = .unknown, ttl: TTL? = nil
  ) throws
    -> Deferred<ViewValue>
  {
    try engine.loop.call {
      try engine.context.run(query, type: QueryContext.RunType(rawValue: type.rawValue)!, ttl: ttl)
    }
  }

  /// Keeps the query's rows synced without a view, until `cleanup`.
  public func preload<R: ZeroRow, S: QueryResult>(
    _ request: QueryRequest<R, S, Context>, ttl: TTL = .defaultPreloadTTL
  ) -> Preload {
    preload(request.query(context), ttl: ttl)
  }

  public func preload(_ query: AnyQuery, ttl: TTL = .defaultPreloadTTL) -> Preload {
    engine.loop.call { engine.context.preload(query, ttl: ttl) }
  }

  /// Runs a mutator on the client now and sends it to the server.
  @discardableResult
  public func mutate(_ request: MutateRequest<Context>) -> MutatorResult {
    engine.loop.call { engine.mutate(name: request.name, args: request.args) }
  }

  /// Closes the connection. Pending mutations and queries fail, and views
  /// stop updating.
  public func close() {
    engine.loop.call { engine.close() }
  }
}

extension Zero where Context == Void {
  public convenience init(
    schema: ZeroSchema, mutators: [any Mutator<Void>] = [], options: ZeroOptions,
    scheduler: any Scheduler = SystemScheduler(), socketFactory: (any SocketFactory)?
  ) throws {
    try self.init(
      schema: schema, context: (), mutators: mutators, options: options, scheduler: scheduler,
      socketFactory: socketFactory)
  }
}

/// How long `run` waits (`RunOptions.type`).
public enum RunType: String, Sendable {
  case unknown, cached, complete
}

/// The connection to zero-cache (`Connection` in
/// `packages/zero-client/src/client/connection.ts`).
@MainActor
public final class Connection {
  private let engine: Engine
  public private(set) var state: ConnectionState
  private var listeners: [(id: Int, listener: (ConnectionState) -> Void)] = []
  private var nextID = 0

  init(_ engine: Engine) {
    self.engine = engine
    state = ConnectionState(engine.connectionManager.state)
    _ = engine.connectionManager.subscribe { [weak self] s in
      guard let self else { return }
      self.state = ConnectionState(s)
      for (_, l) in self.listeners { l(self.state) }
    }
  }

  /// Calls `listener` on every change of `state`.
  @discardableResult
  public func subscribe(_ listener: @escaping (ConnectionState) -> Void) -> () -> Void {
    let id = nextID
    nextID += 1
    listeners.append((id, listener))
    return { [weak self] in self?.listeners.removeAll { $0.id == id } }
  }

  /// Resumes connecting from `needsAuth` or `error`. With `auth`, sets the
  /// token first; when already connected, the server gets the new token.
  public func connect(auth: String? = nil) {
    engine.loop.call { engine.connect(auth: auth.map { .some($0) }) }
  }
}
