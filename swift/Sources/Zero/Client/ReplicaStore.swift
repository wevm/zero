/// A value of the replica: JSON, or a table's row as its source holds it.
/// The rows under `e/` are kept as rows in their table's layout, the one
/// the IVM sources use, so the replica and the sources share each row
/// instead of keeping it both as a JSON object and as a source row.
enum KVValue: Sendable {
  case json(JSONValue)
  case row(Row)

  /// `value` as the replica keeps it under `key`: an object under `e/` of a
  /// table in `layouts` as a row in its layout, anything else as JSON.
  init(_ value: JSONValue, at key: String, layouts: [String: RowLayout]) {
    if case .object(let object) = value, key.utf8.starts(with: Keys.entitiesPrefix.utf8),
      let layout = layouts[Keys.sourceName(key)]
    {
      self = .row(Row(object, layout: layout))
    } else {
      self = .json(value)
    }
  }

  /// The value as JSON: a row as the object of its columns.
  var json: JSONValue {
    switch self {
    case .json(let value): value
    case .row(let row): JSONValue(row)
    }
  }

  /// The row of an object's columns in `layout`, with any others after
  /// them: the row itself when it's in `layout`. Nil for JSON that isn't an
  /// object.
  func row(in layout: RowLayout) -> Row? {
    switch self {
    case .json(let value): value.objectValue.map { Row($0, layout: layout) }
    case .row(let row): row.conformed(to: layout)
    }
  }

  /// The row a source takes for the value: `row(in:)`, and a row of no
  /// columns for JSON that isn't an object.
  func sourceRow(_ layout: RowLayout) -> Row {
    row(in: layout) ?? Row(JSONObject(), layout: layout)
  }
}

extension KVValue: Equatable {
  /// As the values' JSON compares: the same columns, with equal values.
  static func == (a: KVValue, b: KVValue) -> Bool {
    switch (a, b) {
    case (.json(let x), .json(let y)): x == y
    case (.row(let x), .row(let y)): x == y
    default: a.json == b.json
    }
  }
}

/// A sorted key-value map, ordered by UTF-8 bytes like Replicache's B-tree.
/// Copies share nodes, so a copy is O(1) and writes copy only what they touch.
struct KVMap {
  struct Entry {
    let key: String
    var value: KVValue
  }

  private(set) var tree = BTreeSet<Entry> { StringCompare.utf8($0.key, $1.key) }

  init() {}

  var count: Int { tree.count }

  func get(_ key: String) -> KVValue? {
    tree.get(Entry(key: key, value: .json(.null)))?.value
  }

  func has(_ key: String) -> Bool { get(key) != nil }

  mutating func set(_ key: String, _ value: KVValue) {
    tree.add(Entry(key: key, value: value))
  }

  @discardableResult
  mutating func delete(_ key: String) -> Bool {
    tree.delete(Entry(key: key, value: .json(.null)))
  }

  mutating func removeAll() { tree = BTreeSet { StringCompare.utf8($0.key, $1.key) } }

  /// The entries whose key starts with `prefix`, in key order.
  func scan(prefix: String) -> [Entry] {
    var out: [Entry] = []
    let it = tree.values { StringCompare.utf8($0.key, prefix) }
    for e in it {
      guard e.key.utf8.starts(with: prefix.utf8) else { break }
      out.append(e)
    }
    return out
  }

  var entries: BTreeSet<Entry>.ForwardIterator { tree.values() }
}

/// One change between two maps (`DiffOperation` in
/// `packages/replicache/src/btree/node.ts`).
enum KVDiffOp {
  case add(key: String, newValue: KVValue)
  case del(key: String, oldValue: KVValue)
  case change(key: String, oldValue: KVValue, newValue: KVValue)

  var key: String {
    switch self {
    case .add(let k, _), .del(let k, _), .change(let k, _, _): k
    }
  }
}

typealias KVDiff = [KVDiffOp]

extension KVDiff {
  /// The diff from `old` to `new` over `keys`, which must include every key
  /// whose value differs. Ordered by key, as a B-tree diff is.
  static func between(_ old: KVMap, _ new: KVMap, keys: Set<JSKey>) -> KVDiff {
    // Sorting the keys and looking each up in both maps costs about as much
    // per key as a step of the walk of both maps does per 32 entries.
    if keys.count * 32 >= old.count + new.count { return between(old, new) }
    var diff: KVDiff = []
    for key in keys.map(\.string).sorted(by: StringCompare.utf8Less) {
      switch (old.get(key), new.get(key)) {
      case (nil, nil): break
      case (nil, let v?): diff.append(.add(key: key, newValue: v))
      case (let v?, nil): diff.append(.del(key: key, oldValue: v))
      case (let a?, let b?): if a != b { diff.append(.change(key: key, oldValue: a, newValue: b)) }
      }
    }
    return diff
  }

