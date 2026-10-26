import NIOCore
import NIOPosix

#if canImport(Glibc)
  import Glibc
#elseif canImport(Darwin)
  import Darwin
#endif

/// A TCP proxy in front of zero-cache that the chaos tests steer: it can
/// refuse connections, accept them and never answer, complete the
/// WebSocket upgrade and then go silent, stall or throttle what the server
/// sends, and cut connections, abruptly or partway through a frame.
///
/// Everything runs on one event loop of its own, so the knobs are plain
/// state on that loop and each change is applied in order.
final class ChaosProxy: @unchecked Sendable {
  enum Accept {
    /// Forward to zero-cache.
    case pass
    /// Close the connection at once.
    case refuse
    /// Accept, read, and never answer.
    case blackhole
    /// Forward the HTTP upgrade both ways, then drop everything the server
    /// sends: an open WebSocket that never says `connected`.
    case silentAfterUpgrade
  }

  /// The knobs, read on the proxy's loop.
  struct Knobs {
    var accept: Accept = .pass
    /// Bytes per second from the server to the client; nil for no limit,
    /// 0 to stall.
    var downstreamRate: Int?
    /// Holds what the client sends.
    var stallUpstream = false
    /// Cuts each connection once the server has sent this many bytes on it:
    /// those bytes reach the client, then the connection closes abruptly.
    var cutAfterDownstreamBytes: Int?
  }

  let port: Int
  private let group: MultiThreadedEventLoopGroup
  private let loop: any EventLoop
  private let server: any Channel
  private let upstreamHost: String
  private let upstreamPort: Int
  // Confined to `loop`.
  private var knobs = Knobs()
  private var links: [ObjectIdentifier: Link] = [:]
  private var acceptedCount = 0
  private var ticker: RepeatedTask?

  /// Proxies `127.0.0.1:<port>` to the zero-cache at `upstream` (a `ws://`
  /// or `http://` origin).
  init(upstream: String) async throws {
    let rest = upstream.split(separator: "://").last.map(String.init) ?? upstream
    let authority = rest.split(separator: "/").first.map(String.init) ?? rest
    let parts = authority.split(separator: ":")
    upstreamHost = String(parts[0])
    upstreamPort = parts.count > 1 ? Int(parts[1])! : 80
    group = MultiThreadedEventLoopGroup(numberOfThreads: 1)
    loop = group.next()
    let box = ProxyBox()
    server = try await ServerBootstrap(group: group)
      .serverChannelOption(.socketOption(.so_reuseaddr), value: 1)
      .childChannelInitializer { channel in
        channel.eventLoop.makeCompletedFuture {
          try box.proxy!.accepted(channel)
        }
      }
      .bind(host: "127.0.0.1", port: 0).get()
    port = server.localAddress!.port!
    box.proxy = self
    ticker = loop.scheduleRepeatedTask(initialDelay: .milliseconds(10), delay: .milliseconds(10)) {
      [unowned self] _ in
      for link in self.links.values { link.pump(tick: true) }
    }
  }

  /// The origin a client connects to, `http://127.0.0.1:<port>`.
  var cacheURL: String { "http://127.0.0.1:\(port)" }

  /// Changes the knobs, and applies them to the open connections.
  func set(_ change: @escaping @Sendable (inout Knobs) -> Void) async {
    try? await loop.submit {
      change(&self.knobs)
      for link in self.links.values { link.pump(tick: false) }
    }.get()
  }

  /// Closes every open connection abruptly, without a WebSocket close.
  func cutAll() async {
    try? await loop.submit {
      for link in self.links.values { link.close(abrupt: true) }
    }.get()
  }

  /// The connections open now.
  var openConnections: Int {
    get async { (try? await loop.submit { self.links.count }.get()) ?? -1 }
  }

  /// The connections accepted so far.
  var accepted: Int {
    get async { (try? await loop.submit { self.acceptedCount }.get()) ?? -1 }
  }

  func shutdown() async {
    try? await loop.submit {
      self.ticker?.cancel()
      for link in self.links.values { link.close(abrupt: true) }
    }.get()
    try? await server.close()
    try? await group.shutdownGracefully()
  }

  // MARK: - On the loop

  fileprivate func accepted(_ client: any Channel) throws {
    acceptedCount += 1
    switch knobs.accept {
    case .refuse:
      client.close(promise: nil)
      return
    case .blackhole:
      let link = Link(proxy: self, client: client, silent: .all)
      links[ObjectIdentifier(link)] = link
      try client.pipeline.syncOperations.addHandler(ClientSide(link))
    case .pass, .silentAfterUpgrade:
      let link = Link(
        proxy: self, client: client,
        silent: knobs.accept == .silentAfterUpgrade ? .afterHeaders : .none)
      links[ObjectIdentifier(link)] = link
      try client.pipeline.syncOperations.addHandler(ClientSide(link))
      ClientBootstrap(group: loop)
        .channelInitializer { channel in
          channel.eventLoop.makeCompletedFuture {
            try channel.pipeline.syncOperations.addHandler(ServerSide(link))
          }
        }
        .connect(host: upstreamHost, port: upstreamPort)
        .whenComplete { result in
          switch result {
          case .success(let channel): link.serverConnected(channel)
          case .failure: link.close(abrupt: true)
          }
        }
    }
  }

