import Foundation
import NIOCore
import NIOPosix
import Synchronization

/// A WebSocket server on the loopback interface that writes raw bytes, for checking a `Transport` against what a browser `WebSocket` does: frames that come with the 101 response, close frames of any code, cuts partway through a frame, pings, redirects, and upgrades that never come.
///
/// Each connection is handed to `script` once its HTTP request has arrived. The script answers it, writes frames, and reads what the client sends.
final class ScriptedServer: Sendable {
  /// A client frame, unmasked.
  struct Frame: Sendable, Equatable {
    var fin: Bool
    var opcode: UInt8
    var payload: [UInt8]

    /// The close frame's code, or nil.
    var closeCode: Int? {
      guard opcode == 8, payload.count >= 2 else { return nil }
      return Int(payload[0]) << 8 | Int(payload[1])
    }
  }

  /// One client connection.
  final class Connection: Sendable {
    let channel: any Channel
    /// The request line and headers.
    let request: [String]
    /// The client's frames, in order, finished when the client closes the TCP connection.
    let frames: AsyncStream<Frame>
    fileprivate let continuation: AsyncStream<Frame>.Continuation

    fileprivate init(channel: any Channel, request: [String]) {
      self.channel = channel
      self.request = request
      (frames, continuation) = AsyncStream<Frame>.makeStream()
    }

    /// The value of the request header `name`.
    func header(_ name: String) -> String? {
      for line in request.dropFirst() {
        guard let colon = line.firstIndex(of: ":") else { continue }
        if line[..<colon].lowercased() == name.lowercased() {
          return line[line.index(after: colon)...].trimmingCharacters(in: .whitespaces)
        }
      }
      return nil
    }

