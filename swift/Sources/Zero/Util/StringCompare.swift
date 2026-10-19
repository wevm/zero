/// The two string orders Zero uses.
///
/// - UTF-8 byte order (code point order) is what `compare-utf8` and SQLite
///   use. Row values, primary keys, and AST normalization sort this way.
/// - UTF-16 code unit order is what JS `<` and `Array#sort` use. Object keys
///   are sorted this way before hashing.
///
/// Both differ from Swift's `String <`, which compares by Unicode canonical
/// equivalence.
public enum StringCompare {
  /// `compareUTF8(a, b)`: negative, zero, or positive.
  @inlinable
  public static func utf8(_ a: String, _ b: String) -> Int {
    // No `a == b` shortcut: Swift's `==` equates canonically equivalent
    // strings ("é" and "e\u{301}"), which are distinct in Zero.
    var ai = a.utf8.makeIterator()
    var bi = b.utf8.makeIterator()
    while true {
      switch (ai.next(), bi.next()) {
      case (nil, nil): return 0
      case (nil, _): return -1
      case (_, nil): return 1
      case let (x?, y?):
        if x != y { return x < y ? -1 : 1 }
      }
    }
  }

  @inlinable
  public static func utf8Less(_ a: String, _ b: String) -> Bool { utf8(a, b) < 0 }

  /// JS `a < b ? -1 : a > b ? 1 : 0` for strings.
  @inlinable
  public static func utf16(_ a: String, _ b: String) -> Int {
    var ai = a.utf16.makeIterator()
    var bi = b.utf16.makeIterator()
    while true {
      switch (ai.next(), bi.next()) {
      case (nil, nil): return 0
      case (nil, _): return -1
      case (_, nil): return 1
      case let (x?, y?):
        if x != y { return x < y ? -1 : 1 }
      }
    }
  }

  @inlinable
  public static func utf16Less(_ a: String, _ b: String) -> Bool { utf16(a, b) < 0 }

  /// Code unit equality, as JS `===` compares strings.
  @inlinable
  public static func equal(_ a: String, _ b: String) -> Bool {
    a.utf8.elementsEqual(b.utf8)
  }
}
