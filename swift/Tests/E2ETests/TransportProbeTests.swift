import Foundation
import Synchronization
import Testing
import Zero

/// A transport the probes run against.
enum TransportKind: String, CustomTestStringConvertible, CaseIterable, Sendable {
  case nio
  #if canImport(Darwin)
    case urlSession
  #endif

  var transport: any Transport {
    switch self {
    case .nio: NIOTransport()
    #if canImport(Darwin)
      case .urlSession: URLSessionTransport()
    #endif
    }
  }

  var testDescription: String { rawValue }
}

/// How a socket ended, as `TransportConnection` reports it to the engine: a clean end of the frames is a normal close.
func ending(
  of socket: any Socket, within timeout: Duration = .seconds(5)
) async -> (
  messages: [SocketMessage], close: SocketClosed?
) {
  let box = Box<(messages: [SocketMessage], close: SocketClosed?)>(([], nil))
  await withTaskGroup(of: Void.self) { group in
    group.addTask {
      do {
        while let m = try await socket.receive() { box.withLock { $0.messages.append(m) } }
        box.withLock { $0.close = SocketClosed(code: 1000, reason: "", wasClean: true) }
      } catch {
        box.withLock { $0.close = SocketClosed(error) }
      }
    }
    group.addTask { try? await Task.sleep(for: timeout) }
    await group.next()
    group.cancelAll()
  }
  return box.withLock { $0 }
}

/// What a browser `WebSocket` does, checked against each `Transport` with a server that writes raw bytes. The engine reads close codes (1009, 4xxx) and relies on every socket it closes being closed.
@Suite(.serialized, .timeLimit(.minutes(2)))
struct TransportProbeTests {
  // MARK: - Frames

  /// Frames written in the same TCP write as the 101 response are the WebSocket's (NIOTransport dropped them once).
  @Test(arguments: TransportKind.allCases)
  func framesThatComeWithThe101AreDelivered(_ kind: TransportKind) async throws {
    let server = try await ScriptedServer { c in
      await c.write(c.accept() + ScriptedServer.text("first") + ScriptedServer.text("second"))
      _ = await c.next()
    }
    defer { Task { await server.shutdown() } }
    let socket = try await kind.transport.connect(url: server.url, secProtocol: nil)
    #expect(try await socket.receive() == .text("first"))
    #expect(try await socket.receive() == .text("second"))
    await socket.close(code: 1000, reason: "")
  }

  /// A message in fragments arrives as one message.
  @Test(arguments: TransportKind.allCases)
  func fragmentedMessageArrivesWhole(_ kind: TransportKind) async throws {
    let server = try await ScriptedServer { c in
      await c.write(
        c.accept() + ScriptedServer.frame(1, Array("hel".utf8), fin: false)
          + ScriptedServer.frame(0, Array("lo é".utf8), fin: true))
      _ = await c.next()
    }
    defer { Task { await server.shutdown() } }
    let socket = try await kind.transport.connect(url: server.url, secProtocol: nil)
    #expect(try await socket.receive() == .text("hello é"))
    await socket.close(code: 1000, reason: "")
  }

  /// Large frames, text and binary, both ways.
  @Test(arguments: TransportKind.allCases)
  func largeFramesBothWays(_ kind: TransportKind) async throws {
    let big = String(repeating: "abcdefghij", count: 2 << 20)  // 20 MB
    let bytes = (0..<(12 << 20)).map { UInt8($0 & 0xFF) }
    let got = Box<[ScriptedServer.Frame]>([])
    let server = try await ScriptedServer { c in
      await c.write(c.accept())
      await c.write(ScriptedServer.text(big))
      await c.write(ScriptedServer.frame(2, bytes))
      for _ in 0..<2 {
        if let f = await c.next(within: .seconds(30)) { got.withLock { $0.append(f) } }
      }
      _ = await c.next()
    }
    defer { Task { await server.shutdown() } }
    let socket = try await kind.transport.connect(url: server.url, secProtocol: nil)
    #expect(try await socket.receive() == .text(big))
    #expect(try await socket.receive() == .binary(bytes))
    // 11 MB, the size of the oversized mutation in `HardenE2ETests`.
    let up = String(repeating: "x", count: 11 << 20)
    try await socket.send(.text(up))
    try await socket.send(.binary([1, 2, 3]))
    let deadline = ContinuousClock.now + .seconds(30)
    while got.withLock({ $0.count }) < 2, ContinuousClock.now < deadline {
      try await Task.sleep(for: .milliseconds(20))
    }
    let frames = got.withLock { $0 }
    #expect(frames.count == 2)
    #expect(frames.first?.opcode == 1 && frames.first?.payload.count == up.utf8.count)
    #expect(frames.last == .init(fin: true, opcode: 2, payload: [1, 2, 3]))
    await socket.close(code: 1000, reason: "")
  }

