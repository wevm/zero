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
    await sleep(1.5)

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
      await sleep(1.5)
    }
    probe.finish()
  }

  static func issues() async {
    probe.setPhase("open")
    await until(10) { probe.visible.contains("i0000") }
    await sleep(1)

    probe.setPhase("down")
    for _ in 0..<60 where deepest() < 400 {
      await probe.drive(velocity: 2000)
      await sleep(0.5)
    }
    probe.setPhase("up")
    for _ in 0..<80 where !atTop("i0000") {
      await probe.drive(velocity: -2000)
      await sleep(0.5)
    }
    await sleep(1)
    probe.finish()
  }

  static func atTop(_ id: String) -> Bool {
    probe.top(of: id).map { abs($0) <= 0.5 } ?? false
  }

  static func deepest() -> Int {
    probe.visible.compactMap { Int($0.dropFirst()) }.max() ?? 0
  }

  static func until(_ seconds: Double, _ done: () -> Bool) async {
    let end = Date().addingTimeInterval(seconds)
    while Date() < end, !done() { await sleep(0.05) }
  }

  static func sleep(_ seconds: Double) async {
    try? await Task.sleep(for: .seconds(seconds))
  }
}
