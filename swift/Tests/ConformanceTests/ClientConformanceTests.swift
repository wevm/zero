import Foundation
import Testing
import ZeroTesting

@testable import Zero

/// Replays the client scenarios (`packages/zero-conformance/src/client/`)
/// and compares what the app and the server observe after every step with
/// what the JS client did. The runner mirrors `harness.ts`.
@Suite(.enabled(if: Vectors.available))
@MainActor
struct ClientConformanceTests {
  @Test func scenariosOnMemory() async throws {
    try await Self.run(storage: .memory)
  }

  @Test func scenariosOnSQLite() async throws {
    let directory = NSTemporaryDirectory() + "zero-client-scenarios-" + UUID().uuidString
    defer { try? FileManager.default.removeItem(atPath: directory) }
    try await Self.run(storage: .sqlite(directory: directory))
  }

  /// Runs the scenarios one main-actor job each: the tests of the other
  /// suites in the process share the main actor, and a UI test waiting on it
  /// would otherwise wait for every scenario.
  static func run(storage: ZeroStorage, vectors: String = "client") async throws {
    let file = try Vectors.file(vectors)
    let startTime = file["shared"]?["startTime"]?.numberValue ?? 0
    let only = ProcessInfo.processInfo.environment["ZERO_CLIENT_SCENARIO"]
    var failed = 0
    var passed = 0
    for scenario in file["cases"]?.arrayValue ?? [] {
      let name = scenario["name"]?.stringValue ?? "?"
      if let only, !name.contains(only) { continue }
      let expected = scenario["expect"]?.arrayValue ?? []
      let runner = try ScenarioRunner(scenario, startTime: startTime, storage: storage)
      let actual = await runner.run()
      if let mismatch = Self.firstMismatch(scenario, expected, actual) {
        failed += 1
        Issue.record(Comment(rawValue: "\(name)\n\(mismatch)"))
      } else {
        passed += 1
      }
      await Task.yield()
    }
    print("[\(vectors) scenarios on \(storage)] \(passed) passed, \(failed) failed")
  }

  static func firstMismatch(
    _ scenario: JSONValue, _ expected: [JSONValue], _ actual: [JSONValue]
  )
    -> String?
  {
    let steps = scenario["steps"]?.arrayValue ?? []
    for i in 0..<max(expected.count, actual.count) {
      let e = i < expected.count ? expected[i] : .null
      let a = i < actual.count ? actual[i] : .null
      if e != a {
        let step = i < steps.count ? JSONSerializer.stringify(steps[i]) : "?"
        var lines = ["step \(i): \(step)"]
        let keys = Set((e.objectValue ?? [:]).keys).union((a.objectValue ?? [:]).keys).sorted()
        for key in keys where e[key] != a[key] {
          lines.append("  \(key):")
          lines.append("    expected \(e[key].map(JSONSerializer.stringify) ?? "(none)")")
          lines.append("    actual   \(a[key].map(JSONSerializer.stringify) ?? "(none)")")
        }
        return lines.joined(separator: "\n")
      }
    }
    return nil
  }
}

/// Runs one scenario (`runScenario` in `harness.ts`).
@MainActor
final class ScenarioRunner {
  let scenario: JSONValue
  let scheduler: ManualScheduler
  let factory = MockSocketFactory()
  var zero: Zero<Void>!
  var events: [JSONValue] = []
  /// View and `listen` listener calls so far, for `batchViewUpdates`.
  var listenerCalls = 0
  var placeholders: [String: String] = [:]

  final class ViewEntryState {
    let view: MaterializedView
    var data: JSONValue = .null
    var resultType = ""
    var notifications = 0
    init(_ view: MaterializedView) { self.view = view }
  }

  final class Settled {
    var value: JSONValue = "pending"
  }

  var views: [(String, ViewEntryState)] = []
  var retired: [ViewEntryState] = []
  var mutations: [(String, Settled, Settled)] = []
  var runs: [(String, Settled)] = []
  var preloads: [(String, Settled, Settled, () -> Void)] = []

  /// A listener added by a `listen` step.
  final class ListenerState {
    var remove: () -> Void = {}
    var calls: [JSONValue] = []
    var threw: String?
  }

  var listeners: [(String, ListenerState)] = []

