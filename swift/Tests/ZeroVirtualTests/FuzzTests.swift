import Foundation
import Testing

@testable import ZeroVirtual

/// Random lists, each driven through 60 random steps on a `LayoutScrollView`:
/// scrolls, jumps, inserts, deletes, resizes, reflows, and viewport changes.
/// After each step settles, the list must hold its invariants (see `check`).
///
/// Every PR runs `ZERO_FUZZ_SEEDS` seeds (200 by default) from
/// `ZERO_FUZZ_FIRST_SEED` (0); the nightly workflow runs thousands of new
/// ones. A failure names its seed. To replay one with a trace of the passes
/// from a step on:
///
///     ZERO_FUZZ_SEED=<seed> ZERO_FUZZ_TRACE=<step> swift test --filter FuzzTests
@MainActor
@Suite("Fuzz") struct FuzzTests {
  /// Seeds that once failed, kept after the fix. 760: rows on screen moved
  /// when rows inserted above pushed them out of the page (#94).
  @Test("seeds that once failed", arguments: [760 as UInt64])
  func knownSeed(_ seed: UInt64) {
    if let failure = Fuzzer.failure(seed: seed) { Issue.record("seed \(seed): \(failure)") }
  }

  @Test("random seeds") func randomSeeds() {
    let env = ProcessInfo.processInfo.environment
    if let only = env["ZERO_FUZZ_SEED"].flatMap(UInt64.init) {
      let trace = env["ZERO_FUZZ_TRACE"].flatMap(Int.init)
      if let failure = Fuzzer.failure(seed: only, traceFrom: trace) {
        Issue.record("seed \(only): \(failure)")
      }
      return
    }
    let first = env["ZERO_FUZZ_FIRST_SEED"].flatMap(UInt64.init) ?? 0
    let count = env["ZERO_FUZZ_SEEDS"].flatMap(UInt64.init) ?? 200
    var failed = 0
    for seed in first..<first + count {
      guard let failure = Fuzzer.failure(seed: seed) else { continue }
      failed += 1
      Issue.record("seed \(seed): \(failure)")
      // A bug usually fails many seeds; the first few are enough.
      if failed == 5 { return }
    }
  }
}

/// SplitMix64: the same numbers for a seed on every platform.
struct SplitMix64: RandomNumberGenerator {
  var state: UInt64
  mutating func next() -> UInt64 {
    state &+= 0x9E37_79B9_7F4A_7C15
    var z = state
    z = (z ^ (z >> 30)) &* 0xBF58_476D_1CE4_E5B9
    z = (z ^ (z >> 27)) &* 0x94D0_49BB_1331_11EB
    return z ^ (z >> 31)
  }
}

/// One seed's list and steps.
@MainActor
struct Fuzzer {
  let seed: UInt64
  var traceFrom: Int?
  var rng: SplitMix64
  let h: Harness
  let view: LayoutScrollView

  /// Ids are numbers, a thousand apart to begin with, so rows can be
  /// inserted between any two.
  static func id(_ n: Int) -> String { String(format: "r%09d", n) }

  init(seed: UInt64, traceFrom: Int? = nil) {
    self.seed = seed
    self.traceFrom = traceFrom
    rng = SplitMix64(state: seed)
    let rows = [5, 30, 300][Int(seed % 3)]
    let stick = seed % 4 == 2
    h = Harness(rowCount: 0) {
      $0.anchoring = .manual
      $0.minPageSize = 40
      $0.stickToBottom = stick
      if seed % 5 == 1 { $0.permalinkID = Fuzzer.id(rows / 2 * 1000) }
    }
    h.data = (0..<rows).map { TestRow(id: Self.id($0 * 1000)) }
    h.startsByValue = true
    view = LayoutScrollView(h)
    for r in h.data where Bool.random(using: &rng) {
      view.heights[r.id] = Double(Int.random(in: 8...90, using: &rng))
    }
  }

