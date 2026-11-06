#if canImport(Network)
  import Foundation
  import Testing
  import ZeroTesting

  @testable import Zero

  /// The URLSession transport against a WebSocket server on the loopback
  /// interface.
  @Suite(.timeLimit(.minutes(1))) struct URLSessionTransportTests {
    @Test func sendsTheProtocolAndExchangesFrames() async throws {
      let server = try WebSocketServer()
      let port = try await server.start()
      defer { server.stop() }
      var events = server.events.makeAsyncIterator()

      let socket = try await URLSessionTransport().connect(
        url: "ws://127.0.0.1:\(port)/sync/v53/connect?clientID=c", secProtocol: "abc")
      guard case .connected(let peer, let protocols)? = await events.next() else {
        Issue.record("not connected")
        return
      }
      #expect(protocols == ["abc"])

      try await socket.send(.text("hello"))
      guard case .message(_, let message)? = await events.next() else {
        Issue.record("no message")
        return
      }
      #expect(message == .text("hello"))

      peer.send(.text("hi"))
      #expect(try await socket.receive() == .text("hi"))
      peer.send(.binary([1, 2, 3]))
      #expect(try await socket.receive() == .binary([1, 2, 3]))
      // Larger than URLSessionWebSocketTask's default limit of 1 MB.
      let large = String(repeating: "x", count: 3 << 20)
      peer.send(.text(large))
      #expect(try await socket.receive() == .text(large))

      peer.close(code: 4001, reason: "bye")
      await #expect(throws: SocketClosed(code: 4001, reason: "bye", wasClean: true)) {
        try await socket.receive()
      }
    }

    @Test func failsToConnectWhenNothingListens() async throws {
      let server = try WebSocketServer()
      let port = try await server.start()
      server.stop()
      try await Task.sleep(for: .milliseconds(100))
      await #expect(throws: SocketClosed.self) {
        try await URLSessionTransport().connect(url: "ws://127.0.0.1:\(port)/", secProtocol: nil)
      }
    }

    /// Records what a socket reports, on the main actor.
    @MainActor final class Recorder: SocketEvents {
      var opened = false
      var received: [SocketMessage] = []
      var closed: SocketClosed?
      func socketDidOpen() { opened = true }
      func socketDidReceive(_ message: SocketMessage) { received.append(message) }
      func socketDidClose(code: Int, reason: String, wasClean: Bool) {
        closed = SocketClosed(code: code, reason: reason, wasClean: wasClean)
      }
    }

    /// When the client's sends ran and finished and the server got the
    /// frames, timed off the main actor, which the client's writer runs on.
    struct FrameTimes: Sendable {
      let start = ContinuousClock.now
      var started = 0
      var sent = 0
      var failures: [String] = []
      var lastSent: Duration = .zero
      var slowestSend: Duration = .zero
      var received = 0
      var lastReceived: Duration = .zero
      var mainActorWait: Duration = .zero
      var mainActorWaitAt: Duration = .zero

      var report: String {
        func ms(_ d: Duration) -> Int { Int(d / .milliseconds(1)) }
        return """
          The server got \(received) of 200 frames, the last at \(ms(lastReceived)) ms. \
          The client started \(started) sends and finished \(sent), the last at \
          \(ms(lastSent)) ms and the slowest in \(ms(slowestSend)) ms; failed: \(failures). \
          The main actor's longest wait was \(ms(mainActorWait)) ms, at \(ms(mainActorWaitAt)) ms.
          """
      }
    }

    struct TimedTransport: Transport {
      let base: any Transport
      let times: Locked<FrameTimes>

      func connect(url: String, secProtocol: String?) async throws -> any Socket {
        TimedSocket(base: try await base.connect(url: url, secProtocol: secProtocol), times: times)
      }
    }

    final class TimedSocket: Socket {
      let base: any Socket
      let times: Locked<FrameTimes>

      init(base: any Socket, times: Locked<FrameTimes>) {
        self.base = base
        self.times = times
      }

      func send(_ message: SocketMessage) async throws {
        let began = ContinuousClock.now
        times.withLock { $0.started += 1 }
        do {
          try await base.send(message)
        } catch {
          times.withLock { $0.failures.append("\(error)") }
          throw error
        }
        let now = ContinuousClock.now
        times.withLock { t in
          t.sent += 1
          t.lastSent = now - t.start
          t.slowestSend = max(t.slowestSend, now - began)
        }
      }

      func receive() async throws -> SocketMessage? { try await base.receive() }

      func close(code: Int, reason: String) async { await base.close(code: code, reason: reason) }
    }

    @Test @MainActor func keepsTheOrderOfFramesSentBeforeAndAfterTheOpen() async throws {
      let server = try WebSocketServer()
      let port = try await server.start()
      defer { server.stop() }

      let times = Locked(FrameTimes())
      let recorder = Recorder()
      let factory = TransportSocketFactory(
        TimedTransport(base: URLSessionTransport(), times: times))
      let connection = factory.open(
        url: "ws://127.0.0.1:\(port)/", secProtocol: nil, events: recorder)
      for i in 0..<200 { connection.send("\(i)") }

      // Off the main actor, so that a busy main actor doesn't delay when the
      // frames are counted.
      let events = server.events
      let reading = Task.detached { () -> (peer: WebSocketServer.Peer, texts: [String])? in
        var events = events.makeAsyncIterator()
        guard case .connected(let peer, _)? = await events.next() else { return nil }
        var texts: [String] = []
        while texts.count < 200, case .message(_, .text(let text))? = await events.next() {
          texts.append(text)
          let now = ContinuousClock.now
          times.withLock { t in
            t.received += 1
            t.lastReceived = now - t.start
          }
        }
        return (peer, texts)
      }
      let probe = Task.detached {
        while !Task.isCancelled {
          let asked = ContinuousClock.now
          await MainActor.run {}
          let waited = ContinuousClock.now - asked
          times.withLock { t in
            if waited > t.mainActorWait {
              t.mainActorWait = waited
              t.mainActorWaitAt = asked - t.start
            }
          }
          try? await Task.sleep(for: .milliseconds(50))
        }
      }
      let watchdog = Task {
        try await Task.sleep(for: .seconds(30))
        Issue.record("After 30 s: \(times.withLock { $0.report })")
      }
      defer {
        probe.cancel()
        watchdog.cancel()
      }
      guard let read = await reading.value else {
        Issue.record("not connected")
        return
      }
      #expect(read.texts == (0..<200).map(String.init))
      #expect(recorder.opened)

      read.peer.close(code: 4003, reason: "done")
      while recorder.closed == nil { try await Task.sleep(for: .milliseconds(10)) }
      #expect(recorder.closed == SocketClosed(code: 4003, reason: "done", wasClean: true))
    }

    @Test @MainActor func sendsWhileTheMainActorIsBusy() async throws {
      let server = try WebSocketServer()
      let port = try await server.start()
      defer { server.stop() }
      let events = server.events
      let received = Locked(0)
      let reading = Task.detached {
        for await event in events {
          guard case .message = event else { continue }
          received.withLock { $0 += 1 }
        }
      }
      defer { reading.cancel() }

      let recorder = Recorder()
      let connection = TransportSocketFactory(URLSessionTransport()).open(
        url: "ws://127.0.0.1:\(port)/", secProtocol: nil, events: recorder)
      defer { connection.close(code: nil) }
      while !recorder.opened { try await Task.sleep(for: .milliseconds(10)) }

      for i in 0..<200 { connection.send("\(i)") }
      // The main actor stays busy; the frames still go out.
      usleep(1_000_000)
      #expect(received.withLock { $0 } == 200)
    }

    @Test @MainActor func connectsAClientThroughTheDefaultFactory() async throws {
      let server = try WebSocketServer()
      let port = try await server.start()
      defer { server.stop() }
      var events = server.events.makeAsyncIterator()

      var options = ZeroOptions(
        cacheURL: "http://127.0.0.1:\(port)", userID: "u", storageKey: UUID().uuidString,
        storage: .memory)
      options.logSink = RecordingLogSink()
      let zero = try Zero(schema: ZeroSchema(tables: []), options: options)
      defer { zero.close() }
      guard case .connected(let peer, let protocols)? = await events.next() else {
        Issue.record("not connected")
        return
      }
      let decoded = try SecProtocols.decode(#require(protocols.first))
      #expect(decoded["initConnectionMessage"]?[0] == "initConnection")

      peer.send(.text(#"["connected",{"wsid":"w"}]"#))
      while zero.connection.state != .connected { try await Task.sleep(for: .milliseconds(10)) }
    }
  }
#endif
