/// `RUN_LOOP_INTERVAL_MS`: the backoff before retrying a failed connection.
let runLoopIntervalMs: Double = 5_000
/// `CONNECT_TIMEOUT_MS`: each phase of a connection attempt gets this long.
let connectTimeoutMs: Double = 10_000

/// The options of a Zero client (`ZeroOptions` in
/// `packages/zero-client/src/client/options.ts`).
public struct ZeroOptions: Sendable {
  /// The zero-cache URL, `http` or `https`. Without one the client runs
  /// locally and never connects.
  public var cacheURL: String?
  /// The user the client syncs for. Omit it for a logged-out client.
  public var userID: String?
  /// The auth token zero-cache and the API server see.
  public var auth: String?
  /// Distinguishes local stores of the same user and URLs.
  public var storageKey: String?
  public var pingTimeoutMs: Double = 5_000
  public var disconnectTimeoutMs: Double = 60_000
  /// How long query changes are batched before they are sent.
  public var queryChangeThrottleMs: Double = 10
  /// The longest `Sec-WebSocket-Protocol` header the client sends.
  /// `initConnection` is sent as a message instead when it doesn't fit.
  public var maxHeaderLength: Int = 8 * 1024
  public var mutateURL: String?
  public var mutateHeaders: [String: String]?
  public var queryURL: String?
  public var queryHeaders: [String: String]?
  /// The server needs a newer client. Defaults to doing nothing.
  public var onUpdateNeeded: (@MainActor @Sendable (UpdateNeededReason) -> Void)?
  /// The server no longer has the client's state; the local store was reset.
  public var onClientStateNotFound: (@MainActor @Sendable () -> Void)?

  public init(
    cacheURL: String? = nil, userID: String? = nil, auth: String? = nil,
    storageKey: String? = nil
  ) {
    self.cacheURL = cacheURL
    self.userID = userID
    self.auth = auth
    self.storageKey = storageKey
  }
}

/// Why the client must be updated (`UpdateNeededReason`).
public struct UpdateNeededReason: Sendable, Hashable {
  /// `VersionNotSupported`, `SchemaVersionNotSupported`, or `NewClientGroup`.
  public var type: String
  public var message: String?

  public var json: JSONValue {
    var o: [String: JSONValue] = ["type": .string(type)]
    if let message { o["message"] = .string(message) }
    return .object(o)
  }
}

/// A mutator bound to the app's context: runs it in a transaction.
typealias MutatorFunction = @MainActor (Transaction, JSONValue?) throws -> JSONValue?

/// The client engine (`Zero` in `packages/zero-client/src/client/zero.ts`,
/// with the parts of Replicache it uses): the run loop that keeps a
/// connection, the replica and its pending mutations, and the query and
/// mutation bookkeeping. Everything runs synchronously on the main actor;
/// see `EventLoop`.
@MainActor
final class Engine {
  let schema: ZeroSchema
  let options: ZeroOptions
  let clientID: String
  let clientGroupID: String
  let profileID: String
  let userID: String?
  let loop: EventLoop
  let scheduler: LoopScheduler

  isolated deinit {
    loop.stop()
  }
  private let socketFactory: (any SocketFactory)?
  private let socketOrigin: String?
  private let mutators: [String: MutatorFunction]
  private let clientSchema: ClientSchema
  private let clientToServer: NameMapper

  let store: ReplicaStore
  let ivmMain: IVMBranch
  private(set) var context: QueryContext!
  let connectionManager: ConnectionManager
  private(set) var mutationTracker: MutationTracker!
  private(set) var queryManager: QueryManager!
  private var pokeHandler: PokeHandler!

  private var auth: String?
  private var repClosed = false
  private var socket: SocketHandle?
  private var connectCookie: JSONValue = .null
  private var initConnectionQueries: OrderedMap<JSKey, UpQueriesPatchOp>?
  private var lastMutationIDReceived: Double = 0
  private var lastMutationIDsSent: [String: Double] = [:]
  private var additionalConnectParams: [(String, String)]?
  private var currentAttempt: ConnectAttempt?
  private var connectionReady = ReadyPromise()
  private var recentSentMessages: [JSONValue] = []
  private var mutationRejection: ConnectionError?
  private var clientGroupDisabled = false

  // The run loop's current wait. Each wait gets a new token, so a wait that
  // was superseded does nothing when it ends.
  private var loopToken = 0
  private var abortPingTimeout: () -> Void = {}
  private var onPong: () -> Void = {}

  // The push loop (Replicache's push `ConnectionLoop`).
  private var pushTimer: (any ScheduledTimer)?
  private var pushInFlight = false
  private var pushRequested = false
  private var pushRequestCounter = 0

