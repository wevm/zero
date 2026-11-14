/// Inspects the client's queries, local data, and metrics, and the
/// server's, for debugging (`zero.inspector`, the `Inspector` of
/// `packages/zero-client/src/client/inspector/inspector.ts`).
///
/// The requests to the server go over the sync connection, once it is
/// connected. Unlike JS, which asks for zero-cache's admin password in an
/// HTML dialog when the server needs it, a request then throws
/// ``InspectorError/authenticationRequired``: call
/// ``authenticate(password:)`` first. The password it accepts is presented
/// again on later connections, which the server authenticates separately.
@MainActor
public final class Inspector {
  let state: InspectorState

  init(engine: @escaping @MainActor () -> Engine?) {
    state = InspectorState(engine: engine)
  }

  /// This client.
  public var client: InspectorClient {
    InspectorClient(state, id: state.clientID, clientGroupID: state.clientGroupID)
  }

  /// This client's group: the clients that share its local data.
  public var clientGroup: InspectorClientGroup { client.clientGroup }

  /// The client's query metrics with the server's (`inspector.metrics()`).
  public func metrics() async throws -> Metrics {
    try await state.run { engine, done in
      let client = engine.queryManager.metrics
      engine.inspect(["op": "metrics"], expect: "metrics") { r in
        done(
          r.flatMap { v in
            Result {
              Metrics(
                queryMaterializationClient: client.materializationClient,
                queryMaterializationEndToEnd: client.materializationEndToEnd,
                queryUpdateClient: client.updateClient,
                queryMaterializationServer: try digest(v["query-materialization-server"]),
                queryUpdateServer: try digest(v["query-update-server"]))
            }
          })
      }
    }
  }

  /// Every client the local store has a record of.
  public func clients() async throws -> [InspectorClient] {
    try await state.run { _, done in done(.success(self.state.clients(ofGroup: nil))) }
  }

  /// The clients the local store has a record of that have queries on the
  /// server, in the order the server answers.
  public func clientsWithQueries() async throws -> [InspectorClient] {
    try await state.run { engine, done in
      self.state.withQueries(engine, self.state.clients(ofGroup: nil), done)
    }
  }

  /// zero-cache's version.
  public func serverVersion() async throws -> String {
    try await state.run { engine, done in
      engine.inspect(["op": "version"], expect: "version") { r in
        done(r.map { $0.stringValue ?? "" })
      }
    }
  }

  /// How the server runs a query: a named one by its name and arguments,
  /// an unnamed one by its AST with the server's names.
  public func analyzeQuery(
    _ query: AnyQuery, options: AnalyzeQueryOptions? = nil
  ) async throws -> AnalyzeQueryResult {
    if let id = query.customQueryID {
      return try await analyzeNamedQuery(name: id.name, args: id.args, options: options)
    }
    return try await analyze(serverAST: { $0.queryManager.serverAST(query.ast) }, options)
  }

  public func analyzeQuery<R: ZeroRow, S: QueryValue>(
    _ query: Query<R, S>, options: AnalyzeQueryOptions? = nil
  ) async throws -> AnalyzeQueryResult {
    try await analyzeQuery(query.any, options: options)
  }

  public func analyzeQuery<R: ZeroRow, S: QueryValue, Context: Sendable>(
    _ request: QueryRequest<R, S, Context>, options: AnalyzeQueryOptions? = nil
  ) async throws -> AnalyzeQueryResult {
    try await analyzeNamedQuery(name: request.name, args: request.args, options: options)
  }

  /// How the server runs a query given by its AST, which is sent as is:
  /// it must already have the server's table and column names.
  public func analyzeServerAST(
    _ ast: AST, options: AnalyzeQueryOptions? = nil
  ) async throws -> AnalyzeQueryResult {
    try await analyze(serverAST: { _ in ast }, options)
  }

