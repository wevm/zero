import Foundation
import Testing
import Zero

/// `AST`'s JSON through the API an app uses to compare its queries with a
/// server's: `Codable` reads the protocol's JSON, and `json` writes it back
/// as JS does, byte for byte.
@Suite(.enabled(if: Vectors.available))
struct ASTJSONTests {
  /// JS's `normalizeAST` writes every node's fields in a fixed order, as
  /// its builder does, so that equal queries have the same JSON. `json` of
  /// each normalized AST is that text, and the AST reads back as the same
  /// AST from its JSON text.
  @Test func writesTheProtocolJSONByteForByte() throws {
    var files = [
      ("ast", try Vectors.data("ast")), ("ast-features", try Vectors.data("ast-features")),
    ]
    if let generated = Vectors.generatedDirectory {
      files.append(
        ("ast-fuzz", try Data(contentsOf: generated.appendingPathComponent("ast-fuzz.json"))))
    }
    for (name, data) in files {
      let file = try JSONParser.parse(Array(data))
      let cases = (file["cases"]?.arrayValue ?? []).filter { $0["normalized"] != nil }
      let texts = Self.caseValues(data, key: "normalized")
      #expect(!cases.isEmpty && texts.count == cases.count, "\(name)")
      for (c, text) in zip(cases, texts) {
        let ast = try JSONDecoder().decode(AST.self, from: Data(text.utf8))
        #expect(try AST(json: c["normalized"]!).json.description == text, "\(name)")
        #expect(ast == (try AST(json: c["normalized"]!)), "\(name): \(text)")
        #expect(try JSONDecoder().decode(AST.self, from: JSONEncoder().encode(ast)) == ast)
        #expect(try JSONValueDecoder.decode(AST.self, from: c["ast"]!) == AST(json: c["ast"]!))
      }
    }
  }

  @Test func rejectsJSONThatIsNotAnAST() {
    #expect {
      try JSONDecoder().decode(AST.self, from: Data(#"{"table":"issue","limit":"ten"}"#.utf8))
    } throws: { error in
      guard case Swift.DecodingError.dataCorrupted(let context) = error else { return false }
      return context.debugDescription.contains("limit")
    }
  }

  /// The text of each case's `ast` as JS wrote it (`JSON.stringify`): the
  /// vector file holds `JSON.stringify(file, null, 2)`, which differs from
  /// it only by the whitespace between tokens.
  static func caseValues(_ data: Data, key name: String) -> [String] {
    var text: [UInt8] = []
    var inString = false
    var escaped = false
    for b in data {
      if inString {
        if escaped {
          escaped = false
        } else if b == UInt8(ascii: "\\") {
          escaped = true
        } else if b == UInt8(ascii: "\"") {
          inString = false
        }
      } else if b == UInt8(ascii: "\"") {
        inString = true
      } else if b == 0x20 || b == 0x0A || b == 0x0D || b == 0x09 {
        continue
      }
      text.append(b)
    }
    // The cases are the objects at depth 3: the file, `cases`, a case.
    let key = Array("\"\(name)\":".utf8)
    var asts: [String] = []
    var depth = 0
    var i = 0
    inString = false
    while i < text.count {
      let b = text[i]
      if inString {
        if b == UInt8(ascii: "\\") {
          i += 1
        } else if b == UInt8(ascii: "\"") {
          inString = false
        }
      } else if depth == 3, b == UInt8(ascii: "\""), text[i...].starts(with: key) {
        let start = i + key.count
        let end = Self.endOfValue(text, start)
        asts.append(String(decoding: text[start..<end], as: UTF8.self))
        i = end
        continue
      } else if b == UInt8(ascii: "\"") {
        inString = true
      } else if b == UInt8(ascii: "{") || b == UInt8(ascii: "[") {
        depth += 1
      } else if b == UInt8(ascii: "}") || b == UInt8(ascii: "]") {
        depth -= 1
      }
      i += 1
    }
    return asts
  }

  /// Where the object that starts at `start` ends.
  static func endOfValue(_ text: [UInt8], _ start: Int) -> Int {
    var depth = 0
    var inString = false
    var i = start
    repeat {
      let b = text[i]
      if inString {
        if b == UInt8(ascii: "\\") {
          i += 1
        } else if b == UInt8(ascii: "\"") {
          inString = false
        }
      } else if b == UInt8(ascii: "\"") {
        inString = true
      } else if b == UInt8(ascii: "{") || b == UInt8(ascii: "[") {
        depth += 1
      } else if b == UInt8(ascii: "}") || b == UInt8(ascii: "]") {
        depth -= 1
      }
      i += 1
    } while depth > 0
    return i
  }
}