  /// A frame over the transport's maximum message size. A browser takes it; the engine must at least not mistake the transport's own limit for the server's 1009.
  @Test(arguments: TransportKind.allCases)
  func frameOverTheMaximumMessageSize(_ kind: TransportKind) async throws {
    let bytes = [UInt8](repeating: 7, count: (64 << 20) + 1)
    let server = try await ScriptedServer { c in
      await c.write(c.accept())
      await c.write(ScriptedServer.frame(2, bytes))
      _ = await c.next(within: .seconds(20))
    }
    defer { Task { await server.shutdown() } }
    let socket = try await kind.transport.connect(url: server.url, secProtocol: nil)
    let end = await ending(of: socket, within: .seconds(30))
    print(
      "[probe] \(kind) 64 MB + 1 frame: \(end.messages.map { m -> Int in if case .binary(let b) = m { b.count } else { -1 } }) then \(String(describing: end.close))"
    )
    if end.messages.isEmpty {
      // Not delivered: the close must not read as the server's 1009 (message too big), after which the engine drops the client group.
      #expect(end.close?.code != 1009, "the transport's own limit reads as the server's 1009")
    }
    await socket.close(code: 1000, reason: "")
  }

  // MARK: - Closes

  /// The server's close frame: its code and reason, as a browser's `CloseEvent` has them.
  @Test(arguments: TransportKind.allCases)
  func serverCloseCodesAndReasons(_ kind: TransportKind) async throws {
    for (code, reason) in [
      (1000, "bye"), (1001, "going away"), (1009, "too big"), (1011, "oops"), (4001, "x"),
      (4403, "unicode é😀"),
    ] {
      let server = try await ScriptedServer { c in
        await c.write(c.accept() + ScriptedServer.text("hi") + ScriptedServer.close(code, reason))
        // Answer the client's close, then hang up.
        _ = await c.next()
        await c.closeTCP()
      }
      let socket = try await kind.transport.connect(url: server.url, secProtocol: nil)
      let end = await ending(of: socket)
      print("[probe] \(kind) server close \(code): \(String(describing: end.close))")
      #expect(end.messages == [.text("hi")], "\(code)")
      #expect(end.close?.code == code, "\(code)")
      if code != 1000 {
        #expect(end.close == SocketClosed(code: code, reason: reason, wasClean: true), "\(code)")
      }
      await socket.close(code: 1000, reason: "")
      #expect(await server.allClosed(within: .seconds(5)), "\(code): the socket stayed open")
      await server.shutdown()
    }
  }

  /// A close frame without a code: 1005 in a browser.
  @Test(arguments: TransportKind.allCases)
  func serverCloseWithoutACode(_ kind: TransportKind) async throws {
    let server = try await ScriptedServer { c in
      await c.write(c.accept() + ScriptedServer.close(nil))
      _ = await c.next()
      await c.closeTCP()
    }
    defer { Task { await server.shutdown() } }
    let socket = try await kind.transport.connect(url: server.url, secProtocol: nil)
    let end = await ending(of: socket)
    print("[probe] \(kind) empty close: \(String(describing: end.close))")
    #expect(end.close?.code == 1005 || end.close?.code == 1000)
  }