  let storage: ZeroStorage
  /// The scenario's own storage, as `#scenario-<n>` in `harness.ts`.
  let scenarioKey: Int
  static var scenarioCount = 0
  var restarts = 0
  /// The IDs placeholders in downstream messages stand for.
  var ids: [String: String] = [:]
  /// The current client's log, and how much of it was reported.
  var log = RecordingLogSink()
  var logsSeen = 0
  /// The `inspect` calls, how many are still running, and how many
  /// inspector requests were sent.
  var inspectCalls: [(String, Settled)] = []
  var inspectRunning = 0
  var inspectCount = 0
  /// The IDs the runner gives inspector requests, so that runs repeat.
  var inspectIDs = 0

  init(_ scenario: JSONValue, startTime: Double, storage: ZeroStorage) throws {
    self.scenario = scenario
    self.storage = storage
    Self.scenarioCount += 1
    scenarioKey = Self.scenarioCount
    scheduler = ManualScheduler(now: startTime)
    zero = try makeZero(scenario["options"] ?? [:])
    placeholders = [zero.clientID: "$clientID", zero.clientGroupID: "$clientGroupID"]
    ids = ["$clientID": zero.clientID]
  }

  func makeZero(_ o: JSONValue, mutators registered: JSONValue? = nil) throws -> Zero<Void> {
    var options = ZeroOptions()
    log = RecordingLogSink()
    options.logSink = log
    options.logLevel =
      switch o["logLevel"]?.stringValue {
      case "debug": .debug
      case "info": .info
      case "error": .error
      default: .warn
      }
    switch o["cacheURL"] {
    case .null?: options.cacheURL = nil
    case .string(let s)?: options.cacheURL = s
    default: options.cacheURL = "http://zero.test"
    }
    switch o["userID"] {
    case .null?: options.userID = nil
    case .string(let s)?: options.userID = s
    default: options.userID = "u1"
    }
    options.auth = o["auth"]?.stringValue
    if let v = o["pingTimeoutMs"]?.numberValue { options.pingTimeoutMs = v }
    if let v = o["disconnectTimeoutMs"]?.numberValue { options.disconnectTimeoutMs = v }
    if let v = o["queryChangeThrottleMs"]?.numberValue { options.queryChangeThrottleMs = v }
    if let v = o["maxHeaderLength"]?.numberValue { options.maxHeaderLength = Int(v) }
    if let v = o["slowMaterializeThreshold"]?.numberValue { options.slowMaterializeThreshold = v }
    options.hiddenDisconnectDelayMs = o["hiddenTabDisconnectDelay"]?.numberValue ?? 300_000
    options.mutateURL = o["mutateURL"]?.stringValue
    options.mutateHeaders = o["mutateHeaders"]?.objectValue?.dictionary.compactMapValues(
      \.stringValue)
    options.queryURL = o["queryURL"]?.stringValue
    options.queryHeaders = o["queryHeaders"]?.objectValue?.dictionary.compactMapValues(
      \.stringValue)
    if o["batchViewUpdates"]?.boolValue == true {
      options.batchViewUpdates = { [weak self] apply in
        let before = self?.listenerCalls ?? 0
        apply()
        guard let self, self.listenerCalls > before else { return }
        self.events.append(["batchViewUpdates": .number(Double(self.listenerCalls - before))])
      }
    }
    if let traceparent = o["traceparent"]?.stringValue {
      options.getTraceparent = { traceparent }
    }
    options.storageKey = "\(o["storageKey"]?.stringValue ?? "")#scenario-\(scenarioKey)"
    options.storage = storage
    options.onUpdateNeeded = { [weak self] reason in
      self?.events.append(["onUpdateNeeded": reason.json])
    }
    options.onClientStateNotFound = { [weak self] in
      self?.events.append("onClientStateNotFound")
    }
    let mutators =
      registered?.stringValue == "issues"
      ? ClientApp.mutators.filter { $0.name.hasPrefix("issue.") } : ClientApp.mutators
    let zero = try Zero(
      schema: ClientApp.schema, mutators: mutators, options: options, scheduler: scheduler,
      socketFactory: factory)
    zero.engine.inspectRPCs.makeID = { [unowned self] in
      inspectIDs += 1
      return "inspect-request-\(inspectIDs)"
    }
    return zero
  }