  /// The diff from `old` to `new` over every key of both: one walk of the
  /// two maps in key order.
  static func between(_ old: KVMap, _ new: KVMap) -> KVDiff {
    var diff: KVDiff = []
    var olds = old.entries
    var news = new.entries
    var a = olds.next()
    var b = news.next()
    while true {
      switch (a, b) {
      case (nil, nil):
        return diff
      case (let x?, nil):
        diff.append(.del(key: x.key, oldValue: x.value))
        a = olds.next()
      case (nil, let y?):
        diff.append(.add(key: y.key, newValue: y.value))
        b = news.next()
      case (let x?, let y?):
        let order = StringCompare.utf8(x.key, y.key)
        if order < 0 {
          diff.append(.del(key: x.key, oldValue: x.value))
          a = olds.next()
        } else if order > 0 {
          diff.append(.add(key: y.key, newValue: y.value))
          b = news.next()
        } else {
          if x.value != y.value {
            diff.append(.change(key: x.key, oldValue: x.value, newValue: y.value))
          }
          a = olds.next()
          b = news.next()
        }
      }
    }
  }

  /// An add of each entry: the diff from an empty map.
  init(adding entries: some Sequence<KVMap.Entry>) {
    self = entries.map { .add(key: $0.key, newValue: $0.value) }
  }

  /// The ops whose key starts with `prefix`.
  func withPrefix(_ prefix: String) -> KVDiff {
    filter { $0.key.utf8.starts(with: prefix.utf8) }
  }
}

/// A Replicache write transaction over a copy of a map. Records the keys it
/// writes so a later diff only has to look at those.
@MainActor
final class KVWrite {
  private(set) var map: KVMap
  private(set) var writes = Set<JSKey>()
  /// Set when the whole map was cleared.
  private(set) var cleared = false

  init(_ map: KVMap) { self.map = map }

  func has(_ key: String) -> Bool { map.has(key) }
  func get(_ key: String) -> KVValue? { map.get(key) }

  func set(_ key: String, _ value: KVValue) {
    map.set(key, value)
    writes.insert(JSKey(key))
  }

  @discardableResult
  func delete(_ key: String) -> Bool {
    writes.insert(JSKey(key))
    return map.delete(key)
  }

  func clear(keysOf old: KVMap) {
    for e in old.entries { writes.insert(JSKey(e.key)) }
    map.removeAll()
    cleared = true
  }
}

/// A patch operation of a pull response (`PatchOperationInternal`).
enum KVPatchOp {
  case put(key: String, value: JSONValue)
  case update(key: String, merge: RowRecord?, constrain: [String]?)
  case del(key: String)
  case clear

  var key: String? {
    switch self {
    case .put(let k, _), .update(let k, _, _), .del(let k): k
    case .clear: nil
    }
  }
}

/// The local state Replicache keeps for Zero, without its storage machinery:
/// the last server snapshot, and the pending local mutations whose effects
/// are applied on top of it in `main`.
@MainActor
final class ReplicaStore {
  struct Snapshot {
    var map = KVMap()
    /// The server's cookie for the snapshot, `null` before the first poke.
    var cookie: JSONValue = .null
    var lastMutationIDs: [JSKey: Double] = [:]
  }

  struct PendingMutation {
    let id: Double
    let clientID: String
    let name: String
    let args: JSONValue
    /// `performance.now()` when the mutation was made.
    let timestamp: Double
    /// The keys its last run wrote.
    var writes: Set<JSKey>
  }

  let clientID: String
  private(set) var snapshot = Snapshot()
  private(set) var main = KVMap()
  private(set) var pending: [PendingMutation] = []

  init(clientID: String, snapshot: Snapshot = Snapshot(), pending: [PendingMutation] = []) {
    self.clientID = clientID
    self.snapshot = snapshot
    self.main = snapshot.map
    self.pending = pending
  }

  var cookie: JSONValue { snapshot.cookie }

  /// The ID the next mutation of `clientID` gets.
  func nextMutationID(for clientID: String) -> Double {
    if let last = pending.last(where: { $0.clientID == clientID }) { return last.id + 1 }
    return (snapshot.lastMutationIDs[JSKey(clientID)] ?? 0) + 1
  }

