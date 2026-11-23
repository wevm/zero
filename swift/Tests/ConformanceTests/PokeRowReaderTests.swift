import Testing

@testable import Zero

/// The client reads pokes with `PokeRowReader` before the decoders. On the
/// downstream and poke-chunk vectors, it must read a message only when the
/// decoder accepts it, and then as the same parts.
@Suite(.enabled(if: Vectors.available))
struct PokeRowReaderTests {
  /// Part bodies the vectors don't have: keys out of zero-cache's order,
  /// given twice, escaped, or not the table's; and ones the decoder
  /// rejects.
  static let bodies = [
    #"{"pokeID":"p","rowsPatch":[{"op":"put","tableName":"t","value":{"id":"a","j":1}}]}"#,
    #"{"pokeID":"p","rowsPatch":[{"tableName":"t","op":"put","value":{"id":"a"}}]}"#,
    #"{"pokeID":"p","rowsPatch":[{"op":"put","tableName":"t","value":{"id":"a","id":"b"}}]}"#,
    #"{"pokeID":"p","rowsPatch":[{"op":"put","tableName":"t","value":{"id":"a"},"value":{"id":"b"}}]}"#,
    #"{"pokeID":"p","rowsPatch":[{"op":"put","tableName":"t","value":{"id":"a"}}],"rowsPatch":[]}"#,
    #"{"pokeID":"p","pokeID":"q","rowsPatch":[]}"#,
    #"{"pokeID":"p","rowsPatch":[{"op":"put","tableName":"t","value":{"\u0069d":"a","j":{"k":[1]}},"x":0}]}"#,
    #"{"pokeID":"p","rowsPatch":[{"op":"put","tableName":"t","value":{"id":"a","x":1}}]}"#,
    #"{"pokeID":"p","rowsPatch":[{"op":"put","tableName":"\u0074","value":{"id":"a"}}]}"#,
    #"{"pokeID":"p","rowsPatch":[{"op":"put","tableName":"t","value":{}}]}"#,
    #"{"pokeID":"p","rowsPatch":[{"op":"put","tableName":"t","value":[]}]}"#,
    #"{"pokeID":"p","rowsPatch":[{"op":"put","tableName":"t"}]}"#,
    #"{"rowsPatch":[{"op":"put","tableName":"t","value":{"id":"a"}}]}"#,
    #"{"pokeID":"p","rowsPatch":null}"#,
    #"{"pokeID":"p","rowsPatch":[{"op":"put","tableName":"t","value":{"id":"a",}}]}"#,
    #"{"pokeID":"p","rowsPatch":[{"op":"put","tableName":"t","value":{"id":"a"}]}"#,
  ]

  static func texts() throws -> [String] {
    var cases = try Vectors.cases("protocol-downstream")
    if Vectors.generatedAvailable {
      cases += try Vectors.generatedCases("protocol-downstream-fuzz")
      cases += try Vectors.generatedCases("harden-downstream-text")
    }
    let texts = try cases.map { c in
      if let text = c["text"]?.stringValue ?? c["messageText"]?.stringValue { return text }
      return JSONSerializer.stringify(try ProtocolConformanceTests.messageAndOutput(c).0)
    }
    var edges: [String] = []
    for body in bodies {
      let text = #"["pokePart","# + body
      edges += [text + "]", text + "] ", text + "]x", text + ",1]"]
    }
    return texts + edges
  }

  static func chunks() throws -> [[ArraySlice<UInt8>]] {
    var cases = try Vectors.cases("protocol-poke-chunks")
    if Vectors.generatedAvailable {
      cases += try Vectors.generatedCases("protocol-poke-chunks-fuzz")
      cases += try Vectors.generatedCases("harden-poke-bytes")
    }
    let chunks = cases.compactMap { c in
      try? c["messages"]!.arrayValue!.map {
        try PokeChunk.payload(ofBinaryMessage: ProtocolConformanceTests.bytes(hex: $0.stringValue!))
      }
    }
    var edges: [[ArraySlice<UInt8>]] = []
    for body in bodies {
      let payloads = ["[" + body + "]", "[" + body + "," + body + "]", "[" + body + "]x"]
      edges += (payloads + ["\u{FEFF}" + payloads[0]]).map { [ArraySlice($0.utf8)] }
    }
    return chunks + edges
  }