  /// Closes the client and starts a new one on the same storage
  /// (`restart` in `harness.ts`).
  func restart(_ step: JSONValue) throws {
    zero.close()
    settle()
    if let ms = step["advance"]?.numberValue { scheduler.advance(by: ms) }
    let n = restarts
    restarts += 1
    let old = (clientID: zero.clientID, clientGroupID: zero.clientGroupID)
    zero = try makeZero(step["options"] ?? scenario["options"] ?? [:], mutators: step["mutators"])
    placeholders[old.clientID] = "$clientID.\(n)"
    ids["$clientID.\(n)"] = old.clientID
    if old.clientGroupID != zero.clientGroupID {
      placeholders[old.clientGroupID] = "$clientGroupID.\(n)"
    }
    placeholders[zero.clientID] = "$clientID"
    placeholders[zero.clientGroupID] = "$clientGroupID"
    ids["$clientID"] = zero.clientID
    views = []
    listeners = []
    retired = []
    mutations = []
    runs = []
    preloads = []
    inspectCalls = []
    logsSeen = 0
  }

  func normalize(_ v: JSONValue, key: String? = nil) -> JSONValue {
    switch v {
    case .string(let s):
      if key == "requestID" { return "$requestID" }
      return .string(placeholders[s] ?? s)
    case .array(let a):
      return .array(a.map { normalize($0) })
    case .object(let o):
      var out: JSONObject = [:]
      for (k, e) in o { out[placeholders[k] ?? k] = normalize(e, key: k) }
      return .object(out)
    default:
      return v
    }
  }

  func substitute(_ v: JSONValue) -> JSONValue {
    switch v {
    case .string(let s):
      return .string(ids[s] ?? s)
    case .array(let a):
      return .array(a.map(substitute))
    case .object(let o):
      var out: JSONObject = [:]
      for (k, e) in o { out[ids[k] ?? k] = substitute(e) }
      return .object(out)
    default:
      return v
    }
  }

  func socket(_ index: JSONValue?) throws -> MockSocket {
    let sockets = factory.sockets
    if let i = index?.numberValue {
      guard Int(i) < sockets.count else { throw ClientApp.Failure("No socket") }
      return sockets[Int(i)]
    }
    guard let last = sockets.last else { throw ClientApp.Failure("No socket") }
    return last
  }

  static func ttl(_ v: JSONValue?) -> TTL? {
    switch v {
    case .number(let n)?: .milliseconds(n)
    case .string(let s)?: .string(s)
    default: nil
    }
  }

