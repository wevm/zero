import Testing

@testable import Zero

/// Differential fuzz of the JSON parser and serializer, the downstream
/// message decoder, and the binary poke chunk decoder, against the vectors of
/// `packages/zero-conformance/src/primitives/harden-json.test.ts`: mutated
/// inputs must be accepted and rejected as JS does, without trapping.
@Suite(.enabled(if: Vectors.available))
struct HardenJSONTests {
  /// One case: `JSON.parse` acceptance, and `JSON.stringify` of the result.
  static func checkJSON(_ c: JSONValue) {
    let input = c["input"]!.stringValue!
    let ok = c["ok"]!.boolValue!
    var value: JSONValue
    do throws(JSONParser.Error) {
      value = try JSONParser.parse(input)
    } catch {
      #expect(!ok, "rejected \(String(reflecting: input)): \(error)")
      return
    }
    // The vectors hold arrays thousands of levels deep, which only an
    // object or a row frees without recursion.
    defer { JSONValue.discard(&value) }
    #expect(ok, "accepted \(String(reflecting: input))")
    guard ok else { return }
    let expected = Self.replacingLoneSurrogateEscapes(c["stringify"]!.stringValue!)
    let actual = JSONSerializer.stringify(value)
    #expect(
      actual.utf16.elementsEqual(expected),
      "\(String(reflecting: input)): Swift \(actual.debugDescription), JS \(String(decoding: expected, as: UTF16.self).debugDescription)"
    )
  }

  /// `JSON.stringify` output with each escaped lone surrogate (`\ud800`),
  /// which a Swift `String` can't hold, as the U+FFFD the parser reads it
  /// as (`JSONParser`).
  static func replacingLoneSurrogateEscapes(_ s: String) -> [UInt16] {
    let units = Array(s.utf16)
    var out: [UInt16] = []
    var i = 0
    while i < units.count {
      guard units[i] == 0x5C, i + 1 < units.count else {
        out.append(units[i])
        i += 1
        continue
      }
      if units[i + 1] == 0x75, i + 6 <= units.count,
        let unit = UInt16(String(decoding: units[i + 2..<i + 6], as: UTF16.self), radix: 16),
        (0xD800...0xDFFF).contains(unit)
      {
        out.append(0xFFFD)
        i += 6
      } else {
        out += units[i...i + 1]
        i += 2
      }
    }
    return out
  }

  /// On the test's thread, 512 KB on macOS.
  @Test func json() throws {
    for c in try Vectors.cases("harden-json") { Self.checkJSON(c) }
  }

  @Test(.enabled(if: Vectors.generatedAvailable)) func jsonFuzz() throws {
    for c in try Vectors.generatedCases("harden-json-fuzz") { Self.checkJSON(c) }
  }

  /// Downstream messages edited as text: accepted when `JSON.parse` and
  /// `downstreamSchema` accept them.
  @Test(.enabled(if: Vectors.generatedAvailable)) func downstreamText() throws {
    for c in try Vectors.generatedCases("harden-downstream-text") {
      let text = c["text"]!.stringValue!
      let ok = c["ok"]!.boolValue!
      do {
        _ = try Downstream(text: text)
        #expect(ok, "accepted \(String(reflecting: text))")
      } catch {
        #expect(!ok, "rejected \(String(reflecting: text)): \(error)")
      }
    }
  }

  /// Binary poke chunks edited byte by byte: accepted when `TextDecoder`,
  /// `JSON.parse`, and `pokePartsSchema` accept them.
  @Test(.enabled(if: Vectors.generatedAvailable)) func pokeBytes() throws {
    for c in try Vectors.generatedCases("harden-poke-bytes") {
      let messages = c["messages"]!.arrayValue!.map {
        ProtocolConformanceTests.bytes(hex: $0.stringValue!)
      }
      let ok = c["ok"]!.boolValue!
      do {
        var payloads: [ArraySlice<UInt8>] = []
        for m in messages { payloads.append(try PokeChunk.payload(ofBinaryMessage: m)) }
        _ = try PokeChunk.decode(payloads)
        #expect(ok, "accepted \(c["messages"]!)")
      } catch {
        #expect(!ok, "rejected \(c["messages"]!): \(error)")
      }
    }
  }
}