  /// How the server runs a named query, which it resolves from the name
  /// and arguments.
  public func analyzeNamedQuery(
    name: String, args: [JSONValue], options: AnalyzeQueryOptions? = nil
  ) async throws -> AnalyzeQueryResult {
    var request: JSONObject = [
      "op": "analyze-query", "name": .string(name), "args": .array(args),
    ]
    request["options"] = options?.json
    return try await state.analyze(request)
  }

  private func analyze(
    serverAST: @escaping (Engine) -> AST, _ options: AnalyzeQueryOptions?
  ) async throws -> AnalyzeQueryResult {
    try await state.run { engine, done in
      var request: JSONObject = ["op": "analyze-query", "ast": serverAST(engine).json]
      request["options"] = options?.json
      engine.inspect(request, expect: "analyze-query") { r in
        done(r.map(AnalyzeQueryResult.init(json:)))
      }
    }
  }

  /// Presents zero-cache's admin password. True when the server accepts it
  /// (or runs in a development mode that doesn't check it); it is then
  /// presented again on later connections.
  public func authenticate(password: String) async throws -> Bool {
    try await state.run { engine, done in engine.authenticate(password, done) }
  }

  /// The planner's debug events (`AnalyzeQueryResult.joinPlans`) as text.
  public func formatPlannerEvents(_ events: [JSONValue]) -> String {
    formatPlanDebugEvents(events)
  }
}

/// A client the local store has a record of (`Client` in
/// `packages/zero-client/src/client/inspector/client.ts`).
@MainActor
public struct InspectorClient: Sendable {
  let state: InspectorState
  public let id: String
  public let clientGroup: InspectorClientGroup

  init(_ state: InspectorState, id: String, clientGroupID: String) {
    self.state = state
    self.id = id
    self.clientGroup = InspectorClientGroup(state, id: clientGroupID)
  }

  /// The client's queries on the server.
  public func queries() async throws -> [InspectorQuery] {
    try await state.run { engine, done in
      self.state.queries(engine, ["op": "queries", "clientID": .string(self.id)], done)
    }
  }

  /// The client's local data: every key and value of its replica, with its
  /// pending mutations applied.
  public func map() async throws -> [String: JSONValue] {
    try await state.run { engine, done in
      done(
        Result {
          var map: [String: JSONValue] = [:]
          for e in try self.state.kv(engine, self.id).entries { map[e.key] = e.value }
          return map
        })
    }
  }

  /// The client's local rows of `table`.
  public func rows(_ table: String) async throws -> [Row] {
    try await state.run { engine, done in
      done(
        Result {
          try self.state.kv(engine, self.id).scan(prefix: Keys.entitiesPrefix + table + "/")
            .compactMap { $0.value.objectValue.map(Row.init) }
        })
    }
  }
}

/// A client group (`ClientGroup` in
/// `packages/zero-client/src/client/inspector/client-group.ts`).
@MainActor
public struct InspectorClientGroup: Sendable {
  let state: InspectorState
  public let id: String

  init(_ state: InspectorState, id: String) {
    self.state = state
    self.id = id
  }

  /// The group's clients the local store has a record of.
  public func clients() async throws -> [InspectorClient] {
    try await state.run { _, done in done(.success(self.state.clients(ofGroup: self.id))) }
  }

  /// The group's clients that have queries on the server, in the order the
  /// server answers.
  public func clientsWithQueries() async throws -> [InspectorClient] {
    try await state.run { engine, done in
      self.state.withQueries(engine, self.state.clients(ofGroup: self.id), done)
    }
  }

  /// The group's queries on the server.
  public func queries() async throws -> [InspectorQuery] {
    try await state.run { engine, done in self.state.queries(engine, ["op": "queries"], done) }
  }
}