  /// Commits a mutation that ran in `write` over `main`.
  func commitMutation(_ mutation: PendingMutation, _ write: KVWrite) -> KVDiff {
    let diff = KVDiff.between(main, write.map, keys: write.writes)
    pending.append(mutation)
    main = write.map
    return diff
  }

  /// The keys whose value in `main` may differ from the snapshot's.
  var pendingWrites: Set<JSKey> {
    pending.reduce(into: Set<JSKey>()) { $0.formUnion($1.writes) }
  }

  /// Replaces the snapshot and the pending mutations after a poke. Returns
  /// the diff of `main`.
  func advance(
    to snapshot: Snapshot, main newMain: KVMap, pending newPending: [PendingMutation],
    touched: Set<JSKey>?
  ) -> KVDiff {
    let diff =
      touched.map { KVDiff.between(main, newMain, keys: $0) } ?? KVDiff.between(main, newMain)
    self.snapshot = snapshot
    self.pending = newPending
    self.main = newMain
    return diff
  }

  /// Applies `patch` to `write` (`apply` in
  /// `packages/replicache/src/sync/patch.ts`). The rows of the tables in
  /// `layouts` are kept in their layouts.
  static func apply(
    _ patch: [KVPatchOp], to write: KVWrite, layouts: [String: RowLayout]
  ) throws(PatchError) {
    for op in optimize(patch) {
      switch op {
      case .clear:
        write.clear(keysOf: write.map)
      case .put(let key, let value):
        write.set(key, KVValue(value, at: key, layouts: layouts))
      case .update(let key, let merge, let constrain):
        var entries: [(String, JSONValue)] = []
        switch write.get(key) {
        case nil: break
        case .row(let row): entries += row.map { ($0.key, $0.value) }
        case .json(.object(let o)): entries += o.map { ($0.key, $0.value) }
        case .json: throw PatchError("Expected object for update of \(key)")
        }
        if let merge { entries += merge.map { ($0.key, $0.value) } }
        var next: RowRecord = [:]
        for (k, v) in entries
        where constrain == nil || constrain!.isEmpty
          || constrain!.contains(where: { StringCompare.equal($0, k) })
        {
          next[k] = v
        }
        write.set(key, KVValue(.object(next), at: key, layouts: layouts))
      case .del(let key):
        if write.has(key) { write.delete(key) }
      }
    }
  }

  struct PatchError: Error, CustomStringConvertible {
    let description: String
    init(_ d: String) { description = d }
  }

  /// `optimizePatch`: drops what the last `clear` and later puts and dels of
  /// the same key make irrelevant. A patch with an `update` is applied as
  /// sent, from its last `clear`.
  ///
  /// JS also sorts the ops by key, for its B-tree's `put`s. That leaves one
  /// op per key, so their order doesn't change the map, and the diff that
  /// follows is in key order whatever it is.
  static func optimize(_ patch: [KVPatchOp]) -> [KVPatchOp] {
    guard !patch.isEmpty else { return [] }
    var start = 0
    var result: [KVPatchOp] = []
    var clearIndex = -1
    for i in stride(from: patch.count - 1, through: 0, by: -1) {
      if case .clear = patch[i] {
        result.append(.clear)
        clearIndex = i
        start = i + 1
        break
      }
    }
    var order: [JSKey] = []
    var ops: [JSKey: KVPatchOp] = [:]
    for op in patch[start...] {
      switch op {
      case .put(let key, _), .del(let key):
        let k = JSKey(key)
        if ops[k] == nil { order.append(k) }
        ops[k] = op
      case .update:
        return clearIndex == -1 ? patch : Array(patch[clearIndex...])
      case .clear:
        break
      }
    }
    for k in order {
      let op = ops[k]!
      if clearIndex != -1, case .del = op { continue }
      result.append(op)
    }
    return result
  }
}

/// `compareCookies` (`packages/replicache/src/cookies.ts`) for the cookies
/// Zero uses: null, strings, numbers, and objects with an `order`.
func compareCookies(_ a: JSONValue, _ b: JSONValue) -> Double {
  if a == b { return 0 }
  if a.isNull { return -1 }
  if b.isNull { return 1 }
  func value(_ c: JSONValue) -> JSONValue {
    if case .object(let o) = c { return o["order"] ?? .null }
    return c
  }
  let va = value(a)
  let vb = value(b)
  if case .number(let x) = va, case .number(let y) = vb { return x - y }
  func string(_ v: JSONValue) -> String {
    switch v {
    case .string(let s): s
    case .number(let n): JSNumber.format(n)
    default: JSONSerializer.stringify(v)
    }
  }
  return Double(StringCompare.utf16(string(va), string(vb)))
}