  func perform(_ step: JSONValue) throws {
    let op = step["op"]?.stringValue ?? ""
    switch op {
    case "materialize":
      let query = try ClientApp.request(step["query"]!.stringValue!, step["args"])
      let view = try zero.materialize(query, ttl: Self.ttl(step["ttl"]) ?? .defaultTTL)
      let entry = ViewEntryState(view)
      let name = step["view"]!.stringValue!
      // A view replaced under its name stays alive, as in JS.
      retired += views.filter { $0.0 == name }.map(\.1)
      views.removeAll { $0.0 == name }
      views.append((name, entry))
      _ = view.addListener { [unowned self] data, resultType, _ in
        entry.data = data.json ?? .null
        entry.resultType = resultType.rawValue
        entry.notifications += 1
        self.listenerCalls += 1
      }
    case "destroy":
      let name = step["view"]!.stringValue!
      views.first { $0.0 == name }?.1.view.destroy()
      views.removeAll { $0.0 == name }
    case "updateTTL":
      let name = step["view"]!.stringValue!
      if let ttl = Self.ttl(step["ttl"]) { views.first { $0.0 == name }?.1.view.updateTTL(ttl) }
    case "preload":
      let query = try ClientApp.request(step["query"]!.stringValue!, step["args"])
      let p = zero.preload(query, ttl: Self.ttl(step["ttl"]) ?? .defaultPreloadTTL)
      let cached = Settled()
      let complete = Settled()
      preloads.append((step["preload"]!.stringValue!, cached, complete, p.cleanup))
      p.cached.onSettle { cached.value = Self.settled($0) }
      p.complete.onSettle { complete.value = Self.settled($0) }
    case "cleanup":
      let name = step["preload"]!.stringValue!
      if let p = preloads.first(where: { $0.0 == name }) {
        zero.engine.loop.dispatch { p.3() }
      }
    case "run":
      let name = step["run"]!.stringValue!
      let entry = Settled()
      runs.removeAll { $0.0 == name }
      runs.append((name, entry))
      let query = try ClientApp.request(step["query"]!.stringValue!, step["args"])
      let type = RunType(rawValue: step["type"]?.stringValue ?? "unknown")!
      let result = try zero.run(query, type: type)
      result.onSettle { r in
        switch r {
        case .success(let data): entry.value = ["data": data.json ?? .null]
        case .failure(let e as QueryError): entry.value = ["rejected": e.data?.json ?? .null]
        case .failure: entry.value = ["rejected": [:]]
        }
      }
    case "mutate":
      let name = step["mutator"]!.stringValue!
      guard ClientApp.mutators.contains(where: { $0.name == name }) else {
        throw ClientApp.Failure("Unknown name \(name)")
      }
      let result = zero.engine.loop.dispatch {
        zero.engine.mutate(name: name, args: step["args"])
      }
      let client = Settled()
      let server = Settled()
      mutations.append((step["mutation"]!.stringValue!, client, server))
      result.client.onSettle { if case .success(let o) = $0 { client.value = o.json } }
      result.server.onSettle { if case .success(let o) = $0 { server.value = o.json } }
    case "receive":
      try socket(step["socket"]).receive(
        text: JSONSerializer.stringify(substitute(step["message"]!)))
    case "receiveAll":
      let s = try socket(step["socket"])
      for m in step["messages"]?.arrayValue ?? [] {
        s.receive(text: JSONSerializer.stringify(substitute(m)))
      }
    case "receiveChunk":
      let payload: [UInt8]
      if let bytes = step["bytes"]?.arrayValue {
        payload = bytes.map { UInt8($0.numberValue!) }
      } else if let text = step["text"]?.stringValue {
        payload = Array(text.utf8)
      } else {
        payload = Array(JSONSerializer.stringify(substitute(step["parts"] ?? [])).utf8)
      }
      try socket(step["socket"]).receive(.binary([PokeChunk.messageType] + payload))
    case "receiveText":
      try socket(step["socket"]).receive(text: step["text"]!.stringValue!)
    case "receiveBinary":
      try socket(step["socket"]).receive(
        .binary((step["bytes"]?.arrayValue ?? []).map { UInt8($0.numberValue!) }))
    case "socketClose":
      try socket(step["socket"]).serverClose(
        code: Int(step["code"]?.numberValue ?? 1006), reason: step["reason"]?.stringValue ?? "",
        wasClean: step["wasClean"]?.boolValue ?? false)
    case "advance":
      scheduler.advance(by: step["ms"]?.numberValue ?? 0)
    case "visibility":
      zero.setVisible(step["state"] == "visible")
    case "onState":
      let name = step["state"]!.stringValue!
      let then = step["then"]!
      var unsubscribe: (() -> Void)?
      unsubscribe = zero.connection.subscribe { [weak self] state in
        guard let self, state.status.rawValue == name else { return }
        unsubscribe?()
        unsubscribe = nil
        if then["op"] == "close" {
          self.zero.close()
        } else {
          self.zero.connection.connect(auth: then["auth"]?.stringValue)
        }
      }
    case "connect":
      zero.connection.connect(auth: step["auth"]?.stringValue)
    case "close":
      zero.close()
    case "restart":
      try restart(step)
    case "listen":
      let name = step["view"]!.stringValue!
      guard let view = views.first(where: { $0.0 == name })?.1.view else {
        throw ClientApp.Failure("No view \(name)")
      }
      let after = Int(step["after"]?.numberValue ?? 1)
      var pending = step["actions"]?.arrayValue
      let entry = ListenerState()
      let listener = step["listener"]!.stringValue!
      listeners.removeAll { $0.0 == listener }
      listeners.append((listener, entry))
      var count = 0
      entry.remove = view.addListener { [unowned self] data, resultType, _ in
        entry.calls.append(["data": data.json ?? .null, "resultType": .string(resultType.rawValue)])
        self.listenerCalls += 1
        count += 1
        if count > after, let actions = pending {
          pending = nil
          for action in actions {
            do { try self.perform(action) } catch {
              entry.threw = entry.threw ?? errorMessage(error)
            }
          }
        }
      }
    case "unlisten":
      let name = step["listener"]!.stringValue!
      listeners.first { $0.0 == name }?.1.remove()
    case "inspect":
      try inspect(step)
    default:
      throw ClientApp.Failure("Unknown op \(op)")
    }
  }

