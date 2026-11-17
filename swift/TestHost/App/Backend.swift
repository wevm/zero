import Foundation
import Observation
import QuartzCore
import ZeroTesting
import ZeroVirtual

/// A client of a `LoopbackServer` holding a long list of issues and a chat
/// of messages of many lengths.
@MainActor
final class Backend {
  let server = LoopbackServer()
  let zero: Zero<Void>
  private(set) var messageCount: Int

  init(issues: Int = 2000, messages: Int = 200) {
    // `-messages <n>`: a longer chat.
    let messages = max(messages, UserDefaults.standard.integer(forKey: "messages"))
    messageCount = messages
    // `-latency <ms>`: the server that far away, as a real one is.
    let latency = UserDefaults.standard.double(forKey: "latency")
    server.put("issues", (0..<issues).map(Self.issue))
    server.put("comments", (0..<messages).map { Self.message($0) })
    // `-storage-key <key>`: the client keeps its rows in SQLite under the
    // key, from one launch to the next, as an app's client does.
    let options = ZeroOptions(
      cacheURL: "http://loopback.test", userID: "u",
      storageKey: Self.storageKey ?? UUID().uuidString,
      storage: Self.storageKey == nil ? .memory : .sqlite())
    zero = try! Zero(
      schema: .test, options: options,
      socketFactory: latency > 0 ? DelayedSockets(server, seconds: latency / 1000) : server)
  }

  static let storageKey = UserDefaults.standard.string(forKey: "storage-key")

  static func issueID(_ i: Int) -> String { "i" + String(format: "%04d", i) }
  static func messageID(_ i: Int) -> String { "m" + String(format: "%04d", i) }

  private static func issue(_ i: Int) -> [String: JSONValue] {
    [
      "id": .string(issueID(i)), "title": .string("Issue \(i)"),
      "description": .string(words(i % 5 == 0 ? 30 : 0, seed: i)), "closed": .bool(i % 3 == 0),
      "owner_id": .null, "createdAt": .number(Double(i)),
    ]
  }

  /// `-tall-messages 1`: one message in 9 runs to 150 to 400 words, taller
  /// than the screen.
  static let tallMessages = UserDefaults.standard.bool(forKey: "tall-messages")

  /// Message `i`: 1 to 60 words, so rows are many heights.
  static func message(_ i: Int, words count: Int? = nil) -> [String: JSONValue] {
    let n = tallMessages && i % 9 == 4 ? 150 + i * 31 % 251 : i * 7919 % 60 + 1
    return [
      "id": .string(messageID(i)), "authorId": .string(["ana", "ben", "you"][i % 3]),
      "issue_id": "chat", "text": .string("\(i): " + words(count ?? n, seed: i)),
      "createdAt": .number(Double(1_000 + i)),
    ]
  }

  private static func words(_ n: Int, seed: Int) -> String {
    let lorem = "lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor"
      .split(separator: " ")
    return (0..<n).map { String(lorem[($0 + seed) % lorem.count]) }.joined(separator: " ")
  }

  /// Another user's message arrives.
  func receive() {
    server.put("comments", Self.message(messageCount))
    messageCount += 1
  }

  private var removedIssues = 0

  /// Another user deletes the first issue left.
  func removeFirstIssue() {
    server.delete("issues", ["id": .string(Self.issueID(removedIssues))])
    removedIssues += 1
  }

  /// The last message is edited to be much longer.
  func growLast() {
    server.put("comments", Self.message(messageCount - 1, words: 120))
  }
}

/// A socket factory whose sockets deliver what they receive `seconds` late,
/// in order: a server's round trip, from a server in the process.
@MainActor
final class DelayedSockets: SocketFactory {
  private let base: any SocketFactory
  private let seconds: Double
  /// The sockets' events: a socket holds its own weakly.
  private var events: [Delayed] = []

  init(_ base: any SocketFactory, seconds: Double) {
    self.base = base
    self.seconds = seconds
  }

  func open(url: String, secProtocol: String?, events: any SocketEvents) -> any SocketConnection {
    let delayed = Delayed(events, seconds: seconds)
    self.events.append(delayed)
    return base.open(url: url, secProtocol: secProtocol, events: delayed)
  }

  private final class Delayed: SocketEvents {
    weak var events: (any SocketEvents)?
    let seconds: Double
    private var queue: [(due: Double, deliver: () -> Void)] = []

