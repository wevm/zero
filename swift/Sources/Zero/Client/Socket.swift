import Synchronization

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

  init(transport: any Transport, url: String, secProtocol: String?, events: any SocketEvents) {
    self.events = events
    let (frames, outgoing) = AsyncStream<Outgoing>.makeStream()
    self.outgoing = outgoing
    Task { @MainActor [weak self] in
      let socket: any Socket
      do {
        socket = try await transport.connect(url: url, secProtocol: secProtocol)
      } catch {
        self?.finish(SocketClosed(error))
        return
      }
      guard let self, !self.closed else {
        await socket.close(code: 1000, reason: "")
        return
      }
      // One writer keeps the frames in order; each send in a task of its
      // own could overtake the one before.
      Task {
        for await frame in frames {
          switch frame {
          case .text(let text):
            try? await socket.send(.text(text))
          case .close(let code):
            await socket.close(code: code, reason: "")
            return
          }
        }
      }
      self.events?.socketDidOpen()
      // Frames are read, and parsed when the engine is listening, off the
      // main actor: parsing a large poke there would hold up the app. One
      // reader, one stream: they arrive here in the order they came.
      let parsing = self.events is any ParsingSocketEvents
      let (received, sink) = AsyncStream<Received>.makeStream()
      let reader = Task { await Self.read(socket, parsing: parsing, into: sink) }
      for await item in received {
        if self.closed {
          reader.cancel()
          return
        }
        switch item {
        case .raw(let message):
          self.events?.socketDidReceive(message)
        case .parsed(let incoming):
          (self.events as? any ParsingSocketEvents)?.socketDidReceive(parsed: incoming)
        case .closed(let close):
          self.finish(close)
        }
      }
    }
  }

  private enum Received: Sendable {
    case raw(SocketMessage)
    case parsed(Incoming)
    case closed(SocketClosed)
  }

  /// Reads `socket` until it closes, parsing each frame when `parsing`.
  @concurrent
  nonisolated private static func read(
    _ socket: any Socket, parsing: Bool, into sink: AsyncStream<Received>.Continuation
  ) async {
    defer { sink.finish() }
    var decoder = IncomingDecoder()
    do {
      while let message = try await socket.receive() {
        if Task.isCancelled { return }
        sink.yield(parsing ? .parsed(decoder.decode(message)) : .raw(message))
      }
      sink.yield(.closed(SocketClosed(code: 1000, reason: "", wasClean: true)))
    } catch {
      sink.yield(.closed(SocketClosed(error)))
    }
  }

  func send(_ text: String) {
    guard !closed else { return }
    outgoing.yield(.text(text))
  }

  func close(code: Int?) {
    guard !closed else { return }
    closed = true
    events = nil
    outgoing.yield(.close(code ?? 1000))
    outgoing.finish()
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

/// Events that take frames already parsed. The engine's sockets take these,
/// so that a `TransportConnection` can parse off the main actor.
@MainActor
protocol ParsingSocketEvents: SocketEvents {
  /// A frame `IncomingDecoder` parsed, in the order the frames came.
  func socketDidReceive(parsed incoming: Incoming)
}

/// A frame from the server, parsed: what the engine works on.
///
/// Sendable so it can be parsed off the main actor and applied on it. The
/// JSON in it (`JSONObject`, `Row`) is copy-on-write storage that nothing
/// writes after the parse; a later change copies it first, as the storage
/// isn't uniquely referenced while another copy is alive.
enum Incoming: Sendable {
  /// A text message. For a `pokeEnd`, the binary chunks of the poke,
  /// decoded.
  case message(Downstream, decodedChunks: DecodedPokeChunks?)
  /// A binary poke chunk's payload; its ID when an `IncomingDecoder` kept it.
  case pokeChunk(PokeChunkID?, ArraySlice<UInt8>)
  /// A binary message of another type: its first byte, nil when empty.
  case unknownBinary(UInt8?)
  /// A text message that doesn't parse: how the engine reports it.
  case invalid(message: String)

  /// Parses one frame on its own (`#onMessage`'s parse).
  static func parse(_ message: SocketMessage, chunk: PokeChunkID? = nil) -> Incoming {
    let text: String
    switch message {
    case .binary(let bytes):
      guard bytes.first == PokeChunk.messageType else { return .unknownBinary(bytes.first) }
      return .pokeChunk(chunk, bytes.dropFirst())
    case .text(let t):
      text = t
    }
    do {
      return .message(try Downstream(text: text), decodedChunks: nil)
    } catch {
      return .invalid(
        message: "Invalid message received from server: \(valitaMessage(error, text))\(text)")
    }
  }
}

/// One binary poke chunk of one socket.
struct PokeChunkID: Sendable, Hashable {
  var socket: UInt64
  var index: Int
}

/// The poke parts the chunks `chunks` decode to, or why they don't.
struct DecodedPokeChunks: Sendable {
  var chunks: [PokeChunkID]
  var parts: Result<[PokePartBody], DecodingError>
}

/// Parses one socket's frames, in order. Pure, so it runs on whatever
/// thread reads the socket. It collects the binary chunks since the last
/// `pokeStart` so the `pokeEnd` after them comes with them decoded; the
/// engine uses those only when they are exactly the chunks it holds.
struct IncomingDecoder: Sendable {
  private static let sockets = Atomic<UInt64>(0)

  private let socket = Self.sockets.add(1, ordering: .relaxed).newValue
  private var nextChunk = 0
  private var chunkIDs: [PokeChunkID] = []
  private var chunks: [ArraySlice<UInt8>] = []

  mutating func decode(_ message: SocketMessage) -> Incoming {
    var id: PokeChunkID?
    if case .binary = message {
      id = PokeChunkID(socket: socket, index: nextChunk)
      nextChunk += 1
    }
    let incoming = Incoming.parse(message, chunk: id)
    switch incoming {
    case .pokeChunk(let id?, let bytes):
      chunkIDs.append(id)
      chunks.append(bytes)
    case .message(.pokeStart, _):
      clearChunks()
    case .message(.pokeEnd(let body), _):
      defer { clearChunks() }
      if !chunks.isEmpty, body.cancel != true {
        let chunks = self.chunks
        return .message(
          .pokeEnd(body),
          decodedChunks: DecodedPokeChunks(
            chunks: chunkIDs,
            parts: Result { () throws(DecodingError) in try PokeChunk.decode(chunks) }))
      }
    default:
      break
    }
    return incoming
  }

  private mutating func clearChunks() {
    chunkIDs.removeAll()
    chunks.removeAll()
  }
}