  /// The server hangs up without a close frame, orderly or with a reset: 1006, not clean.
  @Test(arguments: TransportKind.allCases)
  func serverHangsUpWithoutACloseFrame(_ kind: TransportKind) async throws {
    for abrupt in [false, true] {
      let server = try await ScriptedServer { c in
        await c.write(c.accept() + ScriptedServer.text("hi"))
        try? await Task.sleep(for: .milliseconds(200))
        await c.closeTCP(abrupt: abrupt)
      }
      let socket = try await kind.transport.connect(url: server.url, secProtocol: nil)
      let end = await ending(of: socket)
      print("[probe] \(kind) hang-up (abrupt \(abrupt)): \(String(describing: end.close))")
      #expect(end.messages == [.text("hi")])
      #expect(end.close?.code == 1006, "abrupt \(abrupt)")
      #expect(end.close?.wasClean == false, "abrupt \(abrupt)")
      await server.shutdown()
    }
  }

  /// The server goes away partway through a frame: a browser doesn't deliver the partial frame, and the close is abrupt.
  @Test(arguments: TransportKind.allCases)
  func serverGoesAwayMidFrame(_ kind: TransportKind) async throws {
    let server = try await ScriptedServer { c in
      let whole = ScriptedServer.text(String(repeating: "y", count: 100_000))
      await c.write(c.accept() + ScriptedServer.text("hi") + Array(whole.prefix(5000)))
      try? await Task.sleep(for: .milliseconds(200))
      await c.closeTCP()
    }
    defer { Task { await server.shutdown() } }
    let socket = try await kind.transport.connect(url: server.url, secProtocol: nil)
    let end = await ending(of: socket)
    print(
      "[probe] \(kind) mid-frame: \(end.messages.count) messages, \(String(describing: end.close))")
    // URLSession returns the part of the frame it has as a whole message, and a browser doesn't: a known difference, not fixed yet.
    withKnownIssue("URLSession delivers a cut frame", isIntermittent: true) {
      #expect(end.messages == [.text("hi")])
    } when: {
      kind != .nio
    }
    #expect(end.close?.code == 1006)
  }

