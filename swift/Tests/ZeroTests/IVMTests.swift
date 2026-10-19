import Testing

@testable import Zero

/// A smoke test of the query pipeline for builds without the conformance
/// vectors; `IVMConformanceTests` checks the engine against JS.
@Suite struct IVMTests {
  static let schema = ZeroSchema(
    tables: [
      TableSchema(
        name: "issue",
        columns: [("id", ColumnSchema(.string)), ("title", ColumnSchema(.string))],
        primaryKey: ["id"]),
      TableSchema(
        name: "comment",
        columns: [
          ("id", ColumnSchema(.string)), ("issueID", ColumnSchema(.string)),
          ("body", ColumnSchema(.string)),
        ],
        primaryKey: ["id"]),
    ],
    relationships: [
      "issue": [
        "comments": [
          RelationshipHop(
            sourceField: ["id"], destField: ["issueID"], destSchema: "comment", cardinality: .many)
        ]
      ]
    ])

  final class Delegate: BuilderDelegate {
    let sources: [String: MemorySource]
    init(_ sources: [String: MemorySource]) { self.sources = sources }
    func getSource(_ tableName: String) -> (any Source)? { sources[tableName] }
  }

  @Test func materializesAndUpdates() throws {
    let issues = MemorySource(Self.schema.tables["issue"]!)
    let comments = MemorySource(Self.schema.tables["comment"]!)
    for (id, title) in [("i1", "b"), ("i2", "a"), ("i3", "c")] {
      issues.push(.add(["id": .string(id), "title": .string(title)]))
    }
    comments.push(.add(["id": "c1", "issueID": "i2", "body": "x"]))

    let query = AnyQuery(Self.schema, "issue")
      .whereExists("comments")
      .related("comments") { $0.orderBy("id", .desc) }
      .orderBy("title", .asc)
      .limit(2)
    let delegate = Delegate(["issue": issues, "comment": comments])
    let view = ArrayView(
      try buildPipeline(query.ast, delegate, queryID: "q"), format: query.format, complete: true)
    var notified = 0
    _ = view.addListener { _, _ in notified += 1 }

    func titles() -> [String] { view.data.entries.map { $0.row["title"]!.stringValue! } }
    #expect(titles() == ["a"])

    // A comment on i1 makes it pass the EXISTS; it sorts after i2.
    comments.push(.add(["id": "c2", "issueID": "i1", "body": "y"]))
    view.flush()
    #expect(titles() == ["a", "b"])

    // i3 gets a comment, but the limit of 2 keeps it out.
    comments.push(.add(["id": "c3", "issueID": "i3", "body": "z"]))
    comments.push(.add(["id": "c4", "issueID": "i2", "body": "w"]))
    view.flush()
    #expect(titles() == ["a", "b"])
    let i2Comments = view.data.entries[0].relationships["comments"]!.entries
    #expect(i2Comments.map { $0.row["id"]!.stringValue! } == ["c4", "c1"])

    // Removing i2's comments drops it and lets i3 in.
    comments.push(.remove(["id": "c1", "issueID": "i2", "body": "x"]))
    comments.push(.remove(["id": "c4", "issueID": "i2", "body": "w"]))
    view.flush()
    #expect(titles() == ["b", "c"])
    #expect(notified > 0)
  }
}
