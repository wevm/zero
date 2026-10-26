import Testing

@testable import Zero

/// Replays the hardening IVM vectors from
/// `packages/zero-conformance/src/ivm/harden-ivm.test.ts` (written to
/// `generated/harden-ivm-*.json`): deep `related`/`whereExists` nesting,
/// junctions, `flip`/`scalar`, OR of EXISTS, batches of pushes flushed once,
/// and whether the view notified its listeners after each batch.
///
/// Numbers JSON can't carry are tagged as `{"$number": "NaN"}` and so on.
@Suite(.enabled(if: Vectors.available))
struct HardenIVMTests {
  /// Errors JS throws that Swift throws too, rather than trapping on.
  /// Errors JS throws from a push that Swift reports rather than trapping.
  static let pushErrors = [
    "Partitioned take does not allow unpartitioned fetches",
    "Singular relationship",
    "node does not exist",
    "old node does not exist",
  ]

  static let comparableErrors = [
    "not(exists()) is not supported on the client",
    "Partitioned take does not allow unpartitioned fetches",
    "Singular relationship",
  ]

  static func decode(_ v: JSONValue) -> JSONValue {
    switch v {
    case .array(let a): return .array(a.map(decode))
    case .object(let o):
      if o.count == 1, let s = o["$number"]?.stringValue {
        switch s {
        case "NaN": return .number(.nan)
        case "Infinity": return .number(.infinity)
        case "-Infinity": return .number(-.infinity)
        default: return .number(-0.0)
        }
      }
      return .object(o.mapValues(decode))
    default: return v
    }
  }

  static func encode(_ v: JSONValue) -> JSONValue {
    switch v {
    case .number(let n) where n.isNaN: return ["$number": "NaN"]
    case .number(let n) where n == .infinity: return ["$number": "Infinity"]
    case .number(let n) where n == -.infinity: return ["$number": "-Infinity"]
    case .number(let n) where n == 0 && n.sign == .minus: return ["$number": "-0"]
    case .array(let a): return .array(a.map(encode))
    case .object(let o): return .object(o.mapValues(encode))
    default: return v
    }
  }

  static func positiveZero(_ v: JSONValue) -> JSONValue {
    switch v {
    case .number(let n) where n == 0: return .number(0)
    case .array(let a): return .array(a.map(positiveZero))
    case .object(let o): return .object(o.mapValues(positiveZero))
    default: return v
    }
  }

  enum Outcome { case passed, skipped, failed(String) }

