import Testing
import ZeroTesting

@testable import Zero

/// Edges of the connection the client scenarios can't express: log levels,
/// the `maxHeaderLength` boundary, and a transport that calls back from
/// inside `send`. Expected values follow `packages/zero-client/src/client/zero.ts`.
@Suite @MainActor struct HardenConnectionTests {
  static let create = defineMutator("issue.create") { (tx, id: String) in
    try tx.mutate.issue.insert(
      Issue(id: id, title: "", description: "", closed: false, createdAt: 1))
  }

  let scheduler = ManualScheduler(now: 1_700_000_000_000)
  let log = RecordingLogSink()

  func makeZero(
    _ sockets: any SocketFactory, maxHeaderLength: Int? = nil, auth: String? = nil
  ) throws -> Zero<Void> {
    var options = ZeroOptions(
      cacheURL: "http://zero.test", userID: "u", storageKey: randomID(), storage: .memory)
    options.logSink = log
    options.auth = auth
    options.onClientStateNotFound = {}
    options.onUpdateNeeded = { _ in }
    if let maxHeaderLength { options.maxHeaderLength = maxHeaderLength }
    return try Zero(
      schema: .test, mutators: [Self.create], options: options, scheduler: scheduler,
      socketFactory: sockets)
  }

  func connect(_ sockets: MockSocketFactory) throws -> MockSocket {
    scheduler.runDueTimers()
    let socket = try #require(sockets.sockets.last)
    socket.receive(text: #"["connected",{"wsid":"w"}]"#)
    scheduler.runDueTimers()
    return socket
  }

  func levels(startingWith prefix: String) -> [LogLevel] {
    log.entries.filter { $0.message.hasPrefix(prefix) }.map(\.level)
  }

  // MARK: - Log levels (075b4215e, 79adc09fb, 42316d521)

  /// 075b4215e: a ClientNotFound while connecting is logged at warn, both
  /// the error message and the connect failure, and nothing is logged as an
  /// error.
  @Test func clientNotFoundIsLoggedAtWarn() throws {
    let sockets = MockSocketFactory()
    let zero = try makeZero(sockets)
    scheduler.runDueTimers()
    try #require(sockets.sockets.last).receive(
      text: #"["error",{"kind":"ClientNotFound","message":"m","origin":"zeroCache"}]"#)
    scheduler.runDueTimers()
    #expect(levels(startingWith: "ClientNotFound:\n\n") == [.warn])
    #expect(levels(startingWith: "Failed to connect") == [.warn])
    #expect(log.entries.filter { $0.level == .error } == [])
    zero.close()
  }

  /// 79adc09fb: an auth error while connected (including a push or
  /// transform the API server answered with 401 or 403) is logged at warn,
  /// and nothing is logged as an error.
  @Test(arguments: [
    #"{"kind":"Unauthorized","message":"m","origin":"zeroCache"}"#,
    #"{"kind":"AuthInvalidated","message":"m","origin":"zeroCache"}"#,
    #"{"kind":"PushFailed","message":"m","origin":"zeroCache","reason":"http","status":401,"mutationIDs":[]}"#,
    #"{"kind":"TransformFailed","message":"m","origin":"zeroCache","reason":"http","status":403,"queryIDs":["q"]}"#,
  ])
  func authErrorsAreLoggedAtWarn(_ body: String) throws {
    let sockets = MockSocketFactory()
    let zero = try makeZero(sockets, auth: "t1")
    let socket = try connect(sockets)
    socket.receive(text: #"["error",\#(body)]"#)
    scheduler.runDueTimers()
    #expect(zero.connection.state.status == .needsAuth)
    let kind = try #require(try JSONParser.parse(Array(body.utf8))["kind"]?.stringValue)
    #expect(levels(startingWith: "\(kind):\n\n") == [.warn])
    #expect(log.entries.filter { $0.level == .error } == [])
    zero.close()
  }

  /// 075b4215e: an error the client can't recover from keeps the error
  /// level, for the message and the connect failure.
  @Test func versionNotSupportedIsLoggedAtError() throws {
    let sockets = MockSocketFactory()
    let zero = try makeZero(sockets)
    scheduler.runDueTimers()
    try #require(sockets.sockets.last).receive(
      text: #"["error",{"kind":"VersionNotSupported","message":"m","origin":"zeroCache"}]"#)
    scheduler.runDueTimers()
    #expect(levels(startingWith: "VersionNotSupported:\n\n") == [.error])
    #expect(levels(startingWith: "Failed to connect") == [.error])
    zero.close()
  }

  // MARK: - maxHeaderLength

  /// `createSocket` moves `initConnection` out of the header only when the
  /// encoded protocol is longer than `maxHeaderLength` (`>`), so a header
  /// of exactly that length still carries it.
  @Test func initConnectionAtExactlyMaxHeaderLengthStaysInTheHeader() throws {
    let probe = MockSocketFactory()
    let first = try makeZero(probe, maxHeaderLength: 1_000_000)
    scheduler.runDueTimers()
    let length = try #require(probe.sockets.last?.secProtocol).utf8.count
    first.close()

    for (maxHeaderLength, inHeader) in [(length, true), (length - 1, false)] {
      let sockets = MockSocketFactory()
      let zero = try makeZero(sockets, maxHeaderLength: maxHeaderLength)
      scheduler.runDueTimers()
      let socket = try #require(sockets.sockets.last)
      let decoded = try SecProtocols.decode(socket.secProtocol ?? "")
      #expect((decoded["initConnectionMessage"] != nil) == inHeader, "\(maxHeaderLength)")
      socket.receive(text: #"["connected",{"wsid":"w"}]"#)
      scheduler.runDueTimers()
      let sentInit = socket.messages.contains { $0.hasPrefix(#"["initConnection""#) }
      #expect(sentInit == !inHeader, "\(maxHeaderLength)")
      zero.close()
    }
  }

  // MARK: - A transport that calls back from inside send

  /// Delivers what `reply` returns for a sent frame before `send` returns.
  final class EagerSocketFactory: SocketFactory {
    let mock = MockSocketFactory()
    var reply: (String, MockSocket) -> Void = { _, _ in }

    final class Connection: SocketConnection {
      let socket: MockSocket
      let factory: EagerSocketFactory
      init(_ socket: MockSocket, _ factory: EagerSocketFactory) {
        self.socket = socket
        self.factory = factory
      }
      func send(_ text: String) {
        socket.send(text)
        factory.reply(text, socket)
      }
      func close(code: Int?) { socket.close(code: code) }
    }

    func open(
      url: String, secProtocol: String?, events: any SocketEvents
    )
      -> any SocketConnection
    {
      _ = mock.open(url: url, secProtocol: secProtocol, events: events)
      return Connection(mock.sockets.last!, self)
    }
  }

  /// A pong delivered from inside the ping's `send` still counts: the
  /// client stays connected on the same socket.
  @Test(.timeLimit(.minutes(1)))
  func pongFromInsideThePingSend() throws {
    let factory = EagerSocketFactory()
    let zero = try makeZero(factory)
    let socket = try connect(factory.mock)
    factory.reply = { text, socket in
      if text.hasPrefix(#"["ping""#) { socket.receive(text: #"["pong",{}]"#) }
    }
    for _ in 0..<3 { scheduler.advance(by: 5000) }
    #expect(zero.connection.state == .connected)
    #expect(factory.mock.sockets.count == 1)
    #expect(socket.messages.filter { $0.hasPrefix(#"["ping""#) }.count == 3)
    zero.close()
  }

  /// The socket drops from inside the push's `send`: the client
  /// reconnects and pushes the mutation again on the new socket.
  @Test(.timeLimit(.minutes(1)))
  func closeFromInsideThePushSend() throws {
    let factory = EagerSocketFactory()
    let zero = try makeZero(factory)
    let first = try connect(factory.mock)
    factory.reply = { text, socket in
      if text.hasPrefix(#"["push""#), socket === first { socket.serverClose(code: 1006) }
    }
    zero.mutate(Self.create("a"))
    scheduler.runDueTimers()
    #expect(factory.mock.sockets.count == 2)
    let second = try connect(factory.mock)
    #expect(zero.connection.state == .connected)
    #expect(second.messages.filter { $0.hasPrefix(#"["push""#) }.count == 1)
    zero.close()
  }

  /// An auth error delivered from inside the `send` of the handshake's
  /// `changeDesiredQueries`: the error wins, and the client waits in
  /// needs-auth with no socket instead of reporting connected.
  @Test(.timeLimit(.minutes(1)))
  func authErrorFromInsideTheHandshakeSend() throws {
    let factory = EagerSocketFactory()
    let zero = try makeZero(factory)
    let all = defineQuery("issues.all") { () in zql.issue.orderBy(\.id, .asc) }
    scheduler.runDueTimers()
    let view = try zero.materialize(all())
    factory.reply = { text, socket in
      if text.hasPrefix(#"["changeDesiredQueries""#) {
        socket.receive(
          text: #"["error",{"kind":"Unauthorized","message":"no","origin":"zeroCache"}]"#)
      }
    }
    let socket = try #require(factory.mock.sockets.last)
    socket.receive(text: #"["connected",{"wsid":"w"}]"#)
    scheduler.runDueTimers()
    #expect(socket.closed)
    #expect(zero.connection.state.status == .needsAuth)
    scheduler.advance(by: 20_000)
    #expect(zero.connection.state.status == .needsAuth)
    #expect(factory.mock.sockets.count == 1)
    view.destroy()
    zero.close()
  }
}
