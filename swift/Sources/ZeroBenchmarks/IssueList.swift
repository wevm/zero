import Foundation
import Zero
import ZeroTesting

/// Changes to zbugs' issue list after its first sync: pokes and mutations
/// that go through its joins (labels, creator, view state), its
/// `whereExists` (the project), and its `limit`.
enum IssueList {
  /// The rows of the first sync, as JSON objects, by table.
  @MainActor static let rows: [String: [JSONObject]] = {
    var out: [String: [JSONObject]] = [:]
    for text in FirstSync.rows() {
      let op = (try? JSONParser.parse(text))?.objectValue
      guard let table = op?["tableName"]?.stringValue, let value = op?["value"]?.objectValue
      else { continue }
      out[table, default: []].append(value)
    }
    return out
  }()

  /// The issues the list can show: open and public, newest change first.
  @MainActor static let listed: [JSONObject] = rows["issue"]!.filter {
    $0["open"] == .bool(true) && $0["visibility"] == .string("public")
  }

  /// Sets an issue's `title`, or its `modified`, which moves it in the list.
  struct UpdateIssue: Codable, Sendable {
    var id: String
    var title: String?
    var modified: Double?
  }

  static let updateIssue = defineMutator("updateIssue") { (tx, args: UpdateIssue) in
    var row: [String: JSONValue] = ["id": .string(args.id)]
    if let title = args.title { row["title"] = .string(title) }
    if let modified = args.modified { row["modified"] = .number(modified) }
    try tx.mutate.issue.update(row)
  }

  /// A `rowsPatch` put of `value` into `table`.
  static func put(_ table: String, _ value: JSONObject) -> String {
    #"{"op":"put","tableName":"\#(table)","value":"#
      + JSONSerializer.stringify(.object(value)) + "}"
  }

  /// A `rowsPatch` del of the row of `table` with key `id`.
  static func del(_ table: String, _ id: JSONObject) -> String {
    #"{"op":"del","tableName":"\#(table)","id":"# + JSONSerializer.stringify(.object(id)) + "}"
  }

  /// A connected client after zbugs' first sync, on `storage`, with the
  /// issue list's first page.
  @MainActor static func client(
    storage: ZeroStorage = .memory
  ) throws -> (
    Client, MaterializedView
  ) {
    let client = try Client(
      rows: 0, connected: true, schema: FirstSync.schema, mutators: [updateIssue],
      storage: storage)
    let view = try client.zero.materialize(FirstSync.issueList)
    client.receive(client.nextPoke(rows: FirstSync.rows()))
    check(view.data.entries.count == FirstSync.pageSize + 1, "a page of issues")
    return (client, view)
  }
}

extension Benchmarks {
  /// Pokes and mutations into zbugs' issue list, after its first sync.
  static func issueList(_ bench: Bench) throws {
    try bench.group("issue list") {
      let (client, view) = try IssueList.client()
      let listed = IssueList.listed
      var n = 0
      // Newer than every synced change, so a moved issue goes to the top.
      func now() -> Double {
        n += 1
        return 1_770_000_000_000 + Double(n)
      }
      @MainActor func pageIDs() -> [String] {
        view.data.entries.map { $0.row["id"]!.stringValue! }
      }
      @MainActor func issue(_ id: String) -> JSONObject {
        listed.first { $0["id"]!.stringValue == id }!
      }

      bench.run(
        "poke: 1 issue's title, in the page",
        setup: { () -> [String] in
          let ids = pageIDs()
          var row = issue(ids[n % ids.count])
          row["title"] = .string("Retitled \(now())")
          return client.nextPoke(rows: [IssueList.put("issue", row)])
        }
      ) { frames in
        client.receive(frames)
      }

      bench.run(
        "poke: 1 issue modified, to the top",
        setup: { () -> [String] in
          var row = listed[n % listed.count]
          row["modified"] = .number(now())
          return client.nextPoke(rows: [IssueList.put("issue", row)])
        }
      ) { frames in
        client.receive(frames)
      }
      check(pageIDs().count == FirstSync.pageSize + 1, "still a page")

      // Adds a label to an issue in the page, then removes it.
      let labels = IssueList.rows["label"]!
      let projectID = IssueList.rows["project"]![0]["id"]!
      var added: JSONObject?
      bench.run(
        "poke: add or remove 1 label of an issue in the page",
        setup: { () -> [String] in
          if let key = added {
            added = nil
            return client.nextPoke(rows: [IssueList.del("issueLabel", key)])
          }
          let ids = pageIDs()
          let key: JSONObject = [
            "labelID": labels[n % labels.count]["id"]!,
            "issueID": .string(ids[(n / labels.count) % ids.count]),
          ]
          _ = now()
          var row = key
          row["projectID"] = projectID
          added = key
          return client.nextPoke(rows: [IssueList.put("issueLabel", row)])
        }
      ) { frames in
        client.receive(frames)
      }
      if let key = added {
        client.receive(client.nextPoke(rows: [IssueList.del("issueLabel", key)]))
      }

      bench.run(
        "poke: 50 issues modified",
        setup: { () -> [String] in
          client.nextPoke(
            rows: (0..<50).map { i in
              var row = listed[(n * 50 + i * 7) % listed.count]
              row["modified"] = .number(now())
              return IssueList.put("issue", row)
            })
        }
      ) { frames in
        client.receive(frames)
      }
      check(pageIDs().count == FirstSync.pageSize + 1, "still a page")

      // Mutations stay pending: the client doesn't run its timers, so it
      // never pushes them.
      bench.run("optimistic update: 1 issue's title, in the page") {
        let ids = pageIDs()
        let id = ids[n % ids.count]
        client.zero.mutate(IssueList.updateIssue(.init(id: id, title: "Retitled \(now())")))
      }
      bench.run("optimistic update: 1 issue modified, to the top") {
        let id = listed[n % listed.count]["id"]!.stringValue!
        client.zero.mutate(IssueList.updateIssue(.init(id: id, modified: now())))
      }
      check(pageIDs().count == FirstSync.pageSize + 1, "still a page")
      withExtendedLifetime(view) {}
    }
  }

