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

@MainActor
final class TransportConnection: SocketConnection {
  private weak var events: (any SocketEvents)?
  private var socket: (any Socket)?
  private var pending: [String] = []
  private var closed = false
  private var task: Task<Void, Never>?

  init(transport: any Transport, url: String, secProtocol: String?, events: any SocketEvents) {
    self.events = events
    task = Task { @MainActor [weak self] in
      let socket: any Socket
      do {
        socket = try await transport.connect(url: url, secProtocol: secProtocol)
      } catch {
        self?.finish(code: 1006, reason: String(describing: error), wasClean: false)
        return
      }
      guard let self, !self.closed else {
        await socket.close(code: 1000, reason: "")
        return
      }
      self.socket = socket
      self.events?.socketDidOpen()
      for text in self.pending { self.write(text) }
      self.pending = []
      do {
        while let message = try await socket.receive() {
          if self.closed { return }
          self.events?.socketDidReceive(message)
        }
        self.finish(code: 1000, reason: "", wasClean: true)
      } catch {
        self.finish(code: 1006, reason: String(describing: error), wasClean: false)
      }
    }
  }

  func send(_ text: String) {
    guard !closed else { return }
    if socket == nil {
      pending.append(text)
    } else {
      write(text)
    }
  }

  private func write(_ text: String) {
    guard let socket else { return }
    Task { try? await socket.send(.text(text)) }
  }

  func close(code: Int?) {
    guard !closed else { return }
    closed = true
    events = nil
    if let socket {
      Task { await socket.close(code: code ?? 1000, reason: "") }
    }
  }

  private func finish(code: Int, reason: String, wasClean: Bool) {
    guard !closed else { return }
    closed = true
    let events = self.events
    self.events = nil
    events?.socketDidClose(code: code, reason: reason, wasClean: wasClean)
  }
}