  /// The seed's first problem, with the steps before it; nil when every
  /// step holds.
  static func failure(seed: UInt64, traceFrom: Int? = nil) -> String? {
    var fuzzer = Fuzzer(seed: seed, traceFrom: traceFrom)
    return fuzzer.run()
  }

  mutating func run() -> String? {
    _ = rest()
    var steps: [String] = []
    for step in 0..<60 {
      let (described, holds) = takeStep()
      guard let described else { continue }
      steps.append(described)
      if let traceFrom, step >= traceFrom { trace(step, described) }
      let problems = check(holds: holds, settled: rest())
      if !problems.isEmpty {
        return "step \(step): \(problems.joined(separator: "; "))"
          + " (after \(steps.suffix(5).joined(separator: " | ")))"
      }
    }
    return nil
  }

  /// One random step; the rows on screen that must hold still through it,
  /// with their tops from the viewport's top. nil for a step that can't be
  /// taken, such as a delete from an empty list.
  mutating func takeStep() -> (String?, [(key: String, top: Double)]) {
    let n = h.data.count
    let top = view.scrollOffset
    let onScreen = view.host.rowFrames().filter {
      $0.maxY > top + 0.5 && $0.minY < top + view.viewportHeight
    }
    let first = onScreen.first.flatMap { f in h.data.firstIndex { AnyHashable($0.id) == f.key } }
    // A change that ends at or above the first row on screen mustn't move
    // the rows on screen, unless the list is at its top.
    func holding(above end: Int) -> [(key: String, top: Double)] {
      guard let first, end <= first, top > 0 else { return [] }
      return onScreen.map { ($0.key.base as! String, $0.minY - top) }
    }
    switch Int.random(in: 0..<100, using: &rng) {
    case 0..<30:
      let distance = Double(Int.random(in: -2500...2500, using: &rng))
      let parts = Int.random(in: 1...6, using: &rng)
      for _ in 0..<parts { view.userScroll(view.scrollOffset + distance / Double(parts)) }
      return ("scroll \(distance) in \(parts)", [])
    case 30..<36:
      let toTop = Bool.random(using: &rng)
      view.userScroll(toTop ? 0 : .infinity)
      return ("scroll to the \(toTop ? "top" : "bottom")", [])
    case 36..<50:
      let at = Int.random(in: 0...n, using: &rng)
      let lo = at == 0 ? -1 : Int(h.data[at - 1].id.dropFirst())!
      let hi = at == n ? lo + 100_000 : Int(h.data[at].id.dropFirst())!
      let k = Int.random(in: 1...5, using: &rng)
      // Spread between the neighbours' ids; a narrow gap gives fewer, and
      // ids are unique, as primary keys are.
      let ids = Set((1...k).map { lo + ($0 * (hi - lo)) / (k + 1) }.filter { $0 > lo && $0 < hi })
      let holds = holding(above: at)
      h.data.insert(contentsOf: ids.sorted().map { TestRow(id: Self.id($0)) }, at: at)
      return ("insert \(ids.count) at \(at)", holds)
    case 50..<66:
      guard n > 0 else { return (nil, []) }
      let at = Int.random(in: 0..<n, using: &rng)
      let k = min(n - at, Int.random(in: 1...8, using: &rng))
      let holds = holding(above: at + k)
      h.data.removeSubrange(at..<at + k)
      return ("delete \(k) at \(at)", holds)
    case 66..<78:
      guard n > 0 else { return (nil, []) }
      let at = Int.random(in: 0..<n, using: &rng)
      let k = min(n - at, Int.random(in: 1...4, using: &rng))
      let holds = holding(above: at + k)
      for r in h.data[at..<at + k] {
        view.heights[r.id] = Double(Int.random(in: 5...120, using: &rng))
      }
      return ("resize \(k) at \(at)", holds)
    case 78..<84:
      view.viewportHeight = Double(Int.random(in: 150...700, using: &rng))
      return ("viewport \(view.viewportHeight)", [])
    case 84..<92:
      guard n > 0 else { return (nil, []) }
      let id = h.data[Int.random(in: 0..<n, using: &rng)].id
      let align = [VirtualScrollAlignment.auto, .start, .center, .end]
        .randomElement(using: &rng)!
      h.core.scrollToItem(id, align: align)
      return ("jump to \(id) \(align)", [])
    default:
      // The width changed: every row's height scales, and is measured again.
      let scale = [0.5, 0.8, 1.25, 2.0].randomElement(using: &rng)!
      for r in h.data {
        view.heights[r.id] = min(200, max(10, ((view.heights[r.id] ?? 20) * scale).rounded()))
      }
      view.host.invalidateHeights()
      return ("reflow \(scale)", [])
    }
  }