  /// A reader with a table for each table the parts put rows in, its
  /// columns in `order`.
  static func reader(_ parts: [PokePartBody], _ order: ([String]) -> [String]) -> PokeRowReader {
    var columns: [String: Set<String>] = [:]
    for part in parts {
      for case .put(let table, let value) in part.rowsPatch ?? [] {
        columns[table, default: []].formUnion(value.keys)
      }
    }
    var tables: [TableSchema] = []
    var layouts: [String: RowLayout] = [:]
    for (table, names) in columns {
      let names = order(names.sorted(by: StringCompare.utf16Less))
      guard let first = names.first else { continue }
      tables.append(
        TableSchema(
          name: table, columns: names.map { ($0, ColumnSchema(.json)) }, primaryKey: [first]))
      layouts[table] = RowLayout(names)
    }
    return PokeRowReader(NameMapper.serverToClient(ZeroSchema(tables: tables)), layouts: layouts)
  }

  @Test func readsAsTheDecodersDo() throws {
    let texts = try Self.texts()
    let chunks = try Self.chunks()
    var parts: [PokePartBody] = []
    for text in texts {
      if case .pokePart(let part) = try? Downstream(text: text) { parts.append(part) }
    }
    for c in chunks { parts += (try? PokeChunk.decode(c)) ?? [] }
    var rowsRead = 0
    // Every column, in and out of the order zero-cache sends them; and
    // half of them, so that rows with others are left to the decoder.
    for reader in [
      Self.reader(parts) { $0 }, Self.reader(parts) { $0.reversed() },
      Self.reader(parts) { Array($0.prefix(($0.count + 1) / 2)) },
    ] {
      for text in texts {
        let decoded = try? Downstream(text: text)
        let read = reader.pokePart(text)
        guard case .pokePart(let want) = decoded else {
          #expect(read == nil, "read what the decoder rejects: \(text)")
          continue
        }
        guard let read else { continue }
        #expect(read.json == want.json, "\(text)")
        for case .putRow in read.rowsPatch ?? [] { rowsRead += 1 }
      }
      for c in chunks {
        let decoded = try? PokeChunk.decode(c)
        let read = reader.parts(c)
        guard let want = decoded else {
          #expect(read == nil, "read chunks the decoder rejects: \(c)")
          continue
        }
        guard let read else { continue }
        #expect(JSONValue.array(read.map(\.json)) == JSONValue.array(want.map(\.json)), "\(c)")
      }
    }
    #expect(rowsRead > 0, "the reader read rows into their layouts")
  }

  /// A row value as deep as the parser takes, on a thread with the stack
  /// of a test or cooperative thread on macOS (`SweepWireTests`); one level
  /// more is left to the decoder, which rejects it.
  @Test func deepRowValue() async throws {
    func poke(_ depth: Int) -> String {
      #"["pokePart",{"pokeID":"p","rowsPatch":[{"op":"put","tableName":"t","value":{"id":"x","j":"#
        + String(repeating: "[", count: depth) + String(repeating: "]", count: depth) + "}}]}]"
    }
    let text = poke(JSONParser.maxDepth - 5)
    let deeper = poke(JSONParser.maxDepth - 4)
    let results = try await SweepWireTests.onThread(stackSize: 512 << 10) {
      let schema = ZeroSchema(tables: [
        TableSchema(
          name: "t", columns: [("id", ColumnSchema(.string)), ("j", ColumnSchema(.json))],
          primaryKey: ["id"])
      ])
      let reader = PokeRowReader(
        NameMapper.serverToClient(schema), layouts: IVMBranch.layouts(schema))
      guard let read = reader.pokePart(text), case .pokePart(let want) = try Downstream(text: text)
      else { return [false] }
      var isRow = false
      if case .putRow = read.rowsPatch?.first { isRow = true }
      return [isRow, read.json == want.json, reader.pokePart(deeper) == nil]
    }
    #expect(results == [true, true, true])
  }
}