  init(
    schema: ZeroSchema, options: ZeroOptions, mutators: [String: MutatorFunction],
    scheduler base: any Scheduler, socketFactory: (any SocketFactory)?
  ) throws {
    if options.userID == "" {
      throw ConnectionError.client(
        .internal,
        message:
          "ZeroOptions.userID should not be empty. Omit it entirely for logged-out clients.")
    }
    try Self.checkAuthValid(options.userID, options.auth)
    self.schema = schema
    self.options = options
    self.userID = options.userID
    self.mutators = mutators
    self.socketFactory = socketFactory
    self.socketOrigin = try options.cacheURL.map(Self.socketOrigin)
    self.loop = EventLoop(scheduler: base)
    self.scheduler = LoopScheduler(base, loop)
    self.clientID = randomID()
    self.clientGroupID = randomID()
    self.profileID = "p" + randomID()
    self.clientSchema = schema.clientSchema
    self.clientToServer = NameMapper.clientToServer(schema)
    self.store = ReplicaStore(clientID: clientID)
    self.ivmMain = IVMBranch(schema: schema)
    let loop = self.loop
    self.connectionManager = ConnectionManager(
      disconnectTimeout: options.disconnectTimeoutMs, scheduler: scheduler,
      microtask: { loop.microtask($0) })

    _ = connectionManager.subscribe { [weak self] state in
      if case .closed(let reason) = state { self?.queryManager.handleClosed(reason) }
    }
    mutationTracker = MutationTracker(
      ackMutations: { [weak self] id in self?.sendIfConnected(.ackMutationResponses(id)) },
      onFatalError: { [weak self] error in self?.disconnect(error) })
    mutationTracker.setClientID(clientID)
    context = QueryContext(
      branch: ivmMain,
      addCustomQuery: { [weak self] ast, id, ttl, got in
        self?.queryManager.addCustom(ast, name: id.name, args: id.args, ttl: ttl, gotCallback: got)
          ?? {}
      },
      updateCustomQuery: { [weak self] id, ttl in
        self?.queryManager.updateCustom(name: id.name, args: id.args, ttl: ttl)
      },
      batchViewUpdates: { $0() },
      microtask: { loop.microtask($0) })
    _ = connectionManager.subscribe { [weak self] state in self?.onConnectionStateChange(state) }
    onConnectionStateChange(connectionManager.state)
    queryManager = QueryManager(
      clientID: clientID, schema: schema, mutationTracker: mutationTracker, scheduler: scheduler,
      queryChangeThrottleMs: options.queryChangeThrottleMs, recentQueriesMaxSize: 0,
      send: { [weak self] body in self?.sendIfConnected(.changeDesiredQueries(body)) },
      onFatalError: { [weak self] error in self?.disconnect(error) })
    pokeHandler = PokeHandler(
      clientID: clientID, schema: schema, scheduler: scheduler,
      apply: { [weak self] poke in try self?.applyPoke(poke) },
      onApplied: { [weak self] poke in
        guard let self, let lmid = poke.lastMutationIDChanges[self.clientID] else { return }
        try self.mutationTracker.lmidAdvanced(lmid)
      },
      onPokeError: { [weak self] message in self?.onPokeError(message) })

    // The replica is loaded: report its got queries and mutation results,
    // as the first runs of the store watches do, then build the pipelines.
    fireWatchers(KVDiff.between(KVMap(), store.main), initial: true)
    context.markPipelinesReady()
    loop.microtask { [weak self] in self?.startRunLoop() }
  }

  static func checkAuthValid(_ userID: String?, _ auth: String?) throws {
    if userID == nil, let auth, !auth.isEmpty {
      throw ConnectionError.client(
        .internal, message: "ZeroOptions.userID is required when auth is set.")
    }
  }

  /// The WebSocket origin of the zero-cache URL (`getServer`, `toWSString`).
  static func socketOrigin(_ url: String) throws -> String {
    let scheme: String
    if url.hasPrefix("http://") {
      scheme = "ws://"
    } else if url.hasPrefix("https://") {
      scheme = "wss://"
    } else {
      throw ConnectionError.client(
        .internal, message: "ZeroOptions.cacheURL must use the \"http\" or \"https\" scheme.")
    }
    var rest = String(url.drop { $0 != "/" }.dropFirst(2))
    if !rest.contains("/") { rest += "/" }
    return scheme + rest
  }

  // MARK: - Sending

  private func sendIfConnected(_ message: Upstream) {
    if connectionManager.is(.connected) { send(message) }
  }

  private func send(_ message: Upstream) {
    guard let socket else { return }
    let text = message.text
    if message.tag != "ping" {
      if recentSentMessages.count >= 5 { recentSentMessages.removeFirst() }
      recentSentMessages.append([
        "type": .string(message.tag), "size": .number(Double(text.utf16.count)),
        "snippet": .string(String(text.utf16.prefix(200)) ?? text),
      ])
    }
    socket.send(text)
  }

  // MARK: - The run loop

  private func startRunLoop() {
    guard socketOrigin != nil else {
      connectionManager.disconnected(
        .client(.noSocketOrigin, message: "No server socket origin provided"))
      return
    }
    setAuth(options.auth)
    connectionManager.initialized()
    iterate()
  }

  private func nextLoopToken() -> Int {
    loopToken += 1
    return loopToken
  }

  /// One turn of the JS run loop: acts on the connection status.
  private func iterate() {
    guard connectionManager.shouldContinueRunLoop else { return }
    let token = nextLoopToken()
    switch connectionManager.state.status {
    case .connecting, .disconnected:
      // The app is visible, which wins the race with a state change.
      loop.microtask { [weak self] in
        guard let self, self.loopToken == token else { return }
        self.beginAttempt()
      }
    case .connected:
      waitWhileConnected(token)
    case .needsAuth, .error:
      waitForConnectRequest(token)
    case .closed:
      return
    case .initializing:
      preconditionFailure("The run loop runs once initialized")
    }
  }

