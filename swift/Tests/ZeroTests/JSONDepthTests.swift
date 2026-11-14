import Foundation
import Testing

@testable import Zero

/// JSON nested as deep as the parser accepts doesn't overflow the stack of a
/// thread with 512 KB, the size of a test or cooperative thread on macOS
/// (the iPhone main thread has 1 MB), in a debug build either.
struct JSONDepthTests {
  static let depth = JSONParser.maxDepth

  /// Arrays, objects, and both by turns with a sibling at every level, each
  /// with a twin whose innermost value differs.
  static let texts: [(text: String, other: String)] = {
    let d = depth
    func alternating(_ innermost: String) -> String {
      var open = "", close = ""
      for level in 1..<d {
        if level % 2 == 1 {
          open += #"[1,"#
          close = "]" + close
        } else {
          open += #"{"a":true,"b":"#
          close = "}" + close
        }
      }
      return open + innermost + close
    }
    return [
      (
        String(repeating: "[", count: d) + String(repeating: "]", count: d),
        String(repeating: "[", count: d - 1) + "[0]" + String(repeating: "]", count: d - 1)
      ),
      (
        String(repeating: #"{"a":"#, count: d - 1) + "{}" + String(repeating: "}", count: d - 1),
        String(repeating: #"{"a":"#, count: d - 1) + #"{"a":0}"#
          + String(repeating: "}", count: d - 1)
      ),
      (alternating("[]"), alternating("[null]")),
    ]
  }()

  @Test func deepestValuesOnSmallStack() async throws {
    let results = try await onThread(stackSize: 512 << 10) {
      var results: [Bool] = []
      for (text, otherText) in Self.texts {
        var value = try JSONParser.parse(text)
        var copy = try JSONParser.parse(text)
        var other = try JSONParser.parse(otherText)
        results.append(JSONSerializer.stringify(value) == text)
        results.append(value == copy)
        results.append(value != other)
        results.append(value.hashValue == copy.hashValue)
        results.append(
          QueryHash.hashOfNameAndArgs("q", [value]) == QueryHash.hashOfNameAndArgs("q", [copy]))
        // Freed in a row, in an object, and on its own; and shared, so the
        // second row frees it.
        var row = Row(["v": value])
        let row2 = row
        row = Row()
        _ = row2
        var object = JSONObject()
        object["v"] = copy
        copy = .null
        object = JSONObject()
        JSONValue.discard(&value)
        JSONValue.discard(&other)
      }
      return results
    }
    #expect(results == Array(repeating: true, count: 15))
  }

  @Test func deeperIsRejectedOnSmallStack() async throws {
    let (arrays, objects, mixed) = (Self.texts[0].text, Self.texts[1].text, Self.texts[2].text)
    let cases = [
      // One more level.
      ("[" + arrays + "]", "JSON nested too deeply", Self.depth),
      (#"{"a":"# + objects + "}", "JSON nested too deeply", Self.depth * 5),
      // Errors after the deepest values, which are freed.
      (arrays.dropLast() + ",", "Unexpected end of JSON input", arrays.utf8.count),
      (mixed.dropLast() + ",x", "Unexpected token", mixed.utf8.count),
      (arrays + " x", "Unexpected non-whitespace character after JSON", arrays.utf8.count + 1),
    ]
    let errors = try await onThread(stackSize: 512 << 10) {
      cases.map { text, _, _ -> JSONParser.Error? in
        do throws(JSONParser.Error) {
          _ = try JSONParser.parse(text)
          return nil
        } catch {
          return error
        }
      }
    }
    #expect(errors == cases.map { JSONParser.Error(message: $0.1, offset: $0.2) })
  }

  @Test func arrayUniqueness() {
    // Not a literal, which can be a constant that is never freed.
    var a = (0..<2).map { JSONValue.number(Double($0)) }
    let alone = a.isUniquelyReferenced()
    var b = a
    let shared = [a.isUniquelyReferenced(), b.isUniquelyReferenced()]
    b.append(3)
    let apart = a.isUniquelyReferenced()
    #expect(alone)
    #expect(shared == [false, false])
    #expect(apart)
  }

  /// Runs `body` on a thread with a stack of `stackSize` bytes.
  func onThread<T: Sendable>(
    stackSize: Int, _ body: @escaping @Sendable () throws -> T
  ) async throws -> T {
    try await withCheckedThrowingContinuation { (done: CheckedContinuation<T, any Error>) in
      let thread = Thread { done.resume(with: Result { try body() }) }
      thread.stackSize = stackSize
      thread.start()
    }
  }
}
