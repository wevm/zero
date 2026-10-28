#if canImport(Network)
  import Foundation
  import Testing
  import ZeroTesting

  @testable import Zero

  #if canImport(UIKit)
    import UIKit
  #else
    import AppKit
  #endif

  /// The app's lifecycle against a WebSocket server on the loopback interface: the notifications the system posts, posted by hand, in the orders and at the moments that race with connecting and closing.
  ///
  /// The notifications reach every client in the process, so these run alone, when `ZERO_LIFECYCLE_TESTS` is set.
  @Suite(
    .serialized, .timeLimit(.minutes(2)),
    .enabled(if: ProcessInfo.processInfo.environment["ZERO_LIFECYCLE_TESTS"] != nil))
  @MainActor struct AppLifecycleTests {
    /// A server that acknowledges every connection, and counts them.
    @MainActor final class Server {
      let server: WebSocketServer
      let port: UInt16
      private(set) var connected = 0
      private(set) var closed = 0
      private(set) var messages: [String] = []
      /// Whether to send `connected` to the next connections.
      var acknowledge = true
      private var peers: [WebSocketServer.Peer] = []
      private var task: Task<Void, Never>?

      init() async throws {
        server = try WebSocketServer()
        port = try await server.start()
        task = Task { @MainActor [server] in
          for await event in server.events {
            switch event {
            case .connected(let peer, _):
              self.connected += 1
              self.peers.append(peer)
              if self.acknowledge { peer.send(.text(#"["connected",{"wsid":"w"}]"#)) }
            case .message(let peer, .text(let text)):
              self.messages.append(text)
              if text.hasPrefix(#"["ping""#) { peer.send(.text(#"["pong",{}]"#)) }
            case .message: break
            case .closed:
              self.closed += 1
            }
          }
        }
      }

      var open: Int { connected - closed }

      /// Acknowledges the connections open so far.
      func acknowledgeAll() {
        for p in peers { p.send(.text(#"["connected",{"wsid":"w"}]"#)) }
      }

      func stop() {
        task?.cancel()
        server.stop()
      }
    }

    let log = RecordingLogSink()

    func makeZero(_ server: Server) throws -> Zero<Void> {
      var options = ZeroOptions(
        cacheURL: "http://127.0.0.1:\(server.port)", userID: "u", storageKey: UUID().uuidString,
        storage: .memory)
      options.logLevel = .debug
      options.logSink = log
      return try Zero(schema: .notes, mutators: [Notes.put], options: options)
    }

    /// The app goes to the background, or the Mac to sleep.
    func hide() {
      #if canImport(UIKit)
        NotificationCenter.default.post(
          name: UIApplication.didEnterBackgroundNotification, object: UIApplication.shared)
      #else
        NSWorkspace.shared.notificationCenter.post(
          name: NSWorkspace.willSleepNotification, object: NSWorkspace.shared)
      #endif
    }

    /// The app comes back, or the Mac wakes.
    func show() {
      #if canImport(UIKit)
        NotificationCenter.default.post(
          name: UIApplication.willEnterForegroundNotification, object: UIApplication.shared)
      #else
        NSWorkspace.shared.notificationCenter.post(
          name: NSWorkspace.didWakeNotification, object: NSWorkspace.shared)
      #endif
    }

    func within(_ timeout: Duration, _ condition: () -> Bool) async throws -> Bool {
      let deadline = ContinuousClock.now + timeout
      while !condition() {
        if ContinuousClock.now >= deadline { return false }
        try await Task.sleep(for: .milliseconds(10))
      }
      return true
    }

    func eventually(
      _ what: String, timeout: Duration = .seconds(10), _ condition: () -> Bool
    ) async throws {
      let deadline = ContinuousClock.now + timeout
      while !condition() {
        guard ContinuousClock.now < deadline else {
          Issue.record("Timed out waiting for \(what)")
          for entry in log.entries.suffix(25) {
            print("[lifecycle]   \(entry.level) \(entry.message.prefix(200))")
          }
          throw CancellationError()
        }
        try await Task.sleep(for: .milliseconds(10))
      }
    }

    @Test func hiddenDisconnectsAndShownReconnects() async throws {
      let server = try await Server()
      defer { server.stop() }
      let zero = try makeZero(server)
      defer { zero.close() }
      try await eventually("connected") { zero.connection.state == .connected }
      hide()
      try await eventually("disconnected") { zero.connection.state.status == .disconnected }
      try await eventually("the socket closed") { server.open == 0 }
      show()
      try await eventually("reconnected") { zero.connection.state == .connected }
      #expect(server.connected == 2)
      #expect(server.open == 1)
    }

    /// Hidden while the socket is open but not yet acknowledged: the client ends up disconnected with no socket open, and connects when shown.
    @Test func hiddenWhileConnecting() async throws {
      let server = try await Server()
      defer { server.stop() }
      server.acknowledge = false
      let zero = try makeZero(server)
      defer { zero.close() }
      try await eventually("the socket") { server.connected == 1 }
      hide()
      try await Task.sleep(for: .milliseconds(100))
      server.acknowledgeAll()
      try await eventually("disconnected") { zero.connection.state.status == .disconnected }
      let closed = try await within(.seconds(10)) { server.open == 0 }
      print(
        "[lifecycle] hidden while connecting: server connected \(server.connected), closed \(server.closed), \(zero.connection.state)"
      )
      #expect(closed, "the socket stayed open")
      server.acknowledge = true
      show()
      try await eventually("reconnected") { zero.connection.state == .connected }
      #expect(server.open == 1)
    }

    /// Many transitions in a row, without waiting between them: the client ends up in the state of the last one, with at most one socket open.
    @Test func rapidTransitionsSettleOnTheLast() async throws {
      let server = try await Server()
      defer { server.stop() }
      let zero = try makeZero(server)
      defer { zero.close() }
      try await eventually("connected") { zero.connection.state == .connected }
      for round in 0..<3 {
        for _ in 0..<50 {
          hide()
          show()
        }
        if round > 0 {
          for _ in 0..<round { await Task.yield() }
        }
      }
      try await eventually("connected after the transitions") {
        zero.connection.state == .connected
      }
      try await eventually("one socket open", timeout: .seconds(5)) { server.open == 1 }
      for _ in 0..<50 {
        show()
        hide()
      }
      try await eventually("disconnected after the transitions") {
        zero.connection.state.status == .disconnected
      }
      try await eventually("no socket open", timeout: .seconds(5)) { server.open == 0 }
      print("[lifecycle] rapid transitions: \(server.connected) connections")
    }

    /// Transitions after `close()` do nothing: no socket is opened and the client stays closed.
    @Test func transitionsAfterCloseDoNothing() async throws {
      let server = try await Server()
      defer { server.stop() }
      let zero = try makeZero(server)
      try await eventually("connected") { zero.connection.state == .connected }
      zero.close()
      try await eventually("the socket closed") { server.open == 0 }
      for _ in 0..<5 {
        hide()
        show()
      }
      zero.close()
      try await Task.sleep(for: .milliseconds(500))
      #expect(server.connected == 1)
      #expect(zero.connection.state.status == .closed)
    }

    /// A `Zero` released without `close()`: its observers go with it, and later transitions don't reach it.
    @Test func releasedZeroStopsObserving() async throws {
      let server = try await Server()
      defer { server.stop() }
      weak var weakZero: Zero<Void>?
      do {
        let zero = try makeZero(server)
        weakZero = zero
        try await eventually("connected") { zero.connection.state == .connected }
      }
      for _ in 0..<5 {
        hide()
        show()
      }
      try await Task.sleep(for: .milliseconds(500))
      print("[lifecycle] released Zero alive: \(weakZero != nil), connections: \(server.connected)")
      weakZero?.close()
    }

    #if canImport(UIKit)
      /// In the background, the client waits for the server to confirm the pending mutations before it disconnects, up to 10 seconds; back in the foreground before that, it stays connected.
      @Test func backgroundWaitsForPendingMutationsUntilTheForeground() async throws {
        let server = try await Server()
        defer { server.stop() }
        let zero = try makeZero(server)
        defer { zero.close() }
        try await eventually("connected") { zero.connection.state == .connected }
        zero.mutate(Notes.put(.init(id: "n1", text: "pending")))
        try await eventually("the push") { server.messages.contains { $0.hasPrefix(#"["push""#) } }
        hide()
        try await Task.sleep(for: .seconds(2))
        #expect(zero.connection.state == .connected, "disconnected before the server confirmed")
        show()
        try await Task.sleep(for: .seconds(10))
        #expect(zero.connection.state == .connected, "the first trip's timeout disconnected it")
      }

      /// The server never confirms: the client disconnects after the 10 seconds.
      @Test func backgroundGivesUpWaitingAfterTenSeconds() async throws {
        let server = try await Server()
        defer { server.stop() }
        let zero = try makeZero(server)
        defer { zero.close() }
        try await eventually("connected") { zero.connection.state == .connected }
        zero.mutate(Notes.put(.init(id: "n1", text: "pending")))
        let start = ContinuousClock.now
        hide()
        try await eventually("disconnected", timeout: .seconds(15)) {
          zero.connection.state.status == .disconnected
        }
        let waited = ContinuousClock.now - start
        print("[lifecycle] gave up after \(waited)")
        #expect(waited > .seconds(9))
      }
    #endif
  }
#endif
