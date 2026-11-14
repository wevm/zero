import Testing
import ZeroTesting

@testable import Zero

/// `zero.inspector`, the query metrics, and `slowMaterializeThreshold`,
/// against a mock zero-cache. The formatting tests expect what the JS
/// sources (`astToZQL`, `formatPlannerEvents`, `toFixed`) return for the
/// same input, run with the repo's Node.
@Suite @MainActor struct InspectorTests {
  static let all = defineQuery("issues.all") { () in zql.issue.orderBy(\.id, .asc) }
  static let hash = QueryHash.hashOfNameAndArgs("issues.all", [])

  let scheduler = ManualScheduler(now: 1_700_000_000_000)
  let sockets = MockSocketFactory()
  let log = RecordingLogSink()

  func makeZero(
    storageKey: String = randomID(), sockets: MockSocketFactory? = nil,
    _ configure: (inout ZeroOptions) -> Void = { _ in }
  ) throws -> Zero<Void> {
    var options = ZeroOptions(
      cacheURL: "http://zero.test", userID: "u", storageKey: storageKey, storage: .memory)
    options.logSink = log
    // No pings while the clock moves.
    options.pingTimeoutMs = 600_000
    configure(&options)
    return try Zero(
      schema: .test, options: options, scheduler: scheduler, socketFactory: sockets ?? self.sockets)
  }

  /// Acknowledges the newest socket.
  @discardableResult
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

  static let got = #""gotQueriesPatch":[{"op":"put","hash":"\#(hash)"}]"#
  static func row(_ id: String) -> String {
    #"{"op":"put","tableName":"issues","value":{"id":"\#(id)","title":"","description":"","closed":false,"owner_id":null,"createdAt":1}}"#
  }

  /// Lets the inspector's tasks run, and the work they queue.
  func settle() async {
    for _ in 0..<5 {
      await Task.yield()
      scheduler.runDueTimers()
    }
  }

  /// The bodies of the `inspect` messages sent on `socket`.
  func requests(_ socket: MockSocket) -> [JSONValue] {
    socket.messages.compactMap { text in
      let m = try? JSONParser.parse(Array(text.utf8))
      return m?[0] == "inspect" ? m?[1] : nil
    }
  }

  func answer(_ socket: MockSocket, _ request: JSONValue?, _ op: String, _ value: JSONValue) {
    let message: JSONValue = [
      "inspect", ["id": request?["id"] ?? .null, "op": .string(op), "value": value],
    ]
    socket.receive(text: JSONSerializer.stringify(message))
    scheduler.runDueTimers()
  }

  /// The request without its random ID.
  func withoutID(_ request: JSONValue?) -> JSONValue? {
    guard var o = request?.objectValue else { return nil }
    o["id"] = nil
    return .object(o)
  }

  // MARK: - Metrics and slowMaterializeThreshold

  @Test func logsAMaterializationSlowerThanTheThreshold() async throws {
    for (threshold, level, message) in [
      (5_000.0, LogLevel.warn, "Slow query materialization (including server/network)"),
      (10_000, .debug, "Materialized query (including server/network)"),
    ] {
      let zero = try makeZero {
        $0.slowMaterializeThreshold = threshold
        $0.logLevel = .debug
      }
      let view = try zero.materialize(Self.all())
      let socket = try connect()
      scheduler.advance(by: 6_000)
      poke(socket, from: nil, to: "01", Self.got)
      #expect(view.resultType == .complete)
      let logged = log.entries.filter { $0.message.hasPrefix(message) }.last
      #expect(logged?.level == level)
      #expect(logged?.message.hasPrefix("\(message) \(Self.hash) {\"table\":\"issue\"") == true)
      #expect(logged?.message.hasSuffix(" 6000") == true)
      let metrics = try #require(zero.engine.queryManager.queryMetrics(Self.hash))
      #expect(metrics.materializationEndToEnd == 6_000)
      // The manual clock doesn't move while the view hydrates.
      #expect(metrics.materializationClient == 0)
      let all = zero.engine.queryManager.metrics
      #expect(all.materializationEndToEnd.count() == 1)
      #expect(all.materializationEndToEnd.quantile(0.5) == 6_000)
      #expect(all.materializationClient.count() == 1)
      zero.close()
    }
  }