  /// Ends the current wait of the run loop with `error`
  /// (the `catch` of the JS run loop).
  private func loopFailed(_ error: ConnectionError) {
    let (transition, reason) = ConnectionTransition.of(error)
    var backoffMs = runLoopIntervalMs
    var sleepMs: Double?
    switch transition {
    case .none:
      if let backoff = reason.backoff {
        if let min = backoff.minBackoffMs { backoffMs = max(backoffMs, min) }
        if let max = backoff.maxBackoffMs { backoffMs = Swift.min(backoffMs, max) }
        additionalConnectParams = backoff.reconnectParams.map { params in
          params.sorted { $0.key < $1.key }.map { ($0.key, $0.value) }
        }
      }
      sleepMs = backoffMs
    case .needsAuth:
      connectionManager.needsAuth(reason)
    case .error:
      connectionManager.error(reason)
    case .disconnected:
      connectionManager.disconnected(reason)
    case .closed:
      break
    }
    let token = nextLoopToken()
    if let sleepMs, sleepMs > 0 {
      _ = scheduler.schedule(after: sleepMs) { [weak self] in
        guard let self, self.loopToken == token else { return }
        self.iterate()
      }
    } else {
      loop.microtask { [weak self] in
        guard let self, self.loopToken == token else { return }
        self.iterate()
      }
    }
  }

  /// `throwIfConnectionError`: whether a state the run loop wakes up to
  /// stops it. Dropped sockets and timeouts just reconnect.
  private static func connectionError(_ state: ConnectionManager.State) -> ConnectionError? {
    switch state {
    case .needsAuth(let r), .error(let r), .closed(let r): return r
    case .connecting(_, _, let r?), .disconnected(let r):
      switch r.clientKind {
      case .connectTimeout?, .abruptClose?, .cleanClose?, .hidden?: return nil
      default: return r
      }
    default: return nil
    }
  }

  /// Wakes the loop on the next state change, unless the wait is over.
  private func onStateChange(_ token: Int, _ body: @escaping (ConnectionManager.State) -> Void) {
    connectionManager.onNextStateChange { [weak self] state in
      guard let self, self.loopToken == token else { return }
      body(state)
    }
  }

  private func resumeAfterStateChange(_ state: ConnectionManager.State) {
    if let error = Self.connectionError(state) {
      loopFailed(error)
    } else {
      iterate()
    }
  }

  /// Connected: ping after `pingTimeoutMs` without poke traffic.
  private func waitWhileConnected(_ token: Int) {
    let ready = connectionReady
    var timer: (any ScheduledTimer)?
    let end = { [weak self] in
      guard let self else { return }
      // The `finally` of the connected case.
      if !self.connectionManager.is(.connected), self.connectionReady === ready {
        self.connectionReady = ReadyPromise()
      }
    }
    abortPingTimeout = { [weak self] in
      timer?.cancel()
      self?.loop.microtask {
        guard let self, self.loopToken == token else { return }
        end()
        self.iterate()
      }
    }
    timer = scheduler.schedule(after: options.pingTimeoutMs) { [weak self] in
      guard let self, self.loopToken == token else { return }
      self.ping(token, end)
    }
    onStateChange(token) { [weak self] state in
      end()
      self?.resumeAfterStateChange(state)
    }
  }

  private func ping(_ token: Int, _ end: @escaping () -> Void) {
    let token = nextLoopToken()
    send(.ping)
    onPong = { [weak self] in
      self?.loop.microtask {
        guard let self, self.loopToken == token else { return }
        end()
        self.iterate()
      }
    }
    _ = scheduler.schedule(after: options.pingTimeoutMs) { [weak self] in
      guard let self, self.loopToken == token else { return }
      let error = ConnectionError.client(.pingTimeout, message: "Server ping request failed")
      self.disconnect(error)
      end()
      self.loopFailed(error)
    }
    onStateChange(token) { [weak self] state in
      end()
      self?.resumeAfterStateChange(state)
    }
  }

  /// needs-auth and error: wait for `connect()`.
  private func waitForConnectRequest(_ token: Int) {
    connectionManager.onConnectRequest { [weak self] in
      guard let self, self.loopToken == token else { return }
      self.connectionManager.resumeFromConnectRequest()
      self.iterate()
    }
    onStateChange(token) { [weak self] _ in self?.iterate() }
  }

  // MARK: - Connecting

  final class ConnectAttempt {
    var events: [(Double, String)] = []
    var aborted = false
    var abortReason: ConnectionError?
    var timeout: (any ScheduledTimer)?
    var finished = false
    var onConnected: (() -> Void)?
  }

  private func addConnectEvent(_ attempt: ConnectAttempt, _ event: String) {
    attempt.events.append((scheduler.now, event))
  }

