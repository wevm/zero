import Testing
import ZeroTesting

@testable import Zero

/// A client over `TransportSocketFactory`, which parses frames off the main
/// actor: they still apply in order, as the engine applies frames it parses
/// itself.
@Suite @MainActor struct TransportSocketTests {
  /// A socket whose frames the test sends, and that ends when they run out.
  final class FrameSocket: Socket {
    let frames: AsyncStream<SocketMessage>
    let input: AsyncStream<SocketMessage>.Continuation

    init() { (frames, input) = AsyncStream.makeStream() }

    func send(_ message: SocketMessage) async throws {}
    func receive() async throws -> SocketMessage? {
      for await frame in frames { return frame }
      return nil
    }
    func close(code: Int, reason: String) async { input.finish() }
  }

  struct FrameTransport: Transport {
    let socket = FrameSocket()
    func connect(url: String, secProtocol: String?) async throws -> any Socket { socket }
  }

  let scheduler = ManualScheduler(now: 1_700_000_000_000)
  let transport = FrameTransport()

  func makeZero() throws -> Zero<Void> {
    var options = ZeroOptions(
      cacheURL: "http://zero.test", userID: "u", storageKey: randomID(), storage: .memory)
    options.logSink = RecordingLogSink()
    return try Zero(
      schema: .test, options: options, scheduler: scheduler,
      socketFactory: TransportSocketFactory(transport))
  }

  /// Runs timers until `done`, letting the socket's frames arrive.
  func wait(until done: () -> Bool) async throws {
    for _ in 0..<1_000 where !done() {
      try await Task.sleep(for: .milliseconds(2))
      scheduler.runDueTimers()
    }
    #expect(done())
  }

  static func issue(_ id: String) -> String {
    #"{"op":"put","tableName":"issues","value":{"id":"\#(id)","title":"t","description":"","#
      + #""closed":false,"owner_id":null,"createdAt":1}}"#
  }

  @Test func appliesTextAndBinaryPokesInOrder() async throws {
    let zero = try makeZero()
    defer { zero.close() }
    let view = try zero.materialize(AnyQuery(.test, "issue"))
    defer { view.destroy() }
    scheduler.runDueTimers()
    transport.socket.input.yield(.text(#"["connected",{"wsid":"w1"}]"#))
    try await wait { zero.connection.state == .connected }

    let input = transport.socket.input
    input.yield(.text(#"["pokeStart",{"pokeID":"p1","baseCookie":null}]"#))
    input.yield(.text(#"["pokePart",{"pokeID":"p1","rowsPatch":[\#(Self.issue("i1"))]}]"#))
    input.yield(.text(#"["pokeEnd",{"pokeID":"p1","cookie":"01"}]"#))
    // A poke in binary chunks, split inside a multi-byte character.
    let patches = Array(
      #"[{"pokeID":"p2","rowsPatch":[\#(Self.issue("i2é")),\#(Self.issue("i3"))]}]"#.utf8)
    let split = patches.firstIndex(of: 0xC3)! + 1
    input.yield(.text(#"["pokeStart",{"pokeID":"p2","baseCookie":"01"}]"#))
    input.yield(.binary([PokeChunk.messageType] + patches[..<split]))
    input.yield(.binary([PokeChunk.messageType] + patches[split...]))
    input.yield(.text(#"["pokeEnd",{"pokeID":"p2","cookie":"02"}]"#))
    try await wait { view.data.entries.count == 3 }
    #expect(view.data.entries.map { $0.json["id"] } == ["i1", "i2é", "i3"])

    // A frame that doesn't parse ends the connection as one parsed on the
    // main actor does.
    input.yield(.text(#"["pokeStart",{"pokeID":"p3"}"#))
    try await wait { zero.connection.state.status == .error }
    guard case .error(let reason) = zero.connection.state else { return }
    #expect(reason.hasPrefix("Invalid message received from server: "))
    #expect(reason.hasSuffix(#"["pokeStart",{"pokeID":"p3"}"#))
  }
}