  /// An `inspect` step: calls `zero.inspector` and records how its result
  /// settles (`inspect` in `harness.ts`). The calls are async, so each runs
  /// in a task that `settle` lets run.
  func inspect(_ step: JSONValue) throws {
    let call = step["call"]!.stringValue!
    let entry = Settled()
    if let i = inspectCalls.firstIndex(where: { $0.0 == call }) {
      inspectCalls[i].1 = entry
    } else {
      inspectCalls.append((call, entry))
    }
    let inspector = zero.inspector
    let options = step["options"].map { o in
      AnalyzeQueryOptions(
        vendedRows: o["vendedRows"]?.boolValue, syncedRows: o["syncedRows"]?.boolValue,
        joinPlans: o["joinPlans"]?.boolValue)
    }
    func record<T>(_ body: @escaping () async throws -> T, _ convert: @escaping (T) -> JSONValue) {
      inspectRunning += 1
      Task { @MainActor [self] in
        do {
          entry.value = ["resolved": convert(try await body())]
        } catch {
          entry.value = ["rejected": .string(Self.inspectError(error))]
        }
        inspectRunning -= 1
      }
    }
    let clients = { [self] (clients: [InspectorClient]) -> JSONValue in
      let out = clients.map { c -> (String, JSONValue) in
        let id = placeholders[c.id] ?? c.id
        return (id, ["id": .string(id), "clientGroupID": normalize(.string(c.clientGroup.id))])
      }
      return .array(out.sorted { StringCompare.utf16($0.0, $1.0) < 0 }.map(\.1))
    }
    let queries = { (queries: [InspectorQuery]) -> JSONValue in .array(queries.map(Self.queryJSON))
    }
    switch step["method"]?.stringValue {
    case "metrics": record({ try await inspector.metrics() }, Self.metricsJSON)
    case "serverVersion": record({ try await inspector.serverVersion() }) { .string($0) }
    case "clients": record({ try await inspector.clients() }, clients)
    case "clientsWithQueries": record({ try await inspector.clientsWithQueries() }, clients)
    case "clientQueries": record({ try await inspector.client.queries() }, queries)
    case "clientMap":
      // Swift's map is a dictionary; JS's iterates in the store's key order.
      record({ try await inspector.client.map() }) { [self] map in
        .array(
          map.sorted { StringCompare.utf8($0.key, $1.key) < 0 }.map { k, v in
            var k = k
            for (id, placeholder) in placeholders {
              k = k.replacingOccurrences(of: id, with: placeholder)
            }
            return [.string(k), v]
          })
      }
    case "clientRows":
      let table = step["table"]?.stringValue ?? ""
      record({ try await inspector.client.rows(table) }) { .array($0.map { .object($0.object) }) }
    case "clientGroupClients": record({ try await inspector.clientGroup.clients() }, clients)
    case "clientGroupClientsWithQueries":
      record({ try await inspector.clientGroup.clientsWithQueries() }, clients)
    case "clientGroupQueries": record({ try await inspector.clientGroup.queries() }, queries)
    case "analyzeQuery":
      let query = try ClientApp.request(step["query"]?.stringValue ?? "", step["args"])
      record({ try await inspector.analyzeQuery(query, options: options) }, \.json)
    case "analyzeServerAST":
      let ast = try AST(json: step["ast"] ?? .null)
      record({ try await inspector.analyzeServerAST(ast, options: options) }, \.json)
    case "analyzeNamedQuery":
      let name = step["name"]?.stringValue ?? ""
      let args = step["queryArgs"]?.arrayValue ?? []
      record(
        { try await inspector.analyzeNamedQuery(name: name, args: args, options: options) }, \.json)
    case "authenticate":
      let password = step["password"]?.stringValue ?? ""
      record({ try await inspector.authenticate(password: password) }) { .bool($0) }
    case "formatPlannerEvents":
      entry.value = [
        "resolved": .string(inspector.formatPlannerEvents(step["events"]?.arrayValue ?? []))
      ]
    case let method:
      throw ClientApp.Failure("Unknown inspector method \(method ?? "")")
    }
  }

  /// JS's message for an inspector error (`errorMessage` in `harness.ts`).
  /// Where zero-cache refuses an unauthenticated request, JS prompts for
  /// the password, and without one fails with `Authentication failed`;
  /// Swift fails with `InspectorError.authenticationRequired`, whose
  /// message says to call `authenticate(password:)`.
  static func inspectError(_ error: any Error) -> String {
    switch error {
    case InspectorError.authenticationRequired: "Authentication failed"
    case InspectorError.server(let message): message
    case let e as InspectorError: e.description
    default: errorMessage(error)
    }
  }

