import XCTest

/// Every display frame of the list's transitions, as `FrameProbe` checks
/// them in the app: no frame may show the rows anywhere but where the
/// scroll put them. The app drives the transitions itself
/// (`-probe-script`); the probe's summary is the `probe` element's value.
@MainActor
class FrameProbeUITests: XCTestCase {
  var app: XCUIApplication!
  /// The screens' lists as UIKit or AppKit views (`VirtualListView`).
  var native: Bool { false }

  override func setUp() async throws {
    continueAfterFailure = true
  }

  /// The scroll bars' style, pinned. Without a trackpad, as on CI's
  /// runners, AppKit switches a launched app's scroll views from overlay
  /// to legacy scroll bars a few frames in: the list narrows by the
  /// scroller's width and its rows wrap again, which moves them on screen
  /// in a frame the probe counts wrong, whenever the switch comes after
  /// the first frame drawn.
  static let scrollBars: [String] = {
    #if os(macOS)
      ["-AppleShowScrollBars", "Always"]
    #else
      []
    #endif
  }()

  func launch(_ screen: String, script: Bool) {
    app = XCUIApplication()
    app.launchArguments =
      ["-screen", screen] + (script ? ["-probe-script", screen] : []) + ["-probe", "1"]
      + ["-native", native ? "1" : "0"] + Self.scrollBars
    app.launch()
  }

  var summary: String { app.staticTexts["probe"].value as? String ?? "" }

  /// Wrong frames by phase, from `phases=[name:count,…]`.
  func phases(_ s: String) -> [String: Int] {
    guard let open = s.range(of: "phases=["), let close = s[open.upperBound...].firstIndex(of: "]")
    else { return [:] }
    var out: [String: Int] = [:]
    for part in s[open.upperBound..<close].split(separator: ",") {
      let kv = part.split(separator: ":")
      if kv.count == 2, let n = Int(kv[1]) { out[String(kv[0])] = n }
    }
    return out
  }

  func wrong(_ s: String) -> Int? {
    s.split(separator: " ").first { $0.hasPrefix("wrong=") }.flatMap { Int($0.dropFirst(6)) }
  }

  /// `flings=[2.7s/702.0pt,…]`: each fling's length, and whether it ended
  /// at an edge.
  func flings(_ s: String) -> [(seconds: Double, points: Double, edge: Bool)] {
    guard let open = s.range(of: "flings=["), let close = s[open.upperBound...].firstIndex(of: "]")
    else { return [] }
    return s[open.upperBound..<close].split(separator: ",").compactMap { f in
      let p = f.split(separator: "/")
      guard p.count >= 2, let t = Double(p[0].dropLast()), let d = Double(p[1].dropLast(2))
      else { return nil }
      return (t, d, p.count > 2)
    }
  }

  func runScript(_ screen: String, phases expected: [String]) {
    launch(screen, script: true)
    let end = Date().addingTimeInterval(900)
    while Date() < end, !summary.hasSuffix(" done") {
      RunLoop.current.run(until: Date().addingTimeInterval(2))
    }
    let s = summary
    XCTAssertTrue(s.hasSuffix(" done"), "the script didn't finish: \(s)")
    let byPhase = phases(s)
    for name in expected {
      XCTAssertEqual(byPhase[name], 0, "wrong frames in \(name): \(s)")
    }
    XCTAssertEqual(wrong(s), 0, s)
  }

  func testChatShowsNoWrongFrame() {
    runScript("chat", phases: ["open", "older", "latest", "far-up", "receive-latest"])
  }

