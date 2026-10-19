/// A value that doesn't fit the Swift type a row declares for it.
public struct ZeroValueError: Error, Sendable, Hashable, CustomStringConvertible {
  public var description: String

  public init(_ description: String) {
    self.description = description
  }

  static func mismatch(_ expected: String, _ value: JSONValue) -> ZeroValueError {
    ZeroValueError("expected \(expected), got \(value.typeName) \(value)")
  }
}

/// A Swift type a column holds. Generated row structs use these for their
/// columns; the query builder encodes compared values with them.
public protocol ZeroValue: Sendable {
  /// Reads a value from a row; a missing column reads as `.null`.
  init(zeroValue: JSONValue) throws(ZeroValueError)
  var zeroValue: JSONValue { get }
}

extension String: ZeroValue {
  public init(zeroValue: JSONValue) throws(ZeroValueError) {
    guard case .string(let s) = zeroValue else { throw .mismatch("a string", zeroValue) }
    self = s
  }

  public var zeroValue: JSONValue { .string(self) }
}

extension Double: ZeroValue {
  public init(zeroValue: JSONValue) throws(ZeroValueError) {
    guard case .number(let n) = zeroValue else { throw .mismatch("a number", zeroValue) }
    self = n
  }

  public var zeroValue: JSONValue { .number(self) }
}

/// For a `number()` column the codegen config declares an integer: the
/// value must be exact, which every integer up to 2^53 is.
extension Int: ZeroValue {
  public init(zeroValue: JSONValue) throws(ZeroValueError) {
    guard case .number(let n) = zeroValue, let i = Int(exactly: n),
      abs(i) <= 1 << 53
    else { throw .mismatch("an integer", zeroValue) }
    self = i
  }

  public var zeroValue: JSONValue { .number(Double(self)) }
}

extension Bool: ZeroValue {
  public init(zeroValue: JSONValue) throws(ZeroValueError) {
    guard case .bool(let b) = zeroValue else { throw .mismatch("a boolean", zeroValue) }
    self = b
  }

  public var zeroValue: JSONValue { .bool(self) }
}

extension JSONValue: ZeroValue {
  public init(zeroValue: JSONValue) { self = zeroValue }
  public var zeroValue: JSONValue { self }
}

/// An optional column: `.null` reads as nil.
extension Optional: ZeroValue where Wrapped: ZeroValue {
  public init(zeroValue: JSONValue) throws(ZeroValueError) {
    self = zeroValue == .null ? nil : try Wrapped(zeroValue: zeroValue)
  }

  public var zeroValue: JSONValue { self?.zeroValue ?? .null }
}

/// The JSON arrays a `json()` column can hold.
extension Array: ZeroValue where Element: ZeroValue {
  public init(zeroValue: JSONValue) throws(ZeroValueError) {
    guard case .array(let a) = zeroValue else { throw .mismatch("an array", zeroValue) }
    var out: [Element] = []
    out.reserveCapacity(a.count)
    for e in a { out.append(try Element(zeroValue: e)) }
    self = out
  }

  public var zeroValue: JSONValue { .array(map(\.zeroValue)) }
}

/// A string enumeration (`enumeration<…>()`), generated as a `String` enum.
public protocol ZeroEnum: ZeroValue, RawRepresentable where RawValue == String {}

extension ZeroEnum {
  public init(zeroValue: JSONValue) throws(ZeroValueError) {
    guard case .string(let s) = zeroValue, let v = Self(rawValue: s) else {
      throw .mismatch("one of the cases of \(Self.self)", zeroValue)
    }
    self = v
  }

  public var zeroValue: JSONValue { .string(rawValue) }
}

/// Reads the fields of a JSON object, for generated `json<T>()` types.
public struct ZeroObjectReader: Sendable {
  public let object: [String: JSONValue]

  public init(_ value: JSONValue, _ type: String) throws(ZeroValueError) {
    guard case .object(let o) = value else { throw .mismatch("a \(type) object", value) }
    object = o
  }

  /// A field; a missing one reads as `.null`, so only an optional takes it.
  public func field<V: ZeroValue>(_ name: String) throws(ZeroValueError) -> V {
    do {
      return try V(zeroValue: object[name] ?? .null)
    } catch {
      throw ZeroValueError("\(name): \(error.description)")
    }
  }
}

/// Writes the fields of a JSON object.
public struct ZeroObjectWriter: Sendable {
  public private(set) var object: [String: JSONValue] = [:]

  public init() {}

  public mutating func set(_ name: String, _ value: some ZeroValue) {
    object[name] = value.zeroValue
  }

  /// An optional field (`name?: T`): nil leaves it out.
  public mutating func setIfPresent<V: ZeroValue>(_ name: String, _ value: V?) {
    if let value { object[name] = value.zeroValue }
  }

  public var value: JSONValue { .object(object) }
}

/// Stores a one relationship of a row struct out of line, so two tables can
/// relate to each other (`Issue.oneComment` and `Comment.issue`) without
/// their structs containing each other.
@propertyWrapper
public struct Indirect<Value: Sendable>: Sendable {
  private var storage: [Value]

  public init(wrappedValue: Value?) {
    storage = wrappedValue.map { [$0] } ?? []
  }

  public var wrappedValue: Value? {
    get { storage.first }
    set { storage = newValue.map { [$0] } ?? [] }
  }
}

extension Indirect: Equatable where Value: Equatable {}
extension Indirect: Hashable where Value: Hashable {}

extension Indirect: Codable where Value: Codable {
  public init(from decoder: any Decoder) throws {
    let c = try decoder.singleValueContainer()
    self.init(wrappedValue: c.decodeNil() ? nil : try c.decode(Value.self))
  }

  public func encode(to encoder: any Encoder) throws {
    var c = encoder.singleValueContainer()
    if let v = wrappedValue { try c.encode(v) } else { try c.encodeNil() }
  }
}

extension KeyedDecodingContainer {
  /// Lets a row decode without the relationships it wasn't queried with.
  public func decode<V: Codable>(_ type: Indirect<V>.Type, forKey key: Key) throws -> Indirect<V> {
    try decodeIfPresent(type, forKey: key) ?? Indirect(wrappedValue: nil)
  }
}

extension KeyedEncodingContainer {
  public mutating func encode<V: Codable>(_ value: Indirect<V>, forKey key: Key) throws {
    try encodeIfPresent(value.wrappedValue, forKey: key)
  }
}