    /// The 101 response that accepts the upgrade.
    func accept(protocol: String? = nil) -> [UInt8] {
      let key = header("Sec-WebSocket-Key") ?? ""
      let digest = SHA1.hash(Array((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").utf8))
      var response =
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
        + "Sec-WebSocket-Accept: \(Data(digest).base64EncodedString())\r\n"
      if let p = `protocol` ?? header("Sec-WebSocket-Protocol") {
        response += "Sec-WebSocket-Protocol: \(p)\r\n"
      }
      return Array((response + "\r\n").utf8)
    }

    /// Writes `bytes` in one write.
    func write(_ bytes: [UInt8]) async {
      let channel = self.channel
      try? await channel.eventLoop.submit {
        channel.writeAndFlush(channel.allocator.buffer(bytes: bytes), promise: nil)
      }.get()
    }

    /// Closes the TCP connection, abruptly (a reset) or not.
    func closeTCP(abrupt: Bool = false) async {
      let channel = self.channel
      try? await channel.eventLoop.submit {
        if abrupt, let options = channel as? any SocketOptionProvider {
          _ = options.setSoLinger(linger(l_onoff: 1, l_linger: 0))
        }
        channel.close(promise: nil)
      }.get()
    }

    /// The next frame the client sends, other than pongs unless `pongs`, within `timeout`.
    func next(pongs: Bool = false, within timeout: Duration = .seconds(5)) async -> Frame? {
      await withTaskGroup(of: Frame?.self) { group in
        group.addTask {
          for await frame in self.frames where pongs || frame.opcode != 10 { return frame }
          return nil
        }
        group.addTask {
          try? await Task.sleep(for: timeout)
          return nil
        }
        let first = await group.next()!
        group.cancelAll()
        return first
      }
    }
  }

  /// A server frame, unmasked.
  static func frame(_ opcode: UInt8, _ payload: [UInt8], fin: Bool = true) -> [UInt8] {
    var out: [UInt8] = [(fin ? 0x80 : 0) | opcode]
    if payload.count < 126 {
      out.append(UInt8(payload.count))
    } else if payload.count < 1 << 16 {
      out += [126, UInt8(payload.count >> 8), UInt8(payload.count & 0xFF)]
    } else {
      out.append(127)
      for shift in stride(from: 56, through: 0, by: -8) {
        out.append(UInt8(payload.count >> shift & 0xFF))
      }
    }
    return out + payload
  }

  static func text(_ s: String) -> [UInt8] { frame(1, Array(s.utf8)) }

  /// A close frame with `code` and `reason`; an empty one when `code` is nil.
  static func close(_ code: Int?, _ reason: String = "") -> [UInt8] {
    guard let code else { return frame(8, []) }
    return frame(8, [UInt8(code >> 8), UInt8(code & 0xFF)] + Array(reason.utf8))
  }

  let port: Int
  private let group: MultiThreadedEventLoopGroup
  private let server: any Channel
  private let counts: Box<Counts>
  private let tasks: Box<[Task<Void, Never>]>

  struct Counts: Sendable {
    var accepted = 0
    var open = 0
  }

  init(_ script: @escaping @Sendable (Connection) async -> Void) async throws {
    let counts = Box(Counts())
    let tasks = Box<[Task<Void, Never>]>([])
    self.counts = counts
    self.tasks = tasks
    group = MultiThreadedEventLoopGroup(numberOfThreads: 1)
    server = try await ServerBootstrap(group: group)
      .serverChannelOption(.socketOption(.so_reuseaddr), value: 1)
      .childChannelInitializer { channel in
        counts.withLock {
          $0.accepted += 1
          $0.open += 1
        }
        channel.closeFuture.whenComplete { _ in counts.withLock { $0.open -= 1 } }
        return channel.eventLoop.makeCompletedFuture {
          try channel.pipeline.syncOperations.addHandler(
            Handler { connection in
              let task = Task { await script(connection) }
              tasks.withLock { $0.append(task) }
            })
        }
      }
      .bind(host: "127.0.0.1", port: 0).get()
    port = server.localAddress!.port!
  }

  var url: String { "ws://127.0.0.1:\(port)/" }
  var accepted: Int { counts.withLock { $0.accepted } }
  var openConnections: Int { counts.withLock { $0.open } }

  /// Waits up to `timeout` for every connection to close; whether they did.
  func allClosed(within timeout: Duration) async -> Bool {
    let deadline = ContinuousClock.now + timeout
    while openConnections > 0 {
      if ContinuousClock.now >= deadline { return false }
      try? await Task.sleep(for: .milliseconds(10))
    }
    return true
  }

  func shutdown() async {
    for t in tasks.withLock({ $0 }) { t.cancel() }
    try? await server.close()
    try? await group.shutdownGracefully()
  }

  /// Reads the request, then the client's frames.
  private final class Handler: ChannelInboundHandler {
    typealias InboundIn = ByteBuffer
    let onRequest: (Connection) -> Void
    var buffer: [UInt8] = []
    var connection: Connection?

    init(_ onRequest: @escaping (Connection) -> Void) { self.onRequest = onRequest }

    func channelRead(context: ChannelHandlerContext, data: NIOAny) {
      buffer += Array(buffer: unwrapInboundIn(data))
      if connection == nil {
        guard let end = Self.headerEnd(buffer) else { return }
        let head = String(decoding: buffer[..<end], as: UTF8.self)
        buffer.removeFirst(end + 4)
        let c = Connection(
          channel: context.channel,
          request: head.components(separatedBy: "\r\n"))
        connection = c
        onRequest(c)
      }
      while let (frame, used) = Self.parse(buffer) {
        buffer.removeFirst(used)
        connection?.continuation.yield(frame)
      }
    }

    func channelInactive(context: ChannelHandlerContext) {
      connection?.continuation.finish()
    }

    func errorCaught(context: ChannelHandlerContext, error: any Error) {
      context.close(promise: nil)
    }

    static func headerEnd(_ b: [UInt8]) -> Int? {
      guard b.count >= 4 else { return nil }
      for i in 0...(b.count - 4)
      where b[i] == 13 && b[i + 1] == 10 && b[i + 2] == 13 && b[i + 3] == 10 {
        return i
      }
      return nil
    }

    static func parse(_ b: [UInt8]) -> (Frame, Int)? {
      guard b.count >= 2 else { return nil }
      var i = 2
      var length = Int(b[1] & 0x7F)
      if length == 126 {
        guard b.count >= 4 else { return nil }
        length = Int(b[2]) << 8 | Int(b[3])
        i = 4
      } else if length == 127 {
        guard b.count >= 10 else { return nil }
        length = b[2..<10].reduce(0) { $0 << 8 | Int($1) }
        i = 10
      }
      let masked = b[1] & 0x80 != 0
      var mask: [UInt8] = [0, 0, 0, 0]
      if masked {
        guard b.count >= i + 4 else { return nil }
        mask = Array(b[i..<(i + 4)])
        i += 4
      }
      guard b.count >= i + length else { return nil }
      var payload = Array(b[i..<(i + length)])
      for j in payload.indices { payload[j] ^= mask[j % 4] }
      return (Frame(fin: b[0] & 0x80 != 0, opcode: b[0] & 0x0F, payload: payload), i + length)
    }
  }
}

/// A value shared between tasks.
final class Box<Value: Sendable>: Sendable {
  private let mutex: Mutex<Value>
  init(_ value: Value) { mutex = Mutex(value) }
  func withLock<R: Sendable>(_ body: (inout sending Value) -> sending R) -> R {
    mutex.withLock(body)
  }
}

/// SHA-1, for `Sec-WebSocket-Accept`.
enum SHA1 {
  static func hash(_ message: [UInt8]) -> [UInt8] {
    var h: [UInt32] = [0x6745_2301, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476, 0xC3D2_E1F0]
    var m = message + [0x80]
    while m.count % 64 != 56 { m.append(0) }
    let bits = UInt64(message.count) * 8
    for shift in stride(from: 56, through: 0, by: -8) {
      m.append(UInt8(bits >> UInt64(shift) & 0xFF))
    }
    for chunk in stride(from: 0, to: m.count, by: 64) {
      var w = [UInt32](repeating: 0, count: 80)
      for t in 0..<16 {
        w[t] = m[chunk + t * 4..<chunk + t * 4 + 4].reduce(0) { $0 << 8 | UInt32($1) }
      }
      for t in 16..<80 {
        let x = w[t - 3] ^ w[t - 8] ^ w[t - 14] ^ w[t - 16]
        w[t] = x << 1 | x >> 31
      }
      var (a, b, c, d, e) = (h[0], h[1], h[2], h[3], h[4])
      for t in 0..<80 {
        let (f, k): (UInt32, UInt32) =
          switch t {
          case 0..<20: (b & c | ~b & d, 0x5A82_7999)
          case 20..<40: (b ^ c ^ d, 0x6ED9_EBA1)
          case 40..<60: (b & c | b & d | c & d, 0x8F1B_BCDC)
          default: (b ^ c ^ d, 0xCA62_C1D6)
          }
        let temp = (a << 5 | a >> 27) &+ f &+ e &+ k &+ w[t]
        (e, d, c, b, a) = (d, c, b << 30 | b >> 2, a, temp)
      }
      h[0] = h[0] &+ a
      h[1] = h[1] &+ b
      h[2] = h[2] &+ c
      h[3] = h[3] &+ d
      h[4] = h[4] &+ e
    }
    return h.flatMap { v in [24, 16, 8, 0].map { UInt8(v >> UInt32($0) & 0xFF) } }
  }
}