    init(_ events: any SocketEvents, seconds: Double) {
      self.events = events
      self.seconds = seconds
    }

    func socketDidOpen() { later { $0.socketDidOpen() } }
    func socketDidReceive(_ message: SocketMessage) { later { $0.socketDidReceive(message) } }
    func socketDidClose(code: Int, reason: String, wasClean: Bool) {
      later { $0.socketDidClose(code: code, reason: reason, wasClean: wasClean) }
    }

    private func later(_ deliver: @escaping (any SocketEvents) -> Void) {
      queue.append(
        (
          CACurrentMediaTime() + seconds,
          { [weak self] in
            if let events = self?.events { deliver(events) }
          }
        ))
      if queue.count == 1 { wait() }
    }

    private func wait() {
      guard let due = queue.first?.due else { return }
      Task { @MainActor [weak self] in
        try? await Task.sleep(for: .seconds(max(0, due - CACurrentMediaTime())))
        self?.drain()
      }
    }

    private func drain() {
      let now = CACurrentMediaTime()
      while let first = queue.first, first.due <= now {
        queue.removeFirst()
        first.deliver()
      }
      wait()
    }
  }
}

/// Where a chat page starts: the columns it orders by.
struct ChatStart: Equatable, Sendable, Codable {
  var createdAt: Double
  var id: String
}

enum ChatQueries {
  struct Page: Codable, Sendable {
    var limit: Int
    var start: ChatStart?
    var backward: Bool
  }

  static let page = defineQuery("chatPage") { (a: Page) in
    let dir: AST.Direction = a.backward ? .desc : .asc
    var q = testZQL.comment.where(\.issueId, "chat").orderBy(\.createdAt, dir).orderBy(\.id, dir)
    if let s = a.start { q = q.start([\.createdAt: s.createdAt, \.id: s.id]) }
    return q.limit(a.limit)
  }
  static let message = defineQuery("chatMessage") { (id: String) in
    testZQL.comment.where(\.issueId, "chat").where(\.id, id).one()
  }
  static let latest = defineQuery("chatLatest") {
    testZQL.comment.where(\.issueId, "chat").orderBy(\.createdAt, .desc).orderBy(\.id, .desc).one()
  }
}

enum IssueQueries {
  struct Page: Codable, Sendable {
    var limit: Int
    var start: String?
    var backward: Bool
  }

  static let page = defineQuery("issuePage") { (a: Page) in
    var q = testZQL.issue.orderBy(\.id, a.backward ? .desc : .asc)
    if let s = a.start { q = q.start([\.id: s]) }
    return q.limit(a.limit)
  }
  static let issue = defineQuery("issueByID") { (id: String) in
    testZQL.issue.where(\.id, id).one()
  }
}

typealias ChatList = ZeroVirtualizer<Void, TestComment, String, ChatStart, String>
typealias IssueList = ZeroVirtualizer<Void, TestIssue, String, String, String>

extension Backend {
  /// The chat, where it was when the app last had it open, when the
  /// client keeps its rows (`-storage-key`); and whether it was.
  func chatList() -> (list: ChatList, restored: Bool) {
    let saved = Self.storageKey.map { "chat-scroll-" + $0 }
    let state = saved.flatMap { UserDefaults.standard.data(forKey: $0) }.flatMap {
      try? JSONDecoder().decode(ChatList.ScrollState.self, from: $0)
    }
    let list = ChatList(
      zero: zero, listContext: "chat", estimatedRowHeight: 80, rowKey: \.id,
      toStartRow: { ChatStart(createdAt: $0.createdAt, id: $0.id) },
      pageQuery: { r, _ in
        ChatQueries.page(.init(limit: r.limit, start: r.start, backward: r.direction == .backward))
      },
      rowQuery: { r, _ in ChatQueries.message(r.id) },
      scrollState: state, minPageSize: 20, stickToBottom: true)
    if let saved {
      list.onScrollStateChange = { state in
        UserDefaults.standard.set(try? JSONEncoder().encode(state), forKey: saved)
      }
    }
    return (list, state != nil)
  }

  func issueList() -> IssueList {
    IssueList(
      zero: zero, listContext: "issues", estimatedRowHeight: 44, rowKey: \.id,
      toStartRow: \.id,
      pageQuery: { r, _ in
        IssueQueries.page(.init(limit: r.limit, start: r.start, backward: r.direction == .backward))
      },
      rowQuery: { r, _ in IssueQueries.issue(r.id) })
  }
}