  private func armConnectTimeout(_ attempt: ConnectAttempt, ack: Bool) {
    attempt.timeout?.cancel()
    guard !attempt.aborted else { return }
    attempt.timeout = scheduler.schedule(after: connectTimeoutMs) { [weak self, weak attempt] in
      guard let self, let attempt else { return }
      let events = JSONValue.array(
        attempt.events.map { [.string(isoDateString($0.0)), .string($0.1)] })
      let seconds = JSNumber.format(connectTimeoutMs / 1000)
      let message =
        (ack
          ? "Server did not acknowledge the connection within \(seconds) seconds. "
          : "Connection setup timed out after \(seconds) seconds. ")
        + "Connect events: \(JSONSerializer.stringify(events))"
      self.disconnect(.client(.connectTimeout, message: message))
    }
  }

  private func beginAttempt() {
    precondition(currentAttempt == nil, "Connection attempt already active")
    let attempt = ConnectAttempt()
    currentAttempt = attempt
    addConnectEvent(attempt, "starting the connection")
    armConnectTimeout(attempt, ack: false)
    connectAttempt(attempt)
  }

  /// `#connectAttempt`: opens a socket and waits for `connected`.
  private func connectAttempt(_ attempt: ConnectAttempt) {
    if repClosed {
      finishAttempt(attempt, nil)
      return
    }
    guard let socketOrigin, let socketFactory else {
      finishAttempt(attempt, .client(.internal, message: "No server provided"))
      return
    }
    precondition(
      connectionManager.is(.disconnected) || connectionManager.is(.connecting),
      "connectAttempt() called from invalid state")
    let wsid = randomID()
    connectionManager.connecting()
    addConnectEvent(attempt, "reading the cookie")
    let connectCookie = store.cookie
    addConnectEvent(attempt, "reading the client group ID")
    addConnectEvent(attempt, "initializing active clients")
    if attempt.aborted {
      finishAttempt(attempt, attempt.abortReason)
      return
    }

    // `createSocket`.
    addConnectEvent(attempt, "reading the profile ID")
    let url = ConnectionURL(
      socketOrigin: socketOrigin, clientID: clientID, clientGroupID: clientGroupID,
      userID: userID,
      baseCookie: connectCookie.stringValue ?? connectCookie.numberValue.map(JSNumber.format),
      timestamp: scheduler.monotonicNow, lastMutationID: lastMutationIDReceived, wsid: wsid,
      profileID: profileID, additionalParams: additionalConnectParams ?? [])
    addConnectEvent(attempt, "reading deleted clients")
    addConnectEvent(attempt, "reading desired queries")
    var queriesPatch: OrderedMap<JSKey, UpQueriesPatchOp>? = queryManager.queriesPatch(store: store)
    let initConnection = InitConnectionBody(
      desiredQueriesPatch: queriesPatch!.entries.map(\.value),
      clientSchema: connectCookie.isNull ? clientSchema : nil,
      userPushURL: options.mutateURL, userPushHeaders: options.mutateHeaders,
      userQueryURL: options.queryURL, userQueryHeaders: options.queryHeaders,
      activeClients: [clientID])
    let authToken = auth.flatMap { $0.isEmpty ? nil : $0 }
    var secProtocol = SecProtocols.encode(initConnection: initConnection, authToken: authToken)
    if secProtocol.utf8.count > options.maxHeaderLength {
      secProtocol = SecProtocols.encode(initConnection: nil, authToken: authToken)
      queriesPatch = nil
    }
    addConnectEvent(attempt, "creating the WebSocket")
    let handle = SocketHandle(engine: self)
    handle.connection = socketFactory.open(
      url: url.string, secProtocol: secProtocol, events: handle)

    if attempt.aborted {
      handle.close(code: nil)
      finishAttempt(attempt, attempt.abortReason)
      return
    }
    if repClosed {
      handle.close(code: nil)
      finishAttempt(attempt, nil)
      return
    }
    self.connectCookie = connectCookie
    self.initConnectionQueries = queriesPatch
    socket = handle
    addConnectEvent(attempt, "waiting for the server acknowledgement")
    armConnectTimeout(attempt, ack: true)
    attempt.onConnected = { [weak self, weak attempt] in
      guard let self, let attempt else { return }
      if attempt.aborted {
        self.finishAttempt(attempt, attempt.abortReason)
        return
      }
      self.mutationTracker.onConnected(self.lastMutationIDReceived)
      self.requestPush(now: true)
      self.finishAttempt(attempt, nil)
    }
  }

  /// The run loop's handling of a finished attempt; `error` nil for success.
  private func finishAttempt(_ attempt: ConnectAttempt, _ error: ConnectionError?) {
    guard !attempt.finished else { return }
    attempt.finished = true
    attempt.timeout?.cancel()
    if currentAttempt === attempt { currentAttempt = nil }
    let ready = connectionReady
    if let error {
      if !attempt.aborted { disconnect(error) }
      ready.reject(loop)
      if connectionReady === ready { connectionReady = ReadyPromise() }
      loopFailed(error)
      return
    }
    ready.resolve(loop)
    additionalConnectParams = nil
    if let error = Self.connectionError(connectionManager.state) {
      loopFailed(error)
    } else {
      iterate()
    }
  }

