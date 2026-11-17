import Foundation

/// The transitions `FrameProbe` checks, driven by the app itself
/// (`-probe -probe-script chat|issues`), so they run the same with or
/// without UI tests. Each is a phase of the probe's summary.
@MainActor
enum ProbeScript {
  static var probe: FrameProbe { .shared }

  static func chat(
    latestID: () -> String?, toLatest: () -> Void, receive: () -> Void
  ) async {
    probe.setPhase("open")
    await until(10) { latestID().map { probe.visible.last == $0 } ?? false }
    await sleep(1)

    // Older pages of many heights load under each fling; at each rest the
    // space before the rows is given back; at the top, it's taken away.
    probe.setPhase("older")
    for _ in 0..<80 where !atTop("m0000") {
      await probe.drive(velocity: -2000)
      await sleep(0.6)
    }
    await sleep(1)

    probe.setPhase("latest")
    toLatest()
    await landed()

    // Far up, then Latest: with a message received while away, and without.
    for round in 0..<6 {
      let withReceive = round % 2 == 1
      probe.setPhase("far-up")
      for _ in 0..<10 {
        await probe.drive(velocity: -2500)
        await sleep(0.4)
      }
      await sleep(1)
      probe.setPhase(withReceive ? "receive-latest" : "latest")
      if withReceive {
        receive()
        await sleep(1)
      }
      toLatest()
      await landed()
    }
    probe.finish()
  }

  static func issues(growHeader: () -> Void) async {
    probe.setPhase("open")
    await until(10) { probe.visible.contains("i0000") }
    await sleep(1)

    // The header changes height: on screen, at the top, the rows go down
    // and up with it; above the screen, nothing on screen moves.
    probe.setPhase("header")
    for _ in 0..<5 {
      growHeader()
      await sleep(0.6)
    }
    await probe.drive(velocity: 1500)
    await sleep(0.6)
    for _ in 0..<5 {
      growHeader()
      await sleep(0.6)
    }

    probe.setPhase("down")
    for _ in 0..<60 where deepest() < 400 {
      await probe.drive(velocity: 2000)
      await sleep(0.5)
    }
    probe.setPhase("up")
    for _ in 0..<80 where !atStart() {
      await probe.drive(velocity: -2000)
      await sleep(0.5)
    }
    await sleep(1)

    #if os(macOS)
      // The selection moves down by the arrow key past the screen's
      // bottom, the list scrolling a row at a time, then back up.
      probe.setPhase("keys")
      for down in Array(repeating: true, count: 30) + Array(repeating: false, count: 12) {
        probe.beginJump()
        probe.pressArrow(down: down)
        await sleep(0.15)
      }
      await sleep(1)
    #endif
    probe.finish()
  }

  /// Pushes the list as hard as it goes, counting blank frames (`blank=` in
  /// the summary, by phase): real-time flings, harder each time, through
  /// pages loading; long fast drags; jumps to rows far away (`targets`, by
  /// `jump`), which also show the scroll bar the list's whole length; then
  /// its thumb dragged far, and jumped. With `arrive`, a row arrives every
  /// 0.3 s while it flings and drags, as a chat's messages do.
  static func blanking(
    opened: String, up: Bool = false, targets: [String], jump: (String) -> Void,
    arrive: (() -> Void)? = nil
  ) async {
    probe.setPhase("open")
    await until(10) { probe.visible.contains(opened) }
    await sleep(1)
    let arriving = arrive.map { arrive in
      Task {
        while !Task.isCancelled {
          await sleep(0.3)
          arrive()
        }
      }
    }
    // Opened at the end, the first flings go up.
    let sign = up ? -1.0 : 1
    for v in [4000.0, 8000, 16000, 32000].map({ $0 * sign }) where runs("fling") {
      probe.setPhase("fling-\(Int(abs(v) / 1000))k")
      for _ in 0..<3 {
        await probe.fling(velocity: v)
        await sleep(0.2)
      }
      for _ in 0..<3 {
        await probe.fling(velocity: -v)
        await sleep(0.2)
      }
    }
    if runs("drag") { probe.setPhase("drag") }
    for v in [6000.0, 12000].map({ $0 * sign }) where runs("drag") {
      await probe.drag(velocity: v, seconds: 2)
      await probe.drag(velocity: -v, seconds: 2)
    }
    arriving?.cancel()
    await sleep(0.5)
    if runs("jump") { probe.setPhase("jump") }
    for id in targets where runs("jump") {
      probe.beginJump()
      jump(id)
      await landed()
    }
    if runs("thumb") { probe.setPhase("thumb") }
    for f in [0.3, 0.8, 0.1, 1, 0.5, 0] where runs("thumb") {
      await probe.thumb(to: f, frames: 8)
      await sleep(0.8)
    }
    if runs("thumb-jump") { probe.setPhase("thumb-jump") }
    for f in [0.6, 0.2, 0.9, 0] where runs("thumb-jump") {
      probe.beginJump()
      await probe.thumb(to: f, frames: 1)
      await landed()
    }
    probe.finish()
  }