  fileprivate func removed(_ link: Link) {
    links[ObjectIdentifier(link)] = nil
  }

  fileprivate var currentKnobs: Knobs { knobs }

  /// One proxied connection.
  fileprivate final class Link {
    enum Silence { case none, afterHeaders, all }

    unowned let proxy: ChaosProxy
    let client: any Channel
    var server: (any Channel)?
    var silent: Silence
    var headerTail: [UInt8] = []
    var headersDone = false
    var up: [ByteBuffer] = []
    var down: [ByteBuffer] = []
    var downSent = 0
    var budget = 0
    var closed = false
    var serverGone = false

    init(proxy: ChaosProxy, client: any Channel, silent: Silence) {
      self.proxy = proxy
      self.client = client
      self.silent = silent
    }

    func serverConnected(_ channel: any Channel) {
      if closed {
        channel.close(promise: nil)
        return
      }
      server = channel
      pump(tick: false)
    }

    func fromClient(_ buffer: ByteBuffer) {
      guard silent != .all else { return }
      up.append(buffer)
      pump(tick: false)
    }

    func fromServer(_ buffer: ByteBuffer) {
      var buffer = buffer
      switch silent {
      case .all: return
      case .afterHeaders:
        if headersDone { return }
        // Forward up to the end of the upgrade response's headers.
        let bytes = buffer.readableBytesView
        var end: Int?
        for (i, b) in bytes.enumerated() {
          headerTail.append(b)
          if headerTail.count > 4 { headerTail.removeFirst() }
          if headerTail == [13, 10, 13, 10] {
            end = i + 1
            break
          }
        }
        if let end {
          headersDone = true
          buffer = buffer.getSlice(at: buffer.readerIndex, length: end)!
        }
      case .none: break
      }
      down.append(buffer)
      pump(tick: false)
    }

    func pump(tick: Bool) {
      guard !closed else { return }
      let knobs = proxy.currentKnobs
      if let server, !knobs.stallUpstream, !up.isEmpty {
        for b in up { server.write(b, promise: nil) }
        up = []
        server.flush()
      }
      if let rate = knobs.downstreamRate {
        if tick { budget = min(budget + max(rate / 100, 0), max(rate, 1)) }
      } else {
        budget = Int.max
      }
      var wrote = false
      while !down.isEmpty, budget > 0 {
        var b = down.removeFirst()
        var n = min(b.readableBytes, budget)
        if let cut = knobs.cutAfterDownstreamBytes { n = min(n, max(cut - downSent, 0)) }
        if n < b.readableBytes {
          let head = b.readSlice(length: n)!
          down.insert(b, at: 0)
          b = head
        }
        if b.readableBytes > 0 {
          client.write(b, promise: nil)
          wrote = true
        }
        downSent += n
        if knobs.downstreamRate != nil { budget -= n }
        if let cut = knobs.cutAfterDownstreamBytes, downSent >= cut {
          client.flush()
          close(abrupt: true)
          return
        }
      }
      if wrote { client.flush() }
      if serverGone, down.isEmpty { close(abrupt: false) }
    }

    func close(abrupt: Bool) {
      guard !closed else { return }
      closed = true
      proxy.removed(self)
      for channel in [client, server].compactMap({ $0 }) {
        if abrupt, let options = channel as? any SocketOptionProvider {
          // A reset rather than an orderly shutdown.
          _ = options.setSoLinger(linger(l_onoff: 1, l_linger: 0))
        }
        channel.close(promise: nil)
      }
    }
  }

  private final class ClientSide: ChannelInboundHandler {
    typealias InboundIn = ByteBuffer
    let link: Link
    init(_ link: Link) { self.link = link }
    func channelRead(context: ChannelHandlerContext, data: NIOAny) {
      link.fromClient(unwrapInboundIn(data))
    }
    func channelInactive(context: ChannelHandlerContext) { link.close(abrupt: false) }
    func errorCaught(context: ChannelHandlerContext, error: any Error) {
      link.close(abrupt: true)
    }
  }

  private final class ServerSide: ChannelInboundHandler {
    typealias InboundIn = ByteBuffer
    let link: Link
    init(_ link: Link) { self.link = link }
    func channelRead(context: ChannelHandlerContext, data: NIOAny) {
      link.fromServer(unwrapInboundIn(data))
    }
    func channelInactive(context: ChannelHandlerContext) {
      // The server went away: deliver what it sent, then close.
      link.serverGone = true
      link.pump(tick: false)
    }
    func errorCaught(context: ChannelHandlerContext, error: any Error) {
      link.close(abrupt: true)
    }
  }
}

private final class ProxyBox: @unchecked Sendable {
  var proxy: ChaosProxy?
}