  /// `#disconnect`: closes the socket and moves the connection status.
  func disconnect(_ reason: ConnectionError, closeCode: Int? = nil) {
    if let attempt = currentAttempt, !attempt.aborted {
      attempt.abortReason = reason
      attempt.aborted = true
      attempt.timeout?.cancel()
      loop.microtask { [weak self] in self?.finishAttempt(attempt, reason) }
    }
    if connectionManager.is(.closed) { return }
    socket?.close(code: closeCode)
    socket = nil
    lastMutationIDsSent.removeAll()
    pokeHandler.handleDisconnect()
    queryManager.clearGotQueriesAuthoritative()
    let (transition, r) = ConnectionTransition.of(reason)
    switch transition {
    case .needsAuth: connectionManager.needsAuth(r)
    case .error: connectionManager.error(r)
    case .disconnected: connectionManager.disconnected(r)
    case .closed: connectionManager.closed()
    case .none: if !connectionManager.isInTerminalState { connectionManager.connecting(r) }
    }
  }

  // MARK: - Messages

  fileprivate func socketDidReceive(_ handle: SocketHandle, _ message: SocketMessage) {
    guard handle === socket else { return }
    loop.dispatch { onMessage(message) }
  }

  fileprivate func socketDidClose(
    _ handle: SocketHandle, code: Int, reason: String, wasClean: Bool
  ) {
    guard handle === socket else { return }
    loop.dispatch {
      if code == 1009 {
        var message =
          "A WebSocket message exceeded the server message size limit. "
          + "This is usually caused by a mutation's args containing a large "
          + "value such as a base64-encoded image. Consider uploading "
          + "large files to object storage and storing only the URL in Zero."
        if !recentSentMessages.isEmpty {
          message +=
            "\nRecent sent messages:\n"
            + Self.prettyRecentMessages(recentSentMessages)
        }
        disconnect(.client(.invalidMessage, message: message))
        clientGroupDisabled = true
        loop.microtask { [weak self] in self?.options.onClientStateNotFound?() }
      } else {
        disconnect(
          wasClean
            ? .client(.cleanClose, message: "WebSocket connection closed cleanly")
            : .client(.abruptClose, message: "WebSocket connection closed abruptly"))
      }
    }
  }

  private func onMessage(_ message: SocketMessage) {
    if repClosed { return }
    let text: String
    switch message {
    case .binary(let bytes):
      guard bytes.first == PokeChunk.messageType else {
        disconnect(
          .client(
            .invalidMessage,
            message: "Unknown binary message type: \(bytes.first.map(String.init) ?? "undefined")"
          ))
        return
      }
      abortPingTimeout()
      pokeHandler.handlePokeChunk(bytes.dropFirst())
      return
    case .text(let t):
      text = t
    }
    let downstream: Downstream
    do {
      downstream = try Downstream(text: text)
    } catch {
      disconnect(
        .client(
          .invalidMessage,
          message: "Invalid message received from server: \(valitaMessage(error, text))\(text)"))
      return
    }
    do {
      switch downstream {
      case .connected(let body):
        loop.microtask { [weak self] in self?.handleConnected(body) }
      case .error(let body):
        handleErrorMessage(body)
      case .pong:
        onPong()
      case .pokeStart(let body):
        abortPingTimeout()
        pokeHandler.handlePokeStart(body)
      case .pokePart(let body):
        abortPingTimeout()
        if let lmid = pokeHandler.handlePokePart(body) { lastMutationIDReceived = lmid }
      case .pokeEnd(let body):
        abortPingTimeout()
        if let lmid = pokeHandler.handlePokeEnd(body)?.lastMutationIDChangeForSelf {
          lastMutationIDReceived = lmid
        }
      case .pull:
        abortPingTimeout()
      case .deleteClients:
        break
      case .pushResponse(let body):
        try mutationTracker.processPushResponse(body)
      case .transformError(let errors):
        queryManager.handleTransformErrors(errors)
      case .inspect:
        break
      }
    } catch {
      disconnect(.client(.internal, message: errorMessage(error)))
    }
  }

  private func handleConnected(_ body: ConnectedBody) {
    guard let attempt = currentAttempt else { return }
    addConnectEvent(attempt, "processing the server acknowledgement")
    lastMutationIDsSent.removeAll()
    let patch = queryManager.queriesPatch(store: store, lastPatch: initConnectionQueries)
    if patch.count > 0, initConnectionQueries != nil {
      send(
        .changeDesiredQueries(
          ChangeDesiredQueriesBody(desiredQueriesPatch: patch.entries.map(\.value))))
    } else if initConnectionQueries == nil {
      send(
        .initConnection(
          InitConnectionBody(
            desiredQueriesPatch: patch.entries.map(\.value),
            clientSchema: connectCookie.isNull ? clientSchema : nil,
            userPushURL: options.mutateURL, userPushHeaders: options.mutateHeaders,
            userQueryURL: options.queryURL, userQueryHeaders: options.queryHeaders)))
    }
    initConnectionQueries = nil
    connectionManager.connected()
    if let onConnected = attempt.onConnected {
      attempt.onConnected = nil
      loop.microtask(onConnected)
    }
  }

