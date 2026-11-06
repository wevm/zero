/// What the engine sees of a WebSocket, on the main actor: it opens one per
/// connection attempt, sends text frames, and is told about frames and the
/// close. This is the shape of the browser `WebSocket` the JS client uses.
@MainActor
public protocol SocketFactory: AnyObject {
  /// Starts opening a WebSocket to `url`, sending `secProtocol` (when
  /// non-nil) as the `Sec-WebSocket-Protocol` header. Events arrive on
  /// `events` until the engine closes the socket.
  func open(url: String, secProtocol: String?, events: any SocketEvents) -> any SocketConnection
}

/// An open or opening WebSocket.
@MainActor
public protocol SocketConnection: AnyObject {
  func send(_ text: String)
  /// Closes the socket. No more events are delivered after this.
  func close(code: Int?)
}

/// Receives the events of one socket.
@MainActor
public protocol SocketEvents: AnyObject {
  func socketDidOpen()
  func socketDidReceive(_ message: SocketMessage)
  func socketDidClose(code: Int, reason: String, wasClean: Bool)
}

/// A socket that, when the connection ends partway through a frame, can return the part it has as a whole message, as `URLSessionWebSocketTask` does. A browser `WebSocket` never does.
package protocol DeliversPartialFrames: Socket {
  /// Sends a ping and waits for its pong: throws when the connection has ended, as it has when the last message was a cut-off frame.
  func ping() async throws
}

/// Runs an async `Transport` as a `SocketFactory`.
@MainActor
public final class TransportSocketFactory: SocketFactory {
  let transport: any Transport

  public init(_ transport: any Transport) { self.transport = transport }

  public func open(
    url: String, secProtocol: String?, events: any SocketEvents
  )
    -> any SocketConnection
  {
    TransportConnection(transport: transport, url: url, secProtocol: secProtocol, events: events)
  }
}

/// One socket of a `Transport`. Frames are written in the order they are
/// sent, and the close after them.
@MainActor
final class TransportConnection: SocketConnection {
  private enum Outgoing: Sendable {
    case text(String)
    case close(Int)
  }

  private weak var events: (any SocketEvents)?
  private var closed = false
  private let outgoing: AsyncStream<Outgoing>.Continuation
  /// The task waiting for `Transport.connect`, until it returns. Closing
  /// cancels it, which ends the handshake, as closing a connecting JS
  /// `WebSocket` does: the transport aborts it, or closes the socket if it
  /// opens anyway.
  private var connecting: Task<Void, Never>?
  /// The socket, when it can deliver part of a frame cut off by the connection's end (`DeliversPartialFrames`).
  private var partialFrames: (any DeliversPartialFrames)?
  var deliversPartialFrames: Bool { partialFrames != nil }
  /// The engine's log. At `.debug`, the writer logs a send still waiting after a second, which holds up the frames and the close after it, and when it sends the close. A browser's `send` buffers and returns, so zero-client has no such wait to log.
  var log: Log?

  init(transport: any Transport, url: String, secProtocol: String?, events: any SocketEvents) {
    self.events = events
    let (frames, outgoing) = AsyncStream<Outgoing>.makeStream()
    self.outgoing = outgoing
    connecting = Task { @MainActor [weak self] in
      let socket: any Socket
      do {
        socket = try await transport.connect(url: url, secProtocol: secProtocol)
      } catch {
        self?.finish(SocketClosed(error))
        return
      }
      self?.connecting = nil
      self?.partialFrames = socket as? any DeliversPartialFrames
      guard let self, !self.closed else {
        await socket.close(code: 1000, reason: "")
        return
      }
      // One writer keeps the frames in order; each send in a task of its
      // own could overtake the one before. It runs off the main actor: on
      // it, every frame would wait for the main actor to come back from its
      // send, so a busy main thread would hold up the frames.
      let log = self.log.flatMap { $0.level <= .debug ? $0 : nil }
      Task.detached {
        for await frame in frames {
          switch frame {
          case .text(let text):
            let start = ContinuousClock.now
            let slow = log.map { log in
              Task {
                try? await Task.sleep(for: .seconds(1))
                if !Task.isCancelled {
                  log.debug("A WebSocket send hasn't finished after 1 second")
                }
              }
            }
            try? await socket.send(.text(text))
            slow?.cancel()
            let took = ContinuousClock.now - start
            if took > .seconds(1) {
              log?.debug(
                "A WebSocket send finished after \(JSNumber.format(SystemScheduler.milliseconds(took).rounded())) ms"
              )
            }
          case .close(let code):
            log?.debug("Sending the WebSocket close: code=\(code)")
            await socket.close(code: code, reason: "")
            return
          }
        }
      }
      self.events?.socketDidOpen()
      do {
        while let message = try await socket.receive() {
          if self.closed { return }
          self.events?.socketDidReceive(message)
        }
        self.finish(SocketClosed(code: 1000, reason: "", wasClean: true))
      } catch {
        self.finish(SocketClosed(error))
      }
    }
  }

  func send(_ text: String) {
    guard !closed else { return }
    outgoing.yield(.text(text))
  }

  /// Pings a socket that delivers partial frames: `done` hears whether the connection answered.
  func ping(_ done: @escaping @MainActor (Bool) -> Void) {
    guard let socket = partialFrames else { return done(false) }
    Task { @MainActor in
      let answered: Bool
      do {
        try await socket.ping()
        answered = true
      } catch {
        answered = false
      }
      done(answered)
    }
  }

  func close(code: Int?) {
    guard !closed else { return }
    closed = true
    events = nil
    outgoing.yield(.close(code ?? 1000))
    outgoing.finish()
    connecting?.cancel()
    connecting = nil
  }

  private func finish(_ close: SocketClosed) {
    guard !closed else { return }
    closed = true
    outgoing.finish()
    let events = self.events
    self.events = nil
    events?.socketDidClose(code: close.code, reason: close.reason, wasClean: close.wasClean)
  }
}
