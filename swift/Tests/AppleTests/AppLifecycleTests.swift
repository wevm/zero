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

      /// Its events go to `log`, with the client's, so a test that times out shows what the server saw.
      init(_ log: RecordingLogSink) async throws {
        server = try WebSocketServer(trace: {
          log.log(level: .debug, message: "server: \($0)", context: [:])
        })
        port = try await server.start()
        task = Task { @MainActor [server, log] in
          for await event in server.events {
            switch event {
            case .connected(let peer, _):
              self.connected += 1
              self.peers.append(peer)
              log.log(
                level: .debug,
                message:
                  "server: \(peer.connection.endpoint) open, acknowledging: \(self.acknowledge)",
                context: [:])
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
    let stops = Stops()
    let connectTimeouts = ConnectTimeouts()

    /// Whether the process was stopped during the test: a timer on a queue of its own came back more than a second late, as `within`'s polls do.
    final class Stops: @unchecked Sendable {
      private let queue = DispatchQueue(label: "AppLifecycleTests.Stops")
      private let timer: any DispatchSourceTimer
      private var last = ContinuousClock.now
      private var stopped = false

      init() {
        timer = DispatchSource.makeTimerSource(queue: queue)
        timer.schedule(deadline: .now(), repeating: .milliseconds(100))
        timer.setEventHandler { [weak self] in
          guard let self else { return }
          let now = ContinuousClock.now
          if now - last > .seconds(1) { stopped = true }
          last = now
        }
        timer.resume()
      }

      deinit { timer.cancel() }

      var happened: Bool { queue.sync { stopped } }
    }

    /// The client's connect attempts that timed out.
    @MainActor final class ConnectTimeouts {
      var count = 0
    }

    func makeZero(_ server: Server) throws -> Zero<Void> {
      var options = ZeroOptions(
        cacheURL: "http://127.0.0.1:\(server.port)", userID: "u", storageKey: UUID().uuidString,
        storage: .memory)
      options.logLevel = .debug
      options.logSink = log
      let zero = try Zero(schema: .notes, mutators: [Notes.put], options: options)
      zero.engine.connectionManager.subscribe { [connectTimeouts] state in
        if case .connecting(_, _, .client(.connectTimeout, _)?) = state {
          connectTimeouts.count += 1
        }
      }
      return zero
    }

    /// Whether the client's attempt to connect timed out because the process was stopped. A stop longer than the connect timeout (10 s) while the client is connecting ends the attempt when the process continues, before the server's acknowledgement is read, and the client connects again after its backoff, as zero-client does. Without a stop, a timeout of an attempt that reached the server is a failure the tests catch.
    func timedOutWhileStopped(_ zero: Zero<Void>) -> Bool {
      guard
        case .connecting(_, _, .client(.connectTimeout, _)?) = zero.engine.connectionManager.state
      else { return false }
      return stops.happened
    }

    /// The connections a test expects the server to see, `n`, and, if the process was stopped, up to one more for each attempt that timed out (`timedOutWhileStopped`), which may have reached the server before it ended.
    func connections(_ n: Int) -> ClosedRange<Int> {
      n...n + (stops.happened ? connectTimeouts.count : 0)
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

    /// Whether `condition` holds within `timeout` of the process running. A loaded runner can stop the iOS Simulator's processes for many seconds (CI has had a 10 ms poll come back 21 s late); a poll that comes back more than a second late counts as one second, so the stop doesn't use up the time the client and the server get.
    func within(_ timeout: Duration, _ condition: () -> Bool) async throws -> Bool {
      var waited = Duration.zero
      var last = ContinuousClock.now
      while !condition() {
        if waited >= timeout { return false }
        try await Task.sleep(for: .milliseconds(10))
        let now = ContinuousClock.now
        if now - last > .seconds(1) {
          print("[lifecycle] the process was stopped for \(now - last)")
        }
        waited += min(now - last, .seconds(1))
        last = now
      }
      return true
    }

    /// How long a test waits for the client to connect. An attempt that a stalled system service holds up never reaches the server (freezing the iOS Simulator's `configd_sim` does it, and CI's simulator has done it): the client gives up on it at the connect timeout and tries again within the first retry's longest wait. The wait covers one such attempt, and 2 s for the retry to connect.
    static let connectWait = Duration.milliseconds(
      connectTimeoutMs + fastReconnectInitialMs + 2_000)

    /// Waits up to `connectWait` for `zero` to connect.
    func connected(_ zero: Zero<Void>, _ what: String = "connected") async throws {
      try await eventually(what, timeout: Self.connectWait) { zero.connection.state == .connected }
    }

    func eventually(
      _ what: String, timeout: Duration = .seconds(10), _ condition: () -> Bool
    ) async throws {
      guard try await within(timeout, condition) else {
        Issue.record("Timed out waiting for \(what)")
        for entry in log.entries.suffix(25) {
          print("[lifecycle]   \(entry.level) \(entry.message.prefix(200))")
        }
        throw CancellationError()
      }
    }

    @Test func hiddenDisconnectsAndShownReconnects() async throws {
      let server = try await Server(log)
      defer { server.stop() }
      let zero = try makeZero(server)
      defer { zero.close() }
      try await connected(zero)
      hide()
      try await eventually("disconnected") { zero.connection.state.status == .disconnected }
      try await eventually("the socket closed") { server.open == 0 }
      show()
      try await connected(zero, "reconnected")
      #expect(connections(2).contains(server.connected))
      #expect(server.open == 1)
    }

    /// Hidden while the socket is open but not yet acknowledged: the client ends up disconnected with no socket open, and connects when shown. If the attempt timed out first (`timedOutWhileStopped`), the client closed the socket and, hidden, waits to be shown, still connecting, as zero-client does.
    @Test func hiddenWhileConnecting() async throws {
      let server = try await Server(log)
      defer { server.stop() }
      server.acknowledge = false
      let zero = try makeZero(server)
      defer { zero.close() }
      try await eventually("the socket", timeout: Self.connectWait) { server.connected == 1 }
      hide()
      try await Task.sleep(for: .milliseconds(100))
      server.acknowledgeAll()
      try await eventually("disconnected") {
        zero.connection.state.status == .disconnected || timedOutWhileStopped(zero)
      }
      let closed = try await within(.seconds(10)) { server.open == 0 }
      print(
        "[lifecycle] hidden while connecting: server connected \(server.connected), closed \(server.closed), \(zero.connection.state)"
      )
      #expect(closed, "the socket stayed open")
      server.acknowledge = true
      show()
      try await connected(zero, "reconnected")
      #expect(server.open == 1)
    }

    /// Many transitions in a row, without waiting between them: the client ends up in the state of the last one, with at most one socket open.
    @Test func rapidTransitionsSettleOnTheLast() async throws {
      let server = try await Server(log)
      defer { server.stop() }
      let zero = try makeZero(server)
      defer { zero.close() }
      try await connected(zero)
      for round in 0..<3 {
        for _ in 0..<50 {
          hide()
          show()
        }
        if round > 0 {
          for _ in 0..<round { await Task.yield() }
        }
      }
      try await connected(zero, "connected after the transitions")
      try await eventually("one socket open", timeout: .seconds(10)) { server.open == 1 }
      for _ in 0..<50 {
        show()
        hide()
      }
      try await eventually("disconnected after the transitions") {
        zero.connection.state.status == .disconnected
      }
      // The last socket may be one cancelled while connecting, which a busy iOS Simulator takes seconds to close (as in hiddenWhileConnecting).
      try await eventually("no socket open", timeout: .seconds(10)) { server.open == 0 }
      print("[lifecycle] rapid transitions: \(server.connected) connections")
    }

    /// Transitions after `close()` do nothing: no socket is opened and the client stays closed.
    @Test func transitionsAfterCloseDoNothing() async throws {
      let server = try await Server(log)
      defer { server.stop() }
      let zero = try makeZero(server)
      try await connected(zero)
      zero.close()
      try await eventually("the socket closed") { server.open == 0 }
      for _ in 0..<5 {
        hide()
        show()
      }
      zero.close()
      try await Task.sleep(for: .milliseconds(500))
      #expect(connections(1).contains(server.connected))
      #expect(zero.connection.state.status == .closed)
    }

    /// A `Zero` released without `close()`: its observers go with it, and later transitions don't reach it.
    @Test func releasedZeroStopsObserving() async throws {
      let server = try await Server(log)
      defer { server.stop() }
      weak var weakZero: Zero<Void>?
      do {
        let zero = try makeZero(server)
        weakZero = zero
        try await connected(zero)
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
        let server = try await Server(log)
        defer { server.stop() }
        let zero = try makeZero(server)
        defer { zero.close() }
        try await connected(zero)
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
        let server = try await Server(log)
        defer { server.stop() }
        let zero = try makeZero(server)
        defer { zero.close() }
        try await connected(zero)
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