  /// A short run of `blanking`'s hardest moves, for a recording: flings at
  /// 16,000 and 32,000 points a second, the scroll bar's thumb dragged, and
  /// a jump away and back.
  static func video(opened: String, up: Bool, far: String, jump: (String) -> Void) async {
    probe.setPhase("open")
    await until(10) { probe.visible.contains(opened) }
    await sleep(1.5)
    let sign = up ? -1.0 : 1
    probe.setPhase("fling")
    for v in [16000.0, 32000] {
      for _ in 0..<2 {
        await probe.fling(velocity: v * sign)
        await sleep(0.3)
      }
      await probe.fling(velocity: -v * sign)
      await sleep(0.3)
    }
    probe.setPhase("thumb")
    for f in up ? [0.6, 0.2, 1] : [0.4, 0.8, 0] {
      await probe.thumb(to: f, frames: 10)
      await sleep(0.5)
    }
    probe.setPhase("jump")
    for id in [far, opened] {
      probe.beginJump()
      jump(id)
      await landed()
    }
    probe.finish()
  }

  /// Opens the chat at its latest message, scrolls up `-park-up` points
  /// from there (none: stays at the bottom), and stops, for `reopen` to
  /// open it again where it is (`-storage-key`).
  static func park(latestID: () -> String?) async {
    probe.setPhase("open")
    await until(10) { latestID().map { probe.visible.last == $0 } ?? false }
    await sleep(1)
    let up = UserDefaults.standard.double(forKey: "park-up")
    if up > 0 {
      probe.setPhase("park")
      await probe.drag(velocity: -2000, seconds: up / 2000)
      await sleep(1)
    }
    UserDefaults.standard.set(rowTops(), forKey: "probe-parked")
    UserDefaults.standard.set(up == 0, forKey: "probe-parked-bottom")
    log("PARKED")
    probe.finish()
  }

  /// The chat opened again where `park` left it, from the rows the client
  /// kept: in its first frame, and once the server has confirmed them;
  /// left at the bottom, at the latest message by then.
  static func reopen(latestID: () -> String?) async {
    probe.setPhase("open")
    await until(10) { !probe.visible.isEmpty }
    log("OPENED")
    let parked = UserDefaults.standard.stringArray(forKey: "probe-parked") ?? []
    if !same(rowTops(), parked) { probe.flag("opened at \(rowTops()), not \(parked)") }
    // The server's answer: two `-latency` legs away, and then some.
    await sleep(2 + 3 * UserDefaults.standard.double(forKey: "latency") / 1000)
    log("SETTLED")
    if UserDefaults.standard.bool(forKey: "probe-parked-bottom") {
      if probe.visible.last != latestID() {
        probe.flag("settled at \(rowTops()), not at \(latestID() ?? "-")")
      }
    } else if !same(rowTops(), parked) {
      probe.flag("settled at \(rowTops()), not \(parked)")
    }
    probe.finish()
  }

  /// The rows on screen and their tops, as `id@top`.
  private static func rowTops() -> [String] {
    probe.visible.map { id in "\(id)@\(probe.top(of: id).map { Int($0.rounded()) } ?? 0)" }
  }

  /// The same rows, each within a point of the same place.
  private static func same(_ a: [String], _ b: [String]) -> Bool {
    func split(_ s: String) -> (String, Int) {
      let p = s.split(separator: "@")
      return (String(p[0]), Int(p[1]) ?? 0)
    }
    return a.count == b.count
      && zip(a.map(split), b.map(split)).allSatisfy { $0.0 == $1.0 && abs($0.1 - $1.1) <= 1 }
  }

  private static func log(_ what: String) {
    print("PROBE \(what) \(rowTops().joined(separator: " "))")
  }

  /// Whether `blanking` runs its phase `name`: all of them, or those
  /// `-probe-phases` lists, such as `thumb,thumb-jump`.
  static func runs(_ name: String) -> Bool {
    guard let only = UserDefaults.standard.string(forKey: "probe-phases") else { return true }
    return only.split(separator: ",").contains { $0 == name }
  }

  /// The list's start is at its top: the header, or the first row.
  static func atStart() -> Bool {
    atTop("header") || atTop("i0000")
  }

  static func atTop(_ id: String) -> Bool {
    probe.top(of: id).map { abs($0) <= 0.5 } ?? false
  }

  static func deepest() -> Int {
    probe.visible.compactMap { Int($0.dropFirst()) }.max() ?? 0
  }

  /// A jump's window settles before the next phase drives the list: until
  /// it has, the jump owns the position, and a drive would be undone. A
  /// stalled frame (a slow simulator's) can take the whole wait.
  static func landed() async {
    await sleep(1.5)
    await until(10) { !probe.jumping }
  }

  static func until(_ seconds: Double, _ done: () -> Bool) async {
    let end = Date().addingTimeInterval(seconds)
    while Date() < end, !done() { await sleep(0.05) }
  }

  static func sleep(_ seconds: Double) async {
    try? await Task.sleep(for: .seconds(seconds))
  }
}