/// A query on the server, with the client's view of it (`Query` in
/// `packages/zero-client/src/client/inspector/query.ts`).
public struct InspectorQuery: Sendable {
  /// The name of a named query.
  public let name: String?
  /// The arguments of a named query.
  public let args: [JSONValue]?
  /// Whether the server has sent the query's rows.
  public let got: Bool
  public let ttl: TTL
  /// When the query became inactive, in milliseconds since 1970.
  public let inactivatedAt: Double?
  public let rowCount: Double
  public let deleted: Bool
  /// The query's hash.
  public let id: String
  public let clientID: String
  /// The client's metrics of the query, with the server's.
  public let metrics: Metrics
  /// The client's query as builder code, such as `issue.where('id', 'i1')`.
  public let clientZQL: String?
  /// The server's query as builder code.
  public let serverZQL: String?
  /// The client's materialization time, in milliseconds.
  public let hydrateClient: Double?
  /// The server's materialization time, in milliseconds.
  public let hydrateServer: Double?
  /// The time from materializing to the server's rows, in milliseconds.
  public let hydrateTotal: Double?
  public let updateClientP50: Double?
  public let updateClientP95: Double?
  public let updateServerP50: Double?
  public let updateServerP95: Double?

  init(row: JSONValue, clientAST: AST?, clientMetrics: QueryClientMetrics?) throws {
    clientID = row["clientID"]?.stringValue ?? ""
    id = row["queryID"]?.stringValue ?? ""
    inactivatedAt = row["inactivatedAt"]?.numberValue
    ttl = TTL.milliseconds(row["ttl"]?.numberValue ?? 0).normalized
    name = row["name"]?.stringValue
    args = row["args"]?.arrayValue
    got = row["got"]?.boolValue ?? false
    rowCount = row["rowCount"]?.numberValue ?? 0
    deleted = row["deleted"]?.boolValue ?? false
    let serverAST = try row["ast"].flatMap { $0.isNull ? nil : try AST(json: $0) }
    serverZQL = serverAST.map { $0.table + astToZQL($0) }
    clientZQL = clientAST.map { $0.table + astToZQL($0) }

    // `mergeMetrics`.
    let server: JSONValue? = row["metrics"].flatMap { $0.isNull ? JSONValue?.none : $0 }
    func single(_ value: Double?) -> TDigest {
      let d = TDigest()
      if let value { d.add(value) }
      return d
    }
    let merged = Metrics(
      queryMaterializationClient: single(clientMetrics?.materializationClient),
      queryMaterializationEndToEnd: single(clientMetrics?.materializationEndToEnd),
      queryUpdateClient: clientMetrics?.updateClient ?? TDigest(),
      queryMaterializationServer: single(server?["query-hydration-server-ms"]?.numberValue),
      queryUpdateServer: try server.map { try digest($0["query-update-server"]) } ?? TDigest())
    metrics = merged
    func percentile(_ d: TDigest, _ q: Double) -> Double? {
      let n = d.quantile(q)
      return n.isNaN ? nil : n
    }
    hydrateClient = clientMetrics?.materializationClient
    hydrateServer = server?["query-hydration-server-ms"]?.numberValue
    hydrateTotal = clientMetrics?.materializationEndToEnd
    updateClientP50 = percentile(merged.queryUpdateClient, 0.5)
    updateClientP95 = percentile(merged.queryUpdateClient, 0.95)
    updateServerP50 = percentile(merged.queryUpdateServer, 0.5)
    updateServerP95 = percentile(merged.queryUpdateServer, 0.95)
  }
}

/// Query metrics, by their JS names (`Metrics` in `lazy-inspector.ts`).
/// The client's digests are live: they go on recording.
public struct Metrics: Sendable {
  /// `query-materialization-client`: how long views take to hydrate.
  public let queryMaterializationClient: TDigest
  /// `query-materialization-end-to-end`: how long queries take from
  /// materializing until the server has sent their rows.
  public let queryMaterializationEndToEnd: TDigest
  /// `query-update-client`: how long the client takes to push a change
  /// through a query.
  public let queryUpdateClient: TDigest
  /// `query-materialization-server`.
  public let queryMaterializationServer: TDigest
  /// `query-update-server`.
  public let queryUpdateServer: TDigest