  private func handleErrorMessage(_ body: ErrorBody) {
    if body.kind == .mutationRateLimited {
      lastMutationIDsSent.removeAll()
      return
    }
    disconnect(.server(body))
    switch body.kind {
    case .versionNotSupported:
      options.onUpdateNeeded?(UpdateNeededReason(type: body.kind.rawValue, message: body.message))
    case .schemaVersionNotSupported:
      clientGroupDisabled = true
      loop.microtask { [weak self] in
        self?.options.onUpdateNeeded?(
          UpdateNeededReason(type: "SchemaVersionNotSupported", message: body.message))
      }
    case .clientNotFound:
      clientGroupDisabled = true
      loop.microtask { [weak self] in self?.options.onClientStateNotFound?() }
    case .invalidConnectionRequestLastMutationID, .invalidConnectionRequestBaseCookie:
      loop.microtask { [weak self] in self?.options.onClientStateNotFound?() }
    default:
      break
    }
  }

  private func onPokeError(_ message: String) {
    guard !connectionManager.is(.disconnected) else { return }
    if message.contains("unexpected base cookie") {
      disconnect(
        .client(
          .unexpectedBaseCookie, message: "Server returned unexpected base cookie during sync"))
    } else {
      disconnect(.client(.internal, message: "Poke processing error: \(message)"))
    }
  }

  // MARK: - Auth

  func setAuth(_ auth: String?) {
    // A token without a user was rejected at init.
    self.auth = auth ?? ""
    if let auth, !auth.isEmpty {
      sendIfConnected(.updateAuth(UpdateAuthBody(auth: auth)))
    }
  }

  /// `connection.connect()`: resumes from needs-auth or error.
  func connect(auth: String??) {
    if case .some(let auth) = auth { setAuth(auth) }
    if case .disconnected(let r) = connectionManager.state, r.clientKind == .noSocketOrigin {
      return
    }
    guard connectionManager.isInTerminalState else { return }
    connectionManager.requestConnect()
  }

  // MARK: - Close

  var closed: Bool { repClosed }

  func close() {
    if repClosed { return }
    if !connectionManager.is(.disconnected) {
      disconnect(.client(.clientClosed, message: "Zero instance closed by user"), closeCode: 1000)
    }
    repClosed = true
    pushTimer?.cancel()
    pushTimer = nil
    connectionManager.closed()
  }

  // MARK: - Mutations

  /// `MutatorProxy#onConnectionStateChange`.
  private func onConnectionStateChange(_ state: ConnectionManager.State) {
    switch state {
    case .disconnected(let r) where r.clientKind == .noSocketOrigin:
      mutationRejection = nil
    case .disconnected(let r), .error(let r), .closed(let r):
      mutationRejection = r
      mutationTracker.rejectAllOutstandingMutations(r)
    default:
      mutationRejection = nil
    }
  }

  /// `zero.mutate`, through the `MutatorProxy` and Replicache's `mutate`.
  func mutate(name: String, args: JSONValue?) -> MutatorResult {
    let client = Deferred<MutationOutcome>()
    let server = Deferred<MutationOutcome>()
    if let rejection = mutationRejection {
      client.resolve(.zeroError(message: rejection.message))
      server.resolve(.zeroError(message: rejection.message))
      return MutatorResult(client: client, server: server)
    }
    guard let mutator = mutators[name] else {
      preconditionFailure(
        "Mutator \"\(name)\" is not registered. Mutators must be registered with the Zero constructor before use."
      )
    }
    let (ephemeralID, tracked) = mutationTracker.trackMutation()
    tracked.onSettle { result in
      switch result {
      case .success(let data): server.resolve(.success(data: data))
      case .failure(let error): server.resolve(Self.outcome(error))
      }
    }
    let mutationID = store.nextMutationID(for: clientID)
    let write = KVWrite(store.main)
    let branch = ivmMain.fork()
    mutationTracker.mutationIDAssigned(ephemeralID, mutationID)
    let tx = Transaction(
      clientID: clientID, mutationID: Int(mutationID), reason: .optimistic, token: token,
      write: write, branch: branch, schema: schema)
    let result: JSONValue?
    do {
      result = try mutator(tx, args)
    } catch {
      mutationTracker.rejectMutation(ephemeralID, error)
      client.resolve(Self.outcome(error))
      return MutatorResult(client: client, server: server)
    }
    let diff = store.commitMutation(
      ReplicaStore.PendingMutation(
        id: mutationID, clientID: clientID, name: name, args: args ?? .null,
        timestamp: scheduler.monotonicNow, writes: write.writes),
      write)
    context.processChanges(diff)
    requestPush(now: false)
    fireWatchers(diff)
    client.resolve(.success(data: result))
    return MutatorResult(client: client, server: server)
  }

  private var token: String? { auth.flatMap { $0.isEmpty ? nil : $0 } }

  /// What a mutation's caller hears for an error (`wrapErrorFor`).
  static func outcome(_ error: any Error) -> MutationOutcome {
    if let e = error as? ConnectionError, e.isZeroError { return .zeroError(message: e.message) }
    let app = ApplicationError.wrapping(error)
    return .appError(message: app.message, details: app.details)
  }

  /// The store watches: the mutation tracker on `m/<clientID>/`, then the
  /// query manager on `g/`. A watch hears only about diffs that touch its
  /// prefix, except for its first run.
  private func fireWatchers(_ diff: KVDiff, initial: Bool = false) {
    let mutations = diff.withPrefix(Keys.mutationsPrefix + clientID + "/")
    if initial || !mutations.isEmpty { try? mutationTracker.processMutationResponses(mutations) }
    let got = diff.withPrefix(Keys.gotQueriesPrefix)
    if initial || !got.isEmpty { try? queryManager.processGotQueries(got) }
  }

