import XCTest

/// ZeroVirtual's SwiftUI list (`VirtualList`) in the test app: rows checked
/// by where they are on screen.
@MainActor
class VirtualListUITests: XCTestCase {
  var app: XCUIApplication!
  /// The screens' lists as UIKit or AppKit views (`VirtualListView`).
  var native: Bool { false }

  override func setUp() async throws {
    continueAfterFailure = false
  }

  func launch(_ screen: String) {
    app = XCUIApplication()
    app.launchArguments = ["-screen", screen, "-native", native ? "1" : "0"]
    app.launch()
  }

  var list: XCUIElement { app.scrollViews.firstMatch }

  func row(_ id: String) -> XCUIElement { app.descendants(matching: .any)["row-\(id)"].firstMatch }

  /// The rows on screen, top to bottom.
  func visibleRows() -> [(id: String, frame: CGRect)] {
    // One snapshot of the list, not a query per row: each element's
    // identifier and frame is a round trip to the device.
    guard let snapshot = try? list.snapshot() else { return [] }
    let bounds = snapshot.frame
    var rows: [(id: String, frame: CGRect)] = []
    func walk(_ e: any XCUIElementSnapshot) {
      if e.identifier.hasPrefix("row-") {
        rows.append((String(e.identifier.dropFirst(4)), e.frame))
      }
      e.children.forEach(walk)
    }
    walk(snapshot)
    return
      rows
      .filter { $0.frame.maxY > bounds.minY + 1 && $0.frame.minY < bounds.maxY - 1 }
      .sorted { $0.frame.minY < $1.frame.minY }
  }

  func waitUntil(
    _ what: String, timeout: TimeInterval = 10, file: StaticString = #filePath, line: UInt = #line,
    _ done: () -> Bool
  ) {
    let end = Date().addingTimeInterval(timeout)
    while Date() < end {
      if done() { return }
      RunLoop.current.run(until: Date().addingTimeInterval(0.1))
    }
    XCTFail("timed out waiting until \(what)", file: file, line: line)
  }

  /// Lets a fling come to rest and the list page in what it needs.
  func rest(_ seconds: TimeInterval = 1.5) {
    RunLoop.current.run(until: Date().addingTimeInterval(seconds))
  }

  /// Scrolls toward the top of the list.
  func scrollTowardTop() {
    #if os(macOS)
      list.scroll(byDeltaX: 0, deltaY: 600)
    #else
      list.swipeDown()
    #endif
  }

  /// Scrolls toward the bottom of the list.
  func scrollTowardBottom() {
    #if os(macOS)
      list.scroll(byDeltaX: 0, deltaY: -600)
    #else
      list.swipeUp()
    #endif
  }

  /// The row's bottom is at the list's.
  func atBottom(_ id: String) -> Bool {
    let r = row(id)
    return r.exists && abs(r.frame.maxY - list.frame.maxY) <= 1.5
  }

  /// The top row on screen holds still once the list is at rest.
  func assertHoldsStill(file: StaticString = #filePath, line: UInt = #line) {
    guard let top = visibleRows().first else {
      return XCTFail("no rows on screen", file: file, line: line)
    }
    rest(2.5)
    let now = row(top.id).frame
    XCTAssertEqual(
      now.minY, top.frame.minY, accuracy: 1, "row \(top.id) moved", file: file, line: line)
  }

  // MARK: Chat

  func testChatOpensAtTheLatestMessage() {
    launch("chat")
    waitUntil("the latest message is at the bottom") { atBottom("m0199") }
    XCTAssertGreaterThan(visibleRows().count, 1)
  }

  func testChatFollowsNewMessagesAndALastMessageGrowingTaller() {
    launch("chat")
    waitUntil("the latest message is at the bottom") { atBottom("m0199") }
    app.buttons["receive"].tap()
    waitUntil("a new message is at the bottom") { atBottom("m0200") }
    let height = row("m0200").frame.height
    app.buttons["grow"].tap()
    waitUntil("the grown message is taller, at the bottom") {
      row("m0200").frame.height > height + 20 && atBottom("m0200")
    }
  }

  func testChatScrolledUpStaysAndLatestFollowsAgain() {
    launch("chat")
    waitUntil("the latest message is at the bottom") { atBottom("m0199") }
    scrollTowardTop()
    rest()
    guard let top = visibleRows().first else { return XCTFail("no rows on screen") }
    app.buttons["receive"].tap()
    rest()
    XCTAssertFalse(visibleRows().contains { $0.id == "m0200" }, "followed while scrolled up")
    XCTAssertEqual(row(top.id).frame.minY, top.frame.minY, accuracy: 1)

    app.buttons["latest"].tap()
    waitUntil("the latest message is at the bottom") { atBottom("m0200") }
    app.buttons["receive"].tap()
    waitUntil("a new message is at the bottom again") { atBottom("m0201") }
  }

  func testChatHoldsStillAsOlderMessagesOfManyHeightsLoad() {
    launch("chat")
    waitUntil("the latest message is at the bottom") { atBottom("m0199") }
    var reachedStart = false
    // About 4 messages a swipe, 200 messages: stops once m0000 is in place.
    for _ in 0..<70 where !reachedStart {
      scrollTowardTop()
      rest()
      assertHoldsStill()
      reachedStart = row("m0000").exists && row("m0000").frame.minY >= list.frame.minY - 1
    }
    XCTAssertTrue(reachedStart, "never reached the first message")
    XCTAssertEqual(row("m0000").frame.minY, list.frame.minY, accuracy: 1.5)
  }

  // MARK: A long list

  func testIssuesPageDownAndBackUp() {
    launch("issues")
    waitUntil("the first issue is at the top") {
      row("i0000").exists && abs(row("i0000").frame.minY - list.frame.minY) <= 1.5
    }
    // A swipe moves about 14 issue rows; stop at row 400, not a count.
    var deepest = 0
    for _ in 0..<60 where deepest < 400 {
      scrollTowardBottom()
      rest(1)
      deepest = visibleRows().compactMap { Int($0.id.dropFirst()) }.max() ?? deepest
    }
    XCTAssertGreaterThanOrEqual(deepest, 400, "didn't page far enough down")
    assertHoldsStill()

    var top = false
    for _ in 0..<60 where !top {
      scrollTowardTop()
      rest(1)
      top = row("i0000").exists && abs(row("i0000").frame.minY - list.frame.minY) <= 1.5
    }
    XCTAssertTrue(top, "never got back to the first issue")
  }
}

/// The same, with the screens' lists as UIKit or AppKit views.
final class NativeVirtualListUITests: VirtualListUITests {
  override var native: Bool { true }
}
