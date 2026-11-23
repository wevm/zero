/// Reads the parts of a poke with each synced row straight into its table's
/// layout, the row the replica and the sources keep, instead of into a
/// JSON object the replica then turns into one.
///
/// It reads a `put` as zero-cache writes it, `{"op":"put","tableName":…,
/// "value":{…}}`, when the value's columns are all the table's; any other
/// op it reads as the decoder does. When the text is anything but a poke
/// part that decodes, it gives up (nil), and the decoder reads the text
/// from the start, with its errors.
struct PokeRowReader {
  /// The layout of each table's rows, by the table's name on the server,
  /// for the tables whose columns have the same names on the server.
  private let layouts: [String: RowLayout]

  init(_ serverToClient: NameMapper, layouts: [String: RowLayout]) {
    var byServerName: [String: RowLayout] = [:]
    for (serverName, table) in serverToClient.tables
    where table.allColumnsSame && !isAggregateTableName(serverName) {
      byServerName[serverName] = layouts[table.tableName]
    }
    self.layouts = byServerName
  }

  /// The body of the `pokePart` message `text` (`Downstream(text:)`).
  func pokePart(_ text: String) -> PokePartBody? {
    guard text.utf8.starts(with: #"["pokePart","#.utf8) else { return nil }
    var text = text
    return text.withUTF8 { buffer in
      var r = JSONParser.Reader(buffer)
      do {
        guard try r.open("["), try r.string() != nil, r.read(","),
          let part = try part(&r), r.close("]"), r.atEnd()
        else { return nil }
        return part
      } catch {
        return nil
      }
    }
  }

  /// The parts of a poke's binary chunks (`PokeChunk.decode`).
  func parts(_ chunks: [ArraySlice<UInt8>]) -> [PokePartBody]? {
    var bytes: [UInt8] = []
    for chunk in chunks { bytes.append(contentsOf: chunk) }
    let payload = bytes.starts(with: utf8BOM) ? bytes[3...] : bytes[...]
    guard var text = String(validating: payload, as: UTF8.self) else { return nil }
    return text.withUTF8 { buffer in
      var r = JSONParser.Reader(buffer)
      do {
        guard try r.open("[") else { return nil }
        var parts: [PokePartBody] = []
        if !r.close("]") {
          repeat {
            guard let part = try part(&r) else { return nil }
            parts.append(part)
          } while r.read(",")
          guard r.close("]") else { return nil }
        }
        return r.atEnd() ? parts : nil
      } catch {
        return nil
      }
    }
  }

  /// A part's body: its `rowsPatch` read here, the rest by the decoder.
  private func part(_ r: inout JSONParser.Reader) throws(JSONParser.Error) -> PokePartBody? {
    guard try r.open("{") else { return nil }
    var rest = JSONObject()
    var rows: RowsPatch?
    if !r.close("}") {
      repeat {
        // A key given twice keeps its last value, as it does in an object.
        let key = try r.key()
        if StringCompare.equal(key, "rowsPatch") {
          guard let ops = try rowsPatch(&r) else { return nil }
          rows = ops
        } else {
          rest[key] = try r.value()
        }
      } while r.read(",")
      guard r.close("}") else { return nil }
    }
    var json = JSONValue.object(rest)
    defer { JSONValue.discard(&json) }
    guard var part = try? PokePartBody(json: json) else { return nil }
    part.rowsPatch = rows
    return part
  }

  private func rowsPatch(_ r: inout JSONParser.Reader) throws(JSONParser.Error) -> RowsPatch? {
    guard try r.open("[") else { return nil }
    var ops: RowsPatch = []
    if r.close("]") { return ops }
    repeat {
      guard let op = try rowPatchOp(&r) else { return nil }
      ops.append(op)
    } while r.read(",")
    return r.close("]") ? ops : nil
  }

  /// A `put` of a row whose columns are all its table's, in its layout;
  /// any other op as the decoder reads it.
  private func rowPatchOp(_ r: inout JSONParser.Reader) throws(JSONParser.Error) -> RowPatchOp? {
    let start = r.mark
    if try r.open("{"), try StringCompare.equal(r.key(), "op"),
      let op = try r.string(), StringCompare.equal(op, "put"), r.read(","),
      try StringCompare.equal(r.key(), "tableName"), let table = try r.string(), r.read(","),
      try StringCompare.equal(r.key(), "value"), let layout = layouts[table],
      let row = try r.row(layout), r.close("}")
    {
      return .putRow(tableName: table, row: row)
    }
    r.reset(to: start)
    var json = try r.value()
    defer { JSONValue.discard(&json) }
    return try? RowPatchOp(json: json)
  }
}