  /// The JS names of the metrics, in order.
  public static let names = [
    "query-materialization-client", "query-materialization-end-to-end", "query-update-client",
    "query-materialization-server", "query-update-server",
  ]

  /// The metric of a JS name, such as `query-update-client`.
  public subscript(name: String) -> TDigest? {
    switch name {
    case "query-materialization-client": queryMaterializationClient
    case "query-materialization-end-to-end": queryMaterializationEndToEnd
    case "query-update-client": queryUpdateClient
    case "query-materialization-server": queryMaterializationServer
    case "query-update-server": queryUpdateServer
    default: nil
    }
  }
}

/// What an analysis reports (`AnalyzeQueryOptions` in
/// `packages/zero-protocol/src/inspect-up.ts`); the server's defaults for
/// those left nil.
public struct AnalyzeQueryOptions: Sendable, Hashable {
  /// The rows the query read from the replica (`vendedRows`).
  public var vendedRows: Bool?
  /// The rows the query would sync.
  public var syncedRows: Bool?
  /// The planner's debug events.
  public var joinPlans: Bool?

  public init(vendedRows: Bool? = nil, syncedRows: Bool? = nil, joinPlans: Bool? = nil) {
    self.vendedRows = vendedRows
    self.syncedRows = syncedRows
    self.joinPlans = joinPlans
  }

  var json: JSONValue {
    var o: JSONObject = [:]
    o["vendedRows"] = vendedRows.map(JSONValue.bool)
    o["syncedRows"] = syncedRows.map(JSONValue.bool)
    o["joinPlans"] = joinPlans.map(JSONValue.bool)
    return .object(o)
  }
}

/// How the server ran a query (`AnalyzeQueryResult` in
/// `packages/zero-protocol/src/analyze-query-result.ts`). Counts and rows
/// are by source (table), then by query.
public struct AnalyzeQueryResult: Sendable {
  public let warnings: [String]
  public let syncedRows: [String: [Row]]?
  public let syncedRowCount: Double
  /// When the analysis started, in milliseconds.
  public let start: Double
  public let end: Double
  public let elapsed: Double?
  /// The query after the server applied permissions, as builder code.
  public let afterPermissions: String?
  public let vendedRowCounts: [String: [String: Double]]?
  public let vendedRows: [String: [String: [Row]]]?
  /// SQLite's query plans, by query.
  public let sqlitePlans: [String: [String]]?
  public let readRows: [String: [String: [Row]]]?
  public let readRowCountsByQuery: [String: [String: Double]]?
  public let readRowCount: Double?
  public let dbScansByQuery: [String: [String: Double]]?
  /// The planner's debug events: see ``Inspector/formatPlannerEvents(_:)``.
  public let joinPlans: [JSONValue]?
  /// The result as the server sent it.
  public let json: JSONValue