  /// Digests as their JSON, by their JS names (`metricsJSON`).
  static func metricsJSON(_ metrics: Metrics) -> JSONValue {
    var o: JSONObject = [:]
    for name in Metrics.names {
      o[name] = .array((metrics[name]?.toJSON() ?? []).map(JSONValue.number))
    }
    return .object(o)
  }

  /// A query's fields (`queriesJSON`).
  static func queryJSON(_ q: InspectorQuery) -> JSONValue {
    // `JSON.stringify` writes NaN and the infinities as null.
    func number(_ n: Double?) -> JSONValue {
      n.flatMap { $0.isFinite ? .number($0) : nil } ?? .null
    }
    func string(_ s: String?) -> JSONValue { s.map(JSONValue.string) ?? .null }
    return [
      "name": string(q.name), "args": q.args.map(JSONValue.array) ?? .null, "got": .bool(q.got),
      "ttl": q.ttl.json, "inactivatedAt": number(q.inactivatedAt),
      "rowCount": .number(q.rowCount), "deleted": .bool(q.deleted), "id": .string(q.id),
      "clientID": .string(q.clientID), "metrics": metricsJSON(q.metrics),
      "clientZQL": string(q.clientZQL), "serverZQL": string(q.serverZQL),
      "hydrateClient": number(q.hydrateClient), "hydrateServer": number(q.hydrateServer),
      "hydrateTotal": number(q.hydrateTotal), "updateClientP50": number(q.updateClientP50),
      "updateClientP95": number(q.updateClientP95), "updateServerP50": number(q.updateServerP50),
      "updateServerP95": number(q.updateServerP95),
    ]
  }

  /// The materialization messages logged since the last step, as JS logs
  /// them: the level, and the message, query ID, AST, and ms. Swift logs
  /// one line, `<message> <queryID> <AST JSON> <ms>`.
  func materializationLogs() -> [JSONValue] {
    let entries = log.entries
    defer { logsSeen = entries.count }
    return entries[min(logsSeen, entries.count)...].compactMap { e -> JSONValue? in
      guard
        let message = Self.materializationMessages.first(where: { e.message.hasPrefix($0 + " ") })
      else { return nil }
      let rest = e.message.dropFirst(message.count + 1)
      guard let space = rest.firstIndex(of: " "), let last = rest.lastIndex(of: " "), space < last,
        let ast = try? JSONParser.parse(Array(rest[rest.index(after: space)..<last].utf8)),
        let ms = try? JSONParser.parse(Array(rest[rest.index(after: last)...].utf8))
      else { return nil }
      return normalize([
        "level": .string(e.level.description),
        "args": [.string(message), .string(String(rest[..<space])), ast, ms],
      ])
    }
  }

  /// The messages of `addMetric`'s materialization logs.
  static let materializationMessages = [
    "Slow query materialization (including server/network)",
    "Materialized query (including server/network)",
  ]

  static func settled<T>(_ r: Result<T, any Error>) -> JSONValue {
    switch r {
    case .success: "resolved"
    case .failure(let e as QueryError): ["rejected": e.error?.json ?? .null]
    case .failure: ["rejected": [:]]
    }
  }

  /// Settles the work due now (`settle` in `harness.ts`).
  func settle() {
    zero.engine.loop.dispatch {}
    scheduler.runDueTimers()
  }

  /// `settle`, and while inspector calls are running, lets their tasks run
  /// and settles the work they queue, as `settle` in `harness.ts` lets
  /// promises settle.
  func settleWithInspector() async {
    settle()
    for _ in 0..<20 where inspectRunning > 0 {
      await Task.yield()
      settle()
    }
  }