  @Test func weightsTheSampledUpdatesByTheSampleRate() throws {
    let zero = try makeZero()
    let view = try zero.materialize(Self.all())
    let socket = try connect()
    poke(socket, from: nil, to: "01", Self.got)
    let all = zero.engine.queryManager.metrics.updateClient
    #expect(all.count() == 0)
    // The first of every 32 pushes is timed, and recorded with weight 32.
    var cookie = 1
    func pushRow() {
      let next = cookie + 1
      let row = Self.row("i\(next)")
      poke(socket, from: format2(cookie), to: format2(next), #""rowsPatch":[\#(row)]"#)
      cookie = next
    }
    pushRow()
    #expect(all.count() == 32)
    #expect(all.quantile(0.5) == 0)
    for _ in 0..<31 { pushRow() }
    #expect(all.count() == 32)
    pushRow()
    #expect(all.count() == 64)
    #expect(zero.engine.queryManager.queryMetrics(Self.hash)?.updateClient.count() == 64)
    #expect(view.data.json?.arrayValue?.count == 33)
  }

  func format2(_ n: Int) -> String { n < 10 ? "0\(n)" : "\(n)" }

  // MARK: - RPCs

  @Test func waitsForTheConnectionAndAnswersTheRequest() async throws {
    let zero = try makeZero()
    var ids = 0
    zero.engine.inspectRPCs.makeID = {
      ids += 1
      return "r\(ids)"
    }
    let version = Task { try await zero.inspector.serverVersion() }
    await settle()
    let socket = try connect()
    await settle()
    #expect(requests(socket) == [["op": "version", "id": "r1"]])
    // Answers to other requests are ignored.
    answer(socket, ["id": "other"], "version", "0")
    answer(socket, requests(socket).last, "version", "0.26.0")
    #expect(try await version.value == "0.26.0")
  }

  @Test func throwsTheServersErrorsAndUnexpectedAnswers() async throws {
    let zero = try makeZero()
    let socket = try connect()
    let failing = Task { try await zero.inspector.serverVersion() }
    await settle()
    answer(socket, requests(socket).last, "error", "boom")
    await #expect(throws: InspectorError.server("boom")) { try await failing.value }

    let unexpected = Task { try await zero.inspector.serverVersion() }
    await settle()
    answer(
      socket, requests(socket).last, "metrics",
      ["query-materialization-server": [100], "query-update-server": [100]])
    await #expect(throws: InspectorError.unexpectedResponse(op: "metrics")) {
      try await unexpected.value
    }
  }