  init(json: JSONValue) {
    self.json = json
    func record<T>(_ v: JSONValue?, _ f: (JSONValue) -> T?) -> [String: T]? {
      guard let o = v?.objectValue else { return nil }
      var out: [String: T] = [:]
      for (k, e) in o { if let t = f(e) { out[k] = t } }
      return out
    }
    func rows(_ v: JSONValue) -> [Row]? {
      v.arrayValue?.compactMap { $0.objectValue.map(Row.init) }
    }
    func counts(_ v: JSONValue?) -> [String: [String: Double]]? {
      record(v) { record($0, \.numberValue) }
    }
    func rowsBySource(_ v: JSONValue?) -> [String: [String: [Row]]]? {
      record(v) { record($0, rows) }
    }
    warnings = json["warnings"]?.arrayValue?.compactMap(\.stringValue) ?? []
    syncedRows = record(json["syncedRows"], rows)
    syncedRowCount = json["syncedRowCount"]?.numberValue ?? 0
    start = json["start"]?.numberValue ?? 0
    end = json["end"]?.numberValue ?? 0
    elapsed = json["elapsed"]?.numberValue
    afterPermissions = json["afterPermissions"]?.stringValue
    vendedRowCounts = counts(json["vendedRowCounts"])
    vendedRows = rowsBySource(json["vendedRows"])
    sqlitePlans = record(json["sqlitePlans"]) { $0.arrayValue?.compactMap(\.stringValue) }
    readRows = rowsBySource(json["readRows"])
    readRowCountsByQuery = counts(json["readRowCountsByQuery"])
    readRowCount = json["readRowCount"]?.numberValue
    dbScansByQuery = counts(json["dbScansByQuery"])
    joinPlans = json["joinPlans"]?.arrayValue
  }
}

/// Why an inspector request failed.
public enum InspectorError: Error, Sendable, Hashable, CustomStringConvertible {
  /// The server needs zero-cache's admin password: call
  /// ``Inspector/authenticate(password:)``. JS asks for it in a dialog.
  case authenticationRequired
  /// The server's error response.
  case server(String)
  /// A response of another kind than the request's.
  case unexpectedResponse(op: String)
  /// The connection ended, or couldn't be made, before the response came.
  case connectionLost(String)
  /// Zero was closed.
  case clientClosed
  /// The local store has no record of the client.
  case clientNotFound(String)

  public var description: String {
    switch self {
    case .authenticationRequired:
      "The inspector needs zero-cache's admin password: call "
        + "zero.inspector.authenticate(password:) first."
    case .server(let message): message
    case .unexpectedResponse(let op): "Unexpected inspector response \"\(op)\""
    case .connectionLost(let reason):
      "The connection to zero-cache ended before the inspector's request was answered: \(reason)"
    case .clientClosed: "The inspector's request failed: Zero instance closed by user"
    case .clientNotFound(let id): "Client not found: \(id)"
    }
  }
}

private func digest(_ json: JSONValue?) throws -> TDigest {
  try TDigest(json: json?.arrayValue?.compactMap(\.numberValue) ?? [])
}

/// What the inspector and its clients share: the current client, and the
/// queries it dropped (`#deletedQueries`).
@MainActor
final class InspectorState {
  private let engine: () -> Engine?
  private weak var attached: Engine?
  private(set) var clientID = ""
  private(set) var clientGroupID = ""
  private var deletedQueries: [String: (ast: AST, metrics: QueryClientMetrics)] = [:]

  init(engine: @escaping () -> Engine?) {
    self.engine = engine
    if let engine = engine() { attach(engine) }
  }

  /// Follows `engine`, a new client of the `Zero`.
  func attach(_ engine: Engine) {
    attached = engine
    clientID = engine.clientID
    clientGroupID = engine.clientGroupID
    engine.queryManager.onQueryEvicted = { [weak self] hash, ast, metrics in
      self?.deletedQueries[hash] = (ast, metrics)
    }
  }

  /// Runs `body` on the engine's loop and waits for what it reports.
  func run<T: Sendable>(
    _ body: @escaping (Engine, @escaping (Result<T, any Error>) -> Void) -> Void
  ) async throws -> T {
    try Task.checkCancellation()
    guard let engine = engine() else { throw InspectorError.clientClosed }
    if engine !== attached { attach(engine) }
    let result = Deferred<T>()
    engine.loop.call { body(engine) { result.settle($0) } }
    return try await withTaskCancellationHandler {
      try await result.value
    } onCancel: {
      Task { @MainActor in result.reject(CancellationError()) }
    }
  }

  func analyze(_ request: JSONObject) async throws -> AnalyzeQueryResult {
    try await run { engine, done in
      engine.inspect(request, expect: "analyze-query") { r in
        done(r.map(AnalyzeQueryResult.init(json:)))
      }
    }
  }