  /// The server goes away partway through a frame while a client is connected: the client reconnects, as it does with a browser `WebSocket`, which never delivers the partial frame. Delivered as a message, the partial frame reads as an invalid message, which stops the client in `.error` until the app calls `connect()`.
  @Test(arguments: TransportKind.allCases) @MainActor
  func clientReconnectsAfterTheServerGoesAwayMidFrame(_ kind: TransportKind) async throws {
    for binary in [false, true] {
      let connections = Box(0)
      let server = try await ScriptedServer { c in
        let n = connections.withLock {
          $0 += 1
          return $0
        }
        await c.write(c.accept() + ScriptedServer.text(#"["connected",{"wsid":"w\#(n)"}]"#))
        if n == 1 {
          try? await Task.sleep(for: .milliseconds(300))
          let row =
            #"{"op":"put","tableName":"issues","value":{"id":"x","title":"\#(String(repeating: "y", count: 100_000))"}}"#
          let whole =
            binary
            ? ScriptedServer.frame(2, [PokeChunk.messageType] + Array(row.utf8))
            : ScriptedServer.text(#"["pokePart",{"pokeID":"p1","rowsPatch":[\#(row)]}]"#)
          await c.write(Array(whole.prefix(5000)))
          try? await Task.sleep(for: .milliseconds(50))
          await c.closeTCP()
        } else {
          for await _ in c.frames {}
        }
      }
      let logs = LogCollector()
      var options = ZeroOptions(
        cacheURL: "http://127.0.0.1:\(server.port)", storageKey: UUID().uuidString, storage: .memory
      )
      options.logLevel = .info
      options.logSink = logs
      let zero = try Zero(
        schema: .test, options: options, socketFactory: TransportSocketFactory(kind.transport))
      var states: [ConnectionStatus] = []
      let unsubscribe = zero.connection.subscribe { states.append($0.status) }
      let reconnected = try await within(.seconds(15)) {
        connections.withLock { $0 } >= 2 && zero.connection.state == .connected
      }
      print(
        "[probe] \(kind) client, server gone mid-\(binary ? "binary" : "text") frame: reconnected \(reconnected), states \(states), \(zero.connection.state)"
      )
      if !reconnected {
        for line in logs.all.suffix(8) { print("[probe]   \(line.prefix(300))") }
      }
      // See `serverGoesAwayMidFrame`: URLSession delivers the cut frame, which stops the client in `.error`.
      withKnownIssue("URLSession delivers a cut frame", isIntermittent: true) {
        #expect(reconnected, "binary \(binary): \(zero.connection.state)")
        #expect(!states.contains(.error), "binary \(binary)")
      } when: {
        kind != .nio
      }
      unsubscribe()
      zero.close()
      await server.shutdown()
    }
  }

  /// The client's close: the server gets a close frame with the code, and the socket is closed once the server answers.
  @Test(arguments: TransportKind.allCases)
  func clientCloseSendsTheCode(_ kind: TransportKind) async throws {
    let got = Box<ScriptedServer.Frame?>(nil)
    let server = try await ScriptedServer { c in
      await c.write(c.accept())
      let f = await c.next()
      got.withLock { $0 = f }
      await c.write(ScriptedServer.close(f?.closeCode ?? 1000))
      _ = await c.next()
      await c.closeTCP()
    }
    defer { Task { await server.shutdown() } }
    let socket = try await kind.transport.connect(url: server.url, secProtocol: nil)
    await socket.close(code: 4123, reason: "")
    #expect(await server.allClosed(within: .seconds(5)), "the socket stayed open after its close")
    // The server's script records the frame a moment after the connection closes.
    let deadline = ContinuousClock.now + .seconds(5)
    while got.withLock({ $0 }) == nil, ContinuousClock.now < deadline {
      try await Task.sleep(for: .milliseconds(10))
    }
    let frame = got.withLock { $0 }
    print("[probe] \(kind) client close frame: \(String(describing: frame))")
    #expect(frame?.opcode == 8)
    #expect(frame?.closeCode == 4123)
    // After the close: a receive ends, a second close and a send don't hang or crash.
    let end = await ending(of: socket, within: .seconds(2))
    #expect(end.close != nil, "a receive after the close never returned")
    await socket.close(code: 1000, reason: "")
    try? await socket.send(.text("after"))
  }

  /// The client closes while a receive waits: the receive ends at once, and the socket is closed even when the server never answers the close.
  @Test(arguments: TransportKind.allCases)
  func clientCloseEndsAPendingReceiveAndTheSocket(_ kind: TransportKind) async throws {
    let server = try await ScriptedServer { c in
      await c.write(c.accept())
      // Never answer the close; keep the TCP connection open.
      try? await Task.sleep(for: .seconds(60))
    }
    defer { Task { await server.shutdown() } }
    let socket = try await kind.transport.connect(url: server.url, secProtocol: nil)
    let receiving = Task { await ending(of: socket, within: .seconds(10)) }
    try await Task.sleep(for: .milliseconds(200))
    let start = ContinuousClock.now
    await socket.close(code: 1000, reason: "")
    let end = await receiving.value
    let receiveEnded = ContinuousClock.now - start
    let closed = await server.allClosed(within: .seconds(10))
    print(
      "[probe] \(kind) close against a silent server: receive ended after \(receiveEnded) with \(String(describing: end.close)); TCP closed \(closed) after \(ContinuousClock.now - start)"
    )
    #expect(end.close != nil, "the pending receive never ended")
    #expect(receiveEnded < .seconds(3))
    #expect(closed, "the socket stayed open")
  }

  /// A send after the server hung up throws or does nothing; it doesn't hang or crash.
  @Test(arguments: TransportKind.allCases)
  func sendAfterTheServerHungUp(_ kind: TransportKind) async throws {
    let server = try await ScriptedServer { c in
      await c.write(c.accept())
      await c.closeTCP()
    }
    defer { Task { await server.shutdown() } }
    let socket = try await kind.transport.connect(url: server.url, secProtocol: nil)
    _ = await ending(of: socket)
    for _ in 0..<3 { try? await socket.send(.text("late")) }
    await socket.close(code: 1000, reason: "")
  }

  // MARK: - Ping

  /// The server's ping gets a pong with its payload, and isn't delivered as a message.
  @Test(arguments: TransportKind.allCases)
  func pingGetsAPong(_ kind: TransportKind) async throws {
    let pong = Box<ScriptedServer.Frame?>(nil)
    let server = try await ScriptedServer { c in
      await c.write(c.accept() + ScriptedServer.frame(9, Array("p1".utf8)))
      let f = await c.next(pongs: true)
      pong.withLock { $0 = f }
      await c.write(ScriptedServer.text("after"))
      _ = await c.next()
    }
    defer { Task { await server.shutdown() } }
    let socket = try await kind.transport.connect(url: server.url, secProtocol: nil)
    #expect(try await socket.receive() == .text("after"))
    #expect(pong.withLock { $0 } == .init(fin: true, opcode: 10, payload: Array("p1".utf8)))
    await socket.close(code: 1000, reason: "")
  }

  // MARK: - The handshake

  /// The `Sec-WebSocket-Protocol` header carries the encoded `initConnection` and auth token, up to `maxHeaderLength` (8 KB): it must arrive byte for byte.
  @Test(arguments: TransportKind.allCases)
  func secProtocolArrivesIntact(_ kind: TransportKind) async throws {
    let token = String(repeating: "eyJhbGciOiJIUzI1NiJ9.ä+/=", count: 200)
    let value = SecProtocols.encode(
      initConnection: InitConnectionBody(desiredQueriesPatch: [], clientSchema: nil),
      authToken: token)
    let long = String(String(repeating: value, count: 8192 / value.utf8.count + 1).prefix(8192))
    for protocolValue in [value, long] {
      let seen = Box<String?>(nil)
      let server = try await ScriptedServer { c in
        seen.withLock { $0 = c.header("Sec-WebSocket-Protocol") }
        await c.write(c.accept())
        _ = await c.next()
      }
      let socket = try await kind.transport.connect(url: server.url, secProtocol: protocolValue)
      #expect(seen.withLock { $0 } == protocolValue, "\(protocolValue.utf8.count) bytes")
      await socket.close(code: 1000, reason: "")
      await server.shutdown()
    }
  }

  /// The server declines the upgrade with an HTTP error: the connect fails.
  @Test(arguments: TransportKind.allCases)
  func upgradeDeclinedWithAnHTTPError(_ kind: TransportKind) async throws {
    for status in ["401 Unauthorized", "500 Internal Server Error", "200 OK"] {
      let server = try await ScriptedServer { c in
        await c.write(Array("HTTP/1.1 \(status)\r\nContent-Length: 2\r\n\r\nno".utf8))
        await c.closeTCP()
      }
      await #expect(throws: (any Error).self, "\(status)") {
        let s = try await kind.transport.connect(url: server.url, secProtocol: "p")
        print("[probe] \(kind) \(status) connected: \(String(describing: try? await s.receive()))")
      }
      await server.shutdown()
    }
  }

  /// A redirect: browsers don't follow it for a WebSocket, so the auth token in the protocol header never goes to another server.
  @Test(arguments: TransportKind.allCases)
  func redirectIsNotFollowed(_ kind: TransportKind) async throws {
    let forwarded = Box<String?>(nil)
    let target = try await ScriptedServer { c in
      forwarded.withLock { $0 = c.header("Sec-WebSocket-Protocol") }
      await c.write(c.accept())
      _ = await c.next()
    }
    defer { Task { await target.shutdown() } }
    let server = try await ScriptedServer { c in
      await c.write(
        Array("HTTP/1.1 302 Found\r\nLocation: \(target.url)\r\nContent-Length: 0\r\n\r\n".utf8))
      _ = await c.next(within: .seconds(2))
      await c.closeTCP()
    }
    defer { Task { await server.shutdown() } }
    var connected = false
    do {
      let s = try await kind.transport.connect(url: server.url, secProtocol: "secret-token")
      connected = true
      await s.close(code: 1000, reason: "")
    } catch {
      print("[probe] \(kind) redirect: \(error)")
    }
    print(
      "[probe] \(kind) redirect: connected \(connected), target accepted \(target.accepted), protocol header sent to the target: \(String(describing: forwarded.withLock { $0 }))"
    )
    #expect(!connected)
    #expect(target.accepted == 0, "followed the redirect, with the protocol header")
  }

  /// A server that accepts the TCP connection and never answers the upgrade: cancelling the connect ends it at once and closes the socket, as closing a connecting browser `WebSocket` does. The engine cancels on its connect timeout and on close.
  @Test(arguments: TransportKind.allCases)
  func cancellingAHungUpgradeClosesTheSocket(_ kind: TransportKind) async throws {
    let server = try await ScriptedServer { _ in
      try? await Task.sleep(for: .seconds(60))
    }
    defer { Task { await server.shutdown() } }
    for delay in [Duration.zero, .milliseconds(1), .milliseconds(300)] {
      let accepted = server.accepted
      let transport = kind.transport
      let url = server.url
      let connecting = Task { try await transport.connect(url: url, secProtocol: "p") }
      if delay > .zero { try await Task.sleep(for: delay) }
      if delay == .milliseconds(300) {
        while server.accepted == accepted { try await Task.sleep(for: .milliseconds(10)) }
      }
      let start = ContinuousClock.now
      connecting.cancel()
      let ended = Box(false)
      let watcher = Task {
        _ = try? await connecting.value
        ended.withLock { $0 = true }
      }
      let deadline = ContinuousClock.now + .seconds(5)
      while !ended.withLock({ $0 }), ContinuousClock.now < deadline {
        try await Task.sleep(for: .milliseconds(10))
      }
      print(
        "[probe] \(kind) cancel after \(delay): ended \(ended.withLock { $0 }) after \(ContinuousClock.now - start)"
      )
      #expect(ended.withLock { $0 }, "the connect cancelled after \(delay) never returned")
      #expect(
        await server.allClosed(within: .seconds(5)),
        "the socket cancelled after \(delay) stayed open")
      watcher.cancel()
    }
  }

  // MARK: - Leaks

  /// Sockets opened and closed through `TransportSocketFactory`, some mid-handshake: every one is closed, and none is kept alive.
  @Test(arguments: TransportKind.allCases) @MainActor
  func openAndCloseLeavesNoSocketsOrObjects(_ kind: TransportKind) async throws {
    let server = try await ScriptedServer { c in
      try? await Task.sleep(for: .milliseconds(Int.random(in: 0...20)))
      await c.write(c.accept() + ScriptedServer.text("hi"))
      _ = await c.next()
      await c.closeTCP()
    }
    defer { Task { await server.shutdown() } }
    final class Events: SocketEvents {
      var opened = 0
      func socketDidOpen() { opened += 1 }
      func socketDidReceive(_ message: SocketMessage) {}
      func socketDidClose(code: Int, reason: String, wasClean: Bool) {}
    }
    let events = Events()
    let factory = TransportSocketFactory(kind.transport)
    var weakConnections: [() -> AnyObject?] = []
    for n in 0..<40 {
      let connection = factory.open(url: server.url, secProtocol: "p", events: events)
      weak var w = connection
      weakConnections.append { w }
      try await Task.sleep(for: .milliseconds(n % 8 * 3))
      connection.close(code: nil)
    }
    #expect(
      await server.allClosed(within: .seconds(10)), "\(server.openConnections) sockets left open")
    let deadline = ContinuousClock.now + .seconds(5)
    while weakConnections.contains(where: { $0() != nil }), ContinuousClock.now < deadline {
      try await Task.sleep(for: .milliseconds(20))
    }
    let alive = weakConnections.filter { $0() != nil }.count
    print(
      "[probe] \(kind) open/close x40: \(events.opened) opened, \(server.accepted) accepted, \(alive) alive"
    )
    #expect(alive == 0, "connections kept alive")
  }

  /// A connected socket released after its close is deallocated: the task and its delegate don't keep each other alive.
  @Test(arguments: TransportKind.allCases)
  func closedSocketIsReleased(_ kind: TransportKind) async throws {
    let server = try await ScriptedServer { c in
      await c.write(c.accept())
      let f = await c.next()
      await c.write(ScriptedServer.close(f?.closeCode ?? 1000))
      await c.closeTCP()
    }
    defer { Task { await server.shutdown() } }
    weak var weakSocket: AnyObject?
    do {
      let socket = try await kind.transport.connect(url: server.url, secProtocol: nil)
      weakSocket = socket
      await socket.close(code: 1000, reason: "")
      _ = await ending(of: socket, within: .seconds(2))
    }
    #expect(await server.allClosed(within: .seconds(5)))
    let deadline = ContinuousClock.now + .seconds(5)
    while weakSocket != nil, ContinuousClock.now < deadline {
      try await Task.sleep(for: .milliseconds(20))
    }
    #expect(weakSocket == nil, "the closed socket was never released")
  }
}