  @Test func needsThePasswordAndPresentsItAgainAfterAReconnect() async throws {
    let zero = try makeZero()
    var socket = try connect()
    let first = Task { try await zero.inspector.serverVersion() }
    await settle()
    answer(socket, requests(socket).last, "authenticated", false)
    await #expect(throws: InspectorError.authenticationRequired) { try await first.value }
    // No password yet, so none was presented.
    #expect(requests(socket).map { $0["op"] } == ["version"])

    let accepted = Task { try await zero.inspector.authenticate(password: "pw") }
    await settle()
    #expect(withoutID(requests(socket).last) == ["op": "authenticate", "value": "pw"])
    answer(socket, requests(socket).last, "authenticated", true)
    #expect(try await accepted.value == true)

    // The server authenticates each connection.
    socket.serverClose()
    socket = try connect()
    let second = Task { try await zero.inspector.serverVersion() }
    await settle()
    answer(socket, requests(socket).last, "authenticated", false)
    await settle()
    #expect(withoutID(requests(socket).last) == ["op": "authenticate", "value": "pw"])
    answer(socket, requests(socket).last, "authenticated", true)
    await settle()
    #expect(requests(socket).map { $0["op"] } == ["version", "authenticate", "version"])
    answer(socket, requests(socket).last, "version", "1")
    #expect(try await second.value == "1")

    // A refused password is forgotten.
    socket.serverClose()
    socket = try connect()
    let refused = Task { try await zero.inspector.serverVersion() }
    await settle()
    answer(socket, requests(socket).last, "authenticated", false)
    await settle()
    answer(socket, requests(socket).last, "authenticated", false)
    await #expect(throws: InspectorError.authenticationRequired) { try await refused.value }
    let again = Task { try await zero.inspector.serverVersion() }
    await settle()
    answer(socket, requests(socket).last, "authenticated", false)
    await #expect(throws: InspectorError.authenticationRequired) { try await again.value }
    #expect(
      requests(socket).map { $0["op"] } == ["version", "authenticate", "version"])
  }

  @Test func failsThePendingRequestsWhenTheConnectionEndsOrZeroCloses() async throws {
    let zero = try makeZero()
    let socket = try connect()
    let dropped = Task { try await zero.inspector.serverVersion() }
    await settle()
    socket.serverClose()
    scheduler.runDueTimers()
    let error = await #expect(throws: InspectorError.self) { try await dropped.value }
    #expect(error == .connectionLost("WebSocket connection closed abruptly"))

    try connect()
    let closing = Task { try await zero.inspector.serverVersion() }
    let waiting = Task { try await zero.inspector.metrics() }
    await settle()
    zero.close()
    await #expect(throws: InspectorError.clientClosed) { try await closing.value }
    await #expect(throws: InspectorError.clientClosed) { try await waiting.value }
    await #expect(throws: InspectorError.clientClosed) {
      try await zero.inspector.serverVersion()
    }
  }

  @Test func mergesTheClientsMetricsWithTheServers() async throws {
    let zero = try makeZero()
    let view = try zero.materialize(Self.all())
    let socket = try connect()
    scheduler.advance(by: 1_234)
    poke(socket, from: nil, to: "01", Self.got)
    let metrics = Task { try await zero.inspector.metrics() }
    await settle()
    #expect(withoutID(requests(socket).last) == ["op": "metrics"])
    answer(
      socket, requests(socket).last, "metrics",
      ["query-materialization-server": [100, 5, 1, 7, 2], "query-update-server": [100]])
    let m = try await metrics.value
    // The client's digests are its live ones.
    #expect(
      m.queryMaterializationEndToEnd === zero.engine.queryManager.metrics.materializationEndToEnd)
    #expect(m.queryMaterializationEndToEnd.quantile(0.5) == 1_234)
    #expect(m.queryMaterializationServer.toJSON() == [100, 5, 1, 7, 2])
    #expect(m.queryUpdateServer.count() == 0)
    #expect(m["query-update-client"] === m.queryUpdateClient)
    view.destroy()
  }

  @Test func listsTheQueriesWithTheClientsViewOfThem() async throws {
    let zero = try makeZero()
    // The inspector keeps the queries the client drops from then on.
    let inspector = zero.inspector
    let view = try zero.materialize(Self.all())
    let socket = try connect()
    scheduler.advance(by: 40)
    poke(socket, from: nil, to: "01", Self.got)
    view.destroy()
    let queries = Task { try await inspector.client.queries() }
    await settle()
    #expect(
      withoutID(requests(socket).last) == ["op": "queries", "clientID": .string(zero.clientID)])
    let cid = JSONValue.string(zero.clientID)
    answer(
      socket, requests(socket).last, "queries",
      [
        [
          "clientID": cid, "queryID": .string(Self.hash),
          "ast": ["table": "issues", "orderBy": [["id", "asc"]]], "name": "issues.all", "args": [],
          "got": true, "deleted": false, "ttl": 300_000, "inactivatedAt": nil, "rowCount": 2,
          "metrics": [
            "query-hydration-server-ms": 3, "query-update-server": [100, 2, 3, 4, 1, 10, 2],
          ],
        ],
        [
          "clientID": cid, "queryID": "gone", "ast": nil, "name": nil, "args": nil, "got": false,
          "deleted": true, "ttl": -1, "inactivatedAt": 1_700_000_000_000, "rowCount": 0,
          "metrics": nil,
        ],
        [
          "clientID": cid, "queryID": "slow", "ast": ["table": "users"], "name": nil, "args": nil,
          "got": false, "deleted": false, "ttl": 0, "inactivatedAt": nil, "rowCount": 0,
          "metrics": ["query-hydration-server-ms": 10, "query-update-server": [100]],
        ],
      ])
    let result = try await queries.value
    // The most expensive on the server first.
    #expect(result.map(\.id) == ["slow", Self.hash, "gone"])
    let all = result[1]
    #expect(all.name == "issues.all")
    #expect(all.args == [])
    #expect(all.got)
    #expect(all.ttl == "5m")
    #expect(all.rowCount == 2)
    #expect(all.clientZQL == "issue.orderBy('id', 'asc')")
    #expect(all.serverZQL == "issues.orderBy('id', 'asc')")
    #expect(all.hydrateClient == 0)
    #expect(all.hydrateServer == 3)
    #expect(all.hydrateTotal == 40)
    #expect(all.updateServerP50 == 3.5)
    #expect(all.updateServerP95 == 10)
    #expect(all.updateClientP50 == nil)
    #expect(all.metrics.queryMaterializationEndToEnd.toJSON() == [100, 40, 1])
    let gone = result[2]
    #expect(gone.ttl == .forever)
    #expect(gone.deleted)
    #expect(gone.inactivatedAt == 1_700_000_000_000)
    #expect(gone.clientZQL == nil)
    #expect(gone.serverZQL == nil)
    #expect(gone.hydrateServer == nil)
    #expect(gone.metrics.queryUpdateServer.count() == 0)
    #expect(result[0].ttl == TTL.none)
    #expect(result[0].serverZQL == "users")

    let group = Task { try await inspector.clientGroup.queries() }
    await settle()
    #expect(withoutID(requests(socket).last) == ["op": "queries"])
    answer(socket, requests(socket).last, "queries", [])
    #expect(try await group.value.isEmpty)
  }

  @Test func readsTheLocalClientsAndTheirData() async throws {
    let key = randomID()
    let zero = try makeZero(storageKey: key)
    let other = try makeZero(storageKey: key, sockets: MockSocketFactory())
    defer { other.close() }
    let view = try zero.materialize(Self.all())
    let socket = try connect()
    poke(socket, from: nil, to: "01", Self.got + #","rowsPatch":[\#(Self.row("i1"))]"#)
    #expect(view.data.json?.arrayValue?.count == 1)

    let rows = try await zero.inspector.client.rows("issue")
    #expect(rows.map { $0["id"] } == ["i1"])
    let map = try await zero.inspector.client.map()
    #expect(map["e/issue/i1"]?["id"] == "i1")
    #expect(map["g/\(Self.hash)"] != nil)

    let clients = try await zero.inspector.clients()
    #expect(Set(clients.map(\.id)) == [zero.clientID, other.clientID])
    #expect(clients.allSatisfy { $0.clientGroup.id == zero.clientGroupID })
    #expect(try await zero.inspector.clientGroup.clients().count == 2)
    // The other client shares the group's data.
    let otherClient = try #require(clients.first { $0.id == other.clientID })
    #expect(try await otherClient.rows("issue").count == 1)
    await #expect(throws: InspectorError.clientNotFound("nobody")) {
      try await InspectorClient(zero.inspector.state, id: "nobody", clientGroupID: "g").map()
    }

    // Every client is asked about at once; the answers come in any order.
    let withQueries = Task { try await zero.inspector.clientsWithQueries() }
    await settle()
    let asked = requests(socket).suffix(2)
    #expect(Set(asked.map { $0["clientID"] }) == [.string(zero.clientID), .string(other.clientID)])
    let otherRequest = asked.first { $0["clientID"] == .string(other.clientID) }
    let ownRequest = asked.first { $0["clientID"] == .string(zero.clientID) }
    answer(
      socket, otherRequest, "queries",
      [
        [
          "clientID": .string(other.clientID), "queryID": "q", "ast": nil, "name": "x",
          "args": [], "got": true, "deleted": false, "ttl": 0, "inactivatedAt": nil,
          "rowCount": 0,
        ]
      ])
    answer(socket, ownRequest, "queries", [])
    #expect(try await withQueries.value.map(\.id) == [other.clientID])
  }

  @Test func analyzesQueriesByNameOrByTheServersAST() async throws {
    let zero = try makeZero()
    let socket = try connect()
    let result: JSONValue = [
      "warnings": ["w"], "syncedRowCount": 1, "start": 1, "end": 5, "elapsed": 4,
      "syncedRows": ["issues": [["id": "i1"]]], "readRowCountsByQuery": ["issues": ["q": 3]],
      "joinPlans": [["type": "plan-failed", "attemptNumber": 0, "reason": "x"]],
    ]

    let named = Task { try await zero.inspector.analyzeQuery(Self.all()) }
    await settle()
    #expect(
      withoutID(requests(socket).last) == ["op": "analyze-query", "name": "issues.all", "args": []])
    answer(socket, requests(socket).last, "analyze-query", result)
    let analyzed = try await named.value
    #expect(analyzed.warnings == ["w"])
    #expect(analyzed.syncedRows?["issues"]?.map { $0["id"] } == ["i1"])
    #expect(analyzed.readRowCountsByQuery == ["issues": ["q": 3]])
    #expect(analyzed.elapsed == 4)
    #expect(analyzed.joinPlans?.count == 1)
    #expect(analyzed.json == result)

    // An unnamed query goes by its AST, with the server's names.
    let unnamed = Task {
      try await zero.inspector.analyzeQuery(
        zql.issue.where(\.ownerId, "u1"), options: AnalyzeQueryOptions(syncedRows: true))
    }
    await settle()
    let sent = requests(socket).last
    #expect(sent?["ast"]?["table"] == "issues")
    #expect(sent?["ast"]?["where"]?["left"] == ["type": "column", "name": "owner_id"])
    #expect(sent?["options"] == ["syncedRows": true])
    answer(socket, sent, "analyze-query", result)
    _ = try await unnamed.value

    let server = Task {
      try await zero.inspector.analyzeServerAST(
        AST(table: "custom"), options: AnalyzeQueryOptions(vendedRows: false, joinPlans: true))
    }
    await settle()
    #expect(
      withoutID(requests(socket).last) == [
        "op": "analyze-query", "ast": ["table": "custom"],
        "options": ["vendedRows": false, "joinPlans": true],
      ])
    answer(socket, requests(socket).last, "analyze-query", result)
    _ = try await server.value

    let byName = Task {
      try await zero.inspector.analyzeNamedQuery(name: "issues.byOwner", args: [["ownerId": "u"]])
    }
    await settle()
    #expect(
      withoutID(requests(socket).last) == [
        "op": "analyze-query", "name": "issues.byOwner", "args": [["ownerId": "u"]],
      ])
    answer(socket, requests(socket).last, "analyze-query", result)
    _ = try await byName.value
  }

  // MARK: - Formatting

  @Test(arguments: [
    (
      #"{"table":"issue","where":{"type":"and","conditions":[{"type":"simple","op":"=","left":{"type":"column","name":"id"},"right":{"type":"literal","value":"it's \"x\" \\ y"}},{"type":"simple","op":">","left":{"type":"column","name":"createdAt"},"right":{"type":"literal","value":5}}]},"orderBy":[["id","asc"]],"limit":10}"#,
      #"issue.where('id', 'it\'s "x" \\ y').where('createdAt', '>', 5).orderBy('id', 'asc').limit(10)"#
    ),
    (
      #"{"table":"issue","where":{"type":"or","conditions":[{"type":"simple","op":"=","left":{"type":"column","name":"closed"},"right":{"type":"literal","value":true}},{"type":"correlatedSubquery","op":"EXISTS","related":{"correlation":{"parentField":["id"],"childField":["issueId"]},"subquery":{"table":"comment","alias":"zsubq_comments","where":{"type":"simple","op":"LIKE","left":{"type":"column","name":"body"},"right":{"type":"literal","value":"%a%"}}}}},{"type":"correlatedSubquery","op":"NOT EXISTS","related":{"correlation":{"parentField":["id"],"childField":["issueId"]},"subquery":{"table":"label","alias":"zsubq_labels"}},"flip":true},{"type":"and","conditions":[{"type":"simple","op":"IS","left":{"type":"column","name":"ownerId"},"right":{"type":"literal","value":null}},{"type":"simple","op":"!=","left":{"type":"column","name":"title"},"right":{"type":"static","anchor":"authData","field":"sub"}}]}]}}"#,
      #"issue.where(({and, cmp, exists, not, or}) => or(cmp('closed', true), exists('comments', q => q.where('body', 'LIKE', '%a%')), not(exists('labels', {flip: true})), and(cmp('ownerId', 'IS', null), cmp('title', '!=', authParam('sub')))))"#
    ),
    (
      #"{"table":"issue","related":[{"correlation":{"parentField":["id"],"childField":["issueId"]},"hidden":true,"subquery":{"table":"issueLabel","alias":"labels","related":[{"correlation":{"parentField":["labelId"],"childField":["id"]},"subquery":{"table":"label","alias":"labels","orderBy":[["name","desc"]]}}]}},{"correlation":{"parentField":["id"],"childField":["issueId"]},"subquery":{"table":"comment","alias":"comments","limit":3}}],"where":{"type":"and","conditions":[{"type":"simple","op":"IN","left":{"type":"column","name":"id"},"right":{"type":"literal","value":["a","b"]}},{"type":"simple","op":"=","left":{"type":"json","value":{"type":"column","name":"meta"},"path":["a",0]},"right":{"type":"literal","value":"q"}},{"type":"simple","op":"=","left":{"type":"column","name":"ownerId"},"right":{"type":"static","anchor":"authData","field":["org","id"]}}]},"start":{"row":{"id":"x"},"exclusive":false}}"#,
      #"issue.where('id', 'IN', ["a","b"]).where(({cmp, json}) => cmp(json('meta', 'a', 0), 'q')).where('ownerId', authParam(['org', 'id'])).related('labels', q => q.orderBy('name', 'desc')).related('comments', q => q.limit(3)).start({"id":"x"}, { inclusive: true })"#
    ),
    (
      #"{"table":"issue","groupBy":["ownerId"],"aggregate":{"fn":"count"},"orderBy":[["ownerId","asc"]]}"#,
      #"issue.groupBy('ownerId').count().orderBy('ownerId', 'asc')"#
    ),
    (
      #"{"table":"issue","aggregate":{"fn":"count","distinct":["ownerId","closed"]}}"#,
      #"issue.distinct('ownerId', 'closed').count()"#
    ),
    (
      #"{"table":"issue","groupBy":["ownerId"],"aggregate":{"fn":"sum","field":"meta","path":["n",1]},"start":{"row":{"ownerId":"u"},"exclusive":true}}"#,
      #"issue.groupBy('ownerId').sum(({json}) => json('meta', 'n', 1)).start({"ownerId":"u"})"#
    ),
  ])
  func writesTheQueryAsJSsAstToZQLDoes(json: String, zql: String) throws {
    let ast = try AST(json: JSONParser.parse(Array(json.utf8)))
    #expect(ast.table + astToZQL(ast) == zql)
  }

  @Test func formatsPlannerEventsAsJSDoes() throws {
    let events = try JSONParser.parse(
      Array(
        #"[{"type":"attempt-start","attemptNumber":0,"totalAttempts":5},{"type":"node-cost","attemptNumber":0,"nodeType":"connection","node":"issue","branchPattern":[0,1],"downstreamChildSelectivity":0.5,"costEstimate":{"startupCost":0,"scanEst":100,"cost":101.005,"returnedRows":33.3333333,"selectivity":0.123456789,"limit":10},"filters":{"type":"and","conditions":[{"type":"simple","op":"=","left":{"type":"column","name":"id"},"right":{"type":"literal","value":"x"}},{"type":"simple","op":">","left":{"type":"json","value":{"type":"column","name":"meta"},"path":["a",0]},"right":{"type":"literal","value":3}},{"type":"or","conditions":[{"type":"simple","op":"=","left":{"type":"column","name":"o"},"right":{"type":"static","anchor":"authData","field":["a","b"]}},{"type":"correlatedSubquery","op":"EXISTS","related":{"correlation":{"parentField":["id"],"childField":["issueId"]},"subquery":{"table":"comment"}}}]}]},"ordering":[["id","asc"],["createdAt","desc"]]},{"type":"node-constraint","attemptNumber":0,"nodeType":"connection","node":"issue","branchPattern":[0,1],"constraint":{"id":null,"ownerId":null},"from":"join1"},{"type":"node-cost","attemptNumber":0,"nodeType":"connection","node":"comment","branchPattern":[],"downstreamChildSelectivity":1,"costEstimate":{"startupCost":0,"scanEst":100,"cost":2.675,"returnedRows":33.3333333,"selectivity":1e-9}},{"type":"node-cost","attemptNumber":0,"nodeType":"join","node":"join1","branchPattern":[],"downstreamChildSelectivity":1.5e-8,"costEstimate":{"startupCost":0,"scanEst":100,"cost":1.005,"returnedRows":33.3333333,"selectivity":0.123456789},"joinType":"semi"},{"type":"node-cost","attemptNumber":0,"nodeType":"join","node":"join2","branchPattern":[],"downstreamChildSelectivity":1,"costEstimate":{"startupCost":0,"scanEst":100,"cost":101.005,"returnedRows":33.3333333,"selectivity":0.123456789}},{"type":"plan-complete","attemptNumber":0,"totalCost":2.5,"flipPattern":0,"joinStates":[{"join":"join1","type":"semi"}]},{"type":"attempt-start","attemptNumber":1,"totalAttempts":5},{"type":"plan-failed","attemptNumber":1,"reason":"no plan"},{"type":"best-plan-selected","bestAttemptNumber":0,"totalCost":2.5,"flipPattern":0,"joinStates":[{"join":"join1","type":"semi"},{"join":"join2","type":"flipped"}]}]"#
          .utf8))
    let rule = String(repeating: "─", count: 60)
    let expected = """
      [Attempt 1/5] Pattern 0 (000)
        Connections:
          issue:
            cost=101.00, startup=0.00, scan=100.00
            rows=33.33, selectivity=0.12345679, limit=10
            downstreamChildSelectivity=0.50000000
            constraints={id, ownerId}
            filters=(id = 'x' AND meta.a[0] > 3 AND (o = @authData.a.b OR EXISTS(comment)))
            ordering=id asc, createdAt desc
          comment:
            cost=2.67, startup=0.00, scan=100.00
            rows=33.33, selectivity=0.00000000, limit=none
            downstreamChildSelectivity=1.00000000
            constraints={}
            filters=none
            ordering=none
        Joins:
          join1 (semi):
            cost=1.00, startup=0.00, scan=100.00
            rows=33.33, selectivity=0.12345679, limit=none
            downstreamChildSelectivity=0.00000001
          join2:
            cost=101.00, startup=0.00, scan=100.00
            rows=33.33, selectivity=0.12345679, limit=none
            downstreamChildSelectivity=1.00000000
        ✓ Plan complete: total cost = 2.50

      [Attempt 2/5] Pattern 1 (001)
        ✗ Plan failed: no plan

      \(rule)
      ✓ Best plan: Attempt 1 (cost=2.50)
        Join types:
          join1: semi
          join2: flipped
      \(rule)
      """
    let zero = try makeZero()
    #expect(zero.inspector.formatPlannerEvents(events.arrayValue ?? []) == expected)
  }

  @Test(arguments: [
    (1.005, "1", "1.00", "1.00500000"),
    (2.5, "3", "2.50", "2.50000000"),
    (-0.001, "-0", "-0.00", "-0.00100000"),
    (0.5, "1", "0.50", "0.50000000"),
    (1e-7, "0", "0.00", "0.00000010"),
    (123_456_789.123_456_79, "123456789", "123456789.12", "123456789.12345679"),
    (0, "0", "0.00", "0.00000000"),
    (-0.0, "0", "0.00", "0.00000000"),
    (1e21, "1e+21", "1e+21", "1e+21"),
    (Double.leastNonzeroMagnitude, "0", "0.00", "0.00000000"),
    (0.125, "0", "0.13", "0.12500000"),
    (1.45, "1", "1.45", "1.45000000"),
    (8.345, "8", "8.35", "8.34500000"),
    (99.995, "100", "100.00", "99.99500000"),
    (-2.5, "-3", "-2.50", "-2.50000000"),
    (
      100_000_000_000_000_065_536, "100000000000000065536", "100000000000000065536.00",
      "100000000000000065536.00000000"
    ),
    (1.5e-8, "0", "0.00", "0.00000001"),
  ])
  func roundsAsJSsToFixedDoes(value: Double, zero: String, two: String, eight: String) {
    #expect(toFixed(value, 0) == zero)
    #expect(toFixed(value, 2) == two)
    #expect(toFixed(value, 8) == eight)
  }
}