  func clients(ofGroup id: String?) -> [InspectorClient] {
    guard let engine = engine() else { return [] }
    return engine.database.clients.filter { id == nil || $0.clientGroupID == id }.map {
      InspectorClient(self, id: $0.id, clientGroupID: $0.clientGroupID)
    }
  }

  /// The `queries` request, as `InspectorQuery`s with the most expensive on
  /// the server first.
  func queries(
    _ engine: Engine, _ request: JSONObject,
    _ done: @escaping (Result<[InspectorQuery], any Error>) -> Void
  ) {
    engine.inspect(request, expect: "queries") { [self] r in
      done(
        r.flatMap { v in
          Result {
            let queries = try (v.arrayValue ?? []).map { row in
              let id = row["queryID"]?.stringValue ?? ""
              return try InspectorQuery(
                row: row,
                clientAST: engine.queryManager.clientAST(id) ?? deletedQueries[id]?.ast,
                clientMetrics: engine.queryManager.queryMetrics(id) ?? deletedQueries[id]?.metrics)
            }
            // A stable sort, as JS's.
            return queries.enumerated().sorted { a, b in
              let x = a.element.hydrateServer ?? 0
              let y = b.element.hydrateServer ?? 0
              return x != y ? x > y : a.offset < b.offset
            }.map(\.element)
          }
        })
    }
  }

  /// The clients of `clients` that have queries, asked about all at once
  /// as JS does, in the order the answers come.
  func withQueries(
    _ engine: Engine, _ clients: [InspectorClient],
    _ done: @escaping (Result<[InspectorClient], any Error>) -> Void
  ) {
    if clients.isEmpty { return done(.success([])) }
    var remaining = clients.count
    var found: [InspectorClient] = []
    var failed = false
    for client in clients {
      queries(engine, ["op": "queries", "clientID": .string(client.id)]) { r in
        guard !failed else { return }
        switch r {
        case .failure(let e):
          failed = true
          done(.failure(e))
        case .success(let queries):
          if !queries.isEmpty { found.append(client) }
          remaining -= 1
          if remaining == 0 { done(.success(found)) }
        }
      }
    }
  }

  /// The key-value map of client `id`: its group's, as this client has it
  /// for its own group, and as last stored for another.
  func kv(_ engine: Engine, _ id: String) throws -> KVMap {
    guard let record = engine.database.clients.first(where: { $0.id == id }) else {
      throw InspectorError.clientNotFound(id)
    }
    if record.clientGroupID == engine.clientGroupID { return engine.store.main }
    return try engine.database.storedEntries(ofGroup: record.clientGroupID)
  }
}

/// The inspector's requests in flight on the connection, and the password
/// the server last accepted (`adminPassword`).
@MainActor
final class InspectRPCs {
  /// The admin password the server last accepted, presented again when a
  /// later connection is unauthenticated.
  var adminPassword: String?
  /// Makes each request's ID (`nanoid`). Tests can make them predictable.
  var makeID: () -> String = randomID
  private var pending: [(id: String, done: (Result<InspectDownBody, InspectorError>) -> Void)] =
    []
  private var socketWaiters: [(id: Int, done: (Result<SocketHandle, InspectorError>) -> Void)] =
    []
  private var nextWaiterID = 0

  func expect(_ id: String, _ done: @escaping (Result<InspectDownBody, InspectorError>) -> Void) {
    pending.append((id, done))
  }

  /// An `inspect` message from the server: answers the request of its ID.
  func receive(_ body: InspectDownBody) {
    guard let i = pending.firstIndex(where: { $0.id == body.id }) else { return }
    pending.remove(at: i).done(.success(body))
  }

  func addSocketWaiter(
    _ done: @escaping (Result<SocketHandle, InspectorError>) -> Void
  ) -> Int {
    nextWaiterID += 1
    socketWaiters.append((nextWaiterID, done))
    return nextWaiterID
  }