  func run() async -> [JSONValue] {
    var results: [JSONValue] = []
    for step in scenario["steps"]?.arrayValue ?? [] {
      let sockets = factory.sockets
      let sentCounts = sockets.map(\.messages.count)
      let closedBefore = sockets.map(\.closed)
      var result: JSONObject = [:]
      do {
        try perform(step)
      } catch {
        result["threw"] = .string(errorMessage(error))
      }
      await settleWithInspector()

      let now = factory.sockets
      let opened = now[sockets.count...].map { s -> JSONValue in
        [
          "url": socketURL(s.url),
          "protocol": normalize((try? SecProtocols.decode(s.secProtocol ?? "")) ?? .null),
        ]
      }
      if !opened.isEmpty { result["opened"] = .array(opened) }
      var sent: [JSONValue] = []
      for (i, s) in now.enumerated() {
        let from = i < sentCounts.count ? sentCounts[i] : 0
        for m in s.messages[from...] {
          let message = (try? JSONParser.parse(Array(m.utf8))) ?? .string(m)
          if message[0] == "inspect", let id = message[1]?["id"]?.stringValue,
            placeholders[id] == nil
          {
            placeholders[id] = "$inspect.\(inspectCount)"
            ids["$inspect.\(inspectCount)"] = id
            inspectCount += 1
          }
          sent.append(normalize(message))
        }
      }
      if !sent.isEmpty { result["sent"] = .array(sent) }
      var closed: [JSONValue] = []
      for (i, s) in now.enumerated() where s.closed && !(i < closedBefore.count && closedBefore[i])
      {
        closed.append(.number(Double(i)))
      }
      if !closed.isEmpty { result["closed"] = .array(closed) }
      result["state"] = normalize(zero.connection.state.json)
      if !views.isEmpty {
        var o: JSONObject = [:]
        for (name, v) in views {
          o[name] = [
            "data": v.data, "resultType": .string(v.resultType),
            "notifications": .number(Double(v.notifications)),
          ]
          v.notifications = 0
        }
        result["views"] = .object(o)
      }
      if !mutations.isEmpty {
        var o: JSONObject = [:]
        for (name, c, s) in mutations { o[name] = ["client": c.value, "server": s.value] }
        result["mutations"] = .object(o)
      }
      if !runs.isEmpty {
        var o: JSONObject = [:]
        for (name, r) in runs { o[name] = r.value }
        result["runs"] = .object(o)
      }
      if !preloads.isEmpty {
        var o: JSONObject = [:]
        for (name, c, p, _) in preloads { o[name] = ["cached": c.value, "complete": p.value] }
        result["preloads"] = .object(o)
      }
      if !events.isEmpty {
        result["events"] = .array(events)
        events = []
      }
      if !inspectCalls.isEmpty {
        var o: JSONObject = [:]
        for (name, c) in inspectCalls { o[name] = c.value }
        result["inspector"] = normalize(.object(o))
      }
      let logs = materializationLogs()
      if !logs.isEmpty { result["logs"] = .array(logs) }
      if !listeners.isEmpty {
        var o: JSONObject = [:]
        for (name, l) in listeners {
          var v: JSONObject = ["calls": .array(l.calls)]
          if let threw = l.threw { v["threw"] = .string(threw) }
          o[name] = .object(v)
          l.calls = []
          l.threw = nil
        }
        result["listeners"] = .object(o)
      }
      results.append(.object(result))
    }
    zero.close()
    return results
  }

  /// The connect URL with its query parameters in order (`socketURL`).
  func socketURL(_ url: String) -> JSONValue {
    let (base, query) =
      url.firstIndex(of: "?").map { (String(url[..<$0]), String(url[url.index(after: $0)...])) }
      ?? (url, "")
    let schemeEnd = base.range(of: "://")!.upperBound
    let pathStart = base[schemeEnd...].firstIndex(of: "/") ?? base.endIndex
    var pairs: [JSONValue] = []
    for part in query.split(separator: "&") {
      let kv = part.split(separator: "=", maxSplits: 1, omittingEmptySubsequences: false)
      let k = Self.formDecode(String(kv[0]))
      let v = kv.count > 1 ? Self.formDecode(String(kv[1])) : ""
      switch k {
      case "ts": continue
      case "wsid", "profileID": pairs.append([.string(k), .string("$" + k)])
      default: pairs.append([.string(k), .string(placeholders[v] ?? v)])
      }
    }
    return [
      "origin": .string(String(base[..<pathStart])), "path": .string(String(base[pathStart...])),
      "query": .array(pairs),
    ]
  }

  static func formDecode(_ s: String) -> String {
    var bytes: [UInt8] = []
    var it = Array(s.utf8)[...]
    while let b = it.popFirst() {
      if b == UInt8(ascii: "+") {
        bytes.append(UInt8(ascii: " "))
      } else if b == UInt8(ascii: "%"), it.count >= 2,
        let v = UInt8(String(decoding: it.prefix(2), as: UTF8.self), radix: 16)
      {
        bytes.append(v)
        it = it.dropFirst(2)
      } else {
        bytes.append(b)
      }
    }
    return String(decoding: bytes, as: UTF8.self)
  }
}