  /// The client on SQLite storage, as apps run it: writes go to a
  /// background queue, so a benchmark "and its write" waits for them too.
  static func sqlite(_ bench: Bench) throws {
    try bench.group("sqlite") {
      let directory = NSTemporaryDirectory() + "zero-bench-\(UUID().uuidString)"
      try FileManager.default.createDirectory(
        atPath: directory, withIntermediateDirectories: true)
      defer { try? FileManager.default.removeItem(atPath: directory) }
      let storage = ZeroStorage.sqlite(directory: directory)

      let rows = FirstSync.rows()
      for flush in [false, true] {
        let name =
          "first sync, issueListV2 view, \(rows.count) rows" + (flush ? ", and its write" : "")
        guard bench.wants(name) else { continue }
        bench.run(
          name,
          setup: { () -> (Client, MaterializedView, [String])? in
            guard
              let c = try? Client(
                rows: 0, connected: true, schema: FirstSync.schema, mutators: [], storage: storage),
              let view = try? c.zero.materialize(FirstSync.issueList)
            else { return nil }
            return (c, view, c.nextPoke(rows: rows))
          }
        ) { state in
          guard let (client, view, frames) = state else { return check(false, "a client") }
          client.receive(frames)
          if flush { client.zero.flushStorage() }
          check(view.data.entries.count == FirstSync.pageSize + 1, "a page of issues")
        }
      }

      let n = 1000
      let client = try Client(rows: n, connected: true, storage: storage)
      let view = try client.zero.materialize(users)
      var value = n
      for k in [1, 100] {
        bench.run(
          "poke: put \(k) changed rows, \(n)-row view, and its write",
          setup: { () -> [String] in
            value += 1
            return client.nextPoke(rows: (0..<k).map { User.json($0, value) })
          }
        ) { frames in
          client.receive(frames)
          client.zero.flushStorage()
        }
      }
      let local = try Client(rows: n, storage: storage)
      let localView = try local.zero.materialize(users)
      bench.run("optimistic update, \(n)-row view, and its write") {
        value += 1
        local.zero.mutate(Mutators.updateUser(UpdateUser(a: value % n, value: value)))
        local.zero.flushStorage()
      }
      withExtendedLifetime((view, localView)) {}

      // Opening a client on a stored replica, as an app launches: the
      // open, the read, loading the sources, and the issue list's page.
      if bench.wants("open") {
        let options = try { () -> ZeroOptions in
          let (seed, _) = try IssueList.client(storage: storage)
          seed.zero.flushStorage()
          defer { seed.zero.close() }
          return seed.options
        }()
        final class Holder { var zero: Zero<Void>? }
        bench.run(
          "open: Zero.init and the issue list's page, \(rows.count)-row replica",
          setup: { Holder() }
        ) { holder in
          let zero = try? Zero(
            schema: FirstSync.schema, mutators: [IssueList.updateIssue], options: options,
            scheduler: ManualScheduler(now: 1_700_000_000_000), socketFactory: nil)
          let view = try? zero?.materialize(FirstSync.issueList)
          check(view?.data.entries.count == FirstSync.pageSize + 1, "a page of issues")
          holder.zero = zero
        }
      }
    }
  }
}