  /// Passes, with frames drawn between them, until nothing changes and the
  /// snapshot is complete; whether that happened within 40 passes.
  func settle() -> Bool {
    for _ in 0..<40 {
      let before = (h.queryKey(h.core.queryInputs), view.scrollOffset, h.core.snapshot.spaceBefore)
      view.pass()
      if !view.frameDrawn(),
        (h.queryKey(h.core.queryInputs), view.scrollOffset, h.core.snapshot.spaceBefore)
          == before,
        h.core.snapshot.complete
      {
        return true
      }
    }
    return false
  }

  /// Settles, then again after the core's short and long timers fire.
  func rest() -> Bool {
    var settled = settle()
    h.scheduler.advance(by: 200)
    settled = settle() && settled
    h.scheduler.advance(by: 2500)
    return settle() && settled
  }

  func check(holds: [(key: String, top: Double)], settled: Bool) -> [String] {
    var problems: [String] = []
    if !settled { problems.append("didn't settle") }
    let end = max(0, view.host.contentHeight - view.viewportHeight)
    let offset = view.scrollOffset
    if offset > end + 0.5 { problems.append("offset \(offset) past the end, \(end)") }
    guard !h.data.isEmpty else { return problems }
    let frames = view.host.rowFrames()
    if !frames.contains(where: { $0.maxY > offset && $0.minY < offset + view.viewportHeight }) {
      problems.append("no row on screen at \(offset)")
    }
    let s = h.core.snapshot
    if let f = s.items.first, let i = h.data.firstIndex(where: { $0.id == f.key }) {
      if h.data[i..<min(h.data.count, i + s.items.count)].map(\.id) != s.items.map(\.key) {
        problems.append("the loaded rows aren't a run of the list")
      }
      if i == 0, s.spaceBefore > 0.5 {
        problems.append("\(s.spaceBefore) of space above the first row")
      }
    }
    // At the end of the content the offset is clamped, so rows can move.
    if offset < end - 0.5 {
      for (key, top) in holds {
        if let now = view.rowTop(key), abs(now - top) > 0.5 {
          problems.append("\(key) moved from \(top) to \(now)")
          break
        }
      }
    }
    return problems
  }

  func trace(_ step: Int, _ described: String) {
    for pass in 0..<12 {
      view.pass()
      let frame = view.frameDrawn()
      let s = h.core.snapshot
      let frames = view.host.rowFrames()
      print(
        """
        TRACE seed \(seed) step \(step) (\(described)) pass \(pass): \
        offset \(view.scrollOffset), viewport \(view.viewportHeight), \
        content \(view.host.contentHeight), space \(s.spaceBefore)/\(s.spaceAfter), \
        rows \(s.items.first.map { "\($0.key)@\($0.index)" } ?? "-")…\(s.items.last?.key ?? "-"), \
        drawn \(frames.first?.minY ?? .nan)…\(frames.last?.maxY ?? .nan), \
        anchor \(h.core.queryInputs.anchor), frame \(frame)
        """)
    }
  }
}