  func takeSocketWaiter(_ id: Int) -> ((Result<SocketHandle, InspectorError>) -> Void)? {
    guard let i = socketWaiters.firstIndex(where: { $0.id == id }) else { return nil }
    return socketWaiters.remove(at: i).done
  }

  /// The connection ended: the requests on it fail.
  func connectionLost(_ reason: ConnectionError) {
    fail(reason.clientKind == .clientClosed ? .clientClosed : .connectionLost(reason.message))
  }

  /// The client closed: every request fails, also those waiting to connect.
  func clientClosed() {
    fail(.clientClosed)
    let waiters = socketWaiters
    socketWaiters = []
    for w in waiters { w.done(.failure(.clientClosed)) }
  }

  private func fail(_ error: InspectorError) {
    let failed = pending
    pending = []
    for p in failed { p.done(.failure(error)) }
  }
}

extension Engine {
  /// `rpcNoAuthTry`: sends one request on `socket` and reports the value
  /// of a response of kind `op`. An unauthenticated answer is
  /// `InspectorError.authenticationRequired`.
  func inspectOnce(
    _ socket: SocketHandle, _ request: JSONObject, expect op: String,
    _ done: @escaping (Result<JSONValue, any Error>) -> Void
  ) {
    var request = request
    request["id"] = .string(inspectRPCs.makeID())
    let body: InspectUpBody
    do {
      body = try InspectUpBody(json: .object(request))
    } catch {
      return done(.failure(error))
    }
    sendInspect(socket, body) { r in
      switch r {
      case .failure(let e):
        done(.failure(e))
      case .success(let down):
        if down.op == "error" {
          done(.failure(InspectorError.server(down.value.stringValue ?? "")))
        } else if down.op == op {
          done(.success(down.value))
        } else if down.op == "authenticated", down.value == false {
          done(.failure(InspectorError.authenticationRequired))
        } else {
          done(.failure(InspectorError.unexpectedResponse(op: down.op)))
        }
      }
    }
  }

  /// `rpc`: a request, which, when the connection is unauthenticated, is
  /// sent again after the password the server last accepted, if it still
  /// does.
  func inspect(
    _ request: JSONObject, expect op: String,
    _ done: @escaping (Result<JSONValue, any Error>) -> Void
  ) {
    inspectorSocket { r in
      let socket: SocketHandle
      switch r {
      case .failure(let e): return done(.failure(e))
      case .success(let s): socket = s
      }
      self.inspectOnce(socket, request, expect: op) { r in
        guard case .failure(let e as InspectorError) = r, e == .authenticationRequired,
          let remembered = self.inspectRPCs.adminPassword
        else { return done(r) }
        self.authenticateOn(socket, remembered) { ok in
          switch ok {
          case .failure(let e):
            done(.failure(e))
          case .success(true):
            self.inspectOnce(socket, request, expect: op, done)
          case .success(false):
            self.inspectRPCs.adminPassword = nil
            done(r)
          }
        }
      }
    }
  }

  private func authenticateOn(
    _ socket: SocketHandle, _ password: String,
    _ done: @escaping (Result<Bool, any Error>) -> Void
  ) {
    inspectOnce(
      socket, ["op": "authenticate", "value": .string(password)], expect: "authenticated"
    ) { r in done(r.map { $0.boolValue ?? false }) }
  }

  /// `authenticate`: remembers the password once the server accepts it.
  func authenticate(_ password: String, _ done: @escaping (Result<Bool, any Error>) -> Void) {
    inspectorSocket { r in
      switch r {
      case .failure(let e):
        done(.failure(e))
      case .success(let socket):
        self.authenticateOn(socket, password) { r in
          if case .success(true) = r { self.inspectRPCs.adminPassword = password }
          done(r)
        }
      }
    }
  }
}