  // MARK: - Pushing

  /// `pushConnectionLoop.send(now)`.
  private func requestPush(now: Bool) {
    if repClosed { return }
    pushRequested = true
    guard !pushInFlight else { return }
    if now {
      pushTimer?.cancel()
      pushTimer = nil
      loop.microtask { [weak self] in self?.invokePush() }
    } else if pushTimer == nil {
      pushTimer = scheduler.schedule(after: 0) { [weak self] in
        self?.pushTimer = nil
        self?.invokePush()
      }
    }
  }

  /// `#invokePush`: pushes the pending mutations once connected.
  private func invokePush() {
    guard !pushInFlight, pushRequested, !repClosed, !clientGroupDisabled else { return }
    pushRequested = false
    let pending = store.pending.filter { $0.clientID == clientID }
    guard !pending.isEmpty else { return }
    pushInFlight = true
    connectionReady.onSettle(loop) { [weak self] connected in
      guard let self else { return }
      if connected { self.pusher(pending) }
      self.pushInFlight = false
      if !connected { self.pushRequested = true }
      if self.pushRequested { self.requestPush(now: false) }
    }
  }

  /// `#pusher`: one `push` message per mutation not yet sent on this
  /// connection.
  private func pusher(_ mutations: [ReplicaStore.PendingMutation]) {
    let now = scheduler.now
    pushRequestCounter += 1
    let requestID = "\(clientID)-\(pushRequestCounter)"
    for m in mutations where m.id > (lastMutationIDsSent[m.clientID] ?? 0) {
      let timestamp = now - (scheduler.monotonicNow - m.timestamp).rounded(.toNearestOrAwayFromZero)
      send(
        .push(
          PushBody(
            clientGroupID: clientGroupID,
            mutations: [
              .custom(
                CustomMutation(
                  id: m.id, clientID: m.clientID, name: m.name, args: [m.args],
                  timestamp: timestamp))
            ],
            pushVersion: 1, timestamp: now, requestID: requestID)))
      lastMutationIDsSent[m.clientID] = m.id
    }
  }

  // MARK: - Pokes

  /// `rep.poke`: applies a merged poke to the snapshot and rebases the
  /// pending mutations on it (`handlePullResponseV1` and `maybeEndPull`).
  private func applyPoke(_ poke: MergedPoke) throws {
    let socketAtStart = socket
    let base = store.snapshot
    guard poke.baseCookie == base.cookie else {
      throw PokeError("unexpected base cookie for poke: " + JSONSerializer.stringify(poke.json))
    }
    for (cid, lmid) in poke.lastMutationIDChanges.sorted(by: { $0.key < $1.key }) {
      if let last = base.lastMutationIDs[cid], lmid < last {
        throw PokeError(
          Self.badOrderMessage(
            "\(cid) lastMutationID", JSNumber.format(lmid), JSNumber.format(last)))
      }
    }
    if compareCookies(poke.cookie, base.cookie) < 0 {
      throw PokeError(
        Self.badOrderMessage(
          "cookie", JSONSerializer.stringify(poke.cookie), JSONSerializer.stringify(base.cookie)))
    }
    if poke.cookie == base.cookie { return }

    var snapshot = base
    snapshot.lastMutationIDs.merge(poke.lastMutationIDChanges) { $1 }
    snapshot.cookie = poke.cookie
    let patchWrite = KVWrite(snapshot.map)
    do {
      try ReplicaStore.apply(poke.patch, to: patchWrite)
    } catch {
      throw PokeError(error.description)
    }
    snapshot.map = patchWrite.map

    var touched = patchWrite.writes.union(store.pendingWrites)
    var main = snapshot.map
    var replayed: [ReplicaStore.PendingMutation] = []
    let pending = store.pending.filter { $0.id > (snapshot.lastMutationIDs[$0.clientID] ?? 0) }
    if !pending.isEmpty {
      // The rebase runs over a fork of the views' branch moved to the new
      // snapshot.
      var branch = ivmMain.fork()
      branch.apply(KVDiff.between(store.main, snapshot.map, keys: touched))
      for var m in pending {
        let write = KVWrite(main)
        let fork = branch.fork()
        let tx = Transaction(
          clientID: m.clientID, mutationID: Int(m.id), reason: .rebase, token: token,
          write: write, branch: fork, schema: schema)
        do {
          guard let mutator = mutators[m.name] else {
            throw ApplicationError("Unknown mutator: \(m.name)")
          }
          _ = try mutator(tx, m.args)
          main = write.map
          branch = fork
          m.writes = write.writes
          touched.formUnion(write.writes)
        } catch {
          // The mutation stays pending; its writes are dropped.
          m.writes = []
        }
        replayed.append(m)
      }
    }
    let diff = store.advance(to: snapshot, main: main, pending: replayed, touched: touched)
    context.processChanges(diff)
    fireWatchers(diff)
    if socketAtStart != nil, socket === socketAtStart {
      queryManager.markGotQueriesAuthoritative()
    }
  }

