#if canImport(Darwin) || ZERO_URLSESSION
  import Foundation
  #if canImport(FoundationNetworking)
    import FoundationNetworking
  #endif
  import Synchronization

  /// A `Transport` over `URLSessionWebSocketTask`: the client's socket on
  /// Apple platforms.
  public final class URLSessionTransport: Transport {
    private let session: URLSession
    private let maximumMessageSize: Int

    /// - Parameters:
    ///   - configuration: The session's configuration, for example to add
    ///     headers or to route through a proxy.
    ///   - maximumMessageSize: The largest frame the socket accepts. A poke
    ///     can be larger than `URLSessionWebSocketTask`'s 1 MB default.
    public init(
      configuration: URLSessionConfiguration = .default, maximumMessageSize: Int = 64 << 20
    ) {
      session = URLSession(configuration: configuration)
      self.maximumMessageSize = maximumMessageSize
    }

    deinit {
      session.finishTasksAndInvalidate()
    }

    public func connect(url: String, secProtocol: String?) async throws -> any Socket {
      guard let target = URL(string: url) else {
        throw SocketClosed(code: 1006, reason: "Invalid URL: \(url)", wasClean: false)
      }
      let task = session.webSocketTask(with: target, protocols: secProtocol.map { [$0] } ?? [])
      task.maximumMessageSize = maximumMessageSize
      let socket = URLSessionSocket(task)
      try await socket.open()
      return socket
    }
  }

  /// Resumes a continuation with the first of several values.
  private final class FirstResume<Value: Sendable>: Sendable {
    private let continuation: Mutex<CheckedContinuation<Value, Never>?>

    init(_ continuation: CheckedContinuation<Value, Never>) {
      self.continuation = Mutex(continuation)
    }

    func resume(_ value: Value) {
      let c = continuation.withLock { c in
        defer { c = nil }
        return c
      }
      c?.resume(returning: value)
    }
  }

  /// One `URLSessionWebSocketTask`. Its delegate reports the open and the
  /// close.
  final class URLSessionSocket: NSObject, Socket, URLSessionWebSocketDelegate {
    private struct State {
      var opened: CheckedContinuation<Void, any Error>?
      var close: SocketClosed?
    }

    let task: URLSessionWebSocketTask
    private let state = Mutex(State())

    init(_ task: URLSessionWebSocketTask) {
      self.task = task
    }

    /// Starts the task and waits for the handshake.
    func open() async throws {
      try await withTaskCancellationHandler {
        try await withCheckedThrowingContinuation { (c: CheckedContinuation<Void, any Error>) in
          state.withLock { $0.opened = c }
          task.delegate = self
          task.resume()
        }
      } onCancel: {
        task.cancel()
      }
    }

    private func resumeOpen(_ result: Result<Void, any Error>) {
      let c = state.withLock { s in
        defer { s.opened = nil }
        return s.opened
      }
      c?.resume(with: result)
    }

    func urlSession(
      _ session: URLSession, webSocketTask: URLSessionWebSocketTask,
      didOpenWithProtocol protocol: String?
    ) {
      resumeOpen(.success(()))
    }

    func urlSession(
      _ session: URLSession, webSocketTask: URLSessionWebSocketTask,
      didCloseWith closeCode: URLSessionWebSocketTask.CloseCode, reason: Data?
    ) {
      let close = SocketClosed(
        code: closeCode.rawValue, reason: reason.map { String(decoding: $0, as: UTF8.self) } ?? "",
        wasClean: true)
      state.withLock { $0.close = close }
      resumeOpen(.failure(close))
    }

    func urlSession(
      _ session: URLSession, task: URLSessionTask, didCompleteWithError error: (any Error)?
    ) {
      let close = state.withLock { s in
        if s.close == nil {
          s.close = SocketClosed(
            code: 1006, reason: error.map { $0.localizedDescription } ?? "", wasClean: false)
        }
        return s.close!
      }
      resumeOpen(.failure(close))
    }

    /// A browser `WebSocket` doesn't follow redirects (the WHATWG WebSockets standard fetches the handshake with redirect mode "error"), and following one would send the auth token in `Sec-WebSocket-Protocol` wherever it points. The upgrade fails instead.
    func urlSession(
      _ session: URLSession, task: URLSessionTask,
      willPerformHTTPRedirection response: HTTPURLResponse, newRequest request: URLRequest
    ) async -> URLRequest? {
      nil
    }

    func send(_ message: SocketMessage) async throws {
      switch message {
      case .text(let text): try await task.send(.string(text))
      case .binary(let bytes): try await task.send(.data(Data(bytes)))
      }
    }

    /// How long a message waits to learn whether the connection ended right after it (see `receive`).
    static let cutFrameWindow: Duration = .milliseconds(10)

    /// The receive after the last message, asked for before that message was returned.
    private let next = Mutex<Task<URLSessionWebSocketTask.Message, any Error>?>(nil)

    func receive() async throws -> SocketMessage? {
      let pending = next.withLock { n in
        defer { n = nil }
        return n
      }
      let message: URLSessionWebSocketTask.Message
      do {
        if let pending {
          message = try await pending.value
        } else {
          message = try await task.receive()
        }
      } catch {
        throw closeReason(error)
      }
      // When the connection ends partway through a frame, URLSession returns the part it has as a whole message, and only the receive after it tells that the connection ended; a browser drops the part and reports an abrupt close. So the next message is asked for now, and this one waits for it up to `cutFrameWindow`: when the connection ends without a close frame by then, this one is dropped.
      let following = Task { [task] in try await task.receive() }
      next.withLock { $0 = following }
      if case .failure(let error)? = await Self.outcome(of: following, within: Self.cutFrameWindow)
      {
        let close = closeReason(error)
        if !close.wasClean { throw close }
      }
      switch message {
      case .string(let text): return .text(text)
      case .data(let data): return .binary([UInt8](data))
      @unknown default: return nil
      }
    }

    /// The outcome of `task` if it settles within `window`.
    private static func outcome(
      of task: Task<URLSessionWebSocketTask.Message, any Error>, within window: Duration
    ) async -> Result<URLSessionWebSocketTask.Message, any Error>? {
      await withCheckedContinuation { c in
        let first = FirstResume(c)
        let timer = Task {
          try? await Task.sleep(for: window)
          first.resume(nil)
        }
        Task {
          let result = await task.result
          timer.cancel()
          first.resume(result)
        }
      }
    }

    /// How the socket closed, once `receive` fails: the close frame when one
    /// came, otherwise an abrupt close.
    private func closeReason(_ error: any Error) -> SocketClosed {
      if let close = state.withLock({ $0.close }) { return close }
      if task.closeCode != .invalid {
        return SocketClosed(
          code: task.closeCode.rawValue,
          reason: task.closeReason.map { String(decoding: $0, as: UTF8.self) } ?? "",
          wasClean: true)
      }
      return SocketClosed(code: 1006, reason: error.localizedDescription, wasClean: false)
    }

    func close(code: Int, reason: String) async {
      task.cancel(
        with: URLSessionWebSocketTask.CloseCode(rawValue: code) ?? .normalClosure,
        reason: reason.isEmpty ? nil : Data(reason.utf8))
    }
  }
#endif