  /// Relaunched, the chat opens where it was, from the rows the client
  /// kept: in its first frame, and with no frame wrong or blank as the
  /// server, a second away (so its rows can't come first), confirms them.
  /// Left at the bottom, it follows the messages that came while it was
  /// closed.
  func testChatReopensWhereItWas() {
    for (up, messages) in [(3000, 200), (0, 210)] {
      let key = UUID().uuidString
      for (script, args) in [
        ("park", ["-park-up", "\(up)"]), ("reopen", ["-messages", "\(messages)"]),
      ] {
        app = XCUIApplication()
        app.launchArguments =
          ["-screen", "chat", "-probe-script", script, "-probe", "1"]
          + ["-native", native ? "1" : "0", "-storage-key", key, "-latency", "1000"] + args
          + Self.scrollBars
        app.launch()
        let end = Date().addingTimeInterval(60)
        while Date() < end, !summary.hasSuffix(" done") {
          RunLoop.current.run(until: Date().addingTimeInterval(1))
        }
        let s = summary
        XCTAssertTrue(s.hasSuffix(" done"), "\(script) didn't finish: \(s)")
        XCTAssertEqual(wrong(s), 0, "\(script), \(up) up: \(s)")
        if script == "reopen" { XCTAssertTrue(s.contains(" blank=0 "), "\(up) up: \(s)") }
        // Its rows and where it was saved, before the next launch.
        RunLoop.current.run(until: Date().addingTimeInterval(2))
      }
    }
  }

  func testIssuesShowNoWrongFrame() {
    #if os(macOS)
      runScript("issues", phases: ["open", "header", "down", "up", "keys"])
    #else
      runScript("issues", phases: ["open", "header", "down", "up"])
    #endif
  }

  #if os(iOS)
    /// Real flings, by a finger: each runs to its natural end (about 2.7 s),
    /// pages loading under it, with no wrong frame.
    func flingThrough(_ screen: String, _ moves: [Bool]) {
      launch(screen, script: false)
      let list = app.scrollViews.firstMatch
      XCTAssertTrue(list.waitForExistence(timeout: 10))
      RunLoop.current.run(until: Date().addingTimeInterval(2))
      for up in moves {
        if up { list.swipeDown(velocity: .fast) } else { list.swipeUp(velocity: .fast) }
        RunLoop.current.run(until: Date().addingTimeInterval(3.5))
      }
      let s = summary
      XCTAssertEqual(wrong(s), 0, s)
      let all = flings(s)
      XCTAssertFalse(all.isEmpty, s)
      for f in all where !f.edge {
        XCTAssertGreaterThan(f.seconds, 2.2, "a fling cut short: \(s)")
      }
    }

    /// Up from the latest message, older pages of many heights loading.
    func testChatFlingsUpRunTheirLength() { flingThrough("chat", Array(repeating: true, count: 8)) }

    /// Flung back to the top from a few distances at a few speeds: at rest
    /// the header is at the top, the first issue right below it, with no
    /// gap above it.
    func testIssuesFlingsToTheTopLeaveNoGap() {
      launch("issues", script: false)
      let list = app.scrollViews.firstMatch
      XCTAssertTrue(list.waitForExistence(timeout: 10))
      // The list's start: the header at the top.
      let first = app.descendants(matching: .any)["header"].firstMatch
      RunLoop.current.run(until: Date().addingTimeInterval(2))
      for (away, speed) in [
        (2, 6000.0), (4, 800.0), (6, 3000.0), (10, 1500.0), (3, 3000.0), (8, 6000.0),
      ] {
        for _ in 0..<away {
          list.swipeUp(velocity: .fast)
          RunLoop.current.run(until: Date().addingTimeInterval(1))
        }
        RunLoop.current.run(until: Date().addingTimeInterval(2))
        for _ in 0..<20 {
          list.swipeDown(velocity: XCUIGestureVelocity(rawValue: speed))
          RunLoop.current.run(until: Date().addingTimeInterval(3.5))
          if first.exists, abs(first.frame.minY - list.frame.minY) <= 1 { break }
        }
      }
      let s = summary
      XCTAssertEqual(wrong(s), 0, s)
    }

    /// Down a few hundred rows, then back up as earlier pages load.
    func testIssuesFlingsRunTheirLength() {
      flingThrough("issues", Array(repeating: false, count: 10) + Array(repeating: true, count: 8))
    }
  #endif
}

/// The same, with the screens' lists as UIKit or AppKit views.
final class NativeFrameProbeUITests: FrameProbeUITests {
  override var native: Bool { true }
}