  /// `JSON.stringify(recentSentMessages, null, 2)`.
  static func prettyRecentMessages(_ messages: [JSONValue]) -> String {
    let entries = messages.map { m in
      let fields = ["type", "size", "snippet"].compactMap { key -> String? in
        guard let v = m[key] else { return nil }
        var k = ""
        JSONSerializer.writeString(key, into: &k)
        return "    \(k): \(JSONSerializer.stringify(v))"
      }
      return "  {\n" + fields.joined(separator: ",\n") + "\n  }"
    }
    return "[\n" + entries.joined(separator: ",\n") + "\n]"
  }

  static func badOrderMessage(_ name: String, _ received: String, _ last: String) -> String {
    "Received \(name) \(received) is < than last snapshot \(name) \(last); ignoring client view"
  }
}

/// A connection's readiness, as the pusher waits on it
/// (`#connectionReadyResolver`).
@MainActor
final class ReadyPromise {
  private var settled: Bool?
  private var waiters: [(Bool) -> Void] = []

  func resolve(_ loop: EventLoop) { settle(true, loop) }
  func reject(_ loop: EventLoop) { settle(false, loop) }

  private func settle(_ value: Bool, _ loop: EventLoop) {
    guard settled == nil else { return }
    settled = value
    let waiters = self.waiters
    self.waiters = []
    for w in waiters { loop.microtask { w(value) } }
  }

  /// Calls `waiter` in a microtask once settled.
  func onSettle(_ loop: EventLoop, _ waiter: @escaping (Bool) -> Void) {
    if let settled {
      loop.microtask { waiter(settled) }
    } else {
      waiters.append(waiter)
    }
  }
}

/// The engine's end of one socket.
@MainActor
final class SocketHandle: SocketEvents {
  weak var engine: Engine?
  var connection: (any SocketConnection)!
  private var closed = false

  init(engine: Engine) { self.engine = engine }

  func send(_ text: String) {
    guard !closed else { return }
    connection.send(text)
  }

  func close(code: Int?) {
    guard !closed else { return }
    closed = true
    connection.close(code: code)
  }

  func socketDidOpen() {}

  func socketDidReceive(_ message: SocketMessage) {
    guard !closed else { return }
    engine?.socketDidReceive(self, message)
  }

  func socketDidClose(code: Int, reason: String, wasClean: Bool) {
    guard !closed else { return }
    engine?.socketDidClose(self, code: code, reason: reason, wasClean: wasClean)
  }
}

extension MergedPoke {
  /// The poke as `JSON.stringify` shows it in errors.
  var json: JSONValue {
    var changes: [String: JSONValue] = [:]
    for (k, v) in lastMutationIDChanges { changes[k] = .number(v) }
    return [
      "baseCookie": baseCookie,
      "pullResponse": ["cookie": cookie, "lastMutationIDChanges": .object(changes)],
    ]
  }
}

/// A random ID like the JS client's `nanoid`.
func randomID() -> String {
  let alphabet = Array("0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ_-")
  var g = SystemRandomNumberGenerator()
  return String((0..<21).map { _ in alphabet[Int(g.next() % UInt64(alphabet.count))] })
}

/// `Date.prototype.toISOString` of a time in milliseconds since 1970.
func isoDateString(_ ms: Double) -> String {
  let t = Int64(ms.rounded(.down))
  var days = t / 86_400_000
  var msOfDay = t % 86_400_000
  if msOfDay < 0 {
    msOfDay += 86_400_000
    days -= 1
  }
  // Howard Hinnant's civil_from_days.
  let z = days + 719_468
  let era = (z >= 0 ? z : z - 146_096) / 146_097
  let doe = z - era * 146_097
  let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365
  let doy = doe - (365 * yoe + yoe / 4 - yoe / 100)
  let mp = (5 * doy + 2) / 153
  let d = doy - (153 * mp + 2) / 5 + 1
  let m = mp < 10 ? mp + 3 : mp - 9
  let y = yoe + era * 400 + (m <= 2 ? 1 : 0)
  func pad(_ v: Int64, _ n: Int) -> String {
    let s = String(v)
    return String(repeating: "0", count: max(0, n - s.count)) + s
  }
  return
    "\(pad(y, 4))-\(pad(m, 2))-\(pad(d, 2))T\(pad(msOfDay / 3_600_000, 2)):"
    + "\(pad(msOfDay / 60_000 % 60, 2)):\(pad(msOfDay / 1000 % 60, 2)).\(pad(msOfDay % 1000, 3))Z"
}

/// The message valita gives for a message that doesn't parse
/// (`getMessage` in `packages/shared/src/valita.ts`), for the cases the
/// client can tell apart.
func valitaMessage(_ error: any Error, _ text: String) -> String {
  guard let e = error as? DecodingError else { return errorMessage(error) }
  if e.message.hasPrefix("Unknown message type") || e.message == "Expected [tag, body] tuple" {
    return "Invalid union value: \(text)"
  }
  return valitaMessage(e)
}

func valitaMessage(_ e: DecodingError) -> String {
  if e.message == "Missing property", let key = e.path.last {
    let rest = e.path.dropLast()
    return "Missing property \(key)" + (rest.isEmpty ? "" : " at \(rest.joined(separator: "."))")
  }
  return e.description
}
