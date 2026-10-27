@testable import Zero

/// The named queries and mutators of the client scenarios, the same as
/// `packages/zero-conformance/src/client/app.ts`. Arguments stay JSON, as
/// the scenarios give them.
enum ClientApp {
  static let schema = ZeroSchema.test

  static func zql(_ table: String) -> AnyQuery { AnyQuery(schema, table) }

  static let queries: [String: @Sendable (JSONValue?) -> AnyQuery] = [
    "issues.all": { _ in zql("issue").orderBy("id", .asc) },
    "issues.open": { _ in
      zql("issue")
        .where("closed", false)
        .related("comments") { $0.orderBy("id", .asc) }
        .orderBy("createdAt", .asc)
    },
    "issues.byOwner": { args in zql("issue").where("ownerId", args ?? .null).orderBy("id", .asc) },
    "issues.byID": { args in zql("issue").where("id", args ?? .null).related("owner").one() },
    "issues.firstTwo": { _ in zql("issue").orderBy("title", .asc).limit(2) },
    "issues.withAuthoredComments": { _ in
      zql("issue")
        .whereExists("comments") { $0.whereExists("author", flip: true) { $0.limit(3) } }
        .orderBy("id", .asc)
    },
    "issues.withOneComment": { _ in zql("issue").related("oneComment").orderBy("id", .asc) },
    "issues.withFirstComment": { _ in
      zql("issue")
        .whereExists("comments", flip: true) { $0.orderBy("id", .asc).limit(1) }
        .orderBy("id", .asc)
    },
    "users.all": { _ in zql("user").orderBy("id", .asc) },
    "issueLabels.all": { _ in
      zql("issueLabel").orderBy("issueId", .asc).orderBy("labelId", .asc)
    },
  ]

  /// The named query `name` applied to `args` (undefined when nil).
  static func request(_ name: String, _ args: JSONValue?) throws -> AnyQuery {
    guard let build = queries[name] else { throw Failure("Unknown name \(name)") }
    return build(args).nameAndArgs(name, args.map { [$0] } ?? [])
  }

  struct Failure: Error, CustomStringConvertible {
    let description: String
    init(_ description: String) { self.description = description }
  }

  /// A mutator over JSON arguments.
  struct JSONMutator: Mutator {
    let name: String
    let body: @MainActor @Sendable (Transaction, JSONValue?) throws -> JSONValue?

    @MainActor
    func run(_ tx: Transaction, args: JSONValue?, context: Void) throws -> JSONValue? {
      try body(tx, args)
    }
  }

  /// `args[key] ?? fallback`: JS `??` treats null like a missing key.
  static func field(_ args: JSONValue?, _ key: String, _ fallback: JSONValue) -> JSONValue {
    guard let v = args?[key], !v.isNull else { return fallback }
    return v
  }

  static func issueRow(_ args: JSONValue?) -> Row {
    var row: Row = [
      "title": field(args, "title", ""),
      "description": field(args, "description", ""),
      "closed": field(args, "closed", false),
      "ownerId": field(args, "ownerId", .null),
      "createdAt": field(args, "createdAt", 1),
    ]
    if let id = args?["id"] { row["id"] = id }
    return row
  }

  static let mutators: [JSONMutator] = [
    JSONMutator(name: "issue.create") { tx, args in
      try tx.mutate.issue.insert(issueRow(args))
      return nil
    },
    JSONMutator(name: "issue.upsert") { tx, args in
      try tx.mutate.issue.upsert(issueRow(args))
      return nil
    },
    JSONMutator(name: "issue.update") { tx, args in
      try tx.mutate.issue.update(Row(args?.objectValue ?? [:]))
      return nil
    },
    JSONMutator(name: "issue.delete") { tx, args in
      try tx.mutate.issue.delete(["id": args ?? .null])
      return nil
    },
    JSONMutator(name: "issue.close") { tx, args in
      try tx.mutate.issue.update(["id": args ?? .null, "closed": true])
      return nil
    },
    // Reads through the transaction, so a rebase can change what it writes.
    JSONMutator(name: "issue.closeAllOpen") { tx, _ in
      let open = try tx.run(zql("issue").where("closed", false))
      for issue in open.entries {
        try tx.mutate.issue.update(["id": issue.row["id"] ?? .null, "closed": true])
      }
      return nil
    },
    // Throws on a rebase that finds the issue already there.
    JSONMutator(name: "issue.createUnique") { tx, args in
      let existing = try tx.run(zql("issue").where("id", args?["id"] ?? .null).one())
      if !existing.entries.isEmpty {
        throw ApplicationError("issue exists", details: ["id": field(args, "id", .null)])
      }
      try tx.mutate.issue.insert(issueRow(args))
      return nil
    },
    // Returns data to the caller of the optimistic run.
    JSONMutator(name: "issue.createReturning") { tx, args in
      try tx.mutate.issue.insert(issueRow(args))
      return ["created": field(args, "id", .null)]
    },
    JSONMutator(name: "issue.fail") { _, args in
      throw Failure(args?.stringValue ?? "undefined")
    },
    // Writes and then throws, so nothing it wrote may stay.
    JSONMutator(name: "issue.createThenFail") { tx, args in
      try tx.mutate.issue.insert(issueRow(args))
      throw ApplicationError("failed after writing")
    },
    // Runs CRUD operations on any table, then throws, reads tables back, or
    // returns `return` (`harden.ops` in app.ts).
    JSONMutator(name: "harden.ops") { tx, args in
      if args?["failOnRebase"] == true, tx.reason == .rebase {
        throw ApplicationError("fails on rebase")
      }
      for o in args?["ops"]?.arrayValue ?? [] {
        let table = tx.mutate.table(o["table"]?.stringValue ?? "")
        let value = Row(o["value"]?.objectValue ?? [:])
        switch o["op"]?.stringValue {
        case "insert": try table.insert(value)
        case "upsert": try table.upsert(value)
        case "update": try table.update(value)
        case "delete": try table.delete(value)
        default: throw Failure("Unknown op")
        }
      }
      if let fail = args?["fail"]?.stringValue {
        throw ApplicationError(fail, details: args?["details"])
      }
      if let read = args?["read"]?.arrayValue {
        var out: JSONObject = [:]
        for t in read.compactMap(\.stringValue) { out[t] = try tx.run(zql(t)).json ?? .null }
        return ["mutationID": .number(Double(tx.mutationID)), "read": .object(out)]
      }
      return args?["return"]
    },
    JSONMutator(name: "comment.add") { tx, args in
      var row: Row = [
        "authorId": field(args, "authorId", "u1"),
        "text": field(args, "text", ""),
        "createdAt": field(args, "createdAt", 1),
      ]
      if let id = args?["id"] { row["id"] = id }
      if let issueID = args?["issueId"] { row["issueId"] = issueID }
      try tx.mutate.comment.insert(row)
      return nil
    },
  ]
}