  static func run(_ c: JSONValue, schema: ZeroSchema, datasets: [JSONValue]) throws -> Outcome {
    let result = c["result"]!
    if let error = result["error"]?.stringValue,
      !comparableErrors.contains(where: { error.hasPrefix($0) })
        && !(result["batch"] != nil && pushErrors.contains(where: { error.hasPrefix($0) }))
    {
      // A JS assertion where Swift has a precondition: not compared.
      return .skipped
    }
    // JS threw from this batch's push: no steps were recorded.
    let failingBatch = result["batch"]?.numberValue.map { Int($0) }
    let dataset = datasets[Int(c["dataset"]!.numberValue!)]
    var sources: [String: MemorySource] = [:]
    for name in schema.tableNames {
      let source = MemorySource(schema.tables[name]!)
      for row in dataset["data"]?[name]?.arrayValue ?? [] {
        source.push(.add(Row(decode(row).objectValue!)))
      }
      sources[name] = source
    }
    let built = IVMConformanceTests.build(schema, decode(c["program"]!))
    // With -0 read as 0: JS's `start` keeps 0 where Swift keeps -0, which
    // JSON (and so the hash and the server) can't tell apart.
    if positiveZero(built.ast.json) != positiveZero(decode(c["ast"]!)) {
      return .failed("builder ast\n  swift: \(encode(built.ast.json))\n  js:    \(c["ast"]!)")
    }
    let format = try Format(json: c["format"]!)
    if built.format != format {
      return .failed("builder format\n  swift: \(built.format.json)\n  js:    \(c["format"]!)")
    }
    let recorder: IVMConformanceTests.Recorder
    let view: ArrayView
    var notified = 0
    do {
      let input = try buildPipeline(built.ast, IVMConformanceTests.Delegate(sources), queryID: "q")
      recorder = IVMConformanceTests.Recorder(input)
      view = try PipelineFailure.capture { ArrayView(recorder, format: format, complete: true) }
      view.addListener { _, _ in notified += 1 }
    } catch {
      if let expected = result["error"]?.stringValue {
        return "\(error)" == expected ? .passed : .failed("error \(error) != \(expected)")
      }
      return .failed("unexpected error \(error)")
    }
    if let expected = result["error"], failingBatch == nil {
      return .failed("expected error \(expected)")
    }
    func viewJSON() -> JSONValue { encode(view.data.json ?? .null) }
    // `ViewSnapshot`: applying the difference to the previous IDs gives the
    // new ones, and `updated` is the rows in both whose JSON changed.
    var differ = RowDiffer(primaryKey: recorder.getSchema().primaryKey)
    var previous = differ.next(view.data, .complete, nil)
    func checkSnapshot() -> String? {
      let next = differ.next(view.data, .complete, nil)
      defer { previous = next }
      guard previous.ids.applying(next.difference) == next.ids else {
        return "snapshot difference: \(previous.ids) + \(next.difference) != \(next.ids)"
      }
      let old = Dictionary(
        previous.data.entries.map { (RowID($0.row, primaryKey: differ.primaryKey), $0.json) },
        uniquingKeysWith: { a, _ in a })
      var updated = Set<RowID>()
      for e in next.data.entries {
        let id = RowID(e.row, primaryKey: differ.primaryKey)
        if let o = old[id], o != e.json { updated.insert(id) }
      }
      return updated == next.updated
        ? nil : "snapshot updated: \(next.updated) != \(updated)"
    }
    if failingBatch == nil, viewJSON() != result["initial"]! {
      return .failed("initial view\n  swift: \(viewJSON())\n  js:    \(result["initial"]!)")
    }
    let steps = result["steps"]?.arrayValue ?? []
    for (i, batch) in dataset["batches"]!.arrayValue!.enumerated() {
      recorder.changes.removeAll()
      notified = 0
      do {
        // As `QueryContext.processChanges` pushes.
        try PipelineFailure.capture {
          // As `IVMBranch.apply`, which stops at a failed push.
          for push in batch.arrayValue! where !PipelineFailure.hasFailed {
            let (table, change) = IVMConformanceTests.sourceChange(decode(push))
            sources[table]!.push(change)
          }
        }
      } catch {
        if i == failingBatch, "\(error)" == result["error"]!.stringValue! { return .passed }
        return .failed("batch \(i): unexpected error \(error); js: \(result)")
      }
      if i == failingBatch { return .failed("batch \(i): expected error \(result["error"]!)") }
      view.flush()
      // Steps were recorded only when no push threw.
      if failingBatch != nil { continue }
      let step = steps[i]
      let changes = encode(.array(recorder.changes))
      if changes != step["changes"]! {
        return .failed(
          "batch \(i) \(batch) changes\n  swift: \(changes)\n  js:    \(step["changes"]!)")
      }
      if viewJSON() != step["data"]! {
        return .failed(
          "batch \(i) \(batch) view\n  swift: \(viewJSON())\n  js:    \(step["data"]!)")
      }
      if let mismatch = checkSnapshot() { return .failed("batch \(i): \(mismatch)") }
      if .bool(notified > 0) != step["notified"]! {
        return .failed(
          "batch \(i) \(batch) notified: swift \(notified > 0), js \(step["notified"]!)")
      }
    }
    return .passed
  }

  static func runAll(_ file: JSONValue, maxReported: Int = 10) throws {
    let shared = file["shared"]!
    let schema = try ZeroSchema(json: shared["schema"]!)
    let datasets = shared["datasets"]!.arrayValue!
    var failures = 0
    var passed = 0
    var skipped = 0
    for c in file["cases"]!.arrayValue! {
      switch try run(c, schema: schema, datasets: datasets) {
      case .passed: passed += 1
      case .skipped: skipped += 1
      case .failed(let mismatch):
        failures += 1
        if failures <= maxReported {
          Issue.record("\(c["name"]!.stringValue!): \(c["program"]!)\n\(mismatch)")
        }
      }
    }
    print("[harden ivm] \(passed) passed, \(failures) failed, \(skipped) skipped")
    #expect(failures == 0, "\(failures) cases failed")
  }

  @Test(.enabled(if: Vectors.generatedAvailable)) func hardenFuzz() throws {
    try Self.runAll(try Vectors.generatedFile("harden-ivm-fuzz"))
  }

  @Test(.enabled(if: Vectors.generatedAvailable)) func hardenSpecialNumbers() throws {
    try Self.runAll(try Vectors.generatedFile("harden-ivm-special"))
  }
}
